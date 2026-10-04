//! M8.4.3 §11's other half: the four published tables re-derived from M8.4.2's raw rows.
//!
//! `state_ownership_evidence.rs` proves the committed directory is what the model and the
//! records assemble to right now. That is a reproducibility claim, not a correctness claim: it
//! would still pass if the fold that reads a record row filed it under the wrong category, or if
//! an aggregate counted something other than rows. This file therefore re-reads the run records
//! on its own terms — its own field paths, its own grouping, its own arithmetic — and compares
//! against the published tables field by field. Nothing here calls the assembly.
//!
//! What a failure means is stated in the assertion message, because the two possibilities are
//! different problems: the records moved (then the tables are stale and must be refreshed), or
//! the fold disagrees with a reading of the same bytes (then one of the two is wrong and the
//! milestone cannot publish either).

use evm_pipeline::canonicalization::DUPLICATE_EXACT;
use evm_pipeline::state_ownership::{contracts, kind_of, MeasuredCandidate};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const RECORDS_DIR: &str = "data/evidence/m8/cross-stage";
const EVIDENCE_DIR: &str = "data/evidence/m8/state-ownership";
const CANDIDATES_FILE: &str = "reuse-candidates.json";
const SUMMARY_FILE: &str = "duplicate-summary.json";
const STAGE_PAIRS_FILE: &str = "stage-pairs.json";
const CALLS_FILE: &str = "pipeline-calls.json";
const FIXED_BLOCK_DIR: &str = "fixed-block";
const PREFLIGHT: &str = "preflight";
const BUILD: &str = "build";
const INTRA_STAGE: &str = "intra_stage";
const CROSS_STAGE: &str = "cross_stage";

