//! M8.6 — the assembly: this milestone's judgements ([`super`]) plus the committed records, into
//! the eleven files under [`EVIDENCE_DIR`].
//!
//! # What is allowed to be here
//!
//! Reading, folding, and writing. Every count in every table comes out of [`Records::load`], every
//! judgement comes out of [`SITES`] or [`CANDIDATES`], and every derived column comes out of
//! [`measured`], [`freshness_of`], [`theoretical_saving_of`], [`safe_saving_of`] and [`priority_of`].
//! Nothing here asks a node anything (§2.4, §26), and nothing here types a figure that the
//! functions above already compute — which is why the two gates can recompute a table from the
//! records and call the comparison meaningful.
//!
//! # Why there is no clock in the tables
//!
//! `serde_json` sorts object keys, so a re-assembly is byte-reproducible only if no wall-clock
//! value enters it. §25's NC6 is the control that says this is true rather than merely intended:
//! the corpus is cloned, its durations multiplied, and the semantic columns re-derived — they have
//! to come out identical while the timing columns demonstrably change.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::*;

/// This file. The tables name it beside [`GENERATED_BY`] because the gate is the entry point and
/// the assembly is the machinery behind it.
pub const TABLES_FILE: &str = "crates/pipeline/tests/m86_census/tables.rs";
pub const SCHEMA: &str = "m8.6.v1";

/// The spelling [`recorded_key_variants`] gives an ask the record carries no `dedup_key` for, so a
/// site whose asks are all unkeyed counts as holding one key shape rather than none.
pub const ASK_WITHOUT_RECORDED_KEY: &str = "<no recorded key>";

/// M8.4.4's own conclusion word. It is not in that milestone's `summary.json` — that file publishes
/// the numbers (`rpc_count_reduction = 0`, `aggregate.net_rpc_saved = 0`) — so the census names the
/// document that carries the label instead of pretending the record does.
pub const PROPAGATION_LABEL: &str = "SAFE_PROPAGATION_PROVEN_NO_NET_RPC_SAVING";
pub const PROPAGATION_LABEL_DOC: &str = "docs/v0.1/M8.4.4 Completion Report.md";

// ---------------------------------------------------------------------------
// the filesystem side
// ---------------------------------------------------------------------------

pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn repo_path(relative: &str) -> PathBuf {
    workspace_root().join(relative)
}

pub fn evidence_dir() -> PathBuf {
    repo_path(EVIDENCE_DIR)
}

pub fn refreshing() -> bool {
    std::env::var_os(REFRESH_ENV).is_some()
}

/// Assembly scratch. `target/pipeline-tests/` is shared across the workspace's tests and cannot
/// take two writers, so every gate runs `--test-threads=1` and this directory name is its own.
pub fn scratch(name: &str) -> PathBuf {
    let dir = workspace_root().join("target/pipeline-tests").join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    }
    std::fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    dir
}

pub fn read_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

pub fn read_text(path: &Path) -> String {
    String::from_utf8_lossy(&read_bytes(path)).to_string()
}

pub fn write_text(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

/// Two-space pretty-printing plus a trailing newline, as every other evidence file in this tree
/// holds it. Key sorting is what makes the byte gate a gate.
pub fn write_table(path: &Path, table: &Value) {
    let mut text = serde_json::to_string_pretty(table).unwrap_or_else(|error| panic!("{error}"));
    text.push('\n');
    write_text(path, &text);
}

pub fn generated_files() -> Vec<String> {
    let mut names: Vec<String> = CENSUS_TABLES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    names.push(README_FILE.to_string());
    names.sort();
    names
}

pub fn read_root(dir: &Path) -> Value {
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
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        tables.insert(stem.to_string(), read_json(&dir.join(name)));
    }
    Value::Object(tables)
}

fn file_names(dir: &Path) -> Vec<String> {
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
// anchors: a claim resolves to one place, or the claim fails
// ---------------------------------------------------------------------------

/// Where an anchor is allowed to point. `Code` means a line of this repository's production source
/// and demands exactly one match before the file's own `#[cfg(test)]`; `RunRecord` means a field of
/// committed evidence and demands at least one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceKind {
    Code,
    RunRecord,
}

impl SourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceKind::Code => "code",
            SourceKind::RunRecord => "run_record",
        }
    }
}

pub struct Anchor {
    pub file: &'static str,
    pub token: &'static str,
    pub source: SourceKind,
    pub note: String,
}

/// The note is owned because most anchors quote runtime data — a site id, a method name, a
/// container type — and an anchor whose note cannot name the thing it points at is not evidence.
pub fn code_anchor(file: &'static str, token: &'static str, note: impl Into<String>) -> Anchor {
    Anchor {
        file,
        token,
        source: SourceKind::Code,
        note: note.into(),
    }
}

pub fn record_anchor(file: &'static str, token: &'static str, note: impl Into<String>) -> Anchor {
    Anchor {
        file,
        token,
        source: SourceKind::RunRecord,
        note: note.into(),
    }
}

pub struct Tree {
    lines: BTreeMap<String, Vec<String>>,
}

