//! M8.4.3 §10–§11's evidence tree: `data/evidence/m8/state-ownership/`, assembled from the
//! diagnostic model in [`evm_pipeline::state_ownership`] and from M8.4.2's committed records.
//!
//! # Why a gate writes this directory at all
//!
//! §10 asks for contracts a machine can check, and §11 asks that every aggregate be recomputable
//! from raw records. Both are properties of files, so both are graded here: the four tables are
//! assembled from the model plus one committed record file, and the byte gate below asks whether
//! the committed directory *is* that assembly. Nothing in this file measures anything — §11's
//! 「不要求新增真实 RPC 实验」 is honoured by opening no socket, and §3's ban on new caches, reuse
//! paths, batching and scheduler changes is honoured because a test cannot change production code.
//!
//! # The four tables and where each § is graded
//!
//! - [`OWNERSHIP_MATRIX_FILE`] — §4's eleven categories with §10's roles, §10's four identity
//!   fields spelled out per row, and the four reuse tiers kept apart. §6's per-item rule
//!   (「不得从某一类推及全部」) shows up as eleven independent rows.
//! - [`LIFECYCLE_CONTRACTS_FILE`] — §5's five steps × eleven categories, each row graded by the
//!   basis of that transition alone.
//! - [`STAGE_DEPENDENCY_MATRIX_FILE`] — §6–§9's 23 edges, each answering §6's nine sub-questions
//!   and deciding with one of §17's four options and nothing else.
//! - [`REUSE_VERDICTS_FILE`] — §11's measured arm: M8.4.2's 42 exact-duplicate candidates fed
//!   through [`assess_measured`], every aggregate counted out of the published rows.
//!
//! §12's fifteen decision-rule tests live in `crates/pipeline/src/state_ownership/tests.rs`; §12.15
//! (aggregates recomputable) is graded in both directions — inside one file here
//! ([`the_aggregates_are_the_published_rows_counted`]) and across the two milestones in
//! `crates/pipeline/tests/state_ownership_recompute.rs`.
//!
//! # The number this directory cannot produce
//!
//! `safe_to_reuse_now` is 0 for all 42 measured candidates, and no `proven` fourth tier appears in
//! any of the eleven declarations. That is not a shortfall of the arithmetic: the fourth tier asks
//! whether the chain's state moved between two reads and which component holds the answer
//! afterwards, and neither is a field of a recorded call or a property of this code as written.
//! §14's 「不得把「测试给出相同结果」理解为生产可安全复用」 is why the tables print the record's own
//! verdict beside this milestone's (`record_safe_to_reuse`, `safe_to_reuse_now`) rather than
//! reconciling them into one column.
//!
//! # How these files change
//!
//! Assembly happens under `target/pipeline-tests/`; `M843_STATE_OWNERSHIP_REFRESH=1` copies a fresh
//! assembly over the committed directory, which is the only way these five files change. The
//! records they read — `data/evidence/m8/cross-stage/` — are the runs' own output, and nothing here
//! writes to them. Anchor line numbers are resolved out of the source files at assembly time, so a
//! line number in the evidence is measured rather than copied: when the code moves, the gate fails
//! until the tables are refreshed from the code that now holds.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use evm_pipeline::canonicalization::DUPLICATE_EXACT;
use evm_pipeline::state_ownership::{
    assess_measured, contract_for, kinds, lifecycle_rows, stage_edges, Anchor, EdgeDecision,
    MeasuredCandidate, ReuseTiers, Rule, SourceKind, StateContract, Tier, ALL_KINDS,
    LIFECYCLE_CONTRACTS_FILE, OWNERSHIP_MATRIX_FILE, README_FILE, REUSE_VERDICTS_FILE,
    STAGE_DEPENDENCY_MATRIX_FILE, STATE_OWNERSHIP_FILES,
};

const EVIDENCE_DIR: &str = "data/evidence/m8/state-ownership";
/// §11's measured arm: read, never written. The three live BuildOnly runs M8.4.2 recorded.
const RECORDS_DIR: &str = "data/evidence/m8/cross-stage";
const CANDIDATES_FILE: &str = "reuse-candidates.json";
const CANDIDATES_RECORD: &str = "data/evidence/m8/cross-stage/reuse-candidates.json";
const MODEL_FILE: &str = "crates/pipeline/src/state_ownership.rs";
const WRITER: &str = "crates/pipeline/tests/state_ownership_evidence.rs";
/// The edge table's §6 subset, and §6's own count of pairs on that crossing.
const PREFLIGHT: &str = "preflight";
const BUILD: &str = "build";

/// §10's four recommended statuses — a table may print these words and nothing else.
const STATUS_WORDS: [&str; 4] = ["proven", "partially_proven", "unknown", "not_applicable"];
/// §10's four tiers, in the order one implies the next.
const TIER_WORDS: [&str; 4] = [
    "duplicate",
    "semantically_equivalent",
    "reusable_in_principle",
    "safe_to_reuse_now",
];
/// The words `IdentityForm::identity_fields` may produce. A table cell outside this list is a
/// field a reader typed rather than one the model derived.
const IDENTITY_WORDS: [&str; 11] = [
    "pinned_chain",
    "is_the_value",
    "pinned_height",
    "not_pinned",
    "not_applicable",
    "carried_and_verified",
    "not_carried",
    "number",
    "tag",
    "absent",
    "none",
];
/// §11's file set: four tables and the generated README, no more and no less.
const GENERATED_FILES: [&str; 5] = STATE_OWNERSHIP_FILES;
/// Each measured row carries two pointers: the record it came from, and the category it mapped to.
const REFS_PER_MEASURED_ROW: usize = 2;

/// `M843_STATE_OWNERSHIP_REFRESH` names no directory: its presence is the instruction to copy a
/// fresh assembly over the committed evidence. Nothing else in a test writes there.
fn refreshing() -> bool {
    std::env::var_os("M843_STATE_OWNERSHIP_REFRESH").is_some()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn evidence_dir() -> PathBuf {
    workspace_root().join(EVIDENCE_DIR)
}

fn records_dir() -> PathBuf {
    workspace_root().join(RECORDS_DIR)
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

/// A JSON table as every other evidence file in this repository holds it: two-space
/// pretty-printing and a trailing newline. serde_json without `preserve_order` sorts object keys,
/// which is why a re-assembly is byte-reproducible.
fn write_table(path: &Path, table: &Value) {
    let mut text = serde_json::to_string_pretty(table).unwrap_or_else(|error| panic!("{error}"));
    text.push('\n');
    write_text(path, &text);
}

fn write_text(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

/// Every file under one directory, for the scans that are only meaningful if they miss nothing.
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

// ---------------------------------------------------------------------------
// anchors: a claim resolves to one place, or the claim fails
// ---------------------------------------------------------------------------

/// The source files an anchor can point at, read once per assembly. Re-reading per anchor would
/// take 137 passes over the same dozen crates and would still not cache the fact that matters —
/// how many lines a token matches.
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
            read_text(&workspace_root().join(file))
                .lines()
                .map(str::to_string)
                .collect()
        })
    }
}