/// One measured flow: the method, whether the pair crosses stages, the two stages, and the two
/// caller stamps — the six columns both the records and §6's edge table spell out.
type Flow = (String, String, String, String, String, String);

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_json(absolute: &Path) -> Value {
    let text = std::fs::read_to_string(absolute)
        .unwrap_or_else(|error| panic!("{}: {error}", absolute.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", absolute.display()))
}

fn records(name: &str) -> Value {
    read_json(&repo().join(RECORDS_DIR).join(name))
}

fn published(name: &str) -> Value {
    read_json(&repo().join(EVIDENCE_DIR).join(name))
}

fn rows_of(table: &Value) -> Vec<Value> {
    table["rows"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("a table with no `rows` array: {}", table["file"]))
}

/// A field that must be there and must be a string.
fn text(row: &Value, path: &[&str]) -> String {
    opt(row, path).unwrap_or_else(|| panic!("{} is not a string", path.join(".")))
}

/// A field that may legitimately be absent or null — a stage label an unstamped ask carries, a
/// block term a call sends no argument for.
fn opt(row: &Value, path: &[&str]) -> Option<String> {
    let mut node = row;
    for key in path {
        node = &node[key];
    }
    match node {
        Value::Null => None,
        value => value.as_str().map(|word| word.to_string()),
    }
}

fn number(row: &Value, path: &[&str]) -> u64 {
    let mut node = row;
    for key in path {
        node = &node[key];
    }
    node.as_u64()
        .unwrap_or_else(|| panic!("{} is not a count: {node}", path.join(".")))
}

/// One record row, read straight from the run files — the same shape `assess_measured` takes, but
/// built here rather than borrowed from the assembly, so a wrong path is this file's bug to find.
fn measured(row: &Value) -> MeasuredCandidate {
    MeasuredCandidate {
        run: text(row, &["run"]),
        candidate_id: text(row, &["candidate_id"]),
        method: text(row, &["identity", "method"]),
        category: text(row, &["consumer", "category"]),
        scope: text(row, &["scope"]),
        producer_caller: opt(row, &["producer", "caller"]),
        consumer_caller: opt(row, &["consumer", "caller"]),
        producer_stage: opt(row, &["producer", "stage"]),
        consumer_stage: text(row, &["consumer", "stage"]),
        duplicate_type: text(row, &["duplicate_type"]),
        block_relation: text(row, &["block_relation"]),
        producer_block_form: text(row, &["producer_block_form"]),
        consumer_block_form: text(row, &["consumer_block_form"]),
        producer_block: opt(row, &["producer_block"]),
        consumer_block: opt(row, &["consumer_block"]),
        record_reusable: text(row, &["reusable"]),
        record_safe_to_reuse: row["safe_to_reuse"].as_bool().unwrap_or(false),
    }
}

fn exact_rows(table: &Value) -> Vec<Value> {
    rows_of(table)
        .into_iter()
        .filter(|row| row["duplicate_type"].as_str() == Some(DUPLICATE_EXACT))
        .collect()
}

/// The stage one side of a record row names, in the spelling §6's edge table uses.
///
/// A record's `stage` is null exactly where the ask happens before any stage has a name — the
/// connection's own `eth_chainId` — and the record writes the word `unstamped` in `stage_label`
/// there. Keying those sides on `stage` alone would file them under the empty string and leave
/// the edge table's `unstamped` rows with no records beside them, which reads as a disagreement
/// the records never had. Where both columns are present they say the same word; the assertion
/// below is what makes that a checked fact rather than an assumption.
fn stage_word(record: &Value, side: &str) -> String {
    let stage = opt(record, &[side, "stage"]);
    let label = opt(record, &[side, "stage_label"]);
    if let (Some(stage), Some(label)) = (&stage, &label) {
        assert_eq!(
            stage,
            label,
            "{}: its {side} stage and its {side} stage label are different words, so the flow \
             key would have to pick one",
            text(record, &["candidate_id"])
        );
    }
    stage.or(label).unwrap_or_else(|| {
        panic!(
            "{} names neither a {side} stage nor a {side} stage label",
            text(record, &["candidate_id"])
        )
    })
}

/// The six columns that say which flow a measured duplicate belongs to. Built from a record row
/// here, and from an edge row by `edge_key`, so the two are compared rather than assumed alike.
fn flow_key(record: &Value) -> Flow {
    (
        text(record, &["identity", "method"]),
        text(record, &["scope"]),
        stage_word(record, "producer"),
        stage_word(record, "consumer"),
        opt(record, &["producer", "caller"]).unwrap_or_default(),
        opt(record, &["consumer", "caller"]).unwrap_or_default(),
    )
}

fn edge_key(row: &Value) -> Flow {
    (
        text(row, &["method"]),
        stage_scope(row),
        text(row, &["producer_stage"]),
        text(row, &["consumer_stage"]),
        opt(row, &["producer_caller"]).unwrap_or_default(),
        opt(row, &["consumer_caller"]).unwrap_or_default(),
    )
}

/// The three live arms, taken from the published provenance rather than typed here: if the
/// provenance names a run, that run's own files are what the table has to explain.
fn live_arm_entries() -> Vec<Value> {
    let verdicts = published("reuse-verdicts.json");
    verdicts["assembled_from"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("the published provenance is not a list"))
}

fn live_arms() -> Vec<(String, u64)> {
    live_arm_entries()
        .iter()
        .map(|entry| (text(entry, &["run"]), number(entry, &["reuse_candidates"])))
        .collect()
}

/// §11 and §13, first question: does every duplicate M8.4.2 measured land in a category §4
/// declares, and does the published row still carry the record's own fields unchanged?
#[test]
fn every_duplicate_the_records_measured_lands_in_a_category_the_model_declares() {
    let root = records(CANDIDATES_FILE);
    let exact = exact_rows(&root);
    let verdicts = published("reuse-verdicts.json");
    let published_rows = rows_of(&verdicts);

    assert_eq!(
        exact.len(),
        published_rows.len(),
        "the records hold {} exact duplicates while the table publishes {} verdicts",
        exact.len(),
        published_rows.len()
    );

    let mut by_id: BTreeMap<String, &Value> = BTreeMap::new();
    for row in &published_rows {
        let id = text(row, &["candidate_id"]);
        assert!(
            by_id.insert(id.clone(), row).is_none(),
            "the published table lists {id} twice"
        );
    }

    let declared: BTreeSet<String> = contracts()
        .iter()
        .map(|contract| contract.kind.as_str().to_string())
        .collect();
    let mut unmapped = Vec::new();
    for record in &exact {
        let id = text(record, &["candidate_id"]);
        let ask = measured(record);
        let Some(kind) = kind_of(&ask) else {
            unmapped.push(format!("{id} ({})", ask.method));
            continue;
        };
        assert!(
            declared.contains(kind.as_str()),
            "{id} was filed under `{}`, which §4 never declares",
            kind.as_str()
        );
        let row = *by_id
            .get(&id)
            .unwrap_or_else(|| panic!("the records measured {id} and the table does not carry it"));

        // The record's own words, unchanged: a verdict may add a judgement, it may not edit an
        // observation. The columns the table speaks in its own voice sit at the row's top level;
        // the record's stage, caller and block term travel in `source_record`, which is this
        // file's input echoed into the row it came from.
        for (field, value) in [
            ("run", ask.run.clone()),
            ("method", ask.method.clone()),
            ("category", ask.category.clone()),
            ("scope", ask.scope.clone()),
            ("consumer_stage", ask.consumer_stage.clone()),
            ("block_relation", ask.block_relation.clone()),
            ("record_reusable", ask.record_reusable.clone()),
        ] {
            assert_eq!(
                text(row, &[field]),
                value,
                "{id}: the published `{field}` is not the record's"
            );
        }
        for field in [
            "producer_stage",
            "producer_caller",
            "consumer_caller",
            "producer_block_form",
            "consumer_block_form",
        ] {
            let echoed = opt(row, &["source_record", field]);
            let original = match field {
                "producer_stage" => ask.producer_stage.clone(),
                "producer_caller" => ask.producer_caller.clone(),
                "consumer_caller" => ask.consumer_caller.clone(),
                "producer_block_form" => Some(ask.producer_block_form.clone()),
                _ => Some(ask.consumer_block_form.clone()),
            };
            assert_eq!(
                echoed, original,
                "{id}: the published `source_record.{field}` disagrees with the record, \
                 including where one side is null and the other an empty string"
            );
        }
        // The table's term columns are the model's own answer, and this checks them against the
        // word the record wrote: they agree only while every record row's term is one of the
        // kinds the model recognises. A fourth word in a future record would be filed as a tag,
        // and here it stops being silently re-labelled.
        for side in ["producer", "consumer"] {
            let form = format!("{side}_block_form");
            let term = format!("{side}_term");
            assert_eq!(
                text(row, &[term.as_str()]),
                text(row, &["source_record", form.as_str()]),
                "{id}: the table's {term} is not the term the record wrote",
            );
        }
        // The row prints the producer stage twice — as its own column and inside the echo — and
        // both prints have to be the record's word.
        assert_eq!(
            opt(row, &["producer_stage"]),
            opt(row, &["source_record", "producer_stage"]),
            "{id}: the row's own `producer_stage` is not the stage its `source_record` carries"
        );
        assert_eq!(
            text(row, &["state_kind"]),
            kind.as_str(),
            "{id}: the published category is not the one this method and read category map to"
        );
    }
    assert!(
        unmapped.is_empty(),
        "these measured duplicates match no §4 category, and the table would have had to report \
         them as unmapped: {}",
        unmapped.join(", ")
    );
    assert_eq!(
        verdicts["input"]["unmapped_to_a_category"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "the table says it mapped everything while a row here found otherwise"
    );
}

/// §11's second question: the 42 pairs are not a shapeless count. Grouped by the flow that
/// produced them they are fourteen reads, each asked once per run in three runs — and the §6 edge
/// table's `measured_pairs` column has to be exactly that arithmetic.
#[test]
fn the_fourteen_flows_times_three_runs_reproduce_from_the_raw_rows() {
    let exact = exact_rows(&records(CANDIDATES_FILE));
    let mut flows: BTreeMap<Flow, BTreeSet<String>> = BTreeMap::new();
    for record in &exact {
        // The record's `scope` column has to be the pair of stages the record itself names, or
        // the flows below would key records by one reading and the edge table by another.
        let stages_are_one = stage_word(record, "producer") == stage_word(record, "consumer");
        assert_eq!(
            text(record, &["scope"]),
            if stages_are_one {
                INTRA_STAGE
            } else {
                CROSS_STAGE
            },
            "{}: the record's `scope` column is not the pair of stages it names",
            text(record, &["candidate_id"])
        );
        flows
            .entry(flow_key(record))
            .or_default()
            .insert(text(record, &["run"]));
    }
    assert_eq!(
        flows.len(),
        14,
        "the exact duplicates group into {} flows, not fourteen",
        flows.len()
    );
    for (key, runs) in &flows {
        assert_eq!(
            runs.len(),
            3,
            "the flow {key:?} appears in {} runs, and a flow that is not every run is a run \
             that was dropped from the pairing",
            runs.len()
        );
    }

    let edges = published("stage-dependency-matrix.json");
    let edge_rows = rows_of(&edges);
    let carrying: Vec<&Value> = edge_rows
        .iter()
        .filter(|row| number(row, &["measured_pairs"]) > 0)
        .collect();
    assert_eq!(
        carrying.len(),
        flows.len(),
        "the edge table shows {} rows with measured pairs while the records group into {} flows",
        carrying.len(),
        flows.len()
    );

    let mut pairs_from_edges = 0u64;
    let mut pairs_preflight_to_build = 0u64;
    for row in &carrying {
        let id = text(row, &["id"]);
        let key = edge_key(row);
        let rows_in_flow = exact
            .iter()
            .filter(|record| flow_key(record) == key)
            .count();
        let claimed = number(row, &["measured_pairs"]);
        assert_eq!(
            claimed as usize, rows_in_flow,
            "edge {id} claims {claimed} measured pairs while the records carry {rows_in_flow} \
             rows on that flow — the column is not the rows counted"
        );
        pairs_from_edges += claimed;
        if opt(row, &["producer_stage"]).as_deref() == Some(PREFLIGHT)
            && text(row, &["consumer_stage"]) == BUILD
        {
            pairs_preflight_to_build += claimed;
        }
    }
    assert_eq!(
        number(&edges, &["measured_pairs_total"]),
        pairs_from_edges,
        "the edge table's own total disagrees with the sum of its rows"
    );
    assert_eq!(
        number(&edges, &["measured_pairs_total"]),
        exact.len() as u64,
        "the edges add up to {} pairs while the records measured {} exact duplicates",
        number(&edges, &["measured_pairs_total"]),
        exact.len()
    );
    assert_eq!(
        number(&edges, &["measured_pairs_preflight_to_build"]),
        pairs_preflight_to_build,
        "§6's preflight → build count is not the sum of the edges that cross it"
    );
    assert_eq!(
        pairs_preflight_to_build, 12,
        "the records carry {pairs_preflight_to_build} preflight → build pairs, and §6's claim \
         about the crossing was written against a different measurement"
    );
}

/// An edge row's scope, decided the same way the records decide it: a pair inside one stage is
/// intra-stage, a pair that crosses is cross-stage. Recomputed here rather than read, so the
/// flow key this file builds is this file's own.
fn stage_scope(row: &Value) -> String {
    match (
        opt(row, &["producer_stage"]),
        text(row, &["consumer_stage"]),
    ) {
        (Some(producer), consumer) if producer == consumer => "intra_stage".to_string(),
        _ => "cross_stage".to_string(),
    }
}

/// §11's third question: the merged table is the three runs' own files, not a fourth measurement.
#[test]
fn the_three_run_files_add_up_to_the_root_table_and_to_the_verdicts() {
    let root = records(CANDIDATES_FILE);
    let root_ids: BTreeSet<String> = exact_rows(&root)
        .iter()
        .map(|row| text(row, &["candidate_id"]))
        .collect();
    let arms = live_arms();
    assert_eq!(
        arms.len(),
        3,
        "the provenance names {} live arms",
        arms.len()
    );

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut rows_across_arms = 0u64;
    let mut per_run: BTreeMap<String, u64> = BTreeMap::new();
    for (run, claimed) in &arms {
        let directory = repo()
            .join(RECORDS_DIR)
            .join("runs")
            .join(run)
            .join(CANDIDATES_FILE);
        let table = read_json(&directory);
        let ids: BTreeSet<String> = exact_rows(&table)
            .iter()
            .map(|row| text(row, &["candidate_id"]))
            .collect();
        assert_eq!(
            ids.len() as u64,
            *claimed,
            "{run}'s own file carries {} exact duplicates while the provenance says {claimed}",
            ids.len()
        );
        assert_eq!(
            number(&table, &["candidates"]),
            ids.len() as u64,
            "{run}'s `candidates` field and its rows disagree"
        );
        for id in &ids {
            assert!(
                seen.insert(id.clone()),
                "{id} appears in two run files; a pair cannot be counted twice on the way in"
            );
            assert!(
                root_ids.contains(id),
                "{run} carries {id} which the merged records table does not"
            );
        }
        rows_across_arms += ids.len() as u64;
        per_run.insert(run.clone(), ids.len() as u64);
    }
    assert_eq!(
        seen, root_ids,
        "the three run files and the merged table describe different sets of duplicates"
    );
    assert_eq!(
        rows_across_arms,
        root_ids.len() as u64,
        "the runs add up to {rows_across_arms} while the merged table has {}",
        root_ids.len()
    );

    let verdicts = published("reuse-verdicts.json");
    let verdict_rows = rows_of(&verdicts);
    let mut published_per_run: BTreeMap<String, u64> = BTreeMap::new();
    for row in &verdict_rows {
        *published_per_run.entry(text(row, &["run"])).or_insert(0) += 1;
    }
    assert_eq!(
        published_per_run, per_run,
        "the verdict table's rows do not distribute over the runs the way the records do"
    );
    assert_eq!(
        number(&root, &["pairs"]),
        rows_of(&root).len() as u64,
        "the records' own pair count and their rows disagree"
    );
}

/// §14: the record's verdict and this milestone's verdict are printed beside each other, and
/// neither one rewrites the other. The measured question is whether they ever disagree.
#[test]
fn the_record_verdict_and_the_model_verdict_are_printed_beside_each_other() {
    let root = records(CANDIDATES_FILE);
    let root_rows = rows_of(&root);
    let by_id: BTreeMap<String, &Value> = root_rows
        .iter()
        .map(|row| (text(row, &["candidate_id"]), row))
        .collect();
    let verdicts = published("reuse-verdicts.json");
    let rows = rows_of(&verdicts);

    let mut safe_in_records = 0u64;
    let mut disagreements = 0u64;
    let mut model_says_safe = Vec::new();
    for row in &rows {
        let id = text(row, &["candidate_id"]);
        let record = *by_id
            .get(&id)
            .unwrap_or_else(|| panic!("the verdicts carry {id}, the records do not"));
        let record_safe = record["safe_to_reuse"].as_bool().unwrap_or(false);
        safe_in_records += u64::from(record_safe);
        let echoed = row["record_safe_to_reuse"]
            .as_bool()
            .unwrap_or_else(|| panic!("{id} carries no echoed record verdict"));
        assert_eq!(
            echoed, record_safe,
            "{id}: the published copy of the record's own `safe_to_reuse` is not the record's"
        );
        let model_safe = row["assessment"]["safe_to_reuse_now"]
            .as_bool()
            .unwrap_or_else(|| panic!("{id} carries no model verdict"));
        if model_safe {
            model_says_safe.push(id.clone());
        } else {
            assert!(
                !row["assessment"]["blockers"]
                    .as_array()
                    .is_none_or(Vec::is_empty),
                "{id}: a refusal that names no reason is not a refusal"
            );
        }
        if model_safe != echoed {
            disagreements += 1;
        }
    }
    assert!(
        model_says_safe.is_empty(),
        "this build calls {} measured candidates safe to reuse now, and the milestone that \
         recorded them called none: {model_says_safe:?}",
        model_says_safe.len()
    );
    assert_eq!(
        disagreements, 0,
        "{disagreements} rows have the model and the record answering differently — the table \
         prints both columns, so a non-zero count has to be said out loud rather than averaged"
    );
    assert_eq!(
        number(&verdicts, &["input", "record_safe_to_reuse"]),
        safe_in_records,
        "the table's input figure for the records' own safe-to-reuse verdicts is {safe_in_records} \
         in the records and not what is published"
    );
    assert_eq!(
        verdicts["aggregate"]["record_and_verdict_disagreements"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "the aggregate lists disagreements the rows do not show, or hides ones they do"
    );
}

/// §11's fourth question, and the one a smoothing sentence would hide: the fixed-block arm is a
/// correctness control, and it records no physical duplicate at all. Naming that keeps the 42
/// verdicts attached to the live arm, where they were measured.
#[test]
fn the_fixed_block_arm_recorded_no_duplicate_and_contributes_no_verdict_row() {
    let base = repo().join(RECORDS_DIR).join(FIXED_BLOCK_DIR);
    let mut arms = Vec::new();
    for entry in
        std::fs::read_dir(&base).unwrap_or_else(|error| panic!("{}: {error}", base.display()))
    {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            arms.push(path);
        }
    }
    arms.sort();
    assert!(
        arms.len() >= 3,
        "the fixed-block arm has {} run directories to read, and the comparison it is supposed \
         to be evidence for needs at least three",
        arms.len()
    );

    for directory in &arms {
        let name = directory
            .file_name()
            .expect("a named arm directory")
            .to_string_lossy()
            .to_string();
        let candidates = read_json(&directory.join(CANDIDATES_FILE));
        let summary = read_json(&directory.join(SUMMARY_FILE));
        assert_eq!(
            rows_of(&candidates).len(),
            0,
            "the fixed-block arm {name} now carries duplicate rows; this gate's claim was that \
             it records none, so the 42 live verdicts and this arm are no longer the pair the \
             report describes"
        );
        assert_eq!(
            number(&candidates, &["candidates"]),
            0,
            "the fixed-block arm {name} claims {} candidates over an empty row list",
            number(&candidates, &["candidates"])
        );
        assert_eq!(
            number(&summary, &["duplicate_pairs"]),
            0,
            "{name} counted a physical duplicate, so reuse did not absorb the arm's repeats"
        );
        assert_eq!(
            number(&summary, &["exact_duplicate_pairs"]),
            0,
            "{name} counted an exact duplicate"
        );
        assert!(
            number(&summary, &["total_asks"]) > 0,
            "{name} recorded no asks at all: the zero above would be an empty-input result"
        );
    }

    let verdicts = published("reuse-verdicts.json");
    let live: BTreeSet<String> = live_arms().into_iter().map(|(run, _)| run).collect();
    let verdict_rows = rows_of(&verdicts);
    for row in &verdict_rows {
        let run = text(row, &["run"]);
        assert!(
            live.contains(&run),
            "a verdict row cites run `{run}`, which the published provenance does not list as a \
             live arm"
        );
    }
}

/// §11 and §3: the measured set is what the runs actually asked for, and the runs' whole call log
/// is checked beside it. The gas question in §6 is deliberately a question with no read behind
/// it, so an estimation call appearing in either would mean the runs changed under the diagnosis.
#[test]
fn the_measured_methods_carry_no_gas_estimation_and_the_gas_edge_names_no_read() {
    let root = records(CANDIDATES_FILE);
    let root_rows = rows_of(&root);
    let exact = exact_rows(&root);
    let mut methods: BTreeSet<String> = BTreeSet::new();
    for row in &root_rows {
        methods.insert(text(row, &["identity", "method"]));
    }
    let mut exact_methods: BTreeSet<String> = BTreeSet::new();
    for row in &exact {
        exact_methods.insert(text(row, &["identity", "method"]));
    }
    for forbidden in ["eth_estimateGas", "eth_gasPrice", "eth_sendRawTransaction"] {
        assert!(
            !methods.contains(forbidden),
            "the records carry {forbidden} — this milestone's runs are BuildOnly reads and a \
             submission or an estimation in the set means the input is not the one §11 allowed"
        );
    }

    // §6's gas row claims nine methods over the runs' whole log. Absence is a claim about the
    // log, not about the pairs, so the log is what gets read — through the provenance's own
    // directories, so this cannot check a run the tables do not describe.
    let arms = live_arm_entries();
    assert_eq!(
        arms.len(),
        3,
        "the provenance names {} live arms for this gate to read",
        arms.len()
    );
    let mut asked: BTreeSet<String> = BTreeSet::new();
    let mut call_rows = 0u64;
    for entry in &arms {
        let directory = text(entry, &["directory"]);
        let table = read_json(&repo().join(&directory).join(CALLS_FILE));
        let rows = rows_of(&table);
        for row in &rows {
            asked.insert(text(row, &["method"]));
        }
        assert_eq!(
            number(entry, &["asks"]),
            rows.len() as u64,
            "the provenance prints {directory} as {} asks while its own log carries {}",
            number(entry, &["asks"]),
            rows.len()
        );
        call_rows += rows.len() as u64;
    }
    assert_eq!(
        call_rows,
        number(&records(SUMMARY_FILE), &["total_asks"]),
        "the three logs and the merged input's `total_asks` are different populations"
    );
    for forbidden in ["eth_estimateGas", "eth_gasPrice"] {
        assert!(
            !asked.contains(forbidden),
            "the runs' own call logs carry {forbidden}, and §6's gas row concludes the opposite \
             from the provenance it cites"
        );
    }
    assert_eq!(
        asked.len(),
        9,
        "the runs asked {} distinct methods while §6's gas row publishes nine",
        asked.len()
    );

    let edges = published("stage-dependency-matrix.json");
    let edge_rows = rows_of(&edges);
    let published_measured: BTreeSet<String> = edge_rows
        .iter()
        .filter(|row| number(row, &["measured_pairs"]) > 0)
        .map(|row| text(row, &["method"]))
        .collect();
    assert_eq!(
        published_measured, exact_methods,
        "the edges with measured pairs name {:?} while the exact duplicates carry {:?}",
        published_measured, exact_methods
    );

    let gas: Vec<&Value> = edge_rows
        .iter()
        .filter(|row| text(row, &["id"]).contains("gas_limit"))
        .collect();
    assert_eq!(
        gas.len(),
        1,
        "the gas-limit ceiling edge is published {} times",
        gas.len()
    );
    assert_eq!(
        gas[0]["method"],
        Value::Null,
        "the gas-limit edge now cites a chain read; §6's point was that this build asks the \
         question without one"
    );
    assert_eq!(
        number(gas[0], &["measured_pairs"]),
        0,
        "the gas-limit edge claims measured pairs, which would make it a duplicate pair and not \
         a question"
    );
}

/// §4 forbids one category answering for another, and it also forbids calling something a
/// duplicate because a neighbour was. The category list is therefore re-derived from the records.
#[test]
fn only_the_categories_the_records_show_are_called_duplicates() {
    let root = records(CANDIDATES_FILE);
    let exact = exact_rows(&root);
    let mut touched: BTreeSet<String> = BTreeSet::new();
    for row in &exact {
        let ask = measured(row);
        if let Some(kind) = kind_of(&ask) {
            touched.insert(kind.as_str().to_string());
        }
    }
    let verdicts = published("reuse-verdicts.json");
    let published_split: BTreeSet<String> = verdicts["aggregate"]["by_state_kind"]
        .as_object()
        .cloned()
        .unwrap_or_default()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        published_split, touched,
        "the verdict table splits over {:?} while the records touch {:?}",
        published_split, touched
    );
    let split: BTreeMap<String, u64> = verdicts["aggregate"]["by_state_kind"]
        .as_object()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|(word, value)| (word.clone(), value.as_u64().expect("a count")))
        .collect();
    let mut expected: BTreeMap<String, u64> = BTreeMap::new();
    for row in &exact {
        let kind = kind_of(&measured(row)).expect("every measured row maps somewhere");
        *expected.entry(kind.as_str().to_string()).or_insert(0) += 1;
    }
    assert_eq!(
        split, expected,
        "the per-category split is not the records' rows counted once each"
    );

    let matrix = published("ownership-matrix.json");
    let matrix_rows = rows_of(&matrix);
    for row in &matrix_rows {
        let kind = text(row, &["state_kind"]);
        let holds = &row["reuse_status"]["duplicate"]["holds"];
        let measured_a_duplicate = touched.contains(&kind);
        if measured_a_duplicate {
            assert_eq!(
                holds,
                &Value::Bool(true),
                "the records show exact duplicates for {kind} while §4 declares it is never \
                 duplicated across asks"
            );
        } else {
            assert_ne!(
                holds,
                &Value::Bool(true),
                "§4 calls {kind} a duplicate and the records carry no pair of it — a `false` or a \
                 `null` is the honest answer here, not a `true` inherited from a neighbour"
            );
        }
    }
}

/// §11's last chain-of-custody question, asked of the file that is not this milestone's input:
/// the per-run `stage-pairs.json` rows are the runs' own pairing, and §6's headline number has
/// to agree with it pair for pair.
#[test]
fn the_run_pairs_their_own_stages_and_the_edge_table_say_the_same_thing() {
    let stage_pairs = records(STAGE_PAIRS_FILE);
    let pair_rows = rows_of(&stage_pairs);
    let mut crossing = 0u64;
    for row in &pair_rows {
        let producers = row["producer_stages"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let consumers = row["consumer_stages"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let from_preflight = producers
            .iter()
            .any(|stage| stage.as_str() == Some(PREFLIGHT));
        let into_build = consumers.iter().any(|stage| stage.as_str() == Some(BUILD));
        if from_preflight && into_build {
            crossing += number(row, &["pairs"]);
        }
    }
    let edges = published("stage-dependency-matrix.json");
    assert_eq!(
        number(&edges, &["measured_pairs_preflight_to_build"]),
        crossing,
        "the runs pair preflight's asks with build's {} times while the edge table prints {}",
        crossing,
        number(&edges, &["measured_pairs_preflight_to_build"])
    );

    // The direction matters: §6's claim is about a pair of stages, and an edge row that cited a
    // flow the records never paired would still add up arithmetically.
    let root = records(CANDIDATES_FILE);
    let root_rows = rows_of(&root);
    let mut directed: BTreeMap<(String, String), u64> = BTreeMap::new();
    for row in &root_rows {
        let producer = stage_word(row, "producer");
        let consumer = stage_word(row, "consumer");
        *directed.entry((producer, consumer)).or_insert(0) += 1;
    }
    let edge_rows = rows_of(&edges);
    for row in &edge_rows {
        let pairs = number(row, &["measured_pairs"]);
        if pairs == 0 {
            continue;
        }
        let key = (
            text(row, &["producer_stage"]),
            text(row, &["consumer_stage"]),
        );
        assert!(
            directed.get(&key).copied().unwrap_or(0) >= pairs,
            "the edge {:?} → {:?} prints {pairs} pairs while the records carry {} rows in that \
             direction at all",
            key.0,
            key.1,
            directed.get(&key).copied().unwrap_or(0)
        );
    }
}

/// The tables must describe the records that are on disk now, not the ones a refresh is about to
/// change. This gate says so in one place: it re-reads the record file's own header and compares
/// it against the input block of the published verdicts.
#[test]
fn the_published_input_block_is_the_record_file_as_it_stands() {
    let root = records(CANDIDATES_FILE);
    let verdicts = published("reuse-verdicts.json");
    assert_eq!(
        number(&verdicts, &["input", "pairs"]),
        number(&root, &["pairs"]),
        "the input pair count is not the record's"
    );
    assert_eq!(
        number(&verdicts, &["input", "duplicate_candidates"]),
        number(&root, &["candidates"]),
        "the input candidate count is not the record's"
    );
    assert_eq!(
        number(&verdicts, &["input", "carried"]),
        exact_rows(&root).len() as u64,
        "the carried-row count is not the record's exact duplicates"
    );
    let root_rows = rows_of(&root);
    let mut classes: BTreeMap<String, u64> = BTreeMap::new();
    for row in &root_rows {
        *classes.entry(text(row, &["duplicate_type"])).or_insert(0) += 1;
    }
    let published_classes: BTreeMap<String, u64> = verdicts["input"]["duplicate_type_counts"]
        .as_object()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|(word, value)| (word.clone(), value.as_u64().expect("a count")))
        .collect();
    assert_eq!(
        published_classes, classes,
        "the input's duplicate-class split is not the record's rows counted once each"
    );
    assert_eq!(
        text(&verdicts, &["input", "file"]),
        format!("{RECORDS_DIR}/{CANDIDATES_FILE}"),
        "the table cites a different input than the file this gate read"
    );
}