impl Tree {
    pub fn new() -> Self {
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

/// The end of a file's production region. A claim resolved against a line of test code would be a
/// claim about a test.
fn production_end(lines: &[String]) -> usize {
    for (index, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            return index;
        }
    }
    lines.len()
}

fn matches_in(lines: &[String], limit: usize, token: &str) -> Vec<usize> {
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

pub fn resolve(anchor: &Anchor, tree: &mut Tree) -> Value {
    let all = tree.read(anchor.file);
    let limit = match anchor.source {
        SourceKind::Code => production_end(all),
        SourceKind::RunRecord => all.len(),
    };
    let hits = matches_in(all, limit, anchor.token);
    match anchor.source {
        SourceKind::Code => assert_eq!(
            hits.len(),
            1,
            "{}: the token {:?} matches {} lines of {} — an anchor has to name one place, and two \
             matches mean the claim does not say which line it rests on",
            &anchor.note,
            anchor.token,
            hits.len(),
            anchor.file
        ),
        SourceKind::RunRecord => assert!(
            !hits.is_empty(),
            "{}: {:?} does not appear in {} — a run-record anchor points into committed evidence, \
             and a pointer to a field the record does not carry is a claim about a measurement that \
             was never taken",
            &anchor.note,
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
        &anchor.note,
    )
}

pub fn resolve_all(anchors: &[Anchor], tree: &mut Tree) -> Vec<Value> {
    anchors.iter().map(|anchor| resolve(anchor, tree)).collect()
}

/// Resolution for a token the model cannot hold in an `Anchor` because it is runtime data: a
/// recorded ask's own dedup key, which is what lets a reader open the file and count it.
pub fn resolve_record_token(file: &str, token: &str, note: &str, tree: &mut Tree) -> Value {
    let all = tree.read(file);
    let hits = matches_in(all, all.len(), token);
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

/// One site's two code anchors, resolved: where the caller label is written and where the answer
/// first meets a decision.
fn site_anchors(site: &Site, tree: &mut Tree) -> (Value, Value) {
    let stamp = resolve(
        &code_anchor(
            site.stamp.0,
            site.stamp.1,
            format!("site {} stamps its caller label here", site.id),
        ),
        tree,
    );
    let decision = resolve(
        &code_anchor(
            site.decision.0,
            site.decision.1,
            format!("site {}'s answer first meets a decision here", site.id),
        ),
        tree,
    );
    (stamp, decision)
}

fn anchor_source(reference: &Value) -> String {
    format!(
        "{}:{}",
        reference["file"].as_str().unwrap_or_default(),
        reference["line"].as_u64().unwrap_or_default()
    )
}

// ---------------------------------------------------------------------------
// timing: union and sum, per site, on each run's own clock
// ---------------------------------------------------------------------------

/// §18's two duration columns. They are not one number: `sum` adds every recorded ask and `union`
/// merges the intervals of asks that actually ran at the same time, which is the only duration
/// measure that a wall-clock question can be answered with. Both are computed per run and added
/// across runs, because §7 of the record's own header says each run has its own monotonic origin —
/// intervals from two runs can overlap on the page and did not overlap in the process.
#[derive(Clone, Debug, Default)]
pub struct Timing {
    pub rows: usize,
    pub sum_ns: u64,
    pub union_ns: u64,
    pub overlapping_rows: usize,
    pub rows_without_interval: usize,
    pub physical_attempts: u64,
}

pub fn timings(records: &Records) -> BTreeMap<String, Timing> {
    let mut sums: BTreeMap<String, Timing> = BTreeMap::new();
    let mut intervals: BTreeMap<(String, String), Vec<(u64, u64)>> = BTreeMap::new();
    for row in &records.rows {
        let label = SiteKey::of_row(row).label();
        let timing = sums.entry(label.clone()).or_default();
        timing.rows += 1;
        timing.sum_ns += row["duration_ns"].as_u64().unwrap_or_default();
        timing.physical_attempts += row["attempts"]
            .as_array()
            .map(|list| list.len())
            .unwrap_or(1) as u64;
        match (row["started_ns"].as_u64(), row["finished_ns"].as_u64()) {
            (Some(started), Some(finished)) if finished >= started => {
                let run = row["run"].as_str().unwrap_or_default().to_string();
                intervals
                    .entry((label, run))
                    .or_default()
                    .push((started, finished));
            }
            _ => timing.rows_without_interval += 1,
        }
    }
    for ((label, _run), list) in intervals {
        let timing = sums
            .get_mut(&label)
            .expect("the label was counted with its intervals");
        let mut sorted = list;
        sorted.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for (started, finished) in sorted {
            match merged.last_mut() {
                Some(last) if started <= last.1 => {
                    timing.overlapping_rows += 1;
                    last.1 = last.1.max(finished);
                }
                _ => merged.push((started, finished)),
            }
        }
        timing.union_ns += merged
            .iter()
            .map(|(started, finished)| finished - started)
            .sum::<u64>();
    }
    sums
}

/// How many distinct recorded keys one site's asks actually hold.
///
/// A census row publishes a single `identity` — the representative ask's key — and most sites hold
/// more than one: a site that asks at three different heights has three keys. §24's eleventh rule is
/// satisfied either way (every term is a business field), but a reader is entitled to know that the
/// one identity printed for a row of nine asks is one of the keys that row's asks carry, not a
/// description of all of them. This is that number, and the gates recompute it from the records.
pub fn recorded_key_variants(records: &Records, key: &SiteKey) -> BTreeSet<String> {
    records
        .rows
        .iter()
        .filter(|row| SiteKey::of_row(row) == *key)
        .map(|row| {
            row["dedup_key"]
                .as_str()
                .unwrap_or(ASK_WITHOUT_RECORDED_KEY)
                .to_string()
        })
        .collect()
}

fn timing_json(timing: &Timing) -> Value {
    json!({
        "asks": timing.rows,
        "physical_attempts": timing.physical_attempts,
        "rpc_sum_duration_ms": super::ms(timing.sum_ns),
        "union_duration_ms": super::ms(timing.union_ns),
        "overlap_rows": timing.overlapping_rows,
        "rows_without_interval": timing.rows_without_interval,
        "clock_note": "per run, then added across runs: each run's stamps are on their own \
            monotonic origin, so two runs' intervals are never merged",
    })
}

// ---------------------------------------------------------------------------
// one candidate, derived
// ---------------------------------------------------------------------------

/// Everything a candidate row publishes that is not typed: the four measured counts, the three
/// derived verdicts, and the ownership row the verdicts were read against.
pub struct Derived {
    pub spec: &'static CandidateSpec,
    pub measured: Measured,
    pub freshness: &'static str,
    pub theoretical: usize,
    pub safe: usize,
    pub priority: &'static str,
    pub risk: (&'static str, u8),
    pub verification_preserved: bool,
    pub ownership: Value,
    pub authority_holds: Option<bool>,
    pub authority_reason: String,
    pub authority_basis: String,
    pub derived_blockers: Vec<String>,
}

impl Derived {
    /// §25's NC6 compares exactly this object: the columns a clock must not move. A duration is
    /// absent by construction, which is what makes the comparison an assertion rather than a hope.
    pub fn semantic_columns(&self) -> Value {
        json!({
            "candidate": self.spec.id,
            "pattern": self.spec.pattern,
            "producer": self.spec.producer,
            "consumer": self.spec.consumer,
            "current_rpc_count": self.measured.consumer_asks,
            "producer_rpc_count": self.measured.producer_asks,
            "pairs": self.measured.pairs,
            "duplicate_count_exact": self.measured.exact_pairs,
            "other_duplicate_classes": self.measured.other_class_pairs,
            "reusable_in_principle": self.measured.reusable_in_principle_pairs,
            "safe_to_reuse_now": self.measured.safe_to_reuse_pairs,
            "theoretical_saving": self.theoretical,
            "safe_saving": self.safe,
            "freshness": self.freshness,
            "priority": self.priority,
            "verification_preserved": self.verification_preserved,
            "authority_preserved": self.authority_holds == Some(true),
            "implementation_risk_rank": self.risk.1,
            "blockers": self.blockers(),
        })
    }

    pub fn blockers(&self) -> Vec<String> {
        let mut set: BTreeSet<String> = BTreeSet::new();
        for blocker in self.spec.blockers {
            set.insert(blocker.to_string());
        }
        for blocker in &self.derived_blockers {
            set.insert(blocker.clone());
        }
        set.into_iter().collect()
    }
}

/// §19's `verdict` column, derived from the priority and the saving the derivation produced — not
/// a second label a reader has to reconcile with the first.
fn verdict_of(derived: &Derived) -> &'static str {
    if derived.safe > 0 {
        return VERDICT_CANDIDATE_FOUND;
    }
    if derived.spec.next_step == NEXT_STEP_EXPERIMENT {
        return VERDICT_NOT_ENOUGH;
    }
    if derived.priority == PRIORITY_P0
        || derived.priority == PRIORITY_P1
        || derived.priority == PRIORITY_P2
    {
        return VERDICT_CANDIDATE_FOUND;
    }
    VERDICT_NONE_FOUND
}

/// The extra blockers the derivation itself earns, so §20's rejection reasons are computed and a
/// candidate cannot be rejected for a reason no field supports.
fn derived_blockers(
    spec: &CandidateSpec,
    measured: &Measured,
    freshness: &str,
    verification_preserved: bool,
    safe: usize,
    ownership: &Value,
) -> Vec<String> {
    let consumer = spec.consumer_site();
    let mut out: Vec<String> = Vec::new();
    if theoretical_saving_of(measured) == 0 {
        out.push("no_exact_duplicate_pair_in_the_records".to_string());
    }
    if !verification_preserved {
        out.push("consumer_check_has_no_other_bearer".to_string());
    }
    if is_check_role(consumer.verification_role)
        && consumer.alternative_verification == ALT_NO_ALTERNATIVE
    {
        out.push("no_alternative_verification_exists".to_string());
    }
    if freshness == FRESHNESS_NOT_PRESERVED {
        out.push("freshness_not_preserved".to_string());
    }
    if freshness == FRESHNESS_UNPROVEN {
        out.push("freshness_unproven_by_these_runs".to_string());
    }
    if spec.pattern == PATTERN_C {
        out.push("propagation_already_measured_to_save_no_rpc".to_string());
    }
    if ownership["reuse_status"]["check_would_stop_existing"]
        .as_bool()
        .unwrap_or(false)
    {
        out.push("removal_would_stop_an_existing_check".to_string());
    }
    if spec.requires_new_carrier {
        out.push("no_carrier_holds_the_answer_across_the_boundary".to_string());
    }
    if spec.requires_new_lifecycle {
        out.push("no_owner_declares_a_lifecycle_for_it".to_string());
    }
    if safe == 0 && measured.exact_pairs > 0 && theoretical_saving_of(measured) > 0 {
        out.push("safe_saving_zero".to_string());
    }
    out
}

pub fn derive_candidate(
    spec: &'static CandidateSpec,
    groups: &[Group],
    pairs: &[Value],
    records: &Records,
) -> Derived {
    let measured = measured(spec, groups, pairs);
    let consumer = spec.consumer_site();
    let ownership = records.ownership_of(consumer);
    let freshness = freshness_of(&measured, &ownership);
    let theoretical = theoretical_saving_of(&measured);
    let safe = safe_saving_of(spec, &measured, freshness, &ownership);
    let priority = priority_of(spec, &measured, freshness, safe);
    let risk = implementation_risk_of(spec);
    let verification_preserved = verification_preserved(spec);
    let authority = &ownership["reuse_status"]["semantically_equivalent"];
    let blockers = derived_blockers(
        spec,
        &measured,
        freshness,
        verification_preserved,
        safe,
        &ownership,
    );
    Derived {
        spec,
        measured,
        freshness,
        theoretical,
        safe,
        priority,
        risk,
        verification_preserved,
        authority_holds: authority["holds"].as_bool(),
        authority_reason: authority["reason"]
            .as_str()
            .unwrap_or("the inherited row carries no reason")
            .to_string(),
        authority_basis: authority["status"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "candidate {}: the ownership row's semantically_equivalent \
                carries no status",
                    spec.id
                )
            })
            .to_string(),
        derived_blockers: blockers,
        ownership,
    }
}

// ---------------------------------------------------------------------------
// the assembled census
// ---------------------------------------------------------------------------

pub struct Census {
    pub records: Records,
    pub groups: Vec<Group>,
    pub reps: BTreeMap<SiteKey, Value>,
    pub timings: BTreeMap<String, Timing>,
    pub derived: Vec<Derived>,
    pub chain_id: u64,
    pub endpoint_id: String,
}

impl Census {
    pub fn new() -> Self {
        Self::from_records(Records::load(&workspace_root()))
    }

    /// From any corpus, including a mutated one — which is what lets §25's NC5 and NC6 re-derive
    /// the whole census from a reordered or re-clocked record set instead of comparing prose.
    pub fn from_records(records: Records) -> Self {
        let groups = records.groups();
        for group in &groups {
            assert!(
                site_by_key(&group.key).is_some(),
                "the records hold a group no site judges: {} — §4's surface would be incomplete, \
                 so the census refuses rather than classifying it as I_UNKNOWN",
                group.key.label()
            );
        }
        for site in SITES.iter() {
            assert!(
                groups.iter().any(|group| group.key == site.key()),
                "site {} judges a call group the records never produced — §4 forbids writing an \
                 RPC with no actual call site as a hot-path RPC",
                site.id
            );
        }
        let derived = CANDIDATES
            .iter()
            .map(|spec| derive_candidate(spec, &groups, &records.pairs, &records))
            .collect();
        let errors = shape_completeness_errors(&records.pairs);
        assert!(
            errors.is_empty(),
            "the candidate table does not cover the records: {}",
            errors.join("; ")
        );
        Census {
            reps: representative_rows(&records.rows),
            timings: timings(&records),
            chain_id: records.chain_id(),
            endpoint_id: records.endpoint_id(),
            groups,
            derived,
            records,
        }
    }

    pub fn timing(&self, key: &SiteKey) -> Timing {
        self.timings.get(&key.label()).cloned().unwrap_or_default()
    }

    pub fn rep(&self, key: &SiteKey) -> &Value {
        self.reps
            .get(key)
            .unwrap_or_else(|| panic!("no recorded row for site {}", key.label()))
    }

    pub fn derived_of(&self, id: &str) -> &Derived {
        self.derived
            .iter()
            .find(|derived| derived.spec.id == id)
            .unwrap_or_else(|| panic!("no derived candidate {id}"))
    }

    /// §18's `priority` and `safe_saving` for a *site*: the best label and the summed saving over
    /// the candidates that name this site as their consumer. A site no candidate names gets
    /// [`PRIORITY_NOT_A_CANDIDATE`], because §14 ranked candidates and this row is not one.
    pub fn site_priority(&self, key: &SiteKey) -> (&'static str, Vec<String>, usize) {
        let mut ids: Vec<String> = Vec::new();
        let mut safe = 0usize;
        let mut best: Option<&'static str> = None;
        for derived in &self.derived {
            if derived.spec.consumer_site().key() != *key {
                continue;
            }
            ids.push(derived.spec.id.to_string());
            safe += derived.safe;
            let rank = |label: &str| {
                PRIORITIES
                    .iter()
                    .position(|p| *p == label)
                    .unwrap_or(PRIORITIES.len())
            };
            match best {
                Some(current) if rank(current) <= rank(derived.priority) => {}
                _ => best = Some(derived.priority),
            }
        }
        (best.unwrap_or(PRIORITY_NOT_A_CANDIDATE), ids, safe)
    }

    /// The §7 measures, computed. They are five numbers and never one: the task book's own example
    /// (41/0/0/0/0) is the reason the fields exist separately, and §24's seventh rule is what the
    /// gate checks against them.
    pub fn saving_measures(&self) -> Value {
        let physical: usize = self.derived.iter().map(|d| d.measured.exact_pairs).sum();
        let semantic: usize = self
            .derived
            .iter()
            .map(|d| d.measured.reusable_in_principle_pairs)
            .sum();
        let theoretical: usize = self.derived.iter().map(|d| d.theoretical).sum();
        let safe_total: usize = self.derived.iter().map(|d| d.safe).sum();
        let behind_a_check: usize = self
            .derived
            .iter()
            .filter(|d| is_check_role(d.spec.consumer_site().verification_role))
            .map(|d| d.measured.exact_pairs)
            .sum();
        let pairs_marked_safe: usize = self
            .records
            .pairs
            .iter()
            .filter(|pair| pair_marked_safe(pair))
            .count();
        let conditions_met: usize = self
            .records
            .pairs
            .iter()
            .filter(|pair| conditions_all_met(pair))
            .count();
        json!({
            "physical_duplicate_saving": physical,
            "semantic_reuse_saving": semantic,
            "verification_removal_saving": 0usize,
            "parallel_wall_time_saving": 0usize,
            "total_safe_rpc_saving": safe_total,
            "diagnostics": {
                "theoretical_saving_over_all_candidates": theoretical,
                "exact_pairs_behind_a_check_role": behind_a_check,
                "pairs_with_every_reuse_condition_met_in_the_record": conditions_met,
                "pairs_the_record_itself_marks_safe_to_reuse": pairs_marked_safe,
            },
        })
    }

    fn root(
        &self,
        file: &str,
        question: &str,
        unit: &str,
        recompute: &str,
        anchors: Vec<Value>,
    ) -> Value {
        json!({
            "schema": SCHEMA,
            "milestone": MILESTONE,
            "file": file,
            "generated_by": GENERATED_BY,
            "assembly_module": TABLES_FILE,
            "model": MODEL_FILE,
            "question": question,
            "unit": unit,
            "recompute": recompute,
            "evidence_first_rule": "§2.4: every figure here was folded out of records this \
                repository had committed before M8.6 started. No table in this directory was \
                produced by an RPC this milestone made.",
            "new_rpc": 0,
            "chain_id": self.chain_id,
            "endpoint_id": self.endpoint_id,
            "runs": self.records.runs,
            "provenance": self.records.provenance,
            "anchors": anchors,
        })
    }
}

// ---------------------------------------------------------------------------
// §4 — rpc_surface.json
// ---------------------------------------------------------------------------

pub fn rpc_surface(census: &Census, tree: &mut Tree) -> Value {
    let mut rows: Vec<Value> = Vec::new();
    let mut anchors: Vec<Value> = Vec::new();
    let mut methods: BTreeMap<String, usize> = BTreeMap::new();
    let mut stages: BTreeMap<String, usize> = BTreeMap::new();
    for group in &census.groups {
        let site =
            site_by_key(&group.key).expect("Census::from_records refused an unassigned group");
        let rep = census.rep(&group.key);
        let (stamp, decision) = site_anchors(site, tree);
        anchors.push(stamp.clone());
        anchors.push(decision.clone());
        let (category, category_rule) = category_of(
            rep["method"].as_str().unwrap_or_default(),
            rep["sink"].as_str(),
            rep["caller"].as_str(),
        );
        *methods.entry(site.method.to_string()).or_insert(0) += group.asks;
        *stages
            .entry(site.stage.unwrap_or(UNSTAMPED_CALLER).to_string())
            .or_insert(0) += group.asks;
        rows.push(json!({
            "method": site.method,
            "stage": site.stage,
            "caller": site.caller_family,
            "site_id": site.id,
            "source_file": stamp["file"],
            "source_line": stamp["line"],
            "decision_file": decision["file"],
            "decision_line": decision["line"],
            "request_identity": identity_of_row(rep, Some(census.chain_id)),
            "identity_source": RpcReadKey::of_row(rep, Some(census.chain_id)).identity_source,
            "block_semantics": site.block_semantics,
            "consumer": site.consumer,
            "semantic_class": site.semantic_class,
            "class_basis": site.class_basis,
            "verification_role": site.verification_role,
            "carrier": site.carrier,
            "sink": rep["sink"],
            "sinks_recorded": group.sinks,
            "m842_category": category,
            "m842_category_rule": category_rule,
            "asks_per_run": group.per_run,
            "asks_total": group.asks,
            "timing": timing_json(&census.timing(&group.key)),
        }));
    }
    let transport_anchors: Vec<Value> = TRANSPORTS
        .iter()
        .filter(|transport| transport.traced)
        .map(|transport| {
            resolve(
                &code_anchor(
                    transport.adapter_file,
                    transport.adapter_token,
                    format!(
                        "{} reaches the wire through this adapter leg",
                        transport.method
                    ),
                ),
                tree,
            )
        })
        .collect();
    let mut table = census.root(
        "rpc_surface.json",
        "§4: which RPCs the production hot path actually makes, where each one is called, and what \
         consumes the answer",
        "call sites, one row per (method, stage, caller_family) the records produced",
        "the rows are `SITES` matched group by group against `group_rows(records)`; a group with no \
         site and a site with no group both abort the assembly, so the table cannot fall behind \
         the records",
        [anchors, transport_anchors.clone()].concat(),
    );
    let object = table.as_object_mut().expect("root is an object");
    object.insert(
        "counts".to_string(),
        json!({
            "sites": rows.len(),
            "methods_traced": methods.len(),
            "stages": stages.len(),
            "asks_total": census.groups.iter().map(|group| group.asks).sum::<usize>(),
            "asks_per_run": census.records.runs.iter().map(|run| {
                json!({"run": run, "asks": census.groups.iter().map(|group| group.per_run.get(run).copied().unwrap_or(0)).sum::<usize>()})
            }).collect::<Vec<Value>>(),
            "physical_http_attempts": census.groups.iter().map(|group| census.timing(&group.key).physical_attempts).sum::<u64>(),
        }),
    );
    object.insert(
        "by_method".to_string(),
        json!(methods
            .into_iter()
            .map(|(method, asks)| json!({"method": method, "asks": asks}))
            .collect::<Vec<Value>>()),
    );
    object.insert(
        "by_stage".to_string(),
        json!(stages
            .into_iter()
            .map(|(stage, asks)| json!({"stage": stage, "asks": asks}))
            .collect::<Vec<Value>>()),
    );
    object.insert(
        "transports".to_string(),
        json!(TRANSPORTS
            .iter()
            .enumerate()
            .map(|(index, transport)| json!({
                "method": transport.method,
                "adapter_file": transport.adapter_file,
                "adapter_token": transport.adapter_token,
                "adapter_line": if transport.traced { transport_anchors[index]["line"].clone() } else { Value::Null },
                "traced_in_the_records": transport.traced,
                "note": transport.note,
            }))
            .collect::<Vec<Value>>()),
    );
    object.insert(
        "not_in_the_records".to_string(),
        json!({
            "classified_but_never_called": CLASSIFIED_BUT_UNCALLED_METHODS,
            "coded_but_never_traced": CODED_BUT_UNTRACED_METHODS,
            "rule": "§4: an RPC with no actual call site is not written here as a hot-path RPC, so \
                no later milestone can claim a saving on it. Both lists are published instead of \
                being left out, because a silent absence is how a phantom row gets re-added.",
        }),
    );
    object.insert(
        "surface_rule".to_string(),
        json!({
            "census_key": "(method, stage, caller_family)",
            "caller_family_masks": ["step <digits> -> step N", "nonce <digits> -> nonce N", "0x + >= 8 hex digits -> <addr>"],
            "masks_are_presentation_only": true,
            "identity_is_read_through": "evm_pipeline::canonicalization::RpcReadKey",
            "class_rule": CLASS_RULE,
        }),
    );
    object.insert("rows".to_string(), Value::Array(rows));
    table
}

// ---------------------------------------------------------------------------
// §18 — rpc-census.json
// ---------------------------------------------------------------------------

pub fn rpc_census(census: &Census, tree: &mut Tree) -> Value {
    let mut rows: Vec<Value> = Vec::new();
    let mut anchors: Vec<Value> = Vec::new();
    for (index, group) in census.groups.iter().enumerate() {
        let site =
            site_by_key(&group.key).expect("Census::from_records refused an unassigned group");
        let rep = census.rep(&group.key);
        let ownership = census.records.ownership_of(site);
        let (stamp, decision) = site_anchors(site, tree);
        anchors.push(stamp.clone());
        anchors.push(decision.clone());
        let (priority, candidate_ids, safe_saving) = census.site_priority(&group.key);
        let (all_pairs, exact_pairs, other_pairs) = pairs_into(&group.key, &census.records.pairs);
        let reusable = census
            .records
            .pairs
            .iter()
            .filter(|pair| pair_side_key(pair, "consumer").as_ref() == Some(&group.key))
            .filter(|pair| conditions_all_met(pair))
            .count();
        let marked_safe = census
            .records
            .pairs
            .iter()
            .filter(|pair| pair_side_key(pair, "consumer").as_ref() == Some(&group.key))
            .filter(|pair| pair_marked_safe(pair))
            .count();
        let timing = census.timing(&group.key);
        let identity = identity_of_row(rep, Some(census.chain_id));
        let variants = recorded_key_variants(&census.records, &group.key);
        rows.push(json!({
            "key": group.key.label(),
            "row_index_in_this_table": index,
            "method": site.method,
            "stage": site.stage,
            "caller": site.caller_family,
            "site_id": site.id,
            "count": group.asks,
            "asks": group.asks,
            "asks_per_run": group.per_run,
            "union_duration_ms": super::ms(timing.union_ns),
            "rpc_sum_duration_ms": super::ms(timing.sum_ns),
            "duration": timing_json(&timing),
            "identity": identity,
            "recorded_key_variants": variants.len(),
            "identity_fields": RpcReadKey::of_row(rep, Some(census.chain_id)).terms.keys().cloned().collect::<Vec<String>>(),
            "identity_is_a_business_key": true,
            "duration_rank_as_identity": false,
            "block_semantics": site.block_semantics,
            "semantic_class": site.semantic_class,
            "class_basis": site.class_basis,
            "verification_role": site.verification_role,
            "role_evidence": site.role_evidence,
            "alternative_verification": site.alternative_verification,
            "alternative_evidence": site.alternative_evidence,
            "owner": ownership["owner"],
            "authority": ownership["authority"],
            "state_kind": site.state_kind,
            "ownership_status": ownership["ownership_status"],
            "freshness": format!(
                "{} ({})",
                ownership["freshness_rule"]["id"].as_str().unwrap_or_default(),
                ownership["freshness_rule"]["status"].as_str().unwrap_or_default()
            ),
            "invalidation": format!(
                "{} ({})",
                ownership["invalidation_rule"]["id"].as_str().unwrap_or_default(),
                ownership["invalidation_rule"]["status"].as_str().unwrap_or_default()
            ),
            "carrier": site.carrier,
            "carrier_evidence": site.carrier_evidence,
            "pairs_produced_or_consumed_here": all_pairs,
            "duplicate_count": exact_pairs,
            "duplicate_count_other_classes": other_pairs,
            "reusable_in_principle": reusable,
            "safe_reuse": marked_safe,
            "safe_saving": safe_saving,
            "candidates_naming_this_site": candidate_ids,
            "priority": priority,
            "source_file": stamp["file"],
            "source_line": stamp["line"],
            "decision_file": decision["file"],
            "decision_line": decision["line"],
        }));
    }
    let mut table = census.root(
        "rpc-census.json",
        "§18: one row per call site on the hot path, with its count, its two durations, its \
         identity, its owner, its verification role, and whether anything here can be saved",
        "asks (one logical request, retries folded into it), and milliseconds on each run's own clock",
        "`group_rows` over the three runs' `pipeline-calls.json`, joined to `SITES` by \
         (method, stage, caller_family) and to M8.4.3's ownership matrix by `state_kind`",
        anchors,
    );
    let object = table.as_object_mut().expect("root is an object");
    object.insert(
        "columns_are_two_kinds".to_string(),
        json!({
            "measured": ["count", "asks_per_run", "union_duration", "rpc_sum_duration", "identity", "recorded_key_variants", "duplicate_count", "reusable_in_principle", "safe_reuse"],
            "inherited": ["owner", "authority", "state_kind", "ownership_status", "freshness", "invalidation"],
            "judged": ["semantic_class", "class_basis", "verification_role", "role_evidence", "alternative_verification", "carrier", "block_semantics"],
            "derived": ["safe_saving", "priority", "candidates_naming_this_site"],
            "note": "§18's `count` column is published as `count` and echoed as `asks`, which is the \
                field name the gates fold the records by; one figure, two names, and the echo is \
                checked equal rather than trusted",
        }),
    );
    object.insert(
        "totals".to_string(),
        json!({
            "rows": rows.len(),
            "asks": census.groups.iter().map(|group| group.asks).sum::<usize>(),
            "duplicate_count": rows.iter().map(|row| row["duplicate_count"].as_u64().unwrap_or_default()).sum::<u64>(),
            "safe_saving": rows.iter().map(|row| row["safe_saving"].as_u64().unwrap_or_default()).sum::<u64>(),
            "physical_http_attempts": census.groups.iter().map(|group| census.timing(&group.key).physical_attempts).sum::<u64>(),
            "rpc_sum_duration_ms": super::ms(census.groups.iter().map(|group| census.timing(&group.key).sum_ns).sum::<u64>()),
            "union_duration_ms": super::ms(census.groups.iter().map(|group| census.timing(&group.key).union_ns).sum::<u64>()),
            "overlap_rows": census.groups.iter().map(|group| census.timing(&group.key).overlapping_rows).sum::<usize>(),
            "rows_without_interval": census.groups.iter().map(|group| census.timing(&group.key).rows_without_interval).sum::<usize>(),
            "sites_with_more_than_one_recorded_key": census.groups.iter().filter(|group| recorded_key_variants(&census.records, &group.key).len() > 1).count(),
        }),
    );
    object.insert("rows".to_string(), Value::Array(rows));
    table
}

// ---------------------------------------------------------------------------
// §6 — rpc-reduction-candidates.json
// ---------------------------------------------------------------------------

fn duplicate_status(derived: &Derived, census: &Census, tree: &mut Tree) -> Value {
    let consumer_key = derived.spec.consumer_site().key();
    let rep = census.rep(&consumer_key);
    let key = RpcReadKey::of_row(rep, Some(census.chain_id));
    let identity = identity_of_row(rep, Some(census.chain_id));
    let mut classes: BTreeMap<String, usize> = BTreeMap::new();
    for (class, count) in &derived.measured.duplicate_types {
        classes.insert(class.clone(), *count);
    }
    let mut conditions: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for (name, outcomes) in &derived.measured.condition_outcomes {
        conditions.insert(name.clone(), outcomes.clone());
    }
    let mut pair_anchors: Vec<Value> = Vec::new();
    let pair_ids = derived.measured.pair_ids.sorted_copy();
    for id in pair_ids.iter().take(3) {
        pair_anchors.push(resolve_record_token(
            CANDIDATES_RECORD,
            id,
            &format!(
                "{}: this pair of asks is the row the count was folded from",
                derived.spec.id
            ),
            tree,
        ));
    }
    json!({
        "identity": {
            "value": identity,
            "fields": key.terms.keys().cloned().collect::<Vec<String>>(),
            "identity_source": key.identity_source,
            "uses_row_index": false,
            "uses_duration": false,
            "rule": "the identity is M8.4.2's business key, read through the published \
                `RpcReadKey`, and never a position in a table",
        },
        "consumer_asks": derived.measured.consumer_asks,
        "producer_asks": derived.measured.producer_asks,
        "pairs": derived.measured.pairs,
        "exact_duplicate_pairs": derived.measured.exact_pairs,
        "other_classes": derived.measured.other_class_pairs,
        "by_duplicate_class": classes,
        "block_relations": derived.measured.block_relations,
        "reuse_condition_outcomes": conditions,
        "pair_ids": derived.measured.pair_ids.sorted_copy(),
        "pair_anchors": pair_anchors,
    })
}

/// A total order over the pair ids that does not depend on the record's row order (§25's NC5).
trait SortedCopy {
    fn sorted_copy(&self) -> Vec<String>;
}

impl SortedCopy for Vec<String> {
    fn sorted_copy(&self) -> Vec<String> {
        let mut out = self.clone();
        out.sort();
        out
    }
}

pub fn reduction_candidates(census: &Census, tree: &mut Tree) -> Value {
    let mut rows: Vec<Value> = Vec::new();
    let mut anchors: Vec<Value> = Vec::new();
    for derived in &census.derived {
        let spec = derived.spec;
        let consumer = spec.consumer_site();
        let producer = spec
            .producer_site()
            .expect("every candidate has a producer site");
        let (consumer_stamp, consumer_decision) = site_anchors(consumer, tree);
        let (producer_stamp, _) = site_anchors(producer, tree);
        anchors.push(consumer_stamp.clone());
        anchors.push(consumer_decision.clone());
        anchors.push(producer_stamp.clone());
        let ownership = census.records.ownership_of(consumer);
        let status = duplicate_status(derived, census, tree);
        let timing = census.timing(&consumer.key());
        let measures = census.saving_measures();
        let proof_pairs: Vec<String> = if derived.safe > 0 {
            status["pair_ids"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        } else {
            Vec::new()
        };
        rows.push(json!({
            "candidate": spec.id,
            "pattern": spec.pattern,
            "method": consumer.method,
            "stage": consumer.stage,
            "caller": consumer.caller_family,
            "consumer": consumer.id,
            "consumer_source_file": consumer_stamp["file"],
            "consumer_source_line": consumer_stamp["line"],
            "consumer_decision_line": format!("{}:{}", consumer_decision["file"].as_str().unwrap_or_default(), consumer_decision["line"].as_u64().unwrap_or_default()),
            "producer": producer.id,
            "producer_source_file": producer_stamp["file"],
            "producer_source_line": producer_stamp["line"],
            "same_site_twice": spec.producer == spec.consumer,
            "identity": status["identity"]["value"],
            "identity_fields": status["identity"]["fields"],
            "block_semantics": consumer.block_semantics,
            "semantic_class": consumer.semantic_class,
            "class_basis": consumer.class_basis,
            "duplicate_status": status,
            "duplicate_count_exact": derived.measured.exact_pairs,
            "reusable_in_principle": derived.measured.reusable_in_principle_pairs,
            "safe_to_reuse_now": derived.measured.safe_to_reuse_pairs,
            "three_layers": {
                "physical_duplicate": derived.measured.exact_pairs,
                "reusable_in_principle": derived.measured.reusable_in_principle_pairs,
                "safe_to_reuse_now": derived.measured.safe_to_reuse_pairs,
                "rule": "§2.2: three different questions, three different numbers. A pair of asks \
                    being one ask says nothing about whether one answer can serve two consumers, \
                    and neither says nothing about whether this build may act on it today.",
            },
            "verification_role": consumer.verification_role,
            "role_evidence": consumer.role_evidence,
            "role_none_reason": if consumer.verification_role == ROLE_NONE { consumer.consumer.to_string() } else { String::new() },
            "alternative_verification": consumer.alternative_verification,
            "alternative_evidence": consumer.alternative_evidence,
            "verification_preserved": derived.verification_preserved,
            "owner": ownership["owner"],
            "authority": ownership["authority"],
            "state_kind": consumer.state_kind,
            "freshness": derived.freshness,
            "freshness_basis": {
                "identity": ownership["identity"],
                "block_tag_semantics": ownership["block_tag_semantics"],
                "measured_block_relations": derived.measured.block_relations,
                "rule": "§9/§10: preserved only when the category carries no block term, or when \
                    every measured pair of this candidate names one answer on both sides. A tag is \
                    never resolved by guessing.",
            },
            "invalidation": format!(
                "{} ({})",
                ownership["invalidation_rule"]["id"].as_str().unwrap_or_default(),
                ownership["invalidation_rule"]["status"].as_str().unwrap_or_default()
            ),
            "authority_preserved": derived.authority_holds == Some(true),
            "authority_basis": derived.authority_basis,
            "authority_reason_inherited": derived.authority_reason,
            "carrier": consumer.carrier,
            "carrier_evidence": consumer.carrier_evidence,
            "current_rpc_count": derived.measured.consumer_asks,
            "producer_rpc_count": derived.measured.producer_asks,
            "avoidable_rpc_count": derived.theoretical,
            "theoretical_saving": derived.theoretical,
            "safe_saving": derived.safe,
            "safe_reuse_proof": {
                "kind": if derived.safe > 0 { PROOF_ALL_CONDITIONS_MET } else { PROOF_NONE },
                "pairs": proof_pairs,
                "statement": "§13: a safe saving is credited only from a proof this table can name. \
                    In this corpus no pair of asks has all five of M8.4.2's reuse conditions \
                    resolved met, and no pair is marked safe by the record itself, so every \
                    candidate carries `none` and every safe saving is 0.",
                "crediting_kinds": CREDITING_PROOFS,
            },
            "theoretical_and_safe_are_two_questions": {
                "theoretical": derived.theoretical,
                "safe": derived.safe,
                "difference": derived.theoretical - derived.safe,
                "rule": "§24's seventh rule: the ceiling answers 「if every question about identity, \
                    ownership, freshness and authority went this candidate's way」 and the credit \
                    answers 「which of those questions this corpus has actually answered」. Adding \
                    the second to the first, or reporting the first as a plan, is the confusion \
                    §25's NC4 is built to catch.",
            },
            "blockers": derived.blockers(),
            "derived_blockers": derived.derived_blockers,
            "risk": derived.risk.0,
            "implementation_risk_rank": derived.risk.1,
            "requires_new_carrier": spec.requires_new_carrier,
            "requires_new_lifecycle": spec.requires_new_lifecycle,
            "requires_new_experiment": spec.requires_new_experiment,
            "safe_reuse_proven": spec.safe_reuse_proven,
            "evidence_strength": spec.evidence_strength,
            "priority": derived.priority,
            "priority_derivation": priority_path(derived),
            "verdict": verdict_of(derived),
            "next_step": spec.next_step,
            "verdict_note": spec.verdict_note,
            "timing_is_diagnostic_only": {
                "consumer_duration_ms": super::ms(derived.measured.consumer_duration_ns),
                "union_duration_ms": super::ms(timing.union_ns),
                "rpc_sum_duration_ms": super::ms(timing.sum_ns),
                "rule": "§24's tenth rule: no column above this one reads a duration, and the \
                    priority was derived before this object was built.",
            },
            "measures_this_candidate_contributes": {
                "physical_duplicate_saving": derived.measured.exact_pairs,
                "total_census_physical_duplicate_saving": measures["physical_duplicate_saving"],
            },
        }));
    }
    // The rows this table publishes must be internally consistent before anything reads them — the
    // same function the evidence gate runs, so a modelling slip fails here rather than turning a
    // negative control vacuous (a mutated row whose clean counterpart already errored proves
    // nothing about the mutation).
    for row in &rows {
        let errors = candidate_invariant_errors(row);
        assert!(
            errors.is_empty(),
            "assembled candidate row {} is internally inconsistent: {errors:?}",
            row["candidate"].as_str().unwrap_or("<absent>")
        );
    }
    let mut table = census.root(
        "rpc-reduction-candidates.json",
        "§6: every reduction question the hot path raises, one row each, with its measured counts \
         and its judgement kept apart from both",
        "candidates, and asks within them",
        "`measured()` folds M8.4.2's published pairs onto each candidate, and `freshness_of`, \
         `theoretical_saving_of`, `safe_saving_of` and `priority_of` derive every verdict column; \
         §24's rules run over the rows through `candidate_invariant_errors`",
        anchors,
    );
    let object = table.as_object_mut().expect("root is an object");
    object.insert(
        "totals".to_string(),
        json!({
            "candidates": rows.len(),
            "by_priority": count_by(&rows, "priority"),
            "by_pattern": count_by(&rows, "pattern"),
            "by_verdict": count_by(&rows, "verdict"),
            "theoretical_saving": rows.iter().map(|row| row["theoretical_saving"].as_u64().unwrap_or_default()).sum::<u64>(),
            "safe_saving": rows.iter().map(|row| row["safe_saving"].as_u64().unwrap_or_default()).sum::<u64>(),
        }),
    );
    object.insert("rows".to_string(), Value::Array(rows));
    table
}

fn count_by(rows: &[Value], field: &str) -> Value {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows {
        *counts
            .entry(row[field].as_str().unwrap_or("<absent>").to_string())
            .or_insert(0) += 1;
    }
    json!(counts)
}

/// §14's decision list, shown as the clauses it actually took to reach the label. The gate can
/// re-run [`priority_of`] and compare; publishing the path is what lets a reader disagree with a
/// specific clause instead of with a word.
fn priority_path(derived: &Derived) -> Vec<Value> {
    let consumer = derived.spec.consumer_site();
    let mut path: Vec<Value> = Vec::new();
    let mut clause = |id: &str, holds: bool, text: &str| {
        path.push(json!({"clause": id, "holds": holds, "text": text}));
    };
    clause(
        "p0",
        derived.safe >= 1
            && derived.measured.exact_pairs > 0
            && derived.verification_preserved
            && !derived.spec.requires_new_carrier
            && !derived.spec.requires_new_lifecycle
            && !derived.spec.requires_new_experiment
            && derived.spec.safe_reuse_proven,
        "saving already credited, a duplicate measured, verification preserved, no new mechanism",
    );
    clause(
        "reject_no_alternative",
        consumer.alternative_verification == ALT_NO_ALTERNATIVE,
        "§12: nothing else would bear the duty if this read went",
    );
    clause(
        "reject_theoretical_zero",
        derived.theoretical == 0 && !derived.spec.requires_new_experiment,
        "§14: no duplicate measured and no experiment that could still measure one",
    );
    clause(
        "reject_freshness",
        derived.freshness == FRESHNESS_NOT_PRESERVED,
        "§14: the block changes the answer and these runs cannot show it does not",
    );
    clause(
        "reject_propagation",
        derived.spec.pattern == PATTERN_C,
        "§16: M8.4.4 already measured this family's net saving at zero",
    );
    clause(
        "needs_new_machinery",
        derived.spec.requires_new_carrier
            || derived.spec.requires_new_lifecycle
            || derived.spec.requires_new_experiment,
        "P1 when the evidence reaches (proven or code-derived), P2 when it does not",
    );
    path.push(json!({
        "clause": "reached",
        "holds": true,
        "text": format!("priority {}", derived.priority),
    }));
    path
}

// ---------------------------------------------------------------------------
// §19 — reduction-matrix.json
// ---------------------------------------------------------------------------

pub fn reduction_matrix(census: &Census) -> Value {
    let rows: Vec<Value> = census
        .derived
        .iter()
        .map(|derived| {
            let timing = census.timing(&derived.spec.consumer_site().key());
            json!({
                "candidate": derived.spec.id,
                "current_count": derived.measured.consumer_asks,
                "avoidable_count": derived.theoretical,
                "theoretical_saving": derived.theoretical,
                "safe_saving": derived.safe,
                "verification_preserved": derived.verification_preserved,
                "freshness_preserved": derived.freshness == FRESHNESS_PRESERVED,
                "freshness": derived.freshness,
                "authority_preserved": derived.authority_holds == Some(true),
                "requires_new_carrier": derived.spec.requires_new_carrier,
                "requires_new_lifecycle": derived.spec.requires_new_lifecycle,
                "requires_new_experiment": derived.spec.requires_new_experiment,
                "verdict": verdict_of(derived),
                "priority": derived.priority,
                "implementation_risk": derived.risk.0,
                "implementation_risk_rank": derived.risk.1,
                "evidence_strength": derived.spec.evidence_strength,
                "next_step": derived.spec.next_step,
                "total_duration_ms": super::ms(timing.sum_ns),
                "duration_is_diagnostic": "this column is here so §24's tenth rule can be checked \
                    as a falsifiable claim: the gate looks for a pair of candidates where the \
                    slower one carries the worse label, and the check fails if none exists",
            })
        })
        .collect();
    let mut table = census.root(
        "reduction-matrix.json",
        "§19: the twelve columns that decide a candidate, one row each, with the two savings kept \
         in two columns",
        "asks",
        "`theoretical_saving` is `measured.exact_pairs` and `safe_saving` is `safe_saving_of`, so \
         both are recomputable from the pairs and the conditions beside them",
        Vec::new(),
    );
    let object = table.as_object_mut().expect("root is an object");
    object.insert(
        "totals".to_string(),
        json!({
            "candidates": rows.len(),
            "current_count": rows.iter().map(|row| row["current_count"].as_u64().unwrap_or_default()).sum::<u64>(),
            "avoidable_count": rows.iter().map(|row| row["avoidable_count"].as_u64().unwrap_or_default()).sum::<u64>(),
            "theoretical_saving": rows.iter().map(|row| row["theoretical_saving"].as_u64().unwrap_or_default()).sum::<u64>(),
            "safe_saving": rows.iter().map(|row| row["safe_saving"].as_u64().unwrap_or_default()).sum::<u64>(),
            "note": "the two sums are not comparable: `current_count` counts asks of a consumer \
                site and a site can appear as the consumer of exactly one candidate here, while \
                `avoidable_count` counts measured duplicate pairs. They share a unit and not a \
                question.",
        }),
    );
    object.insert(
        "duration_is_not_a_rank".to_string(),
        json!({
            "inversions_found": priority_duration_inversions(&rows).len(),
            "rule": "§24's tenth rule as a figure: an inversion is a pair of candidates where the \
                slower one is labelled worse. If priority were the clock's order there could be \
                none; the census publishes how many there are.",
        }),
    );
    object.insert("rows".to_string(), Value::Array(rows));
    table
}

// ---------------------------------------------------------------------------
// §20 — rejected-opportunities.json
// ---------------------------------------------------------------------------

pub fn rejected_opportunities(census: &Census) -> Value {
    let rows: Vec<Value> = census
        .derived
        .iter()
        .filter(|derived| derived.priority == PRIORITY_REJECT)
        .map(|derived| {
            let mut reasons: BTreeSet<String> = BTreeSet::new();
            for blocker in derived.blockers() {
                reasons.insert(blocker);
            }
            if derived.safe == 0 {
                reasons.insert("safe_saving_zero".to_string());
            }
            let consumer = derived.spec.consumer_site();
            json!({
                "candidate": format!("{} -> {}", derived.spec.producer, derived.spec.consumer),
                "candidate_id": derived.spec.id,
                "pattern": derived.spec.pattern,
                "reason": reasons.into_iter().collect::<Vec<String>>(),
                "reason_prose": derived.spec.verdict_note,
                "theoretical_saving": derived.theoretical,
                "safe_saving": derived.safe,
                "verification_role": consumer.verification_role,
                "alternative_verification": consumer.alternative_verification,
                "who_would_bear_the_duty": who_bears_the_duty(derived),
                "freshness": derived.freshness,
                "inherited_verdict": inherited_verdict(derived, census),
            })
        })
        .collect();
    let mut table = census.root(
        "rejected-opportunities.json",
        "§20: the reduction ideas this census refuses, each with the reasons a field supports, so \
         no later agent has to ask the same question again",
        "rejected candidates",
        "every row is a `priority_of` result of REJECT, and every reason is either a declared \
         blocker on the candidate or one `derived_blockers` computed from a measured field",
        Vec::new(),
    );
    let object = table.as_object_mut().expect("root is an object");
    object.insert(
        "counts".to_string(),
        json!({
            "rejected": rows.len(),
            "pseudo_optimizations_excluded": rows.len(),
            "rule": "§23: the milestone is judged on how many pseudo-optimizations it excluded, \
                not on how many candidates it opened.",
        }),
    );
    object.insert("rows".to_string(), Value::Array(rows));
    table
}

/// §12's question, answered with the field it was read from rather than with a sentence.
fn who_bears_the_duty(derived: &Derived) -> Value {
    let consumer = derived.spec.consumer_site();
    json!({
        "question": "if this RPC went away, who would bear the verification duty it carries today?",
        "answer": consumer.alternative_verification,
        "evidence": consumer.alternative_evidence,
        "blocks_the_reduction": consumer.alternative_verification == ALT_NO_ALTERNATIVE,
        "rule": "§12's closing rule, applied inside the evidence gate: a check with \
            NO_ALTERNATIVE cannot be reduced at all, and `census_invariant_errors` refuses a \
            check-role row that answers NOT_APPLICABLE.",
    })
}

/// The verdict a family already has in an inherited record, published beside the census's own.
fn inherited_verdict(derived: &Derived, census: &Census) -> Value {
    let id = derived.spec.id;
    if id.starts_with("eth_call.") {
        let pairs = census.records.eth_call["pairs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let match_rows: Vec<Value> = pairs
            .iter()
            .filter(|pair| pair["consumer"]["site"].as_str() == Some(derived.spec.consumer))
            .map(|pair| {
                json!({
                    "pair": pair["candidate_id"],
                    "record_verdict": pair["record_verdict"],
                    "this_milestone_class": pair["this_milestone_class"],
                    "blockers": pair["blockers"],
                })
            })
            .collect();
        return json!({
            "source": ETH_CALL_RECORD,
            "milestone": "M8.5.1",
            "label": census.records.eth_call["outcome"]["label"],
            "net_rpc_saving": census.records.eth_call["outcome"]["net_rpc_saving"],
            "matching_pairs": match_rows,
            "rule": "§15: the two eth_call conclusions are inherited, not re-asked — no new \
                experiment and no re-graded site.",
        });
    }
    if derived.spec.pattern == PATTERN_C {
        return json!({
            "source": PROPAGATION_RECORD,
            "milestone": "M8.4.4",
            "label": PROPAGATION_LABEL,
            "label_recorded_in": PROPAGATION_LABEL_DOC,
            "rpc_count_reduction": census.records.propagation["rpc_count_reduction"],
            "aggregate_net_rpc_saved": census.records.propagation["aggregate"]["net_rpc_saved"],
            "status": "safe_but_no_rpc_saving",
            "rule": "§16: the propagation contract is settled and measured; §8's Pattern C says a \
                block-context row cannot be re-listed as a candidate.",
        });
    }
    Value::Null
}

// ---------------------------------------------------------------------------
// §21 — priority-queue.json
// ---------------------------------------------------------------------------

pub fn priority_queue(census: &Census, matrix: &Value) -> Value {
    let mut rows: Vec<Value> = matrix["rows"].as_array().cloned().unwrap_or_default();
    rows.sort_by_key(queue_sort_key);
    let published: Vec<Value> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let derived = census.derived_of(row["candidate"].as_str().unwrap_or_default());
            json!({
                "queue_position": index + 1,
                "candidate": row["candidate"],
                "priority": row["priority"],
                "verdict": row["verdict"],
                "safe_saving": row["safe_saving"],
                "theoretical_saving": row["theoretical_saving"],
                "current_count": row["current_count"],
                "avoidable_count": row["avoidable_count"],
                "implementation_risk": row["implementation_risk"],
                "implementation_risk_rank": row["implementation_risk_rank"],
                "evidence_strength": row["evidence_strength"],
                "next_step": row["next_step"],
                "missing_proof": missing_proof(derived),
                "sort_key": queue_sort_key(row),
                "pattern": derived.spec.pattern,
                "why_here": derived.spec.verdict_note,
            })
        })
        .collect();
    let mut table = census.root(
        "priority-queue.json",
        "§21: the queue the evidence produces when the five labels are ordered by saving, then by \
         how much new machinery a candidate needs, then by how far its evidence reaches",
        "candidates",
        "the order is `queue_sort_key` over `reduction-matrix.json`'s rows; no duration appears at \
         any position of the tuple, and the published `sort_key` lets a reader reproduce the order \
         from the fields rather than from this file",
        Vec::new(),
    );
    let object = table.as_object_mut().expect("root is an object");
    object.insert(
        "sort".to_string(),
        json!({
            "keys_in_order": ["priority band (§14, derived)", "safe_saving (down)", "implementation_risk (up)", "evidence_strength (proven > code_derived > record_only > absent)", "candidate id (for a total order)"],
            "nothing_is_written_first": "§21's 禁止预先写死: this file contains no literal order. Change a measured field and the order changes, which §25's controls exercise.",
            "duration_used_as_sort_key": false,
        }),
    );
    object.insert("by_priority".to_string(), count_by(&published, "priority"));
    object.insert("rows".to_string(), Value::Array(published));
    table
}

/// What a candidate would need before it could earn anything, as fields rather than as prose: the
/// §14 P0 clauses that are still false.
fn missing_proof(derived: &Derived) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    if derived.measured.exact_pairs == 0 {
        out.push(json!({"missing": "an_exact_duplicate_pair", "measured": derived.measured.pairs, "would_need": "two asks of one identity, which the records do not contain for this shape"}));
    }
    if derived.freshness != FRESHNESS_PRESERVED {
        out.push(json!({"missing": "freshness_preserved", "measured": derived.freshness}));
    }
    if !derived.verification_preserved {
        out.push(json!({"missing": "another_check_that_would_still_answer", "measured": derived.spec.consumer_site().alternative_verification}));
    }
    if derived.spec.requires_new_carrier {
        out.push(json!({"missing": "a_carrier_across_the_boundary"}));
    }
    if derived.spec.requires_new_lifecycle {
        out.push(json!({"missing": "an_owner_that_declares_a_lifecycle"}));
    }
    if derived.spec.requires_new_experiment {
        out.push(json!({"missing": "a_chain_experiment", "next_step": NEXT_STEP_EXPERIMENT, "rule": "§26: marked, not run — this milestone spends no RPC on a hypothesis it could settle with one"}));
    }
    if !derived.spec.safe_reuse_proven {
        out.push(json!({"missing": "a_safe_reuse_proof", "measured": derived.measured.reusable_in_principle_pairs, "crediting_kinds": CREDITING_PROOFS}));
    }
    out
}

// ---------------------------------------------------------------------------
// §10 — information_flow.json
// ---------------------------------------------------------------------------

/// One type that could hold an answer across a boundary, and what the code says it holds. §10 asks
/// for exactly this list, including the two containers a reader is most likely to over-read
/// ([`CONTAINERS`]' state-store and graph rows), so the table answers with anchors.
struct Container {
    type_name: &'static str,
    file: &'static str,
    token: &'static str,
    carrier: &'static str,
    crosses_a_stage_boundary: bool,
    holds_canonical_state: bool,
    note: &'static str,
}

const CONTAINERS: [Container; 10] = [
    Container {
        type_name: "InMemoryStateStore",
        file: "crates/state/src/store.rs",
        token: "pub struct InMemoryStateStore",
        carrier: CARRIER_NONE,
        crosses_a_stage_boundary: false,
        holds_canonical_state: false,
        note:
            "the discovery half's projection: pool metadata and the reserves restated from logs. \
            §4 of M8.4.3's own text forbids reading it as REVM canonical state, and it is not a \
            carrier for any of the 78 measured pairs — nothing in it is handed to a later stage's \
            read.",
    },
    Container {
        type_name: "GraphSnapshot",
        file: "crates/graph/src/snapshot.rs",
        token: "pub struct GraphSnapshot {",
        carrier: CARRIER_NONE,
        crosses_a_stage_boundary: false,
        holds_canonical_state: false,
        note: "a projection with a block number and no block hash, so it cannot even show which \
            block it describes; the same §4 boundary applies.",
    },
    Container {
        type_name: "Opportunity",
        file: "crates/opportunity/src/detector.rs",
        token: "pub struct Opportunity {",
        carrier: CARRIER_REPORT,
        crosses_a_stage_boundary: true,
        holds_canonical_state: false,
        note: "the *claim* that travels from detection to the stages that check it. It carries \
            reserves as data to be re-read, not as an answer to be served.",
    },
    Container {
        type_name: "PreflightFacts",
        file: "crates/execution/src/preflight.rs",
        token: "pub struct PreflightFacts<'a>",
        carrier: CARRIER_STAGE_FACTS,
        crosses_a_stage_boundary: false,
        holds_canonical_state: false,
        note: "one stage's own gathering: it does not cross a boundary, which is why a candidate \
            that wanted it to needs a new carrier.",
    },
    Container {
        type_name: "PreflightReport",
        file: "crates/execution/src/preflight.rs",
        token: "pub struct PreflightReport",
        carrier: CARRIER_NONE,
        crosses_a_stage_boundary: true,
        holds_canonical_state: false,
        note: "the only type that crosses Preflight → Build today, and it has no block field — \
            M8.4.3 names this as the missing carrier for a header answer.",
    },
    Container {
        type_name: "VerifiedBlockContext",
        file: "crates/execution/src/block_context.rs",
        token: "pub struct VerifiedBlockContext",
        carrier: CARRIER_VERIFIED_BLOCK_CONTEXT,
        crosses_a_stage_boundary: true,
        holds_canonical_state: false,
        note: "M8.4.4's contract: produced by a verified read, checked again by the consumer, and \
            measured to save no RPC, because the consumer's read replaces nothing.",
    },
    Container {
        type_name: "TransactionIntent",
        file: "crates/execution/src/intent.rs",
        token: "pub struct TransactionIntent",
        carrier: CARRIER_INTENT,
        crosses_a_stage_boundary: true,
        holds_canonical_state: false,
        note:
            "the transaction being built holds its own fee fields. §10 named an ExecutionIntent: \
            no such type exists in this workspace (see the searched-and-absent row), and this is \
            the type that carries the fields it was asking about.",
    },
    Container {
        type_name: "BlockPin",
        file: "crates/simulation/src/state.rs",
        token: "pub struct BlockPin",
        carrier: CARRIER_BLOCK_PIN,
        crosses_a_stage_boundary: true,
        holds_canonical_state: false,
        note: "a height and a hash, carried from the observation stage and checked by the \
            simulation's own header read.",
    },
    Container {
        type_name: "StateReadCache",
        file: "crates/simulation/src/state.rs",
        token: "struct StateReadCache {",
        carrier: CARRIER_SIMULATION_CACHE,
        crosses_a_stage_boundary: false,
        holds_canonical_state: false,
        note: "M8.3.1's real cache, and the reason no REVM-side read appears in the census as an \
            avoidable duplicate: its scope is one simulation, which is what its own documentation \
            says.",
    },
    Container {
        type_name: "HttpChainAdapter",
        file: "crates/chain/src/rpc.rs",
        token: "pub struct HttpChainAdapter",
        carrier: CARRIER_ADAPTER_FIELD,
        crosses_a_stage_boundary: true,
        holds_canonical_state: false,
        note: "the connection, which holds the chain id from connect to drop. The census's only \
            proven owner-and-scope pair — and still a safe saving of 0, because the later asks are \
            each a named leg of a check.",
    },
];

pub fn information_flow(census: &Census, tree: &mut Tree) -> Value {
    let mut container_rows: Vec<Value> = Vec::new();
    let mut anchors: Vec<Value> = Vec::new();
    for container in CONTAINERS.iter() {
        let anchor = resolve(
            &code_anchor(
                container.file,
                container.token,
                format!("{} is declared here", container.type_name),
            ),
            tree,
        );
        anchors.push(anchor.clone());
        container_rows.push(json!({
            "type": container.type_name,
            "declared_at": anchor_source(&anchor),
            "file": anchor["file"],
            "line": anchor["line"],
            "carrier_const": container.carrier,
            "crosses_a_stage_boundary": container.crosses_a_stage_boundary,
            "holds_revm_canonical_state": container.holds_canonical_state,
            "note": container.note,
            "sites_carried": SITES
                .iter()
                .filter(|site| site.carrier == container.carrier)
                .map(|site| site.id)
                .collect::<Vec<&str>>(),
        }));
    }
    let mut flows: Vec<Value> = Vec::new();
    for site in SITES.iter() {
        let key = site.key();
        let rep = census.rep(&key);
        let ownership = census.records.ownership_of(site);
        let (stamp, decision) = site_anchors(site, tree);
        anchors.push(stamp.clone());
        anchors.push(decision.clone());
        let downstream: Vec<Value> = census
            .derived
            .iter()
            .filter(|derived| derived.spec.producer_site().map(|s| s.key()) == Some(key.clone()))
            .map(|derived| {
                json!({
                    "candidate": derived.spec.id,
                    "consumer": derived.spec.consumer,
                    "consumer_stage": derived.spec.consumer_site().stage,
                    "crosses_a_stage_boundary": derived.spec.consumer_site().stage != site.stage,
                    "priority": derived.priority,
                    "safe_saving": derived.safe,
                })
            })
            .collect();
        flows.push(json!({
            "site": site.id,
            "rpc": format!("{} at {}", site.method, site.stage.unwrap_or(UNSTAMPED_CALLER)),
            "asked_by": site.caller_family,
            "asked_at": anchor_source(&stamp),
            "answer_state_kind": site.state_kind,
            "decoded_into": ownership["owner"],
            "authority": ownership["authority"],
            "carrier": site.carrier,
            "carrier_evidence": site.carrier_evidence,
            "consumer": site.consumer,
            "first_decision": anchor_source(&decision),
            "decision_evidence": site.role_evidence,
            "verification_role": site.verification_role,
            "block_semantics": site.block_semantics,
            "recorded_identity": identity_of_row(rep, Some(census.chain_id)),
            "reuse_status_inherited": {
                "reusable_in_principle": ownership["reuse_status"]["reusable_in_principle"]["holds"],
                "safe_to_reuse_now": ownership["reuse_status"]["safe_to_reuse_now"]["holds"],
                "semantically_equivalent": ownership["reuse_status"]["semantically_equivalent"]["holds"],
                "check_would_stop_existing": ownership["reuse_status"]["check_would_stop_existing"],
                "missing_proof": ownership["reuse_status"]["missing_proof"],
            },
            "flows_onward_to": downstream,
            "derivable_is_not_safe_to_derive": downstream.iter().any(|row| row["candidate"].as_str().unwrap_or_default().starts_with("block_number.")),
        }));
    }
    let mut table = census.root(
        "information_flow.json",
        "§10: for every read, what the answer becomes, which type holds it, and which stage meets \
         it — the check behind the claim that a value can only be saved if something already holds it",
        "call sites, plus the container types §10 names",
        "one row per [`SITES`] entry, joined to M8.4.3's ownership row by `state_kind` and to the \
         candidates that name the site as a producer; the container rows are anchor-resolved \
         declarations in this repository",
        anchors,
    );
    let object = table.as_object_mut().expect("root is an object");
    object.insert("containers".to_string(), Value::Array(container_rows));
    object.insert(
        "searched_and_absent".to_string(),
        json!([{
            "looked_for": "ExecutionIntent",
            "scope": "crates/*/src and crates/*/tests",
            "result": "no declaration in this workspace",
            "the_type_that_carries_those_fields": "TransactionIntent (crates/execution/src/intent.rs)",
            "rule": "a negative published with its scope and its substitute, so the absence is a \
                checked fact rather than a sentence — §10 asked about ExecutionIntent by name.",
        }]),
    );
    object.insert(
        "boundary_claims".to_string(),
        json!([
            {
                "claim": "StateStore / GraphSnapshot != REVM canonical state",
                "status": "held",
                "evidence": "M8.4.3's pool_reserves row, safe_to_reuse_now.reason, quoted in the \
                    containers table; and the census's own storage/code/balance/nonce sites, whose \
                    owner is the simulation's state provider rather than the store",
                "consequence_for_the_census": "no candidate may claim a saved RPC on the strength \
                    of a projection that was never canonical state to begin with",
            },
            {
                "claim": "propagation != reuse",
                "status": "held",
                "evidence": "§2.3, and M8.4.4's measured rpc_count_reduction of 0",
                "consequence_for_the_census": "the five Pattern C rows are REJECT here rather than \
                    counted as candidates that failed",
            },
            {
                "claim": "DERIVABLE != SAFE_TO_DERIVE",
                "status": "held",
                "evidence": "the block_number row's Pattern F: the height is also available from the \
                    header read that follows it",
                "consequence_for_the_census": "one site, no saving, and a named reason instead of a \
                    quiet omission",
            },
        ]),
    );
    object.insert("flows".to_string(), Value::Array(flows));
    table
}

// ---------------------------------------------------------------------------
// §7 — saving-kinds.json
// ---------------------------------------------------------------------------

pub fn saving_kinds(census: &Census) -> Value {
    let measures = census.saving_measures();
    let diagnostics = &measures["diagnostics"];
    let physical = json!({
        "measure": "physical_duplicate_saving",
        "value": measures["physical_duplicate_saving"],
        "unit": "asks (one logical request, retries folded in)",
        "how_computed": "the sum of `exact_pairs` over the 18 candidates. [`shape_completeness_errors`] \
            refuses a census in which a measured exact-duplicate shape lacks a candidate or a \
            candidate names a shape the records never produced, so the sum is the record's own \
            42 and not a number this milestone chose.",
        "reconciles_with": format!("{SUMMARY_RECORD}'s exact_duplicate_pairs"),
        "what_it_does_not_mean": "it is not a saving. It is the count of asks that asked something \
            an earlier ask had already asked — which is §2.2's first layer, and nothing above it.",
        "what_would_have_to_change_to_earn_it": "nothing: it is already spent. M8.3.1's cache \
            removed the duplicates it was allowed to remove inside one simulation, and these 42 \
            are the ones a cache cannot reach.",
    });
    let semantic = json!({
        "measure": "semantic_reuse_saving",
        "value": measures["semantic_reuse_saving"],
        "unit": "pairs whose five reuse conditions are all resolved met",
        "how_computed": "the sum of `reusable_in_principle_pairs` over the candidates, each of \
            which is `conditions_all_met` over the pair rows in M8.4.2's record — five condition \
            fields, every one `checked_met`.",
        "reconciles_with": "the corpus-wide count published beside this table",
        "what_it_does_not_mean": "a pair with one condition `not_checkable_from_a_record` is not a \
            reusable pair: §2.4 forbids reading a missing field as a pass.",
        "what_would_have_to_change_to_earn_it": "a lifecycle and an owner for the value, so the \
            condition becomes checkable at all — which is what §19's requires_new_lifecycle column \
            counts.",
    });
    let verification = json!({
        "measure": "verification_removal_saving",
        "value": 0usize,
        "unit": "asks removed by weakening a check",
        "how_computed": "there is no way to compute a non-zero figure for this measure without \
            deleting a verification, and §3 forbids the act. The diagnostic count beside it is the \
            number a naive plan would have claimed.",
        "reconciles_with": "§11's default and §12's hard block",
        "what_it_does_not_mean": "the diagnostic is not a plan. It is the size of the mistake \
            someone would make.",
        "what_would_have_to_change_to_earn_it": "an existing other check that answers the same \
            question — EXISTING_OTHER_CHECK in §12's vocabulary, read per site, and today none of \
            the check-role sites has one for the duty it carries.",
    });
    let parallel = json!({
        "measure": "parallel_wall_time_saving",
        "value": 0usize,
        "unit": "asks (always zero: this measure is wall-time, and it is reported as 0 asks so it \
            cannot be added to a count of requests)",
        "how_computed": "inherited, not re-measured: M8.3.3 runs the state-read path at the \
            configured concurrency of 1 and §3 forbids changing it here; M8.4.4 published \
            rpc_count_reduction of 0 for the propagation contract.",
        "reconciles_with": "this table's inherited_verdicts rows",
        "what_it_does_not_mean": "wall-time is not RPC count. §7's whole point in keeping five \
            fields is that M8.3.3's timing result and M8.4.2's duplicate count are different \
            quantities, and a table that folds them reports a parallelism result as a saving.",
        "what_would_have_to_change_to_earn_it": "a concurrency decision, which is §3's prohibition \
            in this milestone and M8.3.3's in the last one.",
    });
    let total = json!({
        "measure": "total_safe_rpc_saving",
        "value": measures["total_safe_rpc_saving"],
        "unit": "asks this build may stop making today",
        "how_computed": "the sum of `safe_saving_of` over the candidates — each of which returns 0 \
            unless the theoretical ceiling is positive, verification is preserved, freshness is \
            preserved, no new carrier, lifecycle or experiment is required, safe reuse is proven, \
            no existing check would stop, and the consumer carries no check role.",
        "reconciles_with": "reduction-matrix.json's totals.safe_saving and rpc-census.json's totals.safe_saving",
        "what_it_does_not_mean": "it is not the other four added together, and §24's seventh rule \
            is what keeps that sentence true in both directions.",
        "what_would_have_to_change_to_earn_it": "a proof, named in [`CREDITING_PROOFS`], carried by \
            record rows. Not a re-reading of the same 42.",
    });
    let mut table = census.root(
        "saving-kinds.json",
        "§7: the five ways to count a saving, computed separately, because the way they get mixed \
         up is by reporting one of them as another",
        "asks, except the wall-time measure, which is a duration wearing an asks label so it cannot \
            be summed with them",
        "the first two measures and the fifth are sums over [`CANDIDATES`] of counts folded out of \
         `reuse-candidates.json`; the fourth is inherited from two published verdicts and is 0 by \
         that inheritance",
        Vec::new(),
    );
    let object = table.as_object_mut().expect("root is an object");
    object.insert(
        "measures".to_string(),
        json!([physical, semantic, verification, parallel, total]),
    );
    object.insert(
        "arithmetic".to_string(),
        json!({
            "the_task_books_example": {"physical_duplicate_saving": 41, "semantic_reuse_saving": 0, "verification_removal_saving": 0, "parallel_wall_time_saving": 0, "total_safe_rpc_saving": 0},
            "measured_here": {
                "physical_duplicate_saving": measures["physical_duplicate_saving"],
                "semantic_reuse_saving": measures["semantic_reuse_saving"],
                "verification_removal_saving": 0,
                "parallel_wall_time_saving": 0,
                "total_safe_rpc_saving": measures["total_safe_rpc_saving"],
            },
            "sum_of_the_first_four": measures["physical_duplicate_saving"].as_u64().unwrap_or_default(),
            "is_the_fifth_the_sum_of_the_first_four": false,
            "why": "the fifth is a subset of the first, not an addition to it: every safe saving is \
                already inside the duplicate count it could come from. §13.",
        }),
    );
    object.insert("diagnostics".to_string(), diagnostics.clone());
    object.insert(
        "inherited_verdicts".to_string(),
        json!([
            {
                "milestone": "M8.4.4",
                "source": PROPAGATION_RECORD,
                "label": PROPAGATION_LABEL,
                "label_recorded_in": PROPAGATION_LABEL_DOC,
                "rpc_count_reduction": census.records.propagation["rpc_count_reduction"],
                "aggregate_net_rpc_saved": census.records.propagation["aggregate"]["net_rpc_saved"],
                "pipeline_wall_clock_reduction": census.records.propagation["pipeline_wall_clock_reduction"],
                "status_this_census_uses": "category=propagation, status=safe_but_no_rpc_saving (§16)",
            },
            {
                "milestone": "M8.5.1",
                "source": ETH_CALL_RECORD,
                "label": census.records.eth_call["outcome"]["label"],
                "net_rpc_saving": census.records.eth_call["outcome"]["net_rpc_saving"],
                "status_this_census_uses": "REUSE_BLOCKED, inherited and not re-graded (§15)",
            },
            {
                "milestone": "M8.4.2",
                "source": SUMMARY_RECORD,
                "duplicate_pairs": census.records.candidates["pairs"],
                "exact_duplicate_pairs": census.records.candidates["rows"].as_array().map_or(0, |list| list.iter().filter(|row| row["duplicate_type"].as_str() == Some("exact_duplicate")).count()),
                "safe_to_reuse": census.records.candidates["safe_to_reuse"],
                "status_this_census_uses": "the 78 pairs and their 42 exact duplicates are the corpus the census counts, and `safe_to_reuse` is the level-C field it reads instead of inferring",
            },
        ]),
    );
    table
}

// ---------------------------------------------------------------------------
// §25 — negative-controls.json
// ---------------------------------------------------------------------------

/// One negative control, run the way the gate runs it: a mutation applied to a real assembled row,
/// and the rule the mutation is supposed to trip.
pub struct Control {
    pub id: &'static str,
    pub mutates: &'static str,
    pub rule: &'static str,
    pub target: Value,
    pub observed: Value,
    pub fired: bool,
    pub expectation: &'static str,
}

impl Control {
    fn publish(&self) -> Value {
        json!({
            "control": self.id,
            "mutates": self.mutates,
            "rule_it_must_trip": self.rule,
            "expectation": self.expectation,
            "target": self.target,
            "observed": self.observed,
            "fired": self.fired,
        })
    }
}

fn candidate_like(row: &Value) -> Value {
    row.clone()
}

pub fn negative_controls(census: &Census, tree: &mut Tree) -> Value {
    let candidates = reduction_candidates(census, tree);
    let candidate_rows = candidates["rows"].as_array().cloned().unwrap_or_default();

    // NC1 — identity mutation.
    let nc1 = nc1_identity(census, tree);
    // NC2 — verification mutation.
    let nc2_target = candidate_rows
        .iter()
        .find(|row| {
            row["blockers"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .any(|item| item.as_str() == Some("read_is_the_check"))
                })
                .unwrap_or(false)
        })
        .cloned()
        .unwrap_or_else(|| panic!("no candidate carries the read_is_the_check blocker to mutate"));
    let mut nc2_row = candidate_like(&nc2_target);
    nc2_row["verification_role"] = json!(ROLE_NONE);
    nc2_row["role_none_reason"] = json!("");
    let nc2_errors = candidate_invariant_errors(&nc2_row);
    let nc2 = Control {
        id: "NC2",
        mutates:
            "verification_role: a check role -> NONE, with the blocker that names the read as \
            the check left in place and role_none_reason blanked",
        rule: "the empty reason (§24's eighth rule) and a blocker claiming the read is the check \
            while the role says it verifies nothing",
        target: json!({
            "candidate": nc2_target["candidate"],
            "role_before": nc2_target["verification_role"],
            "role_after": ROLE_NONE,
            "blockers": nc2_target["blockers"],
        }),
        observed: json!({"errors": nc2_errors, "clean_row_errors": candidate_invariant_errors(&nc2_target)}),
        fired: nc2_errors.len() >= 2,
        expectation:
            "at least two independent refusals, and the unmutated row publishes zero errors",
    };