/// The end of a file's production region, i.e. the index of its first `#[cfg(test)]` line. A
/// production claim resolved against a line of test code would be a claim about a test, which is
/// what `SourceKind::Test` exists to say out loud instead.
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
/// committed file that contains the token, and may match more than once because a record repeats a
/// field name once per row.
fn resolve(anchor: &Anchor, tree: &mut Tree) -> Value {
    let all = tree.read(anchor.file);
    let limit = match anchor.source {
        SourceKind::Code => production_end(all),
        _ => all.len(),
    };
    let hits = matches(all, limit, anchor.token);
    match anchor.source {
        SourceKind::Code => assert_eq!(
            hits.len(),
            1,
            "{}: the token {:?} matches {} production lines of {} — a code anchor has to name one \
             place, and two matches mean the claim does not say which line it rests on",
            anchor.note,
            anchor.token,
            hits.len(),
            anchor.file
        ),
        SourceKind::Test => assert_eq!(
            hits.len(),
            1,
            "{}: the token {:?} matches {} lines of {} — a test anchor has to name one place",
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

/// The same resolution for a token the model cannot hold in an `Anchor`: a measured row's own
/// candidate id, which is runtime data and so has no `'static` lifetime to borrow.
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
// table 1: the ownership matrix (§4, §10)
// ---------------------------------------------------------------------------

fn tier_json(tier: &Tier) -> Value {
    json!({
        "holds": tier.holds,
        "status": tier.status.as_str(),
        "reason": tier.reason,
    })
}

fn tiers_json(tiers: &ReuseTiers) -> Value {
    json!({
        TIER_WORDS[0]: tier_json(&tiers.duplicate),
        TIER_WORDS[1]: tier_json(&tiers.semantically_equivalent),
        TIER_WORDS[2]: tier_json(&tiers.reusable_in_principle),
        TIER_WORDS[3]: tier_json(&tiers.safe_to_reuse_now),
        "check_would_stop_existing": tiers.check_would_stop_existing,
        "missing_proof": tiers.missing_proof,
    })
}

fn rule_json(rule: &Rule, tree: &mut Tree) -> Value {
    json!({
        "id": rule.id,
        "status": rule.status.as_str(),
        "text": rule.text,
        "evidence_refs": resolve_all(rule.anchors, tree),
    })
}

/// One §4 row, printed with §10's field names. The four identity fields come from
/// `IdentityForm::identity_fields`, so `chain_id` / `block_number` / `block_hash` /
/// `block_tag_semantics` cannot state something the `identity` field contradicts.
fn ownership_row(contract: &StateContract, tree: &mut Tree) -> Value {
    let fields = contract.identity.identity_fields();
    json!({
        "state_kind": contract.kind.as_str(),
        "producer": contract.producer.as_str(),
        "owner": contract.owner.as_str(),
        "scope": contract.scope.as_str(),
        "identity": contract.identity.as_str(),
        "chain_id": fields.chain_id,
        "block_number": fields.block_number,
        "block_hash": fields.block_hash,
        "block_tag_semantics": fields.block_tag,
        "authority": contract.authority.as_str(),
        "consumers": contract
            .consumers
            .iter()
            .map(|owner| owner.as_str())
            .collect::<Vec<&str>>(),
        "freshness_rule": rule_json(&contract.freshness, tree),
        "invalidation_rule": rule_json(&contract.invalidation, tree),
        "ownership_status": contract.ownership_status.as_str(),
        "reuse_status": tiers_json(&contract.reuse),
        "reason": contract.reason,
        "evidence_refs": resolve_all(contract.anchors, tree),
    })
}

/// Every string under one key, wherever it sits, for the vocabulary and aggregate gates. A table
/// that only ever reached its statuses through this function could not hide a hand-written word.
fn collect_strings(value: &Value, key: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (name, child) in map {
                if name == key {
                    if let Value::String(word) = child {
                        out.push(word.clone());
                    }
                }
                collect_strings(child, key, out);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_strings(child, key, out);
            }
        }
        _ => {}
    }
}

/// One row, one count: the aggregate is the rows grouped by their own field. `collect_strings`
/// walks a whole row and is what the vocabulary gate needs, but a verdict row carries
/// `state_kind` twice (its own and the assessment's), so a grouped count built that way would be
/// twice the rows and the table would stop being a row count.
fn count_by(rows: &[Value], key: &str) -> BTreeMap<String, u64> {
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for row in rows {
        let word = row[key].as_str().unwrap_or_else(|| {
            panic!(
                "a row has no top-level `{key}`, so this table cannot be grouped by it — the \
                 field moved or one row is a different shape"
            )
        });
        *counts.entry(word.to_string()).or_insert(0) += 1;
    }
    counts
}

fn ownership_matrix(tree: &mut Tree) -> Value {
    let rows: Vec<Value> = contracts()
        .iter()
        .map(|contract| ownership_row(contract, tree))
        .collect();
    json!({
        "root": format!("{EVIDENCE_DIR}/"),
        "file": OWNERSHIP_MATRIX_FILE,
        "generated_by": WRITER,
        "model": MODEL_FILE,
        "question": "§4: 谁产生这块链上状态，谁在运行中拥有它，它属于哪个生命周期阶段，失效条件是什么，下游能消费什么",
        "unit": "state category",
        "categories": rows.len(),
        "status_vocabulary": STATUS_WORDS,
        "tier_order": TIER_WORDS,
        "tiers_are_four_questions": true,
        "tier_inequalities": [
            "same value != same state identity",
            "same state identity != same lifecycle ownership",
            "reusable in principle != safe to reuse now"
        ],
        "identity_words": IDENTITY_WORDS,
        "proven_never_comes_from_prose_alone": "every row, rule and step whose status is `proven` \
            carries at least one evidence_ref, and the gate resolves each ref to one line of a \
            real file or to a committed record",
        "aggregate": {
            "ownership_status": count_by(&rows, "ownership_status"),
            "safe_to_reuse_now_tiers_holding_true": rows
                .iter()
                .filter(|row| row["reuse_status"][TIER_WORDS[3]]["holds"].as_bool() == Some(true))
                .count(),
            "categories_declaring_a_check_would_stop_existing": rows
                .iter()
                .filter(|row| {
                    row["reuse_status"]["check_would_stop_existing"].as_bool() == Some(true)
                })
                .count(),
            "categories_without_a_named_owner": rows
                .iter()
                .filter(|row| row["owner"].as_str() == Some("no_owner_in_code"))
                .count(),
            "freshness_rules_proven": rows
                .iter()
                .filter(|row| row["freshness_rule"]["status"].as_str() == Some("proven"))
                .count(),
            "invalidation_rules_proven": rows
                .iter()
                .filter(|row| row["invalidation_rule"]["status"].as_str() == Some("proven"))
                .count(),
        },
        "rows": rows,
    })
}

fn contracts() -> &'static [StateContract] {
    evm_pipeline::state_ownership::contracts()
}

// ---------------------------------------------------------------------------
// table 2: the lifecycle contracts (§5)
// ---------------------------------------------------------------------------

fn lifecycle_table(tree: &mut Tree) -> Value {
    let rows: Vec<Value> = lifecycle_rows()
        .iter()
        .map(|row| {
            json!({
                "state_kind": row.state_kind,
                "step": row.step,
                "status": row.status,
                "basis": row.basis,
                "evidence_refs": resolve_all(row.anchors, tree),
            })
        })
        .collect();
    json!({
        "root": format!("{EVIDENCE_DIR}/"),
        "file": LIFECYCLE_CONTRACTS_FILE,
        "generated_by": WRITER,
        "model": MODEL_FILE,
        "question": "§5: Acquired → Validated → Published → Consumed → Invalidated / Expired, one \
                     row per category and step, graded on that step's own basis",
        "unit": "category × step",
        "steps": ["acquired", "validated", "published", "consumed", "invalidated_or_expired"],
        "categories": kinds().len(),
        "rows_declared": rows.len(),
        "status_vocabulary": STATUS_WORDS,
        "aggregate": {
            "status": count_by(&rows, "status"),
            "by_step": count_by(&rows, "step"),
            "rows_without_an_anchor": rows
                .iter()
                .filter(|row| {
                    row["evidence_refs"]
                        .as_array()
                        .is_none_or(|list| list.is_empty())
                })
                .count(),
        },
        "rows": rows,
    })
}

// ---------------------------------------------------------------------------
// table 3: the stage dependency matrix (§6, §7, §8, §9, §17)
// ---------------------------------------------------------------------------

fn edge_row(edge: &evm_pipeline::state_ownership::StageEdge, tree: &mut Tree) -> Value {
    let answers = &edge.answers;
    json!({
        "id": edge.id,
        "section": edge.section,
        "producer_stage": edge.producer_stage,
        "consumer_stage": edge.consumer_stage,
        "state_kind": edge.state_kind,
        "method": edge.method,
        "producer_caller": edge.producer_caller,
        "consumer_caller": edge.consumer_caller,
        "measured_pairs": edge.measured_pairs,
        "question": edge.question,
        "sub_questions": {
            "what_the_producer_read": answers.what_the_producer_read,
            "what_the_consumer_read": answers.what_the_consumer_read,
            "same_semantics": answers.same_semantics,
            "safety_constraint_served": answers.safety_constraint_served,
            "producer_result_carried": answers.producer_result_carried,
            "consumer_has_own_duty": answers.consumer_has_own_duty,
            "external_change_between": answers.external_change_between,
            "version_or_token_exists": answers.version_or_token_exists,
            "minimal_addition": answers.minimal_addition
        },
        "contract_status": edge.contract.as_str(),
        "decision": edge.decision.as_str(),
        "decision_letter": edge.decision.letter(),
        "reasons": edge.reasons,
        "evidence_refs": resolve_all(edge.anchors, tree),
    })
}

fn stage_dependency_matrix(tree: &mut Tree) -> Value {
    let rows: Vec<Value> = stage_edges()
        .iter()
        .map(|edge| edge_row(edge, tree))
        .collect();
    let measured: u64 = rows
        .iter()
        .map(|row| row["measured_pairs"].as_u64().unwrap_or(0))
        .sum();
    let preflight_to_build: u64 = pairs_on(&rows, PREFLIGHT, BUILD);
    json!({
        "root": format!("{EVIDENCE_DIR}/"),
        "file": STAGE_DEPENDENCY_MATRIX_FILE,
        "generated_by": WRITER,
        "model": MODEL_FILE,
        "question": "§6–§9: 哪些值可能复用，但哪些验证动作仍然必须保留；每条边的判决只能取 §17 的四个选项之一",
        "unit": "stage edge",
        "edges": rows.len(),
        "sections": ["6", "7", "8", "9"],
        "decision_vocabulary": ["A", "B", "C", "D"],
        "decision_meanings": {
            "A": "controlled_experiment_definable — a next-phase controlled experiment is \
                  definable; this says nothing about sharing the value today",
            "B": "contract_design_first — the shape to design is a contract, not a cache",
            "C": "must_refetch — the consumer's read is its own duty",
            "D": "insufficient_evidence — this build cannot answer the question yet"
        },
        "measured_pairs_total": measured,
        "measured_pairs_preflight_to_build": preflight_to_build,
        "aggregate": {
            "decision": count_by(&rows, "decision"),
            "decision_letter": count_by(&rows, "decision_letter"),
            "section": count_by(&rows, "section"),
            "contract_status": count_by(&rows, "contract_status"),
            "edges_with_no_measured_pair": rows
                .iter()
                .filter(|row| row["measured_pairs"].as_u64() == Some(0))
                .count(),
            "edges_where_the_consumer_carries_its_own_duty": rows
                .iter()
                .filter(|row| {
                    row["sub_questions"]["consumer_has_own_duty"].as_bool() == Some(true)
                })
                .count(),
        },
        "rows": rows,
    })
}

fn pairs_on(rows: &[Value], producer: &str, consumer: &str) -> u64 {
    rows.iter()
        .filter(|row| {
            row["producer_stage"].as_str() == Some(producer)
                && row["consumer_stage"].as_str() == Some(consumer)
        })
        .map(|row| row["measured_pairs"].as_u64().unwrap_or(0))
        .sum()
}

// ---------------------------------------------------------------------------
// table 4: the reuse verdicts over M8.4.2's measured candidates (§10, §11, §12.15)
// ---------------------------------------------------------------------------

fn str_field(row: &Value, path: &[&str]) -> Option<String> {
    let mut cursor = row;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    cursor.as_str().map(str::to_string)
}

/// One M8.4.2 candidate row, reduced to the fields this milestone reads. A field the record does
/// not carry stays absent rather than becoming a `false` — §14's 「不得伪造数据」.
fn measured_candidate(row: &Value) -> MeasuredCandidate {
    MeasuredCandidate {
        run: str_field(row, &["run"]).unwrap_or_default(),
        candidate_id: str_field(row, &["candidate_id"]).unwrap_or_default(),
        method: str_field(row, &["identity", "method"]).unwrap_or_default(),
        category: str_field(row, &["consumer", "category"]).unwrap_or_default(),
        scope: str_field(row, &["scope"]).unwrap_or_default(),
        producer_caller: str_field(row, &["producer", "caller"]),
        consumer_caller: str_field(row, &["consumer", "caller"]),
        producer_stage: str_field(row, &["producer", "stage"]),
        consumer_stage: str_field(row, &["consumer", "stage"]).unwrap_or_default(),
        duplicate_type: str_field(row, &["duplicate_type"]).unwrap_or_default(),
        block_relation: str_field(row, &["block_relation"]).unwrap_or_default(),
        producer_block_form: str_field(row, &["producer_block_form"]).unwrap_or_default(),
        consumer_block_form: str_field(row, &["consumer_block_form"]).unwrap_or_default(),
        producer_block: str_field(row, &["producer_block"]),
        consumer_block: str_field(row, &["consumer_block"]),
        record_reusable: str_field(row, &["reusable"]).unwrap_or_default(),
        record_safe_to_reuse: row["safe_to_reuse"].as_bool().unwrap_or(false),
    }
}