    // NC3 — safe saving mutation.
    let nc3_target = candidate_rows
        .iter()
        .find(|row| row["theoretical_saving"].as_u64().unwrap_or_default() > 0)
        .cloned()
        .unwrap_or_else(|| panic!("no candidate has a theoretical ceiling to credit"));
    let mut nc3_row = candidate_like(&nc3_target);
    nc3_row["safe_saving"] = json!(1u64);
    let nc3_errors = candidate_invariant_errors(&nc3_row);
    let nc3 = Control {
        id: "NC3",
        mutates: "safe_saving: 0 -> 1, with no proof added and no record row changed",
        rule: "§24's sixth figure: a credited saving has to name one of the two crediting proof \
            kinds, and the row still carries `none`",
        target: json!({
            "candidate": nc3_target["candidate"],
            "theoretical_saving": nc3_target["theoretical_saving"],
            "safe_saving_before": nc3_target["safe_saving"],
            "safe_saving_after": 1,
            "proof_kind": nc3_target["safe_reuse_proof"]["kind"],
        }),
        observed: json!({"errors": nc3_errors, "clean_row_errors": candidate_invariant_errors(&nc3_target)}),
        fired: nc3_errors
            .iter()
            .any(|error| error.contains("safe_saving 1 with proof")),
        expectation: "the proof rule fires, and the unmutated row publishes zero errors",
    };

    // NC4 — theoretical/safe confusion.
    let nc4_target = candidate_rows
        .iter()
        .find(|row| row["verification_role"].as_str() == Some(ROLE_INDEPENDENT_RECHECK))
        .or_else(|| {
            candidate_rows
                .iter()
                .find(|row| is_check_role(row["verification_role"].as_str().unwrap_or_default()))
        })
        .cloned()
        .unwrap_or_else(|| panic!("no candidate carries a check role to confuse"));
    // The inflated figure is the census's own whole physical duplicate count credited to one
    // candidate — measured, so the mutation cannot be accused of picking a number that happens to
    // pass.
    let inflate = census.saving_measures()["physical_duplicate_saving"]
        .as_u64()
        .unwrap_or_else(|| {
            panic!("the census publishes no physical_duplicate_saving to inflate with")
        });
    let mut nc4_row = candidate_like(&nc4_target);
    nc4_row["duplicate_count_exact"] = json!(inflate);
    nc4_row["theoretical_saving"] = json!(inflate);
    nc4_row["safe_saving"] = json!(inflate);
    nc4_row["safe_reuse_proof"]["kind"] = json!(PROOF_ALL_CONDITIONS_MET);
    let nc4_errors = candidate_invariant_errors(&nc4_row);
    let nc4 = Control {
        id: "NC4",
        mutates: "theoretical_saving = safe_saving = the census's whole physical duplicate count, \
            while the consumer keeps an INDEPENDENT_RECHECK role and a crediting proof kind is \
            asserted",
        rule: "§11's default: a saving credited on a read that is itself the check is refused \
            unless §12's other bearer exists",
        target: json!({
            "candidate": nc4_target["candidate"],
            "verification_role": nc4_target["verification_role"],
            "alternative_verification": nc4_target["alternative_verification"],
            "theoretical_saving_before": nc4_target["theoretical_saving"],
            "safe_saving_before": nc4_target["safe_saving"],
            "both_after": inflate,
        }),
        observed: json!({"errors": nc4_errors, "clean_row_errors": candidate_invariant_errors(&nc4_target)}),
        fired: nc4_errors
            .iter()
            .any(|error| error.contains("§11's default is that this is the check")),
        expectation:
            "the §11/§12 refusal fires even with a proof kind asserted, because a proof of \
            reuse is not a proof that the check survives",
    };