/// The two pointers a measured verdict carries: the record line it was read from, and the §4 row
/// it was filed under. The second is the model's own anchor, so a verdict never cites a category
/// the matrix does not.
fn verdict_refs(measured: &MeasuredCandidate, tree: &mut Tree) -> Vec<Value> {
    let mut refs = vec![resolve_record_token(
        CANDIDATES_RECORD,
        &measured.candidate_id,
        "the measured pair this verdict is about",
        tree,
    )];
    match kind_of_measured(measured) {
        Some(contract) => refs.push(resolve(&contract, tree)),
        None => refs.push(json!({
            "file": MODEL_FILE,
            "token": measured.method,
            "source": SourceKind::RunRecord.as_str(),
            "note": "this ask mapped to none of §4's eleven categories, so the matrix has no row \
                     to point at and the gate reports the row as unmapped",
            "line": Value::Null,
            "matches": 0,
        })),
    }
    refs
}

/// The §4 row a measured ask belongs to, resolved through the same function the fold uses, and
/// turned into the anchor this table can publish.
fn kind_of_measured(measured: &MeasuredCandidate) -> Option<Anchor> {
    let kind = evm_pipeline::state_ownership::kind_of(measured)?;
    let contract = contract_for(kind)?;
    Some(contract.anchors[0])
}

fn reuse_verdicts(tree: &mut Tree) -> Value {
    let records = read_json(&records_dir().join(CANDIDATES_FILE));
    let source_rows = records["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("{RECORDS_DIR}/{CANDIDATES_FILE} has no `rows` array"));
    let mut carried: Vec<Value> = Vec::new();
    let mut unmapped: Vec<String> = Vec::new();
    let mut classes: BTreeMap<String, u64> = BTreeMap::new();
    for row in source_rows {
        let class = str_field(row, &["duplicate_type"]).unwrap_or_default();
        *classes.entry(class.clone()).or_insert(0) += 1;
        if class != DUPLICATE_EXACT {
            continue;
        }
        let measured = measured_candidate(row);
        let id = measured.candidate_id.clone();
        match assess_measured(&measured) {
            Some(assessed) => {
                let mut published =
                    serde_json::to_value(&assessed).expect("an assessed row serialises");
                if let Some(object) = published.as_object_mut() {
                    object.insert(
                        "source_record".to_string(),
                        json!({
                            "file": CANDIDATES_RECORD,
                            "run": measured.run,
                            "candidate_id": id,
                            "producer_stage": measured.producer_stage,
                            "producer_caller": measured.producer_caller,
                            "consumer_stage": measured.consumer_stage,
                            "consumer_caller": measured.consumer_caller,
                            // The block terms travel as the record wrote them, including the tag
                            // word itself. The model's own `*_term` column stops at the kind of
                            // term, because no file under a crate's `src/` may write the head tag
                            // as a literal (§20, `state_is_always_pinned.rs`) — so the word a
                            // reader needs to tell two tags apart comes from the record, not from
                            // source code typing it in.
                            "producer_block_form": measured.producer_block_form,
                            "producer_block": measured.producer_block,
                            "consumer_block_form": measured.consumer_block_form,
                            "consumer_block": measured.consumer_block,
                        }),
                    );
                    object.insert(
                        "evidence_refs".to_string(),
                        json!(verdict_refs(&measured, tree)),
                    );
                }
                carried.push(published);
            }
            None => unmapped.push(id),
        }
    }
    let safe = carried
        .iter()
        .filter(|row| row["assessment"]["safe_to_reuse_now"].as_bool() == Some(true))
        .count();
    let disagree: Vec<String> = carried
        .iter()
        .filter(|row| {
            row["assessment"]["safe_to_reuse_now"].as_bool()
                != row["record_safe_to_reuse"].as_bool()
        })
        .map(|row| row["candidate_id"].as_str().unwrap_or_default().to_string())
        .collect();
    let blockers = blocker_counts(&carried);
    // The blockers tie in bands, and a table that published one "most frequent" name out of a tie
    // would be a claim about which refusal matters. The whole top band is printed instead.
    let top_rows = blockers.values().max().copied().unwrap_or(0);
    let top_names: Vec<Value> = blockers
        .iter()
        .filter(|(_, count)| **count == top_rows)
        .map(|(word, _)| json!(word))
        .collect();
    json!({
        "root": format!("{EVIDENCE_DIR}/"),
        "file": REUSE_VERDICTS_FILE,
        "generated_by": WRITER,
        "model": MODEL_FILE,
        "question": "§10's four tiers applied to §11's measured arm: M8.4.2's exact-duplicate \
                     candidates, re-read as state-ownership questions",
        "unit": "measured candidate",
        "input": {
            "file": CANDIDATES_RECORD,
            "pairs": records["pairs"],
            "duplicate_candidates": records["candidates"],
            "refused_by_block_identity": records["refused_by_block_identity"],
            "record_safe_to_reuse": records["safe_to_reuse"],
            "duplicate_type_counts": classes,
            "carried": carried.len(),
            "unmapped_to_a_category": unmapped,
        },
        "assembled_from": records["assembled_from"].clone(),
        "tiers": TIER_WORDS,
        "tier_inequalities": [
            "same value != same state identity",
            "same state identity != same lifecycle ownership",
            "reusable in principle != safe to reuse now"
        ],
        "aggregate": {
            "candidates": carried.len(),
            "duplicate": count_tier(&carried, TIER_WORDS[0]),
            "semantically_equivalent": count_tier(&carried, TIER_WORDS[1]),
            "reusable_in_principle": count_tier(&carried, TIER_WORDS[2]),
            "safe_to_reuse_now": count_tier(&carried, TIER_WORDS[3]),
            "safe_to_reuse_now_true": safe,
            "rows_without_a_blocker": carried
                .iter()
                .filter(|row| {
                    row["assessment"]["blockers"]
                        .as_array()
                        .is_none_or(|list| list.is_empty())
                })
                .count(),
            "record_and_verdict_disagreements": disagree,
            "by_state_kind": count_by(&carried, "state_kind"),
            "by_producer_consumer": count_by(&carried, "consumer_stage"),
            "blockers": blockers,
            "blockers_at_the_top": {
                "rows": top_rows,
                "count": top_names.len(),
                "blockers": top_names,
            },
        },
        "rows": carried,
    })
}

fn blocker_counts(rows: &[Value]) -> BTreeMap<String, u64> {
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for row in rows {
        if let Value::Array(list) = &row["assessment"]["blockers"] {
            for word in list.iter().filter_map(Value::as_str) {
                *counts.entry(word.to_string()).or_insert(0) += 1;
            }
        }
    }
    counts
}

/// One tier counted over the published rows. `null` is kept as its own bucket because it is the
/// answer §13 refuses to replace with a guess.
fn count_tier(rows: &[Value], key: &str) -> BTreeMap<String, u64> {
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for row in rows {
        let word = match row["assessment"][key].as_bool() {
            Some(true) => "true",
            Some(false) => "false",
            None => "null",
        };
        *counts.entry(word.to_string()).or_insert(0) += 1;
    }
    counts
}

// ---------------------------------------------------------------------------
// the README, printed out of the tables
// ---------------------------------------------------------------------------

fn read_root(dir: &Path) -> Value {
    json!({
        OWNERSHIP_MATRIX_FILE: read_json(&dir.join(OWNERSHIP_MATRIX_FILE)),
        LIFECYCLE_CONTRACTS_FILE: read_json(&dir.join(LIFECYCLE_CONTRACTS_FILE)),
        STAGE_DEPENDENCY_MATRIX_FILE: read_json(&dir.join(STAGE_DEPENDENCY_MATRIX_FILE)),
        REUSE_VERDICTS_FILE: read_json(&dir.join(REUSE_VERDICTS_FILE)),
    })
}

fn root_table<'root>(root: &'root Value, name: &str) -> &'root Value {
    root.get(name)
        .unwrap_or_else(|| panic!("the assembly wrote no {name} table"))
}

fn rows_of(table: &Value) -> Vec<Value> {
    table["rows"].as_array().cloned().unwrap_or_default()
}

fn shown(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Array(values) => values.iter().map(shown).collect::<Vec<String>>().join(", "),
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| format!("{key}={}", shown(value)))
            .collect::<Vec<String>>()
            .join(", "),
    }
}

fn table_row(lines: &mut Vec<String>, cells: &[&str]) {
    lines.push(format!("| {} |", cells.join(" | ")));
}

fn separator_row(lines: &mut Vec<String>, columns: usize) {
    lines.push(format!("|{}|", vec!["---"; columns].join("|")));
}

fn push(lines: &mut Vec<String>, text: &str) {
    lines.push(text.to_string());
}

/// One word's count out of a table's aggregate. A count map that has no entry for the word counted
/// zero rows, and the README and the gate that re-reads it both say `0` rather than leave a reader
/// to tell a zero apart from a missing measurement.
fn count_word(table: &Value, key: &str, word: &str) -> String {
    match &table["aggregate"][key][word] {
        Value::Null => "0".to_string(),
        value => shown(value),
    }
}

/// How the measured rows print a block term, read out of the published rows: the `*_term` column is
/// the record's own `block_form` word, and the word the record actually sent rides in the row's
/// `source_record` echo. Shared by the README builder and the README gate so neither can drift.
fn term_echo(verdicts: &Value) -> String {
    let echoed_word = |row: &Value, side: &str| -> String {
        let term = format!("{side}_term");
        let block = format!("{side}_block");
        row["source_record"][block.as_str()]
            .as_str()
            .unwrap_or_else(|| {
                panic!("a row prints {term} as a tag while its record echoes no word in {block}")
            })
            .to_string()
    };
    let mut both_tag = 0usize;
    let mut one_sided = 0usize;
    let mut split_word = 0usize;
    let mut words: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows_of(verdicts) {
        let producer = row["producer_term"] == json!("tag");
        let consumer = row["consumer_term"] == json!("tag");
        match (producer, consumer) {
            (false, false) => continue,
            (true, true) => both_tag += 1,
            _ => {
                one_sided += 1;
                continue;
            }
        }
        let producer_word = echoed_word(&row, "producer");
        let consumer_word = echoed_word(&row, "consumer");
        if producer_word == consumer_word {
            *words.entry(producer_word).or_insert(0) += 1;
        } else {
            split_word += 1;
        }
    }
    let listed: Vec<String> = words
        .iter()
        .map(|(word, rows)| format!("`{word}` {rows} 行"))
        .collect();
    format!(
        "两侧都是 tag 的行 {both_tag}，单侧 tag 的行 {one_sided}，两侧词不同的行 {split_word}；\
         记录发出的词照原样抄在 `source_record`：{}",
        listed.join("、")
    )
}