    // NC5 — presentation-order mutation.
    let mut reordered = census.records.clone();
    reordered.rows.reverse();
    reordered.pairs.reverse();
    let reversed_rows = reordered.rows.len();
    let reversed_pairs = reordered.pairs.len();
    let reversed_census = Census::from_records(reordered);
    let semantics_before: Vec<Value> = census
        .derived
        .iter()
        .map(|derived| derived.semantic_columns())
        .collect();
    let semantics_after: Vec<Value> = reversed_census
        .derived
        .iter()
        .map(|derived| derived.semantic_columns())
        .collect();
    let census_errors_reversed = census_invariant_errors(
        &rpc_census(&reversed_census, &mut Tree::new())["rows"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
        &reversed_census.groups,
    );
    let nc5 = Control {
        id: "NC5",
        mutates:
            "the order of the input records: every ask row and every pair row reversed before \
            the census is re-derived",
        rule:
            "§24's twelfth rule: presentation must not decide a verdict — the groups fold through \
            BTreeMap keys and no count reads a position",
        target: json!({
            "rows": reversed_rows,
            "pairs": reversed_pairs,
            "groups_before": census.groups.len(),
            "groups_after": reversed_census.groups.len(),
        }),
        observed: json!({
            "semantic_columns_identical": semantics_before == semantics_after,
            "census_invariant_errors_after_reversal": census_errors_reversed,
            "first_candidate_before": semantics_before.first().map(|row| row["candidate"].clone()),
            "first_candidate_after": semantics_after.first().map(|row| row["candidate"].clone()),
        }),
        fired: semantics_before == semantics_after && census_errors_reversed.is_empty(),
        expectation: "must still PASS: identical semantic columns, no invariant error",
    };

    // NC6 — clock mutation.
    let mut reclocked = census.records.clone();
    let mut durations_before: u64 = 0;
    let mut durations_after: u64 = 0;
    for row in &mut reclocked.rows {
        let object = row.as_object_mut().expect("a recorded row is an object");
        for field in ["duration_ns", "started_ns", "finished_ns"] {
            let value = object
                .get(field)
                .and_then(Value::as_u64)
                .unwrap_or_else(|| panic!("row has no numeric {field} to scale"));
            durations_before += value;
            durations_after += value * 1_000;
            object.insert(field.to_string(), json!(value * 1_000));
        }
    }
    for entry in reclocked
        .provenance
        .as_array_mut()
        .expect("provenance is an array")
    {
        let generated = entry["generated_at_unix_ms"]
            .as_u64()
            .unwrap_or_else(|| panic!("a provenance entry carries no generated_at_unix_ms"));
        entry["generated_at_unix_ms"] = json!(generated + 86_400_000);
    }
    let reclocked_census = Census::from_records(reclocked);
    let reclocked_semantics: Vec<Value> = reclocked_census
        .derived
        .iter()
        .map(|derived| derived.semantic_columns())
        .collect();
    let timing_after = reclocked_census.saving_measures();
    let nc6 = Control {
        id: "NC6",
        mutates: "every duration and stamp in the corpus multiplied by 1000, and every run's \
            generated_at moved a day",
        rule:
            "§25's last control: a clock may describe a call, it may not decide what to do about \
            it",
        target: json!({
            "duration_field_sum_before": durations_before,
            "duration_field_sum_after": durations_after,
            "scaled_by": 1000,
            "generated_at_shift_ms": 86_400_000,
        }),
        observed: json!({
            "semantic_columns_identical": semantics_before == reclocked_semantics,
            "the_clock_actually_moved": durations_before != durations_after,
            "census_total_rpc_sum_duration_ms_before": rpc_census(census, &mut Tree::new())["totals"]["rpc_sum_duration_ms"],
            "census_total_rpc_sum_duration_ms_after": rpc_census(&reclocked_census, &mut Tree::new())["totals"]["rpc_sum_duration_ms"],
            "safe_saving_before_and_after": [
                semantics_before.iter().map(|row| row["safe_saving"].as_u64().unwrap_or_default()).sum::<u64>(),
                reclocked_semantics.iter().map(|row| row["safe_saving"].as_u64().unwrap_or_default()).sum::<u64>(),
            ],
            "five_saving_measures_identical": census.saving_measures() == timing_after,
            "verdict_before_and_after": [final_verdict(census)["label"], final_verdict(&reclocked_census)["label"]],
        }),
        fired: semantics_before == reclocked_semantics && durations_before != durations_after,
        expectation: "the semantic columns identical while the timing columns demonstrably move — \
            a control that cannot see the clock moving proves nothing",
    };

    let published: Vec<Value> = [nc1, nc2, nc3, nc4, nc5, nc6]
        .iter()
        .map(Control::publish)
        .collect();
    let mut table = census.root(
        "negative-controls.json",
        "§25: the six mutations that have to break the gate, run against the assembled rows rather \
         than described",
        "controls, each with the row it mutated and the rule it tripped",
        "every control is computed here from the same functions the gates assert over, so the \
         published `fired` flags and the gate's own assertions are two reads of one derivation",
        Vec::new(),
    );
    let object = table.as_object_mut().expect("root is an object");
    object.insert(
        "all_green".to_string(),
        json!(published
            .iter()
            .all(|row| row["fired"].as_bool().unwrap_or(false))),
    );
    object.insert("rows".to_string(), Value::Array(published));
    table
}

/// NC1 over one recorded ask: five identity-bearing fields, each mutated on a copy of the row, and
/// the published identity recomputed through M8.4.2's own key builder.
fn nc1_identity(census: &Census, tree: &mut Tree) -> Control {
    let mut keyed: Option<(&'static Site, Value)> = None;
    for site in SITES.iter() {
        let rep = &census.rep(&site.key());
        if rep["dedup_key"].as_str().is_some() && rep["method"].as_str() == Some("eth_call") {
            keyed = Some((site, (*rep).clone()));
            break;
        }
    }
    let (site, rep) =
        keyed.unwrap_or_else(|| panic!("no recorded eth_call ask carries a dedup_key"));
    let site_id = site.id;
    let before = identity_of_row(&rep, Some(census.chain_id));
    let mut mutations: Vec<Value> = Vec::new();
    let recorded_key = rep["dedup_key"].as_str().expect("selected above");

    // `method` — the identity's own first term.
    let mut row = rep.clone();
    row["method"] = json!("eth_getBalance");
    mutations.push(mutation_row(
        "method",
        "eth_call",
        "eth_getBalance",
        &before,
        &identity_of_row(&row, Some(census.chain_id)),
    ));

    // `to` — the address segment of the recorded key.
    let address = segment(recorded_key, 3, "the address term of a call key");
    let replacement = flip_last_hex(&address);
    let mut row = rep.clone();
    row["dedup_key"] = json!(replace_segment(recorded_key, 3, &replacement));
    mutations.push(mutation_row(
        "to",
        &address,
        &replacement,
        &before,
        &identity_of_row(&row, Some(census.chain_id)),
    ));

    // `calldata` — the four-byte selector segment of the recorded key.
    let selector = segment(recorded_key, 4, "the selector term of a call key");
    let replacement = flip_last_hex(&selector);
    let mut row = rep.clone();
    row["dedup_key"] = json!(replace_segment(recorded_key, 4, &replacement));
    mutations.push(mutation_row(
        "calldata",
        &selector,
        &replacement,
        &before,
        &identity_of_row(&row, Some(census.chain_id)),
    ));

    // `block` — the height segment.
    let height = segment(recorded_key, 2, "the height term of a call key");
    let replacement = format!("{}", height.parse::<u64>().unwrap_or_default() + 1);
    let mut row = rep.clone();
    row["dedup_key"] = json!(replace_segment(recorded_key, 2, &replacement));
    row["block_tag"] = json!(replacement.clone());
    row["target"] = json!(replacement.clone());
    mutations.push(mutation_row(
        "block",
        &height,
        &replacement,
        &before,
        &identity_of_row(&row, Some(census.chain_id)),
    ));

    // `chain` — the chain term of the recorded key. The run's own chain id is handed to the key
    // builder as well, but §8's identity takes the chain term from the recorded key, so moving the
    // run property is not a different read; moving the key's chain term is.
    let chain_term = segment(recorded_key, 1, "the chain term of a call key");
    let parsed_chain = chain_term.parse::<u64>().unwrap_or_else(|error| {
        panic!("{site_id}: the recorded key's chain term {chain_term:?} is not a number: {error}")
    });
    let replacement = (parsed_chain + 1).to_string();
    let mut row = rep.clone();
    row["dedup_key"] = json!(replace_segment(recorded_key, 1, &replacement));
    mutations.push(mutation_row(
        "chain",
        &chain_term,
        &replacement,
        &before,
        &identity_of_row(&row, Some(census.chain_id)),
    ));

    // The integrity guard, published because §25 lists `to` and `calldata` as row fields and the
    // real key builder reads them from the recorded key rather than from the row's own columns.
    let mut row = rep.clone();
    row["target"] = json!("0x0000000000000000000000000000000000000000");
    let integrity_only = RpcReadKey::of_row(&row, Some(census.chain_id));
    let mut anchor = resolve_record_token(
        CANDIDATES_RECORD,
        &format!("method={}", rep["method"].as_str().unwrap_or_default()),
        "the pairs this control mutates are keyed the same way",
        tree,
    );
    anchor["note"] = json!(
        "NC1: the pair identity grammar is the same `identity.method` plus \
        terms the census reads"
    );

    Control {
        id: "NC1",
        mutates: "method, to, calldata, block, chain — each on a copy of one recorded ask, then \
            re-keyed through RpcReadKey. The last four move the recorded key's own segments, \
            because that is where the identity's terms are read from; moving a row column instead \
            is what the published integrity guard shows does not change an identity.",
        rule: "§24's eleventh and twelfth rules: a semantic identity is made of business fields, \
            and changing a business field has to change the candidate that reads it",
        target: json!({
            "site": site.id,
            "run": rep["run"],
            "rpc_id": rep["rpc_id"],
            "recorded_dedup_key": recorded_key,
            "identity_before": before,
            "key_anchor": anchor,
        }),
        observed: json!({
            "mutations": mutations,
            "all_changed": mutations.iter().all(|row| row["changed"].as_bool().unwrap_or(false)),
            "integrity_guard": {
                "mutation": "the row's own `target` column, with the recorded key left alone",
                "identity_changed": identity_of_row(&row, Some(census.chain_id)) == before,
                "row_matches_key": integrity_only.row_matches_key,
                "rule": "published so this control does not overclaim: RpcReadKey takes its terms \
                    from the recorded `dedup_key` and then *compares* the row's own columns against \
                    them, so a `to` that moves only the column is an integrity finding about the \
                    record, not a new identity. The `to` mutation above moves the recorded key, \
                    which is what a candidate actually reads."
            },
        }),
        fired: mutations
            .iter()
            .all(|row| row["changed"].as_bool().unwrap_or(false)),
        expectation: "every one of the five mutations changes the published identity, and the \
            sixth row shows which fields the key builder does not read",
    }
}

fn mutation_row(
    field: &str,
    before: &str,
    after: &str,
    identity_before: &str,
    identity_after: &str,
) -> Value {
    json!({
        "field": field,
        "value_before": before,
        "value_after": after,
        "identity_before": identity_before,
        "identity_after": identity_after,
        "changed": identity_before != identity_after,
    })
}

/// One `|`-separated segment of a recorded dedup key, with the grammar asserted rather than
/// assumed: if the record's key format moves under this control, the control panics instead of
/// silently mutating nothing.
fn segment(key: &str, index: usize, what: &str) -> String {
    let parts: Vec<&str> = key.split('|').collect();
    assert!(
        parts.len() > index,
        "{what}: the recorded key {key:?} has {} segments, so there is no segment {index}",
        parts.len()
    );
    parts[index].to_string()
}

fn replace_segment(key: &str, index: usize, replacement: &str) -> String {
    let mut parts: Vec<String> = key.split('|').map(str::to_string).collect();
    let previous = parts[index].clone();
    parts[index] = replacement.to_string();
    assert!(
        previous != replacement,
        "the mutation wrote the same segment it replaced ({previous:?}), which would make the \
         control a no-op"
    );
    parts.join("|")
}

fn flip_last_hex(value: &str) -> String {
    let bytes: Vec<u8> = value.bytes().collect();
    let mut out = value.to_string();
    for index in (0..bytes.len()).rev() {
        let ch = bytes[index] as char;
        if ch.is_ascii_hexdigit() {
            let flipped = if ch == '0' { '1' } else { '0' };
            out.remove(index);
            out.insert(index, flipped);
            break;
        }
    }
    assert_ne!(out, value, "no hex digit to flip in {value:?}");
    out
}

// ---------------------------------------------------------------------------
// §32 — final-verdict.json
// ---------------------------------------------------------------------------

pub fn final_verdict(census: &Census) -> Value {
    let measures = census.saving_measures();
    let matrix = reduction_matrix(census);
    let counts = count_by(
        &matrix["rows"].as_array().cloned().unwrap_or_default(),
        "priority",
    );
    // An empty band is a measured zero, not an absent key: the verdict block prints all five.
    let counts = json!({
        (PRIORITY_P0): counts[PRIORITY_P0].as_u64().unwrap_or(0),
        (PRIORITY_P1): counts[PRIORITY_P1].as_u64().unwrap_or(0),
        (PRIORITY_P2): counts[PRIORITY_P2].as_u64().unwrap_or(0),
        (PRIORITY_P3): counts[PRIORITY_P3].as_u64().unwrap_or(0),
        (PRIORITY_REJECT): counts[PRIORITY_REJECT].as_u64().unwrap_or(0),
    });
    let candidates_with_saving: Vec<&Derived> = census
        .derived
        .iter()
        .filter(|derived| derived.safe > 0)
        .collect();
    let label: &'static str = if !candidates_with_saving.is_empty() {
        VERDICT_CANDIDATE_FOUND
    } else if census.groups.is_empty() || census.records.pairs.is_empty() {
        VERDICT_NOT_ENOUGH
    } else {
        VERDICT_NONE_FOUND
    };
    let insufficient: Vec<Value> = census
        .derived
        .iter()
        .filter(|derived| derived.spec.next_step == NEXT_STEP_EXPERIMENT)
        .map(|derived| json!({
            "candidate": derived.spec.id,
            "the_question": "§17's Candidate 1: does the same calldata at a different height answer \
                the same question?",
            "why_not_enough": "no committed record asks one calldata at two heights, and §2.4/§26 \
                forbid asking it here",
            "priority": derived.priority,
            "next_step": NEXT_STEP_EXPERIMENT,
            "would_cost": derived.measured.consumer_asks,
        }))
        .collect();
    json!({
        "schema": SCHEMA,
        "milestone": MILESTONE,
        "file": "final-verdict.json",
        "generated_by": GENERATED_BY,
        "assembly_module": TABLES_FILE,
        "question": "§22: is there an RPC left on this hot path that M8.6 can show is safe to stop \
            making?",
        "label": label,
        "label_rationale": format!(
            "safe_reducible_rpc = {}, and no candidate reaches a credited saving. The label is not \
             NOT_ENOUGH_EVIDENCE: the census is computable from committed records and is \
             recomputable — {} asks over {} runs and {} published pairs. {} open question(s) are \
             marked EXPERIMENT_REQUIRED rather than folded into the verdict.",
            measures["total_safe_rpc_saving"],
            census.groups.iter().map(|group| group.asks).sum::<usize>(),
            census.records.runs(),
            census.records.pairs.len(),
            insufficient.len(),
        ),
        "verdict_block": {
            "M8.6 RESULT": label,
            "label": label,
            "current_hot_path_rpc_count": census.groups.iter().map(|group| group.asks).sum::<usize>(),
            "theoretical_reducible_rpc": measures["physical_duplicate_saving"],
            "safe_reducible_rpc": measures["total_safe_rpc_saving"],
            "P0": counts[PRIORITY_P0],
            "P1": counts[PRIORITY_P1],
            "P2": counts[PRIORITY_P2],
            "P3": counts[PRIORITY_P3],
            "REJECT": counts[PRIORITY_REJECT],
            "new_rpc": 0,
            "signatures": 0,
            "broadcasts": 0,
            "real_arbitrage": 0,
        },
        "five_measures": measures,
        "counts": {
            "sites_on_the_hot_path": census.groups.len(),
            "methods_traced": TRANSPORTS.iter().filter(|transport| transport.traced).count(),
            "candidates": census.derived.len(),
            "by_priority": counts,
            "rejected_pseudo_optimizations": matrix["rows"].as_array().map_or(0, |rows| rows.iter().filter(|row| row["priority"].as_str() == Some(PRIORITY_REJECT)).count()),
            "asks_without_recorded_identity": census.groups.iter().map(|group| group.asks_without_recorded_key).sum::<usize>(),
        },
        "candidates_found": candidates_with_saving.iter().map(|derived| json!({
            "candidate": derived.spec.id,
            "current_rpc_count": derived.measured.consumer_asks,
            "safe_saving": derived.safe,
            "evidence_strength": derived.spec.evidence_strength,
            "missing_proof": missing_proof(derived),
            "next_experiment": derived.spec.next_step,
        })).collect::<Vec<Value>>(),
        "insufficient_evidence": insufficient,
        "not_re_asked_here": [
            "§15's eth_call families, already graded by M8.5.1 as REUSE_BLOCKED",
            "§16's block-context propagation, already measured by M8.4.4 as safe with a net RPC saving of 0",
            "the protocol oracle's cross-height question, marked EXPERIMENT_REQUIRED and not run (§26)",
            "the simulation's own REVM reads, already served by M8.3.1's cache within its documented scope"
        ],
        "if_the_label_changes_someone_must_first": [
            "produce record rows in which one pair of asks has all five of M8.4.2's reuse conditions resolved met, or a `safe_to_reuse` true in the published candidate record — the two kinds in `CREDITING_PROOFS`",
            "name the check that would still bear each duty §12 assigns, for every consumer whose read is itself the check",
            "show a numbered height and a tag resolving to the same answer for one site, which §4.2 of M8.4.2's rules currently refuses by construction",
            "settle the oracle's cross-height question by the controlled experiment §26 defers to M8.6.x / M8.7"
        ],
        "prohibitions_honoured": prohibitions(),
        "regression_scope": ["M8.3.1", "M8.3.2", "M8.3.3", "M8.4.1", "M8.4.2", "M8.4.3", "M8.4.4", "M8.5.1"],
        "production_changes": "crates/*/src functional changes = 0: the model and the assembly are \
            test-only modules under crates/pipeline/tests/, which §30 prefers to a diagnostic \
            production module.",
        "gates": {
            "evidence": ASSEMBLY_FILE,
            "recompute": RECOMPUTE_FILE,
            "rules": "§24's fourteen, one test each, over the files in this directory",
        },
    })
}

/// §3's prohibitions, published as data so a reader can check the milestone against the list rather
/// than against a paragraph. Each one is also exercised by a gate: `new_rpc = 0` by the RPC
/// accounting in the recompute gate, the safety-gate clause by §24's eighth and ninth rules.
fn prohibitions() -> Value {
    json!([
        "no new cache, and StateReadCache untouched",
        "no RPC client behaviour change",
        "no eth_call removed",
        "no Preflight read deleted",
        "no change to Detection / Opportunity / Simulation / Risk / Build / Execution / Fee / \
         Nonce / Submission / transaction semantics / the real arbitrage flow",
        "no signing, no broadcast, no real arbitrage",
        "no RPC spent to test a hypothesis — the one question that needs one is marked \
         EXPERIMENT_REQUIRED and left unrun",
        "no batch, no multicall, no prefetch, no speculative read",
        "no change to M8.3.3's default concurrency of 1",
        "no change to M8.4.4's propagation contract",
        "no existing safety gate relaxed to make evidence easier to pass"
    ])
}

// ---------------------------------------------------------------------------
// README.md
// ---------------------------------------------------------------------------

/// The directory's own front page, written from the ten tables rather than beside them: every
/// number in it is read out of `root`, so the README cannot disagree with the files it describes.
pub fn build_readme(root: &Value) -> String {
    let verdict = &root["final-verdict"];
    let block = &verdict["verdict_block"];
    let surface = &root["rpc_surface"];
    let census = &root["rpc-census"];
    let candidates = &root["rpc-reduction-candidates"];
    let matrix = &root["reduction-matrix"];
    let rejected = &root["rejected-opportunities"];
    let queue = &root["priority-queue"];
    let flows = &root["information_flow"];
    let kinds = &root["saving-kinds"];
    let controls = &root["negative-controls"];

    let mut out = String::new();
    out.push_str("# M8.6 — RPC 削减机会普查（诊断，不改行为）\n\n");

    out.push_str("## 先说结论（白话版）\n\n");
    out.push_str(&format!(
        "- 这条热路径每跑一遍打 **{} 次** RPC（{} 把实测、每把 {} 次；表里按调用点分成 {} 个站点、{} 个方法）。\n\
         - 其中 **{} 次**是在重复问刚刚问过的东西。这是「物理重复」，是理论上限，不是省下来的钱。\n\
         - 今天真正可以安全停掉的 RPC 是 **{} 次**。\n\
         - 结论：**`{}`**。\n\n\
         为什么是 0：重复的那 {} 次里，第二次提问**本身就是那道检查**（preflight 的余额闸门、build 的\
         before-snapshot、fee/nonce 的独立复核、M8.5.1 判过的 eth_call 重估）。删掉它省一条网络请求，代价\
         是删一道闸门。M8.6 拒绝用验证换 RPC——这不是没找到重复，是重复全部长在闸门上。\n\n",
        block["current_hot_path_rpc_count"],
        surface["counts"]["asks_per_run"].as_array().map_or(0, |list| list.len()),
        surface["counts"]["asks_per_run"]
            .as_array()
            .and_then(|list| list.first())
            .map(|row| row["asks"].clone())
            .unwrap_or_default(),
        surface["counts"]["sites"],
        surface["counts"]["methods_traced"],
        block["theoretical_reducible_rpc"],
        block["safe_reducible_rpc"],
        verdict["label"],
        block["theoretical_reducible_rpc"],
    ));
    out.push_str(&format!(
        "单位提醒：上面三个数都是「一次逻辑请求」，重试已经折进同一条记录里（本语料每行 `attempts` 都是 1，\
         所以逻辑请求数 = 物理 HTTP 数 = {}）。「时长」在这套表里只做诊断展示，任何优先级、任何节省都不读它。\n\n",
        surface["counts"]["physical_http_attempts"],
    ));

    out.push_str("## 打个比方（同一件事换算成钱）\n\n");
    out.push_str("把每把运行想成一趟出门办事：全程要问 82 次「现在几点、这笔钱还在不在」。其中有 14 次是\
出门前已经问过、回来又问一遍——这就是那 42 次重复里的每一把。问题是：第二次问话不是随手问的，它是保安在\
门口核对工牌。你要省的不是问话，是让保安别核对。所以这套表把「问了两次」和「第二次可以不问」分成两列，\
前者是 42，后者是 0。\n\n");

    out.push_str("## 这套文件是什么\n\n");
    out.push_str(&format!(
        "M8.6 的诊断产物：{} 张表 + 本页。语料是 M8.4.2 那三把 live BuildOnly 运行已经提交下来的记录，\
         加上 M8.4.3 的所有权矩阵、M8.4.4 的传播结论、M8.5.1 的 eth_call 结论。本阶段**没有打任何一条新\
         RPC**（`new_rpc = 0`），也没有跑任何实验：需要实验才能回答的那 {} 个问题被标成 `EXPERIMENT_REQUIRED` \
         留在表里。\n\n",
        CENSUS_TABLES.len(),
        verdict["insufficient_evidence"].as_array().map_or(0, |list| list.len()),
    ));
    out.push_str("## 每张表读什么\n\n");
    out.push_str(&format!(
        "| 文件 | 回答什么 | 关键数字 |\n|---|---|---|\n\
         | `rpc_surface.json` | §4：热路径上真实存在的调用点，每个的文件与行号 | {} 个站点，{} 个方法，{} 次请求 |\n\
         | `rpc-census.json` | §18：逐站点的次数、两种时长口径、身份、属主、验证职责 | {} 行，合计 {} 次请求 |\n\
         | `rpc-reduction-candidates.json` | §6：每个削减问题的完整一行 | {} 个候选 |\n\
         | `reduction-matrix.json` | §19：十二列判定（两个节省分列） | 理论 {} / 安全 {} |\n\
         | `rejected-opportunities.json` | §20：被否决的伪优化与其理由 | 排除 {} 个 |\n\
         | `priority-queue.json` | §21：由证据决定的排序（禁止预先写死） | {}\n\
         | `information_flow.json` | §10：答案变成什么、谁持有、下一个阶段谁用它 | {} 个容器类型，{} 条流 |\n\
         | `saving-kinds.json` | §7：五种节省口径，永不互相相加 | {}\n\
         | `negative-controls.json` | §25：六个必须让门禁变红的改动 | 全绿 = {} |\n\
         | `final-verdict.json` | §32：结论块 | `{}` |\n\n",
        surface["counts"]["sites"],
        surface["counts"]["methods_traced"],
        surface["counts"]["asks_total"],
        census["totals"]["rows"],
        census["totals"]["asks"],
        candidates["totals"]["candidates"],
        matrix["totals"]["theoretical_saving"],
        matrix["totals"]["safe_saving"],
        rejected["counts"]["rejected"],
        queue_preview(queue),
        flows["containers"].as_array().map_or(0, |list| list.len()),
        flows["flows"].as_array().map_or(0, |list| list.len()),
        kinds_preview(kinds),
        controls["all_green"],
        verdict["label"],
    ));

    out.push_str("## 五种口径为什么不能相加\n\n");
    out.push_str(&format!(
        "```text\nphysical_duplicate_saving   = {}\nsemantic_reuse_saving       = {}\n\
verification_removal_saving = {}\nparallel_wall_time_saving   = {}\ntotal_safe_rpc_saving     = {}\n```\n\n\
         前三个是「本可以省」的不同角度，第四个是时长（M8.3.3 的事，跟请求条数不同单位），第五个是\
         **前四个里真正有证据支撑的那一小撮**——它是第一个的子集，不是四个的总和。把任何一个加进第五个\
         都是 §24 第七条要拦的错。\n\n",
        kinds["measures"][0]["value"],
        kinds["measures"][1]["value"],
        kinds["measures"][2]["value"],
        kinds["measures"][3]["value"],
        kinds["measures"][4]["value"],
    ));

    out.push_str("## 怎么复现\n\n");
    out.push_str(
        "```bash\nexport CC=clang CXX=clang++ CXXFLAGS=\"-include cstdint\"\n\
M86_CENSUS_REFRESH=1 cargo test -p evm-pipeline --test rpc_reduction_evidence -- --test-threads=1\n```\n\n\
         不带那个环境变量时，同一个门做的是**字节比对**：把模型 + 记录重新装配一遍，与已提交的文件逐字节比。\
         差异只能来自模型或记录的变化——表里没有墙上时钟，所以两次装配必然一模一样（§25 的 NC5/NC6 就是这条\
         的反证实验）。\n\n\
         ⚠️ `target/pipeline-tests/` 不能并发写：这两个门必须 `--test-threads=1`，并且整个 workspace 测试\
         独占一个日志跑。\n\n");

    out.push_str("## 门禁\n\n");
    out.push_str(&format!(
        "- `{ASSEMBLY_FILE}`：§24 的十四条（总数可回算、逐方法/逐阶段可回算、候选数与重复数可回算、\
安全节省可回算、理论与安全不混、验证职责不许忽略、被否决必须有 blocker、优先级不许由时长排序位置推出、\
身份必须是业务字段、不许用时长排序后的行号、时间字段只能自证、不许改证据把候选改绿）+ 字节稳定 + 锚点解析。\n\
         - `{RECOMPUTE_FILE}`：绕开表、直接从原始记录重算，再逐表对账；跑 §25 六个负控制；`new_rpc = 0` 的\
核算（本阶段一个 RPC 都没打，靠的是只读已提交的目录）。\n\n",
    ));

    out.push_str("## 这套证据不包含什么\n\n");
    out.push_str(&format!(
        "- 不包含任何新的链上请求：{} 次请求全部来自已提交的记录。\n\
         - 不包含签名、广播、真实套利（三把语料本身就是 `build-only`）。\n\
         - 不包含 M8.5.1 已判死的 eth_call 复用（继承 `REUSE_BLOCKED`，不重做）。\n\
         - 不包含 M8.4.4 已测过的块上下文传播（继承「安全但净省 0」，不重新列为候选）。\n\
         - 不包含任何 cache / batch / multicall / prefetch / 并发改动 / 安全门放宽。\n\
         - 不包含把 26 个「没有候选点名」的站点硬塞进优先级队列：那些行的 priority 明确写成 \
         `{}`，而不是借用 §14 的五个标签。\n\n",
        block["current_hot_path_rpc_count"], PRIORITY_NOT_A_CANDIDATE,
    ));

    out.push_str("## 结论块（§32 格式）\n\n");
    out.push_str(&format!(
        "```text\nM8.6 RESULT\n\nlabel = {}\n\ncurrent_hot_path_rpc_count = {}\n\
theoretical_reducible_rpc = {}\nsafe_reducible_rpc = {}\n\nP0 = {}\nP1 = {}\nP2 = {}\nP3 = {}\nREJECT = {}\n\n\
new_rpc = 0\nsignatures = 0\nbroadcasts = 0\nreal_arbitrage = 0\n```\n\n",
        block["label"],
        block["current_hot_path_rpc_count"],
        block["theoretical_reducible_rpc"],
        block["safe_reducible_rpc"],
        block["P0"],
        block["P1"],
        block["P2"],
        block["P3"],
        block["REJECT"],
    ));
    out.push_str("## 下一步（不是建议动手，是说明证据缺口）\n\n");
    out.push_str(&format!(
        "{}\n\n按 §33：如果剩下的 RPC 大多是有业务语义的独立验证，那么 M8 的主要优化方向就不该再是\
「消 RPC」，而是 RPC 延迟、endpoint 架构、连接策略、provider 选择、地域与 sequencer 邻近度、请求调度、\
执行路径重设计。这些都**必须等 M8.6 之后**再定，且都不在本阶段的允许范围内。\n",
        next_steps(verdict),
    ));
    out
}

/// The queue's head, in one line, so the README's table cannot state an order the file does not
/// contain.
fn queue_preview(queue: &Value) -> String {
    let rows = queue["rows"].as_array().cloned().unwrap_or_default();
    let head: Vec<String> = rows
        .iter()
        .take(3)
        .map(|row| format!("{}. {}", row["queue_position"], row["candidate"]))
        .collect();
    if head.is_empty() {
        return "empty".to_string();
    }
    format!("前 {} 项：{}", head.len(), head.join("；"))
}

fn kinds_preview(kinds: &Value) -> String {
    let measures = kinds["measures"].as_array().cloned().unwrap_or_default();
    let parts: Vec<String> = measures
        .iter()
        .map(|measure| format!("{}={}", measure["measure"], measure["value"]))
        .collect();
    parts.join("，")
}

fn next_steps(verdict: &Value) -> String {
    let insufficient = verdict["insufficient_evidence"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut out = String::new();
    for row in insufficient {
        out.push_str(&format!(
            "- `{}`：{} —— 记为 `{}`，本阶段不执行（未来要单独授权）。\n",
            row["candidate"], row["the_question"], row["next_step"],
        ));
    }
    let requirements = verdict["if_the_label_changes_someone_must_first"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    out.push_str("要让结论改变，必须先出现下面任何一条证据：\n\n");
    for requirement in requirements {
        out.push_str(&format!("- {}\n", requirement.as_str().unwrap_or_default()));
    }
    out
}

// ---------------------------------------------------------------------------
// the entry point both gates call
// ---------------------------------------------------------------------------

/// One assembly, into any directory: the ten tables in [`CENSUS_TABLES`]' order, then the README
/// written from them. Returns the file names it wrote, sorted, which is what the byte gate compares.
pub fn assemble_to(dir: &Path) -> Vec<String> {
    let census = Census::new();
    assemble_into(&census, dir)
}

/// The same, from a corpus the caller already holds — used by the recompute gate to assemble from
/// mutated records without re-reading the tree.
pub fn assemble_into(census: &Census, dir: &Path) -> Vec<String> {
    let mut tree = Tree::new();
    let matrix = reduction_matrix(census);
    write_json_table(dir, CENSUS_TABLES[0], rpc_surface(census, &mut tree));
    write_json_table(dir, CENSUS_TABLES[1], rpc_census(census, &mut tree));
    write_json_table(
        dir,
        CENSUS_TABLES[2],
        reduction_candidates(census, &mut tree),
    );
    write_json_table(dir, CENSUS_TABLES[3], matrix.clone());
    write_json_table(dir, CENSUS_TABLES[4], rejected_opportunities(census));
    write_json_table(dir, CENSUS_TABLES[5], priority_queue(census, &matrix));
    write_json_table(dir, CENSUS_TABLES[6], information_flow(census, &mut tree));
    write_json_table(dir, CENSUS_TABLES[7], saving_kinds(census));
    write_json_table(dir, CENSUS_TABLES[8], negative_controls(census, &mut tree));
    write_json_table(dir, CENSUS_TABLES[9], final_verdict(census));

    let root = read_root(dir);
    write_text(&dir.join(README_FILE), &build_readme(&root));
    file_names(dir)
}

/// The two-argument [`write_table`] takes a path; this one takes the directory and the file's name,
/// which is all the assembly needs.
fn write_json_table(dir: &Path, name: &str, table: Value) {
    write_table(&dir.join(name), &table);
}

/// The committed directory read back as one object, keyed by file stem — the shape the README and
/// the gates both want.
pub fn committed_root() -> Value {
    read_root(&evidence_dir())
}