/// The README every figure in is read out of the four tables this assembly just wrote.
fn build_readme(root: &Value) -> String {
    let matrix = root_table(root, OWNERSHIP_MATRIX_FILE);
    let lifecycle = root_table(root, LIFECYCLE_CONTRACTS_FILE);
    let edges = root_table(root, STAGE_DEPENDENCY_MATRIX_FILE);
    let verdicts = root_table(root, REUSE_VERDICTS_FILE);
    let matrix_rows = rows_of(matrix);
    let edge_rows = rows_of(edges);
    let verdict_rows = rows_of(verdicts);
    let counts = count_word;
    let mut lines: Vec<String> = Vec::new();

    push(&mut lines, "# M8.4.3 — 状态所有权与生命周期诊断（证据）");
    lines.push(String::new());
    push(
        &mut lines,
        &format!(
            "§1 的问题：一次链上读取由谁产生、谁拥有、属于哪个生命周期阶段、什么让它失效，下游因此能消费什么。 \
             本目录对 {} 类状态、{} 条阶段边、以及 M8.4.2 记录的 {} 个重复候选各答一遍。它是诊断：没有新增缓存、\
             没有跨 stage 复用、没有改 RPC 次数或顺序、没有删除任何执行前检查（只记录，不消除）。",
            shown(&matrix["categories"]),
            shown(&edges["edges"]),
            shown(&verdicts["aggregate"]["candidates"]),
        ),
    );
    lines.push(String::new());

    push(&mut lines, "## 结论");
    lines.push(String::new());
    push(
        &mut lines,
        &format!(
            "- {} 类状态逐类声明，`ownership_status`：proven {} / partially_proven {} / unknown {}；\
             其中命名不出 owner 的 {} 类。",
            shown(&matrix["categories"]),
            counts(matrix, "ownership_status", "proven"),
            counts(matrix, "ownership_status", "partially_proven"),
            counts(matrix, "ownership_status", "unknown"),
            shown(&matrix["aggregate"]["categories_without_a_named_owner"]),
        ),
    );
    push(
        &mut lines,
        &format!(
            "- {} 条边的判决（§17 的四个选项，没有第五种）：A {}、B {}、C {}、D {}。",
            shown(&edges["edges"]),
            counts(edges, "decision_letter", "A"),
            counts(edges, "decision_letter", "B"),
            counts(edges, "decision_letter", "C"),
            counts(edges, "decision_letter", "D"),
        ),
    );
    push(
        &mut lines,
        &format!(
            "- M8.4.2 的 {} 个 exact-duplicate 候选，按本模型的四级判定：**{}** 条到达 `safe_to_reuse_now`。",
            shown(&verdicts["input"]["duplicate_candidates"]),
            shown(&verdicts["aggregate"]["safe_to_reuse_now_true"]),
        ),
    );
    push(
        &mut lines,
        &format!(
            "- 拒绝的原因逐行点名，沉默的行 {}；命中最多的一档是 {} / {} 行，并列在这一档的原因有 {} 个：`{}`。",
            shown(&verdicts["aggregate"]["rows_without_a_blocker"]),
            shown(&verdicts["aggregate"]["blockers_at_the_top"]["rows"]),
            shown(&verdicts["aggregate"]["candidates"]),
            shown(&verdicts["aggregate"]["blockers_at_the_top"]["count"]),
            shown(&verdicts["aggregate"]["blockers_at_the_top"]["blockers"]),
        ),
    );
    push(
        &mut lines,
        &format!(
            "- 声明里写明「共享这个值会让一项检查消失」的类别：{}。freshness 规则 proven 的类别 {}，\
             invalidation 规则 proven 的类别 {}。",
            shown(&matrix["aggregate"]["categories_declaring_a_check_would_stop_existing"]),
            shown(&matrix["aggregate"]["freshness_rules_proven"]),
            shown(&matrix["aggregate"]["invalidation_rules_proven"]),
        ),
    );
    lines.push(String::new());
    push(
        &mut lines,
        "四级判定是四个不同的问题（§10 的三条不等式）：`same value ≠ same state identity`、\
         `same state identity ≠ same lifecycle ownership`、`reusable in principle ≠ safe to reuse now`。\
         下面每一行把四级分开打印，读一张表就能看到它们各自停在哪儿。",
    );
    lines.push(String::new());

    push(
        &mut lines,
        &format!(
            "## §4 的 {} 类状态（`{OWNERSHIP_MATRIX_FILE}`）",
            shown(&matrix["categories"])
        ),
    );
    lines.push(String::new());
    table_row(
        &mut lines,
        &[
            "state_kind",
            "owner",
            "scope",
            "authority",
            "block_tag_semantics",
            "ownership_status",
            "duplicate",
            "semantically_equivalent",
            "reusable_in_principle",
            "safe_to_reuse_now",
        ],
    );
    separator_row(&mut lines, 10);
    for row in &matrix_rows {
        let tier = |name: &str| shown(&row["reuse_status"][name]["holds"]);
        table_row(
            &mut lines,
            &[
                &shown(&row["state_kind"]),
                &shown(&row["owner"]),
                &shown(&row["scope"]),
                &shown(&row["authority"]),
                &shown(&row["block_tag_semantics"]),
                &shown(&row["ownership_status"]),
                &tier(TIER_WORDS[0]),
                &tier(TIER_WORDS[1]),
                &tier(TIER_WORDS[2]),
                &tier(TIER_WORDS[3]),
            ],
        );
    }
    lines.push(String::new());
    push(
        &mut lines,
        &format!(
            "`holds` 的三种写法各有含义：`true` 是这一级成立，`false` 是这一级被点名拒绝，`null` 是本轮证据答不了这一级 \
             （§13 不许用编造的数据填空）。每行的 `freshness_rule`、`invalidation_rule`、`missing_proof` 和已解析的 \
             `evidence_refs` 都在 `{OWNERSHIP_MATRIX_FILE}` 里。",
        ),
    );
    lines.push(String::new());

    push(
        &mut lines,
        &format!(
            "## §6 的重点链路：preflight 产生的 {} 对（其中 preflight → build {} 对）",
            edge_rows
                .iter()
                .filter(|row| row["producer_stage"].as_str() == Some(PREFLIGHT))
                .map(|row| row["measured_pairs"].as_u64().unwrap_or(0))
                .sum::<u64>(),
            shown(&edges["measured_pairs_preflight_to_build"]),
        ),
    );
    lines.push(String::new());
    let crossing: Vec<&Value> = edge_rows
        .iter()
        .filter(|row| {
            row["producer_stage"].as_str() == Some(PREFLIGHT)
                && row["consumer_stage"].as_str() == Some(BUILD)
        })
        .collect();
    let measured_flows: Vec<String> = crossing
        .iter()
        .filter(|row| row["measured_pairs"].as_u64().unwrap_or(0) > 0)
        .map(|row| {
            format!(
                "`{}` {} 对判 {}",
                row["method"].as_str().unwrap_or("?"),
                row["measured_pairs"].as_u64().unwrap_or(0),
                row["decision_letter"].as_str().unwrap_or("?")
            )
        })
        .collect();
    let questions: Vec<String> = crossing
        .iter()
        .filter(|row| row["measured_pairs"].as_u64().unwrap_or(0) == 0)
        .map(|row| {
            format!(
                "`{}`（{}，判 {}）",
                row["id"].as_str().unwrap_or("?"),
                row["state_kind"].as_str().unwrap_or("?"),
                row["decision_letter"].as_str().unwrap_or("?")
            )
        })
        .collect();
    push(
        &mut lines,
        &format!(
            "§6 禁止把一类状态的结论推及全部，所以这张表一条流一行。preflight → build 上有实测重复的流：{}，合计 {} 对，\
             与 `{STAGE_DEPENDENCY_MATRIX_FILE}` 的 `measured_pairs_preflight_to_build` 一致。同一批边里还有一行只提问、\
             不带重复对：{}。判决并不相同这一事实本身就是结论：一条流的判决推广不到同类另一条，也推广不到它的下一步。",
            measured_flows.join("、"),
            pairs_on(&edge_rows, PREFLIGHT, BUILD),
            questions.join("、"),
        ),
    );
    lines.push(String::new());
    table_row(
        &mut lines,
        &[
            "edge",
            "state_kind",
            "method",
            "consumer",
            "pairs",
            "decision",
            "consumer 自带职责",
            "保留的安全约束",
        ],
    );
    separator_row(&mut lines, 8);
    for row in edge_rows
        .iter()
        .filter(|row| row["producer_stage"].as_str() == Some(PREFLIGHT))
    {
        table_row(
            &mut lines,
            &[
                &shown(&row["id"]),
                &shown(&row["state_kind"]),
                &shown(&row["method"]),
                &shown(&row["consumer_stage"]),
                &shown(&row["measured_pairs"]),
                &shown(&row["decision_letter"]),
                &shown(&row["sub_questions"]["consumer_has_own_duty"]),
                &shown(&row["sub_questions"]["safety_constraint_served"]),
            ],
        );
    }
    lines.push(String::new());
    push(
        &mut lines,
        "「哪些值可能复用，但某些验证动作仍然必须保留」就在这一节：判 A 的边只说明下一轮可以设计一个受控实验，不说明今天可以共享；\
         §6 的「不能为了减少 RPC 而删除执行前安全检查」由 `safety_constraint_served` 一列逐行说明。",
    );
    lines.push(String::new());

    push(
        &mut lines,
        &format!("## §6–§9 的全部边（`{STAGE_DEPENDENCY_MATRIX_FILE}`）"),
    );
    lines.push(String::new());
    table_row(
        &mut lines,
        &[
            "section",
            "edge",
            "producer → consumer",
            "state_kind",
            "decision",
            "measured_pairs",
        ],
    );
    separator_row(&mut lines, 6);
    for row in &edge_rows {
        table_row(
            &mut lines,
            &[
                &shown(&row["section"]),
                &shown(&row["id"]),
                &format!(
                    "{} → {}",
                    shown(&row["producer_stage"]),
                    shown(&row["consumer_stage"])
                ),
                &shown(&row["state_kind"]),
                &shown(&row["decision_letter"]),
                &shown(&row["measured_pairs"]),
            ],
        );
    }
    lines.push(String::new());
    push(
        &mut lines,
        &format!(
            "判决词表：A = {}；B = {}；C = {}；D = {}。{} 条边没有实测重复对，它们的问题来自 §7–§9 的代码事实，\
             而不是本表测到的那 {} 对重复；表里 `measured_pairs` 为 0 的行不假装被测量过。",
            shown(&edges["decision_meanings"]["A"]),
            shown(&edges["decision_meanings"]["B"]),
            shown(&edges["decision_meanings"]["C"]),
            shown(&edges["decision_meanings"]["D"]),
            shown(&edges["aggregate"]["edges_with_no_measured_pair"]),
            shown(&edges["measured_pairs_total"]),
        ),
    );
    lines.push(String::new());

    push(
        &mut lines,
        &format!("## §5 的生命周期（`{LIFECYCLE_CONTRACTS_FILE}`）"),
    );
    lines.push(String::new());
    push(
        &mut lines,
        &format!(
            "{} 行 = {} 类 × {} 步（{}）。状态分布：{}。没有锚点的行 {} —— \
             它们只能是 `unknown` 或 `not_applicable`，门禁不允许一行正面的断言不带出处。",
            shown(&lifecycle["rows_declared"]),
            shown(&lifecycle["categories"]),
            lifecycle["steps"].as_array().map_or(0, |steps| steps.len()),
            shown(&lifecycle["steps"]),
            shown(&lifecycle["aggregate"]["status"]),
            shown(&lifecycle["aggregate"]["rows_without_an_anchor"]),
        ),
    );
    lines.push(String::new());

    push(
        &mut lines,
        &format!("## 实测：M8.4.2 候选的四级判定（`{REUSE_VERDICTS_FILE}`）"),
    );
    lines.push(String::new());
    push(
        &mut lines,
        &format!(
            "输入是 `{CANDIDATES_RECORD}` 的 {} 对（{} exact / {} 同目标不同块 / {} 块身份不明），只有 exact duplicate 进入本表：{} 个。\
             映射到 §4 的 {} 类状态，映射不到任何一类的 {} 个。",
            shown(&verdicts["input"]["pairs"]),
            shown(&verdicts["input"]["duplicate_type_counts"][DUPLICATE_EXACT]),
            shown(&verdicts["input"]["duplicate_type_counts"]["same_target_different_block"]),
            shown(&verdicts["input"]["duplicate_type_counts"]["same_target_block_undetermined"]),
            shown(&verdicts["input"]["carried"]),
            verdicts["aggregate"]["by_state_kind"]
                .as_object()
                .map_or(0, |map| map.len()),
            verdicts["input"]["unmapped_to_a_category"]
                .as_array()
                .map_or(0, Vec::len),
        ),
    );
    lines.push(String::new());
    table_row(&mut lines, &["tier", "true", "false", "null"]);
    separator_row(&mut lines, 4);
    for tier in TIER_WORDS {
        let counts = &verdicts["aggregate"][tier];
        table_row(
            &mut lines,
            &[
                tier,
                &shown(&counts["true"]),
                &shown(&counts["false"]),
                &shown(&counts["null"]),
            ],
        );
    }
    lines.push(String::new());
    push(
        &mut lines,
        &format!(
            "拒绝逐条点名：{}。记录自己给出的 `safe_to_reuse` 与本模型判定不一致的行 {} 行 —— 两个结论各说各话时表把两列并排放 \
             （§14 不许把「测试给出相同结果」读成「生产可安全复用」，也不许用模型的拒绝去改写记录）。",
            shown(&verdicts["aggregate"]["blockers"]),
            verdicts["aggregate"]["record_and_verdict_disagreements"]
                .as_array()
                .map_or(0, Vec::len),
        ),
    );
    lines.push(String::new());
    push(
        &mut lines,
        &format!(
            "`*_term` 两列抄的是记录自己的 `block_form` 词（number / tag / absent），不是模型造的词：{}。\
             模型在 `src/` 下的代码不写标签的那个词 —— §20 的禁令由 `crates/pipeline/tests/state_is_always_pinned.rs` \
             逐个 `src/` 文件扫描，一份诊断文件不豁免。",
            term_echo(verdicts),
        ),
    );
    lines.push(String::new());

    push(&mut lines, "## 这套证据回答不了什么");
    lines.push(String::new());
    push(
        &mut lines,
        "四级判定的最后一级问的是「消费方读的那一刻，链上状态有没有动」和「答案事后由谁持有」。前者要比对两个时刻的状态，这段代码没有这种机制；\
         后者在 RPC 调用记录里没有对应字段。本轮被禁止改生产语义（§3），所以这两项在表里始终是 `null`，不是 `false`。同理，M8.4.2 的记录里没有两次答案的字节，\
         因此每一行的 `same_value` 都是 `null`：同一个请求被证明问了两次，不等于同一个答案被拿到了两次。",
    );
    lines.push(String::new());
    push(
        &mut lines,
        "本轮没有为补齐这些字段再跑一次实验（§11）。要真正回答它们，需要一段能被重复运行的真实执行窗：签名与广播都不在本轮授权范围内。",
    );
    lines.push(String::new());

    push(&mut lines, "## 怎么读锚点");
    lines.push(String::new());
    push(
        &mut lines,
        &format!(
            "每一行都带 `evidence_refs`：`source` 为 `code` 的指向 `{MODEL_FILE}` 之外某个文件生产区间的唯一一行，`test` 指向一个测试文件里的唯一一行，\
             `run_record` 指向一份已提交的证据文件。行号由 `{WRITER}` 现场解析，不是手抄的；代码搬家而表没刷新，门禁就会失败。",
        ),
    );
    lines.push(String::new());
    push(
        &mut lines,
        &format!(
            "本目录的 {} 份文件由一次装配写出；重新生成要显式设 `M843_STATE_OWNERSHIP_REFRESH=1`。运行记录在 `{RECORDS_DIR}/`，本目录只读不写它们。",
            GENERATED_FILES.len(),
        ),
    );
    lines.push(String::new());
    push(&mut lines, "## 文件");
    lines.push(String::new());
    for name in GENERATED_FILES {
        lines.push(format!("- `{name}`"));
    }
    lines.push(String::new());
    push(
        &mut lines,
        &format!(
            "测量这些记录的三把 live run 的配置与代码版本，逐把抄在 `{REUSE_VERDICTS_FILE}` 的 `assembled_from` 里（build `{}`、模式 `{}`、\
             `state_read_reuse` {}、配置并发 {}、真实套利 {}）；本目录没有为它们再跑一次，也没有跑任何新实验。",
            verdicts["assembled_from"][0]["git_revision"],
            verdicts["assembled_from"][0]["execution_mode"],
            shown(&verdicts["assembled_from"][0]["state_read_reuse"]),
            shown(&verdicts["assembled_from"][0]["configured_concurrency"]),
            shown(&verdicts["assembled_from"][0]["successful_real_arbitrage"]),
        ),
    );
    lines.push(String::new());
    push(
        &mut lines,
        &format!(
            "判决共 {} 行，每行至少点出一个拒绝原因：没有任何 blocker 的行 {} 行。",
            verdict_rows.len(),
            shown(&verdicts["aggregate"]["rows_without_a_blocker"]),
        ),
    );
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// the assembly
// ---------------------------------------------------------------------------

/// The whole tree: four tables from the model and one committed record file, and the README printed
/// out of those four files as just written.
fn assemble_to(dir: &Path) -> Vec<String> {
    let mut tree = Tree::new();
    write_table(
        &dir.join(OWNERSHIP_MATRIX_FILE),
        &ownership_matrix(&mut tree),
    );
    write_table(
        &dir.join(LIFECYCLE_CONTRACTS_FILE),
        &lifecycle_table(&mut tree),
    );
    write_table(
        &dir.join(STAGE_DEPENDENCY_MATRIX_FILE),
        &stage_dependency_matrix(&mut tree),
    );
    write_table(&dir.join(REUSE_VERDICTS_FILE), &reuse_verdicts(&mut tree));

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

fn fresh_assembly(name: &str) -> (PathBuf, Vec<String>) {
    let dir = scratch(name);
    let names = assemble_to(&dir);
    (dir, names)
}

fn assert_the_listing(names: &[String]) {
    let mut expected: Vec<String> = GENERATED_FILES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    expected.sort();
    assert_eq!(
        names, &expected,
        "not the file set one assembly of the model and the records writes"
    );
}

fn committed_listing() -> (Vec<String>, Vec<String>) {
    let root = evidence_dir();
    let entries =
        std::fs::read_dir(&root).unwrap_or_else(|error| panic!("{}: {error}", root.display()));
    let mut files: Vec<String> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();
    for entry in entries.filter_map(|entry| entry.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.path().is_dir() {
            dirs.push(name);
        } else {
            files.push(name);
        }
    }
    files.sort();
    dirs.sort();
    (files, dirs)
}

/// The committed root's four tables, read back so every gate grades published figures rather than
/// locals.
fn committed_root() -> Value {
    read_root(&evidence_dir())
}

/// Every `evidence_refs` array in a table, wherever it sits: rows, rules, tiers, sub-objects.
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

fn published_refs(tables: &[&Value]) -> usize {
    let mut arrays = Vec::new();
    for table in tables {
        all_ref_arrays(table, &mut arrays);
    }
    arrays.iter().map(Vec::len).sum()
}

/// The anchors the model declares, counted the same way the assembly walks them.
fn declared_anchors() -> usize {
    contracts()
        .iter()
        .map(|contract| {
            contract.anchors.len()
                + contract.freshness.anchors.len()
                + contract.invalidation.anchors.len()
        })
        .sum::<usize>()
        + lifecycle_rows()
            .iter()
            .map(|row| row.anchors.len())
            .sum::<usize>()
        + stage_edges()
            .iter()
            .map(|edge| edge.anchors.len())
            .sum::<usize>()
}

// ---------------------------------------------------------------------------
// gates
// ---------------------------------------------------------------------------

/// §11's 「每项契约必须能追溯到实际代码、测试或已提交的证据」 as bytes: the committed directory is
/// one assembly of the model and the records, and nothing else is in it.
#[test]
fn the_committed_directory_is_a_reassembly_of_the_model_and_the_records() {
    let dir = scratch("byte-gate");
    let names = assemble_to(&dir);
    assert_the_listing(&names);

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

    let (committed_files, committed_dirs) = committed_listing();
    assert_eq!(
        committed_files, names,
        "{EVIDENCE_DIR}: the committed file set is not what one assembly writes"
    );
    assert!(
        committed_dirs.is_empty(),
        "{EVIDENCE_DIR}: a subdirectory appeared ({committed_dirs:?}) — the four tables are \
         assembled flat, and a directory here would be read by no gate at all"
    );
    for name in &names {
        assert_eq!(
            read_bytes(&dir.join(name)),
            read_bytes(&evidence_dir().join(name)),
            "{name} differs from a fresh assembly: either a figure in it was edited, or \
             `{MODEL_FILE}` or the source records changed after it was written and the tables have \
             to be refreshed from what measures them"
        );
    }
}

/// The same assembly twice. A table stamped with the moment of writing, or one whose rows came out
/// in hash order, would pass the gate above every time nobody ran it twice.
#[test]
fn two_assemblies_write_byte_identical_files() {
    let (first, names) = fresh_assembly("repeat-a");
    let (second, second_names) = fresh_assembly("repeat-b");
    assert_eq!(
        names, second_names,
        "two assemblies wrote different file sets"
    );
    for name in &names {
        assert_eq!(
            read_bytes(&first.join(name)),
            read_bytes(&second.join(name)),
            "{name} differs between two assemblies of the same model and records"
        );
    }
}

/// Every anchor the model declares resolves, and the published tables carry exactly that many
/// refs — plus the two each measured row adds. §10's 「报告必须附上实际代码位置或测试证据」 is a
/// counting statement before it is a reading one: a claim dropped between the model and a table is
/// a claim that stopped being checkable.
#[test]
fn every_anchor_resolves_and_the_tables_carry_every_one_of_them() {
    let root = committed_root();
    let model_tables = [
        root_table(&root, OWNERSHIP_MATRIX_FILE),
        root_table(&root, LIFECYCLE_CONTRACTS_FILE),
        root_table(&root, STAGE_DEPENDENCY_MATRIX_FILE),
    ];
    let declared = declared_anchors();
    let in_model_tables = published_refs(&model_tables);
    assert_eq!(
        in_model_tables, declared,
        "the three model tables print {in_model_tables} refs while the model declares {declared}"
    );
    let verdicts = root_table(&root, REUSE_VERDICTS_FILE);
    let candidates = verdicts["aggregate"]["candidates"].as_u64().unwrap_or(0);
    assert_eq!(
        published_refs(&[verdicts]),
        candidates as usize * REFS_PER_MEASURED_ROW,
        "each measured row carries one record ref and one category ref"
    );

    let mut kinds: BTreeMap<String, u64> = BTreeMap::new();
    let mut arrays = Vec::new();
    for table in &model_tables {
        all_ref_arrays(table, &mut arrays);
    }
    for list in &arrays {
        for reference in list {
            let source = reference["source"]
                .as_str()
                .unwrap_or_else(|| panic!("a published ref carries no `source`: {reference}"));
            *kinds.entry(source.to_string()).or_insert(0) += 1;
            assert!(
                reference["file"].is_string() && reference["token"].is_string(),
                "a published ref is missing its file or token: {reference}"
            );
            assert!(
                reference["line"].as_u64().is_some(),
                "a published ref has no resolved line: {reference}"
            );
        }
    }
    // All three of §10's evidence shapes have to be in use, or one of them is a word nobody meant.
    for source in [
        SourceKind::Code.as_str(),
        SourceKind::Test.as_str(),
        SourceKind::RunRecord.as_str(),
    ] {
        assert!(
            kinds.contains_key(source),
            "no `{source}` anchor in the model tables: §10 asks for code positions *or* test \
             evidence *or* committed records, and a shape that appears nowhere is a claim nobody \
             supported that way"
        );
    }

    // Re-resolve every published ref against the files as they are now, so a stale line number
    // cannot pass on the strength of having once been right.
    let mut tree = Tree::new();
    for list in &arrays {
        for reference in list {
            let file = reference["file"].as_str().unwrap_or_default();
            let token = reference["token"].as_str().unwrap_or_default();
            let source = reference["source"].as_str().unwrap_or_default();
            let line = reference["line"].as_u64().unwrap_or(0);
            let limit = if source == SourceKind::Code.as_str() {
                production_end(tree.read(file))
            } else {
                tree.read(file).len()
            };
            let hits = matches(tree.read(file), limit, token);
            assert!(
                hits.contains(&(line as usize)),
                "a committed ref points at {file}:{line} for {token:?}, and that line no longer \
                 holds the token (it matches {hits:?}) — the code moved and the evidence has to be \
                 refreshed from the code that now holds"
            );
        }
    }
}

/// §10's rule that a claim is never a sentence alone: any row, rule or step whose status is
/// `proven` *or* `partially_proven` carries at least one resolved ref somewhere beneath it, and the
/// two statuses that may go without a source are `unknown` and `not_applicable`.
#[test]
fn a_proven_claim_always_opens_a_real_file() {
    let root = committed_root();
    let mut proven_rows = 0;
    for name in [
        OWNERSHIP_MATRIX_FILE,
        LIFECYCLE_CONTRACTS_FILE,
        STAGE_DEPENDENCY_MATRIX_FILE,
    ] {
        for row in rows_of(root_table(&root, name)) {
            let mut statuses = Vec::new();
            collect_strings(&row, "status", &mut statuses);
            collect_strings(&row, "ownership_status", &mut statuses);
            collect_strings(&row, "contract_status", &mut statuses);
            if !statuses
                .iter()
                .any(|word| word == "proven" || word == "partially_proven")
            {
                continue;
            }
            let mut arrays = Vec::new();
            all_ref_arrays(&row, &mut arrays);
            assert!(
                arrays.iter().any(|list| !list.is_empty()),
                "{name} row {} claims {:?} with no evidence_ref to open — a partial claim is \
                 still a claim, and only `unknown` and `not_applicable` may be written without a \
                 source",
                row["id"]
                    .as_str()
                    .or_else(|| row["state_kind"].as_str())
                    .unwrap_or("?"),
                statuses
            );
            proven_rows += 1;
        }
    }
    assert!(
        proven_rows > 0,
        "not one `proven` claim anywhere in the tables — the gate above would pass on an empty \
         tree, and §4's declarations are meant to be checkable"
    );
}

/// §11's 「聚合结果必须能从原始数据重新计算」 inside each file: the published aggregates are the
/// published rows counted, so no figure was written beside a table instead of out of it.
#[test]
fn the_aggregates_are_the_published_rows_counted() {
    let root = committed_root();

    let matrix = root_table(&root, OWNERSHIP_MATRIX_FILE);
    let rows = rows_of(matrix);
    assert_eq!(
        matrix["categories"].as_u64(),
        Some(rows.len() as u64),
        "the matrix's `categories` is not the length of its own rows"
    );
    assert_eq!(
        matrix["aggregate"]["ownership_status"],
        json!(count_by(&rows, "ownership_status")),
        "the ownership_status aggregate is not the rows counted"
    );
    assert_eq!(
        matrix["aggregate"]["safe_to_reuse_now_tiers_holding_true"].as_u64(),
        Some(
            rows.iter()
                .filter(|row| row["reuse_status"][TIER_WORDS[3]]["holds"].as_bool() == Some(true))
                .count() as u64
        ),
        "the count of categories whose fourth tier holds is not the rows counted"
    );

    let lifecycle = root_table(&root, LIFECYCLE_CONTRACTS_FILE);
    let life_rows = rows_of(lifecycle);
    assert_eq!(
        lifecycle["rows_declared"].as_u64(),
        Some(life_rows.len() as u64),
        "the lifecycle table's row count is not the length of its rows"
    );
    assert_eq!(
        life_rows.len(),
        kinds().len() * 5,
        "§5's five steps times {} categories is what this table is",
        kinds().len()
    );
    assert_eq!(
        lifecycle["aggregate"]["status"],
        json!(count_by(&life_rows, "status")),
        "the lifecycle status aggregate is not the rows counted"
    );
    assert_eq!(
        lifecycle["aggregate"]["by_step"],
        json!(count_by(&life_rows, "step")),
        "the per-step split is not the rows counted"
    );

    let edges = root_table(&root, STAGE_DEPENDENCY_MATRIX_FILE);
    let edge_rows = rows_of(edges);
    assert_eq!(
        edges["edges"].as_u64(),
        Some(edge_rows.len() as u64),
        "the edge table's count is not the length of its rows"
    );
    assert_eq!(
        edges["aggregate"]["decision"],
        json!(count_by(&edge_rows, "decision")),
        "the decision aggregate is not the rows counted"
    );
    let total: u64 = edge_rows
        .iter()
        .map(|row| row["measured_pairs"].as_u64().unwrap_or(0))
        .sum();
    assert_eq!(
        edges["measured_pairs_total"].as_u64(),
        Some(total),
        "the measured-pair total is not the rows added"
    );
    let preflight_to_build = pairs_on(&edge_rows, PREFLIGHT, BUILD);
    assert_eq!(
        edges["measured_pairs_preflight_to_build"].as_u64(),
        Some(preflight_to_build),
        "§6's twelve pairs are not what the edge table's own rows add up to"
    );

    let verdicts = root_table(&root, REUSE_VERDICTS_FILE);
    let verdict_rows = rows_of(verdicts);
    assert_eq!(
        verdicts["aggregate"]["candidates"].as_u64(),
        Some(verdict_rows.len() as u64),
        "the candidate count is not the length of the verdict rows"
    );
    assert_eq!(
        verdicts["aggregate"]["by_state_kind"],
        json!(count_by(&verdict_rows, "state_kind")),
        "the per-category split is not the rows counted"
    );
    for key in ["by_state_kind", "by_producer_consumer"] {
        let split = verdicts["aggregate"][key]
            .as_object()
            .cloned()
            .unwrap_or_default();
        let summed: u64 = split.values().filter_map(Value::as_u64).sum();
        assert_eq!(
            summed,
            verdict_rows.len() as u64,
            "the `{key}` split adds up to {summed} while the table has {} rows — a grouped \
             count is a row count, not an occurrence count",
            verdict_rows.len()
        );
    }
    assert_eq!(
        verdicts["aggregate"]["blockers"],
        json!(blocker_counts(&verdict_rows)),
        "the blocker aggregate is not the rows counted"
    );
    let counts = blocker_counts(&verdict_rows);
    let top = counts.values().max().copied().unwrap_or(0);
    let tied: Vec<Value> = counts
        .iter()
        .filter(|(_, count)| **count == top)
        .map(|(word, _)| json!(word))
        .collect();
    assert_eq!(
        verdicts["aggregate"]["blockers_at_the_top"],
        json!({"rows": top, "count": tied.len(), "blockers": tied}),
        "the top band of refusals is not the rows counted — the README prints this band as the \
         reason no candidate is safe to reuse, so naming one winner out of a tie would be a claim \
         the rows do not make"
    );
    for tier in TIER_WORDS {
        assert_eq!(
            verdicts["aggregate"][tier],
            json!(count_tier(&verdict_rows, tier)),
            "the `{tier}` tier counts are not the rows counted"
        );
    }
    assert_eq!(
        verdicts["aggregate"]["safe_to_reuse_now_true"].as_u64(),
        Some(
            verdict_rows
                .iter()
                .filter(|row| row["assessment"]["safe_to_reuse_now"].as_bool() == Some(true))
                .count() as u64
        ),
        "the headline `safe_to_reuse_now` count is not the rows counted"
    );
}

/// The field that makes a row unique in this table. The stage matrix cannot be keyed by category:
/// §4's eleven categories legitimately face a stage more than once, and each of those meetings is
/// a different question with a different decision.
fn row_key(name: &str, row: &Value) -> String {
    match name {
        LIFECYCLE_CONTRACTS_FILE => format!(
            "{} / {}",
            row["state_kind"].as_str().unwrap_or("?"),
            row["step"].as_str().unwrap_or("?")
        ),
        REUSE_VERDICTS_FILE => row["candidate_id"].as_str().unwrap_or("?").to_string(),
        STAGE_DEPENDENCY_MATRIX_FILE => row["id"].as_str().unwrap_or("?").to_string(),
        _ => row["state_kind"].as_str().unwrap_or("?").to_string(),
    }
}

/// The three model tables and the measured table cover exactly what they claim: eleven categories,
/// one row each, and every carried candidate listed once.
#[test]
fn every_category_and_every_candidate_is_listed_once() {
    let root = committed_root();
    let matrix = root_table(&root, OWNERSHIP_MATRIX_FILE);
    let declared: BTreeSet<String> = ALL_KINDS
        .iter()
        .map(|kind| kind.as_str().to_string())
        .collect();
    let published: BTreeSet<String> = rows_of(matrix)
        .iter()
        .filter_map(|row| row["state_kind"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        published, declared,
        "§4's eleven categories and the matrix's rows are not the same set"
    );

    for name in [
        OWNERSHIP_MATRIX_FILE,
        LIFECYCLE_CONTRACTS_FILE,
        STAGE_DEPENDENCY_MATRIX_FILE,
        REUSE_VERDICTS_FILE,
    ] {
        let rows = rows_of(root_table(&root, name));
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for row in &rows {
            let id = row_key(name, row);
            assert!(
                seen.insert(id.clone()),
                "{name}: {id} is listed twice — a table that repeats a row doubles whatever \
                 aggregate counts it"
            );
        }
    }
}

/// §12.14's rule at the evidence layer, on the published rows: no measured candidate reaches the
/// fourth tier, and no refusal is silent.
#[test]
fn no_measured_candidate_reaches_the_fourth_tier_and_every_refusal_is_named() {
    let root = committed_root();
    let verdicts = root_table(&root, REUSE_VERDICTS_FILE);
    let rows = rows_of(verdicts);
    assert!(!rows.is_empty(), "the measured table has no rows to grade");
    for row in &rows {
        let id = row["candidate_id"].as_str().unwrap_or("?");
        assert!(
            row["assessment"]["safe_to_reuse_now"].as_bool() != Some(true),
            "{id}: a measured pair is published as safe to reuse now, which §14 forbids reading \
             out of a record"
        );
        let blockers = row["assessment"]["blockers"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            !blockers.is_empty(),
            "{id}: refused with no named condition"
        );
        assert_eq!(
            row["record_reusable"].as_str(),
            Some("unknown"),
            "{id}: M8.4.2 recorded this candidate as {:?}",
            row["record_reusable"]
        );
    }
    assert_eq!(
        verdicts["aggregate"]["safe_to_reuse_now_true"].as_u64(),
        Some(0),
        "the published headline is not zero"
    );
    assert_eq!(
        verdicts["aggregate"]["rows_without_a_blocker"].as_u64(),
        Some(0),
        "some row was refused without naming why"
    );
    assert_eq!(
        verdicts["aggregate"]["record_and_verdict_disagreements"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "this milestone's verdict and the record's disagree on a row — both are computed from the \
         same pair, so a disagreement is a rule that drifted"
    );
    assert_eq!(
        verdicts["input"]["unmapped_to_a_category"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "a measured ask belongs to none of §4's eleven categories, which §4 says cannot happen"
    );
}

/// §10's vocabularies are finite and §17's options are four: every status, identity and decision
/// cell in the published tables is a word the model derives, and a row's letter and word are the
/// same option.
#[test]
fn every_table_uses_the_declared_vocabulary() {
    let root = committed_root();
    for name in [
        OWNERSHIP_MATRIX_FILE,
        LIFECYCLE_CONTRACTS_FILE,
        STAGE_DEPENDENCY_MATRIX_FILE,
    ] {
        let table = root_table(&root, name);
        let mut statuses = Vec::new();
        collect_strings(table, "status", &mut statuses);
        collect_strings(table, "ownership_status", &mut statuses);
        collect_strings(table, "contract_status", &mut statuses);
        assert!(
            !statuses.is_empty(),
            "{name} prints no status at all — a table whose vocabulary check passes on an empty \
             list is not a check"
        );
        for word in statuses {
            assert!(
                STATUS_WORDS.contains(&word.as_str()),
                "{name} prints `{word}`, which is not one of §10's four statuses"
            );
        }
        let mut identity = Vec::new();
        collect_strings(table, "chain_id", &mut identity);
        collect_strings(table, "block_number", &mut identity);
        collect_strings(table, "block_hash", &mut identity);
        collect_strings(table, "block_tag_semantics", &mut identity);
        for word in identity {
            assert!(
                IDENTITY_WORDS.contains(&word.as_str()),
                "{name} prints the identity word `{word}`, which the model does not derive"
            );
        }
    }

    let edges = root_table(&root, STAGE_DEPENDENCY_MATRIX_FILE);
    for row in rows_of(edges) {
        let id = row["id"].as_str().unwrap_or("?");
        let letter = row["decision_letter"].as_str().unwrap_or_default();
        let expected = match letter {
            "A" => EdgeDecision::ControlledExperimentDefinable,
            "B" => EdgeDecision::ContractDesignFirst,
            "C" => EdgeDecision::MustRefetch,
            "D" => EdgeDecision::InsufficientEvidence,
            other => panic!("edge {id} decides with {other:?}, and §17 offers four options"),
        };
        assert_eq!(
            row["decision"].as_str(),
            Some(expected.as_str()),
            "edge {id}: the letter {letter} and the word {:?} are not the same option",
            row["decision"].as_str()
        );
        assert_eq!(
            row["decision"].as_str(),
            Some(expected.as_str()),
            "{id}: the row's letter says {letter}, which the model spells `{}`, while the table \
             prints {:?} — §17's option is one answer stated two ways, and the two copies disagree",
            expected.as_str(),
            row["decision"].as_str(),
        );
        let status = row["contract_status"].as_str().unwrap_or_default();
        assert!(
            STATUS_WORDS.contains(&status),
            "{id}: contract status `{status}` is not one of §10's four"
        );
    }
}

/// §6's per-item rule read out of the published rows: the four preflight → build flows are four
/// separate rows, and the header family splits between an input and a check rather than answering
/// the whole category at once.
#[test]
fn the_preflight_to_build_flows_are_four_rows_and_do_not_answer_for_each_other() {
    let root = committed_root();
    let rows = rows_of(root_table(&root, STAGE_DEPENDENCY_MATRIX_FILE));
    let crossing: Vec<&Value> = rows
        .iter()
        .filter(|row| {
            row["producer_stage"].as_str() == Some(PREFLIGHT)
                && row["consumer_stage"].as_str() == Some(BUILD)
        })
        .collect();
    // A row that carries a measured duplicate names the method it was measured on. A row that only
    // asks a question on this crossing has no method and no pair, so it cannot inflate §6's twelve.
    let (measured, questions): (Vec<&Value>, Vec<&Value>) = crossing
        .into_iter()
        .partition(|row| row["measured_pairs"].as_u64().unwrap_or(0) > 0);
    assert_eq!(
        measured.len(),
        4,
        "§6's twelve pairs are four flows, and the rows that carry a count here are {:?}",
        measured
            .iter()
            .map(|row| row["id"].as_str().unwrap_or("?"))
            .collect::<Vec<_>>()
    );
    let mut methods: BTreeSet<String> = BTreeSet::new();
    for row in &measured {
        let id = row["id"].as_str().unwrap_or("?");
        let method = row["method"].as_str().unwrap_or("?").to_string();
        assert!(
            methods.insert(method.clone()),
            "{id}: two rows on this crossing measure {method}, so one flow would answer for two \
             questions and §6 asks them separately"
        );
        assert_eq!(
            row["measured_pairs"].as_u64(),
            Some(3),
            "{id}: §11's arm is three live runs, so a flow carries three pairs"
        );
        assert!(
            row["sub_questions"]["safety_constraint_served"]
                .as_str()
                .unwrap_or_default()
                != "none",
            "{id}: judged without stating which safety constraint the read serves, which is \
             §6's 「不能为了减少 RPC 而删除执行前安全检查」 arriving as a table cell"
        );
    }
    for row in &questions {
        let id = row["id"].as_str().unwrap_or("?");
        assert_eq!(
            row["method"],
            Value::Null,
            "{id}: a row on this crossing with no measured pair still names a method, which \
             means the count above missed a duplicate rather than this edge asking no question"
        );
    }
    assert_eq!(
        pairs_on(&rows, PREFLIGHT, BUILD),
        12,
        "§6's twelve pairs are what the rows on this crossing add up to, and they do not"
    );
    // The header category answers for two different edges with two different decisions, so a
    // reader cannot take "headers are shareable" as a property of the category.
    let headers: Vec<&Value> = rows
        .iter()
        .filter(|row| row["state_kind"].as_str() == Some("block_header"))
        .collect();
    let decisions: BTreeSet<String> = headers
        .iter()
        .map(|row| {
            row["decision_letter"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert!(
        decisions.len() > 1,
        "every block_header edge decided the same way ({decisions:?}) — §6 forbids one category \
         answering for all of its edges, and the header family is exactly where an input and a \
         check part company"
    );
}

/// §11's chain of custody: a measured verdict names the record file and the row it came from, and
/// the provenance block is the runs' own, copied rather than rewritten.
#[test]
fn the_measured_rows_point_back_at_the_records_they_came_from() {
    let root = committed_root();
    let verdicts = root_table(&root, REUSE_VERDICTS_FILE);
    let records = read_json(&records_dir().join(CANDIDATES_FILE));
    assert_eq!(
        verdicts["assembled_from"], records["assembled_from"],
        "the provenance block is not the record's own — §11 says reuse M8.4.2's runs, and a \
         rewritten provenance is a run nobody measured"
    );
    assert_eq!(
        verdicts["input"]["pairs"], records["pairs"],
        "the input pair count is not the record's"
    );
    assert_eq!(
        verdicts["input"]["duplicate_candidates"], records["candidates"],
        "the record's candidate count and this table's input disagree"
    );
    let record_rows = rows_of(&records);
    let exact = record_rows
        .iter()
        .filter(|row| row["duplicate_type"].as_str() == Some(DUPLICATE_EXACT))
        .count();
    assert_eq!(
        verdicts["input"]["carried"].as_u64(),
        Some(exact as u64),
        "the carried rows are not the record's exact duplicates"
    );
    let by_id: BTreeMap<&str, &Value> = record_rows
        .iter()
        .map(|row| (row["candidate_id"].as_str().unwrap_or_default(), row))
        .collect();
    for row in rows_of(verdicts) {
        let source = &row["source_record"];
        assert_eq!(
            source["file"].as_str(),
            Some(CANDIDATES_RECORD),
            "a measured row cites a different source than the record file"
        );
        assert_eq!(
            source["candidate_id"], row["candidate_id"],
            "a measured row's source id and its own id disagree"
        );
        // The echoed block terms are the record's, field for field, and the kind this table
        // prints is the record's own `block_form` word — the model claims no more than that, and
        // a fourth word in the data would stop here rather than be filed under a tag.
        let record = by_id
            .get(row["candidate_id"].as_str().unwrap_or_default())
            .unwrap_or_else(|| panic!("a verdict row cites a candidate id no record row carries"));
        for side in ["producer", "consumer"] {
            let form = format!("{side}_block_form");
            let block = format!("{side}_block");
            assert_eq!(
                source[form.as_str()],
                record[form.as_str()],
                "the echoed {form} is not the record's own field"
            );
            assert_eq!(
                source[block.as_str()],
                record[block.as_str()],
                "the echoed {block} is not the record's own field — the tag word a reader needs \
                 to tell two tags apart has to come from the record, since source code may not \
                 type it"
            );
            assert_eq!(
                row[format!("{side}_term").as_str()],
                record[form.as_str()],
                "the printed kind of {side} term is not the record's `block_form` word"
            );
        }
        let refs = row["evidence_refs"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            refs[0]["file"].as_str(),
            Some(CANDIDATES_RECORD),
            "the first ref of a measured row has to be the record it came from"
        );
        assert_eq!(
            refs[0]["token"], row["candidate_id"],
            "the record ref points at a different id than the row's"
        );
    }
}

/// The boundary §3 and §14 draw, read out of the published tree: nothing here holds a host name, a
/// private key, a key field, a submission method, or a non-integer figure.
#[test]
fn the_directory_holds_no_secret_no_host_no_submission_and_no_float() {
    let files = walk_files(&evidence_dir());
    assert_eq!(
        files.len(),
        GENERATED_FILES.len(),
        "{EVIDENCE_DIR} walks to {} files while one assembly writes {}",
        files.len(),
        GENERATED_FILES.len()
    );
    for path in &files {
        let shown = path.to_string_lossy().to_string();
        let text = read_text(path);
        for forbidden in [
            "giwa.io",
            "\"eth_sendRawTransaction\"",
            "\"private_key\"",
            "-----BEGIN",
        ] {
            assert!(
                !text.contains(forbidden),
                "{shown}: contains {forbidden:?} — a host name, a submission method, a key field \
                 or a key body has no business in diagnostic evidence"
            );
        }
        assert!(
            !holds_a_private_key(&text),
            "{shown}: carries a 64-hex-digit run, which is the shape of a private key. A key \
             belongs in an environment variable at sign time and nowhere else"
        );
        if shown.ends_with(".json") {
            let value = read_json(path);
            let mut floats = Vec::new();
            find_floats(&value, &mut floats);
            assert!(
                floats.is_empty(),
                "{shown}: a non-integer JSON number {floats:?} — every figure in these tables is a \
                 count, so a reader can add them up"
            );
        }
    }
}

/// A 64-hex-digit run anywhere in a text — the shape of a private key, and a check that does not
/// need to name the one key this repository has.
fn holds_a_private_key(text: &str) -> bool {
    let bytes = text.as_bytes();
    let window = 64;
    if bytes.len() < window {
        return false;
    }
    bytes
        .windows(window)
        .any(|window| window.iter().all(u8::is_ascii_hexdigit))
}

fn find_floats(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Number(number) => {
            if number.is_f64() {
                out.push(number.to_string());
            }
        }
        Value::Object(map) => {
            for child in map.values() {
                find_floats(child, out);
            }
        }
        Value::Array(values) => {
            for child in values {
                find_floats(child, out);
            }
        }
        _ => {}
    }
}

/// The README is generated out of the four tables and says what they say — and nothing beyond it.
/// §3's ban on a cache and §14's refusal to make 「减少 RPC 次数」 a success criterion are checked as
/// words, because a milestone that recommends its own conclusion has stopped being a diagnosis.
#[test]
fn the_readme_reports_the_tables_and_recommends_nothing() {
    let (dir, _) = fresh_assembly("readme-words");
    let root = read_root(&dir);
    let text = read_text(&dir.join(README_FILE));
    for name in GENERATED_FILES {
        assert!(
            text.contains(name),
            "the README never names `{name}`, so a reader holding the directory cannot find it"
        );
    }
    let matrix = root_table(&root, OWNERSHIP_MATRIX_FILE);
    let edges = root_table(&root, STAGE_DEPENDENCY_MATRIX_FILE);
    let verdicts = root_table(&root, REUSE_VERDICTS_FILE);
    for sentence in [
        format!(
            "- {} 条边的判决（§17 的四个选项，没有第五种）：A {}、B {}、C {}、D {}。",
            shown(&edges["edges"]),
            count_word(edges, "decision_letter", "A"),
            count_word(edges, "decision_letter", "B"),
            count_word(edges, "decision_letter", "C"),
            count_word(edges, "decision_letter", "D"),
        ),
        format!(
            "**{}** 条到达 `safe_to_reuse_now`。",
            shown(&verdicts["aggregate"]["safe_to_reuse_now_true"])
        ),
        format!(
            "`ownership_status`：proven {} / partially_proven {} / unknown {}；其中命名不出 owner 的 {} 类。",
            count_word(matrix, "ownership_status", "proven"),
            count_word(matrix, "ownership_status", "partially_proven"),
            count_word(matrix, "ownership_status", "unknown"),
            shown(&matrix["aggregate"]["categories_without_a_named_owner"]),
        ),
        format!(
            "preflight → build {} 对",
            shown(&edges["measured_pairs_preflight_to_build"])
        ),
        format!(
            "`*_term` 两列抄的是记录自己的 `block_form` 词（number / tag / absent），不是模型造的词：{}。",
            term_echo(verdicts),
        ),
    ] {
        assert!(
            text.contains(&sentence),
            "the README does not say 「{sentence}」, though the tables print it"
        );
    }
    for forbidden in [
        "建议加缓存",
        "应当实现缓存",
        "可以安全复用",
        "应该共享",
        "should cache",
        "add a cache",
        "reuse is safe",
        "省下",
        "减少 RPC 次数",
        "节省",
    ] {
        assert!(
            !text.contains(forbidden),
            "the README says 「{forbidden}」 — the duplicates, the contracts and the verdicts may be \
             stated and nothing beyond them; §3 forbids the optimisation and §14 forbids making it \
             the success criterion"
        );
    }
    assert!(
        text.contains("只记录，不消除"),
        "the README never states the boundary this milestone works inside"
    );
}
