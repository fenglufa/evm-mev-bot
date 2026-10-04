//! M8.6 §24's evidence gate: the eleven files under `data/evidence/m8/m8.6/`.
//!
//! # What this directory answers
//!
//! §1 asks for one chain of judgement per RPC call site — 阶段 → 调用者 → 语义类型 → identity →
//! 是否重复 → 是否可复用 → 是否承担 verification → 是否可传播 → freshness → 能否替代 → 理论最大节省
//! → 真实可实现节省 → 风险 → 优先级 — and §22 asks whether any of it ends in an RPC that can safely
//! stop being sent. Ten tables plus this page answer it. The model that holds the judgements is
//! [`m86_census`]; the code that turns model plus records into files is [`m86_census::tables`].
//!
//! # Nothing here was measured by asking the node
//!
//! §2.4 and §26 forbid spending an RPC to find a saving, so every count is folded from records this
//! repository already commits: the three M8.4.2 live BuildOnly runs' `pipeline-calls.json`, their
//! `reuse-candidates.json` pairs, M8.4.3's ownership matrix, M8.4.4's propagation summary, M8.5.1's
//! eth_call verdicts. This file opens no socket and holds no client.
//!
//! # What the gate refuses to accept
//!
//! §24's fourteen rules are implemented as fourteen named tests below, and each one recomputes the
//! published figure from the records rather than re-reading the table that states it. The five
//! saving measures stay five numbers (§7), no priority is derived from a duration (§24's tenth), no
//! identity is a row index (eleventh), and no candidate may be credited a saving the record itself
//! does not support (§24's fourteenth, which is what [`m86_census::tables`]' negative controls
//! exercise by mutating real rows).
//!
//! # How these files change
//!
//! Assembly happens under `target/pipeline-tests/`; `M86_CENSUS_REFRESH=1` copies a fresh assembly
//! over the committed directory, which is the only way these eleven files change. Anchor line
//! numbers are resolved out of the source files at assembly time, so a line number in the evidence
//! is measured rather than copied — when the code moves, the byte gate fails until the tables are
//! refreshed from the code that now holds.

#![recursion_limit = "2048"]

mod m86_census;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use m86_census::tables::*;
use m86_census::*;

/// The tree's own front page, named here so the file-set assertion below has one source.
const README: &str = README_FILE;

/// Every committed record path, read straight off the disk by this gate. The gate uses these for
/// the rules §24 states as "可回算" — a figure the table publishes and the raw record also produces
/// when folded by a path that never touches the table.
fn read_json_at(relative: &str) -> Value {
    read_json(&repo_path(relative))
}

/// The three runs' recorded asks, read without the model in the way.
fn raw_call_rows() -> Vec<Value> {
    let mut rows = Vec::new();
    for run in committed_run_names() {
        let calls = read_json_at(&format!("{RUNS_DIR}/{run}/{CALLS_FILE}"));
        let list = calls["rows"]
            .as_array()
            .unwrap_or_else(|| panic!("{run}/{CALLS_FILE}: no `rows` array to fold"));
        rows.extend(list.iter().cloned());
    }
    rows
}

/// The run names as the committed directory lists them — a glob over the filesystem, so a fourth
/// run would be counted here without anything in the model changing.
fn committed_run_names() -> Vec<String> {
    let dir = repo_path(RUNS_DIR);
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().join(CALLS_FILE).is_file())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert!(
        !names.is_empty(),
        "{RUNS_DIR} holds no run with a {CALLS_FILE}"
    );
    names
}

// ---------------------------------------------------------------------------
// the byte gate
// ---------------------------------------------------------------------------

/// §24's container: the committed directory is one assembly of the model and the records, and
/// nothing else is in it. `M86_CENSUS_REFRESH=1` is the only way it changes.
#[test]
fn the_committed_directory_is_a_reassembly_of_the_model_and_the_records() {
    let dir = scratch("m86-byte-gate");
    let names = assemble_to(&dir);
    let mut expected = generated_files();
    expected.sort();
    assert_eq!(
        names, expected,
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
        .unwrap_or_else(|error| {
            panic!(
                "{}: {error} — the evidence tree has to exist before the gate can compare it; run \
             M86_CENSUS_REFRESH=1 once to assemble it",
                committed.display()
            )
        })
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
        let stored = read_bytes(&committed.join(name));
        assert_eq!(
            fresh, stored,
            "{name}: the committed bytes are not what one assembly of the model and the records \
             produces — refresh with {REFRESH_ENV}=1"
        );
    }

    // Two assemblies in the same run must agree byte for byte, or "deterministic" would be a claim
    // about one particular process.
    let twice = scratch("m86-byte-gate-twice");
    assemble_to(&twice);
    for name in &names {
        assert_eq!(
            read_bytes(&dir.join(name)),
            read_bytes(&twice.join(name)),
            "{name}: two assemblies in the same process differ"
        );
    }
}

/// Every anchor the model declares has to resolve to a place. This is the floor under the surface
/// table: if a call site's code moved or disappeared, no count above it means what it says.
///
/// The resolution here is done a second time, inside the gate, by a path that never calls the
/// model's own resolver — the published `line` is compared against the line the token actually
/// occupies in the file on disk. A table that merely repeats what the assembly thought would pass
/// a weaker test and still fail this one.
#[test]
fn every_anchor_the_model_declares_is_published_with_a_resolved_line() {
    let root = read_dir_or_refresh();
    let mut resolved = 0usize;
    let mut files: BTreeSet<String> = BTreeSet::new();
    let mut site_files: BTreeSet<String> = BTreeSet::new();
    for (name, table) in root
        .as_object()
        .expect("the root is a map of tables")
        .iter()
    {
        let Some(anchors) = table["anchors"].as_array() else {
            continue;
        };
        for anchor in anchors {
            let file = anchor["file"].as_str().unwrap_or_default();
            assert!(
                !file.is_empty(),
                "{name}: an anchor that names no file: {anchor}"
            );
            let token = anchor["token"].as_str().unwrap_or_default();
            assert!(
                !token.is_empty(),
                "{name}: an anchor with an empty token: {anchor}"
            );
            let line = anchor["line"].as_u64().unwrap_or_default();
            assert!(line > 0, "{name}: an anchor resolved to no line: {anchor}");

            let (disk_line, disk_matches) = resolve_on_disk(file, token);
            assert_eq!(
                disk_matches, 1,
                "{name}: the token {token:?} matches {disk_matches} lines of {file}, but the \
                 anchor published in {name} claims one place"
            );
            assert_eq!(
                disk_line, line,
                "{name}: the anchor for {token:?} publishes line {line} while the token now sits \
                 at line {disk_line} of {file} — the code moved and the table was not refreshed"
            );
            files.insert(file.to_string());
            resolved += 1;
        }
    }
    assert!(
        resolved >= SITES.len(),
        "the tables resolve {resolved} anchor references, fewer than the {} call sites the model \
         names — a site whose code is not pointed at is a site whose behaviour was not read",
        SITES.len()
    );
    for file in &files {
        assert!(
            repo_path(file).is_file(),
            "an anchor points at {file}, which is not a file in this repository"
        );
    }
    // Every call site's own file must be pointed at by some anchor, or the site's behaviour was
    // never read — the census could otherwise count a method whose code it has not opened.
    for site in SITES {
        site_files.insert(site.stamp.0.to_string());
    }
    let unpointed: BTreeSet<String> = site_files.difference(&files).cloned().collect();
    assert!(
        unpointed.is_empty(),
        "call sites live in these files while no anchor points into them: {unpointed:?}"
    );
}

/// Where a token actually is: the 1-based line of its only match in the file, searching only the
/// lines before the first `#[cfg(test)]` so a test module cannot supply the evidence for a claim
/// about production code. Returns (line, match count) and never fails — the caller states what a
/// wrong answer means.
fn resolve_on_disk(file: &str, token: &str) -> (u64, usize) {
    let path = repo_path(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{}: an anchor could not be checked: {error}",
            path.display()
        )
    });
    let mut hits = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.contains("#[cfg(test)]") {
            break;
        }
        if line.contains(token) {
            hits.push(index as u64 + 1);
        }
    }
    (hits.first().copied().unwrap_or(0), hits.len())
}

/// The committed tree, or a fresh assembly when the tree is not there yet — so a reader who has not
/// run the refresh still gets a useful failure rather than a missing-directory panic.
fn read_dir_or_refresh() -> Value {
    let committed = evidence_dir();
    if committed.is_dir() {
        return read_root(&committed);
    }
    let dir = scratch("m86-from-assembly");
    assemble_to(&dir);
    read_root(&dir)
}

// ---------------------------------------------------------------------------
// §24's fourteen rules, one test each
// ---------------------------------------------------------------------------
//
// Each test below takes a figure the tables publish and folds the same figure out of the committed
// records by a path that never reads the table stating it. The counts come from
// `pipeline-calls.json` and `reuse-candidates.json`; the arithmetic is re-implemented here in a few
// lines per rule rather than borrowed from `m86_census::tables`, because a recompute that calls the
// code being checked proves nothing.

const SURFACE_TABLE: &str = CENSUS_TABLES[0];
const CENSUS_TABLE: &str = CENSUS_TABLES[1];
const CANDIDATE_TABLE: &str = CENSUS_TABLES[2];
const MATRIX_TABLE: &str = CENSUS_TABLES[3];
const REJECTED_TABLE: &str = CENSUS_TABLES[4];
const QUEUE_TABLE: &str = CENSUS_TABLES[5];
const SAVING_TABLE: &str = CENSUS_TABLES[7];
const VERDICT_TABLE: &str = CENSUS_TABLES[9];

/// One table, by the file name it publishes itself under.
fn published(file: &str) -> Value {
    let stem = file.strip_suffix(".json").unwrap_or(file).to_string();
    read_dir_or_refresh()
        .get(&stem)
        .cloned()
        .unwrap_or_else(|| panic!("{stem}: not in the evidence tree"))
}

/// The rows of one table, with the file named in the failure — every rule below counts them, so an
/// absent `rows` array would otherwise read as a zero.
fn rows_of(file: &str) -> Vec<Value> {
    published(file)["rows"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("{file}: publishes no `rows` array"))
}

/// M8.4.2's own pair rows, read straight from the record the census was folded from. Each carries a
/// `candidate_id` naming the pair of asks, which is what ties a published count back to one record
/// row without going through any position in any table.
fn recorded_pairs() -> Vec<Value> {
    read_json_at(CANDIDATES_RECORD)["rows"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("{CANDIDATES_RECORD}: publishes no pair rows"))
}

fn asks(rows: &[Value]) -> u64 {
    rows.iter()
        .map(|row| row["asks"].as_u64().unwrap_or_default())
        .sum()
}

/// What to call a row in a failure: a candidate table names its row, a census table names its site.
fn row_name(row: &Value) -> &str {
    row["candidate"]
        .as_str()
        .or_else(|| row["site_id"].as_str())
        .unwrap_or("<unnamed row>")
}

fn count_by(rows: &[Value], field: &str) -> BTreeMap<String, u64> {
    let mut tally: BTreeMap<String, u64> = BTreeMap::new();
    for row in rows {
        let value = row[field]
            .as_str()
            .unwrap_or_else(|| panic!("a row with no {field} string: {row}"))
            .to_string();
        *tally.entry(value).or_default() += 1;
    }
    tally
}

/// Turn a published `[{name_field: …, asks: …}]` array into the same map shape the raw fold
/// produces, so the two compare as values instead of being looked up one by one.
fn published_tally(file: &str, column: &str, name_field: &str) -> BTreeMap<String, u64> {
    let table = published(file);
    let mut tally: BTreeMap<String, u64> = BTreeMap::new();
    for entry in table[column]
        .as_array()
        .unwrap_or_else(|| panic!("{file}: no {column} array"))
    {
        let key = entry[name_field]
            .as_str()
            .unwrap_or_else(|| panic!("{file}: a {column} entry with no {name_field}: {entry}"))
            .to_string();
        tally.insert(key, entry["asks"].as_u64().unwrap_or_default());
    }
    tally
}

/// §24's first rule: the census's total is the number of records, not a number the table carries.
/// The same fold also settles §26's `new_rpc = 0` for the whole directory, because the runs the
/// tables describe are the runs the committed directory holds.
#[test]
fn rule_1_the_census_total_is_the_record_count_and_no_table_spent_an_rpc() {
    let raw = raw_call_rows();
    let census = published(CENSUS_TABLE);
    let surface = published(SURFACE_TABLE);
    let total = census["totals"]["asks"].as_u64().unwrap_or_default();

    assert_eq!(
        raw.len() as u64,
        total,
        "the census totals {total} asks while the committed runs record {}",
        raw.len()
    );
    assert_eq!(
        surface["counts"]["asks_total"].as_u64().unwrap_or_default(),
        total,
        "the surface table and the census disagree about how many asks the hot path makes"
    );
    assert_eq!(
        asks(rows_of(CENSUS_TABLE).as_slice()),
        total,
        "the census's own rows do not add up to the total in the same file"
    );

    // Per run: the records' own `run` field, not a table column.
    let mut per_run: BTreeMap<String, u64> = BTreeMap::new();
    for row in &raw {
        *per_run
            .entry(
                row["run"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a recorded ask with no run: {row}"))
                    .to_string(),
            )
            .or_default() += 1;
    }
    for entry in surface["counts"]["asks_per_run"]
        .as_array()
        .expect("asks_per_run")
    {
        let run = entry["run"].as_str().expect("a run name");
        assert_eq!(
            entry["asks"].as_u64(),
            per_run.get(run).copied(),
            "{run}: the surface table publishes {} asks, the records hold {:?}",
            entry["asks"],
            per_run.get(run)
        );
    }
    assert_eq!(
        per_run.len(),
        committed_run_names().len(),
        "the tables describe {} runs while {RUNS_DIR} holds {}",
        per_run.len(),
        committed_run_names().len()
    );

    // Logical asks and physical HTTP, folded from each row's own attempt list.
    let physical: u64 = raw
        .iter()
        .map(|row| row["attempts"].as_array().map_or(1, |list| list.len()) as u64)
        .sum();
    assert_eq!(
        physical,
        surface["counts"]["physical_http_attempts"]
            .as_u64()
            .unwrap_or_default(),
        "the physical count does not match the attempt lists in the records"
    );
    assert_eq!(
        physical, total,
        "the census calls every logical ask one HTTP call; a record row with more than one attempt \
         would make that false, and this is where it would show"
    );

    // §26: nothing in this directory was paid for.
    let root = read_dir_or_refresh();
    for (name, table) in root
        .as_object()
        .expect("the root is a map of tables")
        .iter()
    {
        if let Some(new_rpc) = table.get("new_rpc") {
            assert_eq!(
                new_rpc.as_u64(),
                Some(0),
                "{name}: publishes new_rpc {new_rpc} while §26 allows this milestone to spend none"
            );
        }
    }
}

/// §24's second rule: a method's count is the number of recorded asks naming that method, and a
/// method that appears in one of the tallies must appear in both.
#[test]
fn rule_2_every_method_count_recomputes_from_the_raw_rows() {
    let from_records = count_by(raw_call_rows().as_slice(), "method");
    let surface = published(SURFACE_TABLE);
    assert_eq!(
        from_records,
        published_tally(SURFACE_TABLE, "by_method", "method"),
        "the by_method tally is not the raw record fold"
    );
    let mut census_by_method: BTreeMap<String, u64> = BTreeMap::new();
    for row in rows_of(CENSUS_TABLE) {
        *census_by_method
            .entry(
                row["method"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a census row with no method: {row}"))
                    .to_string(),
            )
            .or_default() += row["asks"].as_u64().unwrap_or_default();
    }
    assert_eq!(
        census_by_method, from_records,
        "the census rows and the surface tally disagree about a method's asks"
    );
    assert_eq!(
        surface["counts"]["methods_traced"].as_u64(),
        Some(from_records.len() as u64),
        "the surface table counts {} methods while the records name {}",
        surface["counts"]["methods_traced"],
        from_records.len()
    );
}

/// §24's third rule, for the stage column. The connect-time read carries no stage stamp, so the
/// records themselves say which asks are unstamped — the tally is folded from each row's own
/// `stage` field, with a null read as the same `<unstamped>` label the tables publish.
#[test]
fn rule_3_every_stage_count_recomputes_from_the_raw_rows() {
    let mut from_records: BTreeMap<String, u64> = BTreeMap::new();
    for row in raw_call_rows() {
        let stage = row["stage"].as_str().unwrap_or("<unstamped>").to_string();
        *from_records.entry(stage).or_default() += 1;
    }
    let surface = published(SURFACE_TABLE);
    assert_eq!(
        from_records,
        published_tally(SURFACE_TABLE, "by_stage", "stage"),
        "the by_stage tally is not the raw record fold"
    );
    let mut census_by_stage: BTreeMap<String, u64> = BTreeMap::new();
    let mut census_unstamped = 0usize;
    for row in rows_of(CENSUS_TABLE) {
        let stage = row["stage"].as_str().unwrap_or("<unstamped>").to_string();
        if stage == "<unstamped>" {
            census_unstamped += 1;
        }
        *census_by_stage.entry(stage).or_default() += row["asks"].as_u64().unwrap_or_default();
    }
    assert_eq!(
        census_by_stage, from_records,
        "the census rows are not a partition of the recorded asks by stage"
    );
    // The unstamped asks are one site per group, so the count of stage-less census rows has to be
    // the count of stage-less groups the records form — not a number copied from the tables.
    let mut unstamped_groups: BTreeSet<String> = BTreeSet::new();
    for row in raw_call_rows() {
        if row["stage"].as_str().is_none() {
            unstamped_groups.insert(SiteKey::of_row(&row).label());
        }
    }
    assert_eq!(
        census_unstamped,
        unstamped_groups.len(),
        "the census publishes {census_unstamped} stage-less rows while the records group their \
         unstamped asks into {} sites",
        unstamped_groups.len()
    );
    assert!(
        !unstamped_groups.is_empty(),
        "every recorded ask now carries a stage, so the fallback above is describing a build that \
         no longer exists"
    );
    assert_eq!(
        surface["counts"]["stages"].as_u64(),
        Some(from_records.len() as u64),
        "the surface table counts a different number of stages than the records produce"
    );
}

/// §24's fourth rule: the candidate count is a property of the records. Every exact-duplicate pair
/// shape in M8.4.2's record has to have a candidate, and no candidate may name a shape the record
/// never produced — [`shape_completeness_errors`] is that rule, run here over the pairs this gate
/// read rather than at assembly.
#[test]
fn rule_4_the_candidate_count_recomputes_from_the_record_s_pair_shapes() {
    let pairs = recorded_pairs();
    let errors = shape_completeness_errors(&pairs);
    assert!(
        errors.is_empty(),
        "the candidate set does not cover the record's pair shapes: {errors:?}"
    );
    let candidate_rows = rows_of(CANDIDATE_TABLE);
    assert_eq!(
        candidate_rows.len(),
        CANDIDATES.len(),
        "the table publishes {} candidate rows while the model names {}",
        candidate_rows.len(),
        CANDIDATES.len()
    );
    // One edge, one candidate: no two rows adjudicate the same producer→consumer question, and the
    // only rows claiming no recorded pair are §17's oracle and §9's derivable height.
    let questions_not_duplicates: BTreeSet<String> = CANDIDATES
        .iter()
        .filter(|spec| spec.pattern == PATTERN_D || spec.pattern == PATTERN_F)
        .map(|spec| spec.id.to_string())
        .collect();
    let edges: BTreeSet<(String, String)> = candidate_rows
        .iter()
        .map(|row| {
            (
                row["producer"].as_str().unwrap_or_default().to_string(),
                row["consumer"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert_eq!(
        edges.len(),
        candidate_rows.len(),
        "two candidate rows adjudicate one producer→consumer edge"
    );
    let claim_nothing: BTreeSet<String> = candidate_rows
        .iter()
        .filter(|row| {
            row["duplicate_status"]["pair_ids"]
                .as_array()
                .is_none_or(|ids| ids.is_empty())
        })
        .map(|row| row["candidate"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        claim_nothing, questions_not_duplicates,
        "the candidates that claim no recorded pair are not exactly the two §17/§9 questions: \
         {:?} versus {questions_not_duplicates:?}",
        claim_nothing
    );
    assert_eq!(
        published(CANDIDATE_TABLE)["totals"]["candidates"].as_u64(),
        Some(candidate_rows.len() as u64),
        "the candidate table's own total disagrees with its rows"
    );
    // The same 18 in the matrix and the queue: one candidate, one row everywhere.
    for file in [MATRIX_TABLE, QUEUE_TABLE] {
        assert_eq!(
            rows_of(file).len(),
            candidate_rows.len(),
            "{file} publishes a different number of candidates than {CANDIDATE_TABLE}"
        );
    }
    let names: BTreeSet<String> = candidate_rows
        .iter()
        .map(|row| row["candidate"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        names.len(),
        candidate_rows.len(),
        "two candidate rows carry one candidate id, so a count could be credited twice"
    );
}

/// §24's fifth rule: the duplicate count. M8.4.2's record publishes its own aggregate next to its
/// rows, so the census's 42 is checked against the fold of the rows AND against the number that
/// milestone wrote down.
#[test]
fn rule_5_the_duplicate_count_recomputes_from_the_record_s_pairs() {
    let pairs = recorded_pairs();
    let record = read_json_at(CANDIDATES_RECORD);
    let exact: u64 = pairs
        .iter()
        .filter(|pair| pair["duplicate_type"].as_str() == Some("exact_duplicate"))
        .count() as u64;
    assert_eq!(
        exact,
        record["candidates"].as_u64().unwrap_or_default(),
        "folding the record's own rows gives {exact} exact duplicates while the same file's \
         aggregate says {}",
        record["candidates"]
    );
    let census = published(CENSUS_TABLE);
    assert_eq!(
        census["totals"]["duplicate_count"].as_u64(),
        Some(exact),
        "the census totals a different duplicate count than the record holds"
    );
    assert_eq!(
        rows_of(CENSUS_TABLE)
            .iter()
            .map(|row| row["duplicate_count"].as_u64().unwrap_or_default())
            .sum::<u64>(),
        exact,
        "the census's per-site duplicate columns do not add up to its own total"
    );
    // Every exact pair is claimed by exactly one candidate, and no candidate claims a pair the
    // record does not classify as a duplicate.
    let by_id: BTreeMap<&str, &Value> = pairs
        .iter()
        .map(|pair| (pair["candidate_id"].as_str().unwrap_or_default(), pair))
        .collect();
    let mut claimed: BTreeSet<String> = BTreeSet::new();
    for row in rows_of(CANDIDATE_TABLE) {
        for id in row["duplicate_status"]["pair_ids"]
            .as_array()
            .unwrap_or_else(|| panic!("{}: no pair_ids", row["candidate"]))
            .iter()
            .map(|value| value.as_str().unwrap_or_default())
        {
            let pair = by_id.get(id).unwrap_or_else(|| {
                panic!(
                    "{}: claims {id}, which is not a recorded pair",
                    row["candidate"]
                )
            });
            if pair["duplicate_type"].as_str() == Some("exact_duplicate") {
                let inserted = claimed.insert(id.to_string());
                assert!(
                    inserted,
                    "one exact-duplicate pair {id} is claimed twice, so a total would count it twice"
                );
            }
        }
    }
    assert_eq!(
        claimed.len() as u64,
        exact,
        "the candidates claim {} exact duplicates while the record has {exact}",
        claimed.len()
    );
}

/// §24's sixth rule: safe saving is recomputable, and the only two things that can credit it are
/// in the record. Folded here from the pairs' own `safe_to_reuse` flags and their five conditions,
/// with no candidate row consulted.
#[test]
fn rule_6_the_safe_saving_recomputes_from_the_pairs_themselves() {
    let pairs = recorded_pairs();
    let by_id: BTreeMap<&str, &Value> = pairs
        .iter()
        .map(|pair| (pair["candidate_id"].as_str().unwrap_or_default(), pair))
        .collect();
    let crediting: u64 = pairs
        .iter()
        .filter(|pair| pair_marked_safe(pair) || conditions_all_met(pair))
        .count() as u64;
    let census = published(CENSUS_TABLE);
    assert_eq!(
        census["totals"]["safe_saving"].as_u64(),
        Some(crediting),
        "the census credits {} while the record supports {crediting}",
        census["totals"]["safe_saving"]
    );
    assert_eq!(
        rows_of(CENSUS_TABLE)
            .iter()
            .map(|row| row["safe_saving"].as_u64().unwrap_or_default())
            .sum::<u64>(),
        crediting,
        "the per-site safe_saving columns do not add up to the total"
    );
    // Per candidate: the credited saving is the number of its own pairs the record credits.
    for row in rows_of(CANDIDATE_TABLE) {
        let expected: u64 = row["duplicate_status"]["pair_ids"]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .filter_map(|value| value.as_str())
                    .filter(|id| {
                        by_id
                            .get(id)
                            .is_some_and(|pair| pair_marked_safe(pair) || conditions_all_met(pair))
                    })
                    .count() as u64
            })
            .unwrap_or_default();
        assert_eq!(
            row["safe_saving"].as_u64(),
            Some(expected),
            "{}: publishes safe_saving {} while its own record pairs support {expected}",
            row["candidate"],
            row["safe_saving"]
        );
    }
    assert_eq!(
        published(CANDIDATE_TABLE)["totals"]["safe_saving"].as_u64(),
        Some(crediting),
        "the candidate table's total is not the fold of the record"
    );
    assert_eq!(
        published(SAVING_TABLE)["arithmetic"]["measured_here"]["total_safe_rpc_saving"].as_u64(),
        Some(crediting),
        "§7's fifth measure is not the same fold as §18's safe_saving column"
    );
}

/// §24's seventh rule: the two savings live in two columns and are never one number. The theoretical
/// ceiling is folded from the record's duplicate classes, the safe figure from its crediting flags,
/// and the two come out different — which is the whole point of keeping them apart.
#[test]
fn rule_7_theoretical_and_safe_saving_stay_two_separate_figures() {
    let pairs = recorded_pairs();
    let exact: u64 = pairs
        .iter()
        .filter(|pair| pair["duplicate_type"].as_str() == Some("exact_duplicate"))
        .count() as u64;
    let safe: u64 = pairs
        .iter()
        .filter(|pair| pair_marked_safe(pair) || conditions_all_met(pair))
        .count() as u64;
    let matrix_rows = rows_of(MATRIX_TABLE);
    let theoretical: u64 = matrix_rows
        .iter()
        .map(|row| row["theoretical_saving"].as_u64().unwrap_or_default())
        .sum();
    let safe_published: u64 = matrix_rows
        .iter()
        .map(|row| row["safe_saving"].as_u64().unwrap_or_default())
        .sum();
    assert_eq!(
        (theoretical, safe_published),
        (exact, safe),
        "the matrix's two saving columns do not fold to the record's two figures"
    );
    assert_ne!(
        theoretical, safe_published,
        "both saving columns total the same number, which is how a ceiling gets read as a saving"
    );
    for row in &matrix_rows {
        assert!(
            row["theoretical_saving"].as_u64().unwrap_or_default()
                >= row["safe_saving"].as_u64().unwrap_or_default(),
            "{}: credits more than its own ceiling: {:?}",
            row["candidate"],
            row
        );
        // §19 asks for both columns on every row; a row missing one would let a ceiling be reported
        // as the saving for that candidate.
        assert!(
            row.get("theoretical_saving").is_some() && row.get("safe_saving").is_some(),
            "{}: a saving column is absent, so the two cannot be read apart",
            row["candidate"]
        );
    }
    // §7's five measures stay five numbers, and the fifth is not the sum of the first four.
    let saving = published(SAVING_TABLE);
    assert_eq!(
        saving["measures"].as_array().map(Vec::len),
        Some(SAVING_KINDS.len()),
        "§7's five measures are not five rows"
    );
    assert_eq!(
        saving["arithmetic"]["is_the_fifth_the_sum_of_the_first_four"].as_bool(),
        Some(false),
        "the fifth measure has become a sum of the others"
    );
}

/// §24's eighth rule: a verification role cannot be ignored. Every row answers with a role from
/// §11's six, a `NONE` names what the read is for instead, and a read that IS the check is never
/// credited a saving — which is §11's default, checked here against the record's own pairs.
#[test]
fn rule_8_no_verification_role_is_left_unanswered() {
    let pairs = recorded_pairs();
    for file in [CENSUS_TABLE, CANDIDATE_TABLE] {
        for row in rows_of(file) {
            let role = row["verification_role"].as_str().unwrap_or_default();
            assert!(
                VERIFICATION_ROLES.contains(&role),
                "{file}: {} answers §11 with {role:?}, which is not one of the six roles",
                row_name(&row)
            );
            assert!(
                !is_check_role(role) || row["safe_saving"].as_u64().unwrap_or_default() == 0,
                "{file}: {} credits a saving on a read whose role is {role}",
                row_name(&row)
            );
            // A NONE is allowed only with the read's actual purpose named in its place — §25's NC2
            // is this row blanking both.
            if role == ROLE_NONE {
                assert!(
                    !row["role_none_reason"]
                        .as_str()
                        .unwrap_or_default()
                        .is_empty(),
                    "{file}: {} answers NONE without saying what the read is for",
                    row_name(&row)
                );
            }
            if role.is_empty() {
                panic!(
                    "{file}: {} publishes no verification role at all",
                    row_name(&row)
                );
            }
        }
    }
    // §12's refusal, recomputed: a check with NO_ALTERNATIVE blocks its own reduction.
    let mut check_role_alternatives = 0u64;
    for row in rows_of(CANDIDATE_TABLE) {
        if row["alternative_verification"].as_str() == Some(ALT_NO_ALTERNATIVE) {
            assert_eq!(
                row["safe_saving"].as_u64(),
                Some(0),
                "{}: §12's NO_ALTERNATIVE and a credited saving are both published",
                row["candidate"]
            );
            if is_check_role(row["verification_role"].as_str().unwrap_or_default()) {
                check_role_alternatives += 1;
            }
        }
    }
    // The diagnostic that says how much of the ceiling sits behind a check, folded from the pairs
    // the candidates claim rather than read back out of the table.
    let by_id: BTreeMap<&str, &Value> = pairs
        .iter()
        .map(|pair| (pair["candidate_id"].as_str().unwrap_or_default(), pair))
        .collect();
    let behind_a_check: u64 = rows_of(CANDIDATE_TABLE)
        .iter()
        .filter(|row| is_check_role(row["verification_role"].as_str().unwrap_or_default()))
        .map(|row| {
            row["duplicate_status"]["pair_ids"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(|value| value.as_str())
                        .filter(|id| {
                            by_id.get(id).is_some_and(|pair| {
                                pair["duplicate_type"].as_str() == Some("exact_duplicate")
                            })
                        })
                        .count() as u64
                })
                .unwrap_or_default()
        })
        .sum();
    let diagnostics = published(SAVING_TABLE)["diagnostics"].clone();
    assert_eq!(
        diagnostics["exact_pairs_behind_a_check_role"].as_u64(),
        Some(behind_a_check),
        "the census says {} exact pairs sit behind a check while the pairs themselves say \
         {behind_a_check}",
        diagnostics["exact_pairs_behind_a_check_role"]
    );
    assert!(
        behind_a_check > 0,
        "no exact-duplicate pair sits behind a check role, which would make §11's default a rule \
         about nothing — the ceiling would then be standing over reads nobody relied on"
    );
    assert!(
        check_role_alternatives > 0,
        "no NO_ALTERNATIVE candidate carries a check role, so §12's refusal is not being tested \
         by this table"
    );
}

/// §24's ninth rule: a rejected candidate has to name what blocks it. §20's whole purpose is that a
/// later agent does not have to ask the same question, so a row with an empty reason list is a row
/// that will be reopened.
#[test]
fn rule_9_every_rejected_candidate_names_the_blockers_that_refuse_it() {
    let rejected = rows_of(REJECTED_TABLE);
    let matrix_rows = rows_of(MATRIX_TABLE);
    let reject_count = matrix_rows
        .iter()
        .filter(|row| row["priority"].as_str() == Some(PRIORITY_REJECT))
        .count();
    assert_eq!(
        rejected.len(),
        reject_count,
        "the table rejects {} candidates while the matrix band-counts {reject_count}",
        rejected.len()
    );
    assert_eq!(
        published(REJECTED_TABLE)["counts"]["rejected"].as_u64(),
        Some(rejected.len() as u64),
        "the rejection table's own count is not its row count"
    );
    let candidates: BTreeMap<String, Value> = rows_of(CANDIDATE_TABLE)
        .iter()
        .map(|row| {
            (
                row["candidate"].as_str().unwrap_or_default().to_string(),
                row.clone(),
            )
        })
        .collect();
    for row in &rejected {
        let reasons: Vec<&str> = row["reason"]
            .as_array()
            .map(|list| {
                list.iter()
                    .map(|item| item.as_str().unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            !reasons.is_empty(),
            "{}: rejected with no reason named",
            row["candidate_id"]
        );
        assert_eq!(
            row["safe_saving"].as_u64(),
            Some(0),
            "{}: a rejected candidate still credited a saving",
            row["candidate_id"]
        );
        // Every reason has to be a blocker the candidate itself publishes — either one the model
        // declares or one the record's fields derive. A reason invented for the rejection table
        // would be a refusal nobody could re-check.
        // The link between the two tables is the candidate's short id; its long `candidate` string is
        // the producer→consumer pair spelled out of the candidate row, so a rejection that names an
        // id or a pair the candidate table does not publish cannot pass.
        let candidate = candidates
            .get(row["candidate_id"].as_str().unwrap_or_default())
            .unwrap_or_else(|| {
                panic!(
                    "{}: rejected but not in the candidate table",
                    row["candidate_id"]
                )
            });
        let pair = format!(
            "{} -> {}",
            candidate["producer"].as_str().unwrap_or_default(),
            candidate["consumer"].as_str().unwrap_or_default()
        );
        assert_eq!(
            row["candidate"].as_str(),
            Some(pair.as_str()),
            "{}: the rejection names {:?} while the candidate row's own producer and consumer spell \
             {pair:?}",
            row["candidate_id"],
            row["candidate"]
        );
        let mut declared: Vec<&str> = Vec::new();
        for key in ["blockers", "derived_blockers"] {
            if let Some(list) = candidate[key].as_array() {
                declared.extend(list.iter().filter_map(|item| item.as_str()));
            }
        }
        // The two tables use two vocabularies on purpose. A candidate's own blocker list is the
        // narrow one: `safe_saving_zero` there means "a duplicate the record did measure was not
        // safe to take", so it only appears on a row with an exact-duplicate pair behind it
        // (tables.rs's derived-blocker rule). A rejection has to say why the candidate is not a
        // candidate at all, so it also says the flat "this saves nothing", which is a number both
        // rows publish. The gate therefore accepts a reason only if the candidate declares it as a
        // blocker or its own published figure witnesses it — and refuses a rejection whose only
        // reason is the flat one.
        let witnessed = |reason: &str| {
            reason == "safe_saving_zero" && candidate["safe_saving"].as_u64() == Some(0)
        };
        for reason in &reasons {
            assert!(
                declared.contains(reason) || witnessed(reason),
                "{}: the rejection names {reason:?}, which the candidate's own row neither declares \
                 as a blocker nor publishes a figure for ({declared:?})",
                row["candidate_id"]
            );
        }
        assert!(
            reasons.iter().any(|reason| declared.contains(reason)),
            "{}: the rejection rests only on the flat fact that it saves nothing — no blocker the \
             candidate row itself declares, so §24's ninth rule is not answered",
            row["candidate_id"]
        );
        assert_eq!(
            candidate["priority"].as_str(),
            Some(PRIORITY_REJECT),
            "{}: the candidate table puts this row in band {}",
            row["candidate_id"],
            candidate["priority"]
        );
    }
    // §23's judgement: the milestone is measured on what it excluded.
    assert_eq!(
        published(REJECTED_TABLE)["counts"]["pseudo_optimizations_excluded"].as_u64(),
        Some(rejected.len() as u64),
        "the exclusion count is not the rejection count"
    );
}

/// §24's tenth rule: priority is not where the clock put the candidate. Two checks, both folded
/// from the published rows: the inversion count the matrix advertises is real, and the queue's own
/// order is reproducible from a sort key that holds no duration.
#[test]
fn rule_10_no_priority_is_the_position_of_a_duration_rank() {
    let matrix_rows = rows_of(MATRIX_TABLE);
    let inversions = priority_duration_inversions(&matrix_rows);
    assert_eq!(
        published(MATRIX_TABLE)["duration_is_not_a_rank"]["inversions_found"].as_u64(),
        Some(inversions.len() as u64),
        "the matrix advertises {} duration/priority inversions and the rows produce {}",
        published(MATRIX_TABLE)["duration_is_not_a_rank"]["inversions_found"],
        inversions.len()
    );
    assert!(
        !inversions.is_empty(),
        "no candidate is slower than a candidate the census treats better, so this rule would pass \
         on a table where priority and duration happened to agree — a clock order wearing a \
         priority label"
    );
    assert!(
        priority_is_not_a_duration_proxy(&matrix_rows),
        "reading the bands in priority order produces a duration order after all"
    );
    // The queue's order, recomputed from the sort key — and the key holds no duration.
    let queue_rows = rows_of(QUEUE_TABLE);
    let mut expected = queue_rows.clone();
    expected.sort_by_key(queue_sort_key);
    let published_order: Vec<String> = queue_rows
        .iter()
        .map(|row| row["candidate"].as_str().unwrap_or_default().to_string())
        .collect();
    let recomputed_order: Vec<String> = expected
        .iter()
        .map(|row| row["candidate"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        published_order, recomputed_order,
        "the queue is not ordered by its own published sort key"
    );
    for row in &queue_rows {
        assert_eq!(
            queue_sort_key(row).1,
            u64::MAX - row["safe_saving"].as_u64().unwrap_or_default(),
            "{}: the saving term of the sort key is not the row's safe_saving",
            row["candidate"]
        );
        assert!(
            row["sort_key"]
                .as_array()
                .map(|key| key.len() == 5)
                .unwrap_or(false),
            "{}: a sort key with a different shape than §21's five terms",
            row["candidate"]
        );
    }
    assert_eq!(
        published(QUEUE_TABLE)["sort"]["duration_used_as_sort_key"].as_bool(),
        Some(false),
        "§21's queue grew a duration term"
    );
    // §21's 禁止预先写死: the order is a property of the rows. A duration column appears in the
    // matrix for diagnosis only, and no priority in it can be recovered from it.
    let mut by_duration = matrix_rows.clone();
    by_duration.sort_by(|a, b| {
        b["total_duration_ms"]
            .as_f64()
            .unwrap_or_default()
            .partial_cmp(&a["total_duration_ms"].as_f64().unwrap_or_default())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let duration_order: Vec<&str> = by_duration
        .iter()
        .map(|row| row["priority"].as_str().unwrap_or_default())
        .collect();
    let mut priority_order: Vec<&str> = matrix_rows
        .iter()
        .map(|row| row["priority"].as_str().unwrap_or_default())
        .collect();
    priority_order.sort_unstable();
    let mut sorted_duration_order = duration_order.clone();
    sorted_duration_order.sort_unstable();
    assert_ne!(
        duration_order, priority_order,
        "ordering the candidates by their recorded duration reproduces the priority bands exactly — \
         which is what §24's tenth rule forbids, whatever the columns say about it"
    );
}

/// The term names M8.4.2's read key can hold, copied from the one place that builds them
/// (`crates/pipeline/src/canonicalization.rs:477` onward, `terms_of_recorded_key`). Anything outside
/// this list is not a field of a read — a row number or a timestamp would have to invent a term name,
/// which is exactly how §24's eleventh rule gets broken.
const BUSINESS_IDENTITY_TERMS: [&str; 11] = [
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

/// §24's eleventh rule as a predicate. A term of the key is either `name=value`, whose name must be a
/// business field, or a bare token, which only the parameterless grammar may use.
///
/// The test is on the term, not on the string: `method=eth_getTransactionCount` contains "ns" and is
/// precisely the business field the rule asks for, while a clock would enter as `duration_ns=…`.
fn identity_terms(identity: &str) -> Vec<String> {
    identity
        .split('|')
        .filter(|term| !term.is_empty())
        .map(|term| term.split('=').next().unwrap_or(term).to_string())
        .collect()
}

fn identity_is_business_keyed(identity: &str) -> bool {
    if !identity.starts_with("method=") && !identity.starts_with("paramless|") {
        return false;
    }
    identity
        .split('|')
        .filter(|term| !term.is_empty())
        .all(|term| match term.split_once('=') {
            Some((name, value)) => BUSINESS_IDENTITY_TERMS.contains(&name) && !value.is_empty(),
            None => term == "paramless",
        })
}

/// §24's eleventh rule: every semantic identity is made of business fields. The gate re-keys each
/// census row through the published `RpcReadKey` machinery from the record row it was folded from
/// and compares the strings; an identity that names a duration or a position fails on its face.
#[test]
fn rule_11_every_identity_is_business_fields_and_re_derives_from_the_record() {
    // The predicate has to have teeth: a term that is a clock or a position must not pass it, or the
    // rule below is a check on nothing.
    for forged in [
        "method=eth_call|duration_ns=17000000",
        "method=eth_call|row=6",
        "method=eth_call|index=0",
        "method=eth_call|elapsed_ms=2",
        "method=eth_call|position=3",
        "row=6",
        "method=eth_call|block=",
    ] {
        assert!(
            !identity_is_business_keyed(forged),
            "a key shaped like {forged:?} passed §24's eleventh rule, so the rule as written here \
             cannot catch a clock or a position entering the identity"
        );
    }
    assert!(
        identity_is_business_keyed(
            "method=eth_call|block=37700753|chain=91342|data=0dfe1681|to=0x01"
        ),
        "the predicate refuses a real M8.4.2 key, which would make every row below a false failure"
    );
    let raw = raw_call_rows();
    let chain_id = read_json_at(CANDIDATES_RECORD)["rows"][0]["identity"]["chain_id"]
        .as_u64()
        .unwrap_or_else(|| panic!("the pair record publishes no chain id to re-key with"));
    // One record per site — and not "whoever was read first". A site's printed identity has to be a
    // function of the keys its rows carry, so this gate takes the model's own selection rule here and
    // checks the answer it picked against the raw records below; the recompute gate re-implements the
    // rule a second time from scratch, which is what keeps the two from agreeing by construction.
    let representatives: BTreeMap<String, Value> = representative_rows(&raw)
        .into_iter()
        .map(|(key, row)| (key.label(), row))
        .collect();
    // The same asks, folded a second way: every distinct `dedup_key` a site carries. A census row
    // prints one identity (its representative ask's key), so without this column a reader could take
    // the printed key for a description of the whole row. The spelling for an unkeyed ask is typed
    // out here rather than imported, so the model cannot quietly redefine what it counts.
    const UNKEYED: &str = "<no recorded key>";
    let mut variants: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in &raw {
        let key = row["dedup_key"].as_str().unwrap_or(UNKEYED).to_string();
        variants
            .entry(SiteKey::of_row(row).label())
            .or_default()
            .insert(key);
    }
    let mut multi_key_sites = 0_usize;
    for row in rows_of(CENSUS_TABLE) {
        let label = row["key"].as_str().unwrap_or_default();
        let record = representatives
            .get(label)
            .unwrap_or_else(|| panic!("{label}: a census row no recorded ask carries"));
        let recomputed = identity_of_row(record, Some(chain_id));
        assert_eq!(
            row["identity"].as_str(),
            Some(recomputed.as_str()),
            "{label}: the census publishes {:?} while re-keying the recorded ask gives {recomputed:?}",
            row["identity"]
        );
        assert!(
            row["identity_is_a_business_key"].as_bool() == Some(true),
            "{label}: the row says its own identity is not a business key"
        );
        assert!(
            identity_is_business_keyed(&recomputed),
            "{label}: {recomputed} is not M8.4.2's key grammar — its terms are {terms:?}",
            terms = identity_terms(&recomputed)
        );
        for field in row["identity_fields"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let name = field.as_str().unwrap_or_default();
            assert!(
                BUSINESS_IDENTITY_TERMS.contains(&name),
                "{label}: the identity declares {name:?}, which is not a term M8.4.2's key builds"
            );
        }
        let carried = variants
            .get(label)
            .unwrap_or_else(|| panic!("{label}: a census row no recorded ask carries"));
        assert_eq!(
            row["recorded_key_variants"].as_u64(),
            Some(carried.len() as u64),
            "{label}: the row says its asks hold {} recorded key shapes while the records hold {}",
            row["recorded_key_variants"],
            carried.len()
        );
        assert!(
            carried.len() as u64 <= row["asks"].as_u64().unwrap_or_default(),
            "{label}: more key shapes than asks, which the fold cannot produce"
        );
        if carried.len() > 1 {
            multi_key_sites += 1;
        }
    }
    assert!(
        multi_key_sites > 0,
        "no site holds more than one recorded key, so the column above checks a shape the \
         corpus never produces — and the published total would be a free pass"
    );
    assert_eq!(
        published(CENSUS_TABLE)["totals"]["sites_with_more_than_one_recorded_key"].as_u64(),
        Some(multi_key_sites as u64),
        "the census total for multi-key sites is not the rows above counted"
    );
    // The candidates carry the same identity as the site they ask about, and §8's grammar again.
    for row in rows_of(CANDIDATE_TABLE) {
        let identity = row["identity"].as_str().unwrap_or_default();
        assert!(
            identity.is_empty() || identity_is_business_keyed(identity),
            "{}: identity {identity:?} is not M8.4.2's grammar",
            row["candidate"]
        );
        assert_eq!(
            row["duplicate_status"]["identity"]["uses_duration"].as_bool(),
            Some(false),
            "{}: the record fold says its identity uses a duration",
            row["candidate"]
        );
        assert_eq!(
            row["duplicate_status"]["identity"]["uses_row_index"].as_bool(),
            Some(false),
            "{}: the record fold says its identity uses a row index",
            row["candidate"]
        );
    }
}

/// §24's twelfth rule: no judgement may be a duration-sorted row index. Two directions here — the
/// groups fold out of the records in any order, and the census's own row numbering is not the clock.
#[test]
fn rule_12_no_judgement_reads_a_row_index_or_a_duration_position() {
    let raw = raw_call_rows();
    // Reorder the records by descending duration: the fold must produce exactly the same groups.
    let mut by_duration = raw.clone();
    by_duration.sort_by(|a, b| {
        b["duration_ns"]
            .as_u64()
            .unwrap_or_default()
            .cmp(&a["duration_ns"].as_u64().unwrap_or_default())
    });
    let shape = |groups: Vec<Group>| -> Vec<(String, usize, u64)> {
        groups
            .into_iter()
            .map(|group| (group.key.label(), group.asks, group.duration_ns))
            .collect()
    };
    let forward = shape(group_rows(&raw));
    let shuffled = shape(group_rows(&by_duration));
    assert_eq!(
        forward, shuffled,
        "folding the same records in a clock's order produces different census groups"
    );
    assert!(
        forward.len() == rows_of(CENSUS_TABLE).len(),
        "the records fold into {} groups while the census publishes {} rows",
        forward.len(),
        rows_of(CENSUS_TABLE).len()
    );

    // And the published rows: an index column exists for reading the table, and nothing reads it.
    let census_rows = rows_of(CENSUS_TABLE);
    let mut clock_order: Vec<&Value> = census_rows.iter().collect();
    clock_order.sort_by(|a, b| {
        b["rpc_sum_duration_ms"]
            .as_f64()
            .unwrap_or_default()
            .partial_cmp(&a["rpc_sum_duration_ms"].as_f64().unwrap_or_default())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut positions_moved = 0usize;
    let mut published_positions: Vec<u64> = Vec::new();
    for (place, row) in clock_order.iter().enumerate() {
        if row["row_index_in_this_table"].as_u64() != Some(place as u64) {
            positions_moved += 1;
        }
        let position = row["row_index_in_this_table"].to_string();
        published_positions.push(row["row_index_in_this_table"].as_u64().unwrap_or_default());
        assert_eq!(
            row["duration_rank_as_identity"].as_bool(),
            Some(false),
            "{}: the row says its own identity is a duration rank",
            row["site_id"]
        );
        // A table position cannot enter a judgement, because a judgement is a word from a published
        // vocabulary and the position is a number. Checking the shape, not the digit: a block number
        // or a calldata legitimately contains digits, so "does the identity contain the numeral 6" is
        // not a rule — "is any judgement column a number" is.
        for field in [
            "identity",
            "priority",
            "verification_role",
            "semantic_class",
            "alternative_verification",
            "freshness",
            "invalidation",
            "owner",
            "authority",
            "carrier",
            "block_semantics",
        ] {
            assert!(
                row[field].is_string(),
                "{}: the judgement column {field} is not a word ({:?}), so a row position could be \
                 hiding in it",
                row["site_id"],
                row[field]
            );
            assert_ne!(
                row[field].as_str(),
                Some(position.as_str()),
                "{}: {field} is the row's own table position",
                row["site_id"]
            );
        }
        for term in identity_terms(row["identity"].as_str().unwrap_or_default()) {
            assert_ne!(
                term, position,
                "{}: the identity carries a bare term that is its table position",
                row["site_id"]
            );
        }
    }
    assert!(
        positions_moved > 0,
        "the table's row order already is the duration order, which makes this rule vacuous here — \
         the census would be silently sorted by the clock"
    );
    published_positions.sort_unstable();
    assert_eq!(
        published_positions,
        (0..census_rows.len() as u64).collect::<Vec<u64>>(),
        "the position column is not a permutation of the table's rows, so it is doing more than \
         letting a reader count down the page"
    );
    // Duplicate counts must not depend on which of two identical asks was seen first.
    let pairs = recorded_pairs();
    let mut reversed = pairs.clone();
    reversed.reverse();
    assert_eq!(
        measured_shapes(&pairs),
        measured_shapes(&reversed),
        "the pair shapes depend on the record order"
    );
}

/// What one census group's time columns should say, folded from the raw rows here rather than read
/// from the table: `(sum_ns, union_ns, overlap_rows, rows_without_interval, physical_attempts)`.
/// The merge is deliberately re-implemented in these twenty lines — §24's thirteenth rule asks
/// whether the time columns agree with the records, and calling the assembly to answer that would
/// be asking the number whether it is the number.
fn fold_time(rows: &[Value], label: &str) -> (u64, u64, usize, usize, u64) {
    let mut sum_ns = 0u64;
    let mut without_interval = 0usize;
    let mut physical = 0u64;
    let mut per_run: BTreeMap<String, Vec<(u64, u64)>> = BTreeMap::new();
    for row in rows {
        if SiteKey::of_row(row).label() != label {
            continue;
        }
        sum_ns += row["duration_ns"].as_u64().unwrap_or_default();
        physical += row["attempts"].as_array().map_or(1, |list| list.len()) as u64;
        match (row["started_ns"].as_u64(), row["finished_ns"].as_u64()) {
            (Some(started), Some(finished)) if finished >= started => {
                per_run
                    .entry(row["run"].as_str().unwrap_or_default().to_string())
                    .or_default()
                    .push((started, finished));
            }
            _ => without_interval += 1,
        }
    }
    let mut union_ns = 0u64;
    let mut overlapping = 0usize;
    for (_run, mut list) in per_run {
        list.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for (started, finished) in list {
            match merged.last_mut() {
                Some(last) if started <= last.1 => {
                    overlapping += 1;
                    last.1 = last.1.max(finished);
                }
                _ => merged.push((started, finished)),
            }
        }
        union_ns += merged
            .iter()
            .map(|(started, finished)| finished - started)
            .sum::<u64>();
    }
    (sum_ns, union_ns, overlapping, without_interval, physical)
}

/// §24's thirteenth rule: the time columns validate against the records and against nothing else.
/// Every duration the census publishes is re-folded here from the raw attempt stamps, and no
/// judgement column is allowed to move when they do — that half is §25's NC6.
#[test]
fn rule_13_the_time_columns_agree_with_the_records_and_only_with_them() {
    let raw = raw_call_rows();
    let mut total_sum_ns = 0u64;
    let mut total_union_ns = 0u64;
    let mut overlap_rows = 0usize;
    let mut without_interval = 0usize;
    let mut physical = 0u64;
    for row in rows_of(CENSUS_TABLE) {
        let label = row["key"].as_str().unwrap_or_default();
        let (sum_ns, union_ns, overlapping, without, attempts) = fold_time(&raw, label);
        total_sum_ns += sum_ns;
        total_union_ns += union_ns;
        overlap_rows += overlapping;
        without_interval += without;
        physical += attempts;
        let timing = &row["duration"];
        assert_eq!(
            ms(sum_ns),
            timing["rpc_sum_duration_ms"].as_f64().unwrap_or_default(),
            "{label}: publishes {} against a re-folded {} ms",
            timing["rpc_sum_duration_ms"],
            ms(sum_ns)
        );
        assert_eq!(
            ms(union_ns),
            timing["union_duration_ms"].as_f64().unwrap_or_default(),
            "{label}: publishes a union of {} against a re-merged {} ms",
            timing["union_duration_ms"],
            ms(union_ns)
        );
        assert_eq!(
            timing["overlap_rows"].as_u64(),
            Some(overlapping as u64),
            "{label}: the overlap count is not what the merge produces"
        );
        assert_eq!(
            timing["rows_without_interval"].as_u64(),
            Some(without as u64),
            "{label}: asks with no stamps are neither counted nor ignored"
        );
        assert_eq!(
            timing["physical_attempts"].as_u64(),
            Some(attempts),
            "{label}: the physical count is not the length of the records' attempt lists"
        );
        assert!(
            union_ns <= sum_ns,
            "{label}: a union longer than the sum of its own intervals"
        );
        assert_eq!(
            union_ns == sum_ns,
            overlapping == 0,
            "{label}: union and sum agree exactly when nothing overlaps, and this row breaks that"
        );
    }
    let census_totals = published(CENSUS_TABLE);
    let totals = &census_totals["totals"];
    assert_eq!(
        ms(total_sum_ns),
        totals["rpc_sum_duration_ms"].as_f64().unwrap_or_default(),
        "the census's total is not the fold of the records"
    );
    assert_eq!(
        ms(total_union_ns),
        totals["union_duration_ms"].as_f64().unwrap_or_default(),
        "the census's total union is not the fold of the records"
    );
    assert_eq!(
        totals["overlap_rows"].as_u64(),
        Some(overlap_rows as u64),
        "the total overlap is not the sum of the rows'"
    );
    assert_eq!(
        totals["rows_without_interval"].as_u64(),
        Some(without_interval as u64),
        "the total without intervals is not the sum of the rows'"
    );
    assert_eq!(
        totals["physical_http_attempts"].as_u64(),
        Some(physical),
        "the total physical count is not the sum of the rows'"
    );
    // At concurrency 1 the two durations coincide per run, and that coincidence is itself the
    // evidence behind §7's wall-time measure — published, not asserted here.
    let saving = published(SAVING_TABLE);
    assert_eq!(
        saving["arithmetic"]["measured_here"]["parallel_wall_time_saving"].as_u64(),
        Some(0),
        "the wall-time measure is no longer zero while the overlap columns say nothing overlaps"
    );
    assert_eq!(
        overlap_rows, 0,
        "asks now overlap: M8.3.3's concurrency default was changed, and a census that reads the \
         two duration columns as equal is describing a build that no longer exists"
    );
}

/// §24's fourteenth rule: a hand-edited table cannot make a candidate green.
#[test]
fn rule_14_a_hand_edited_table_cannot_make_a_candidate_green() {
    if refreshing() {
        return;
    }
    let committed = evidence_dir();
    if !committed.is_dir() {
        panic!(
            "{} is missing — the byte gate above is the only writer, and it has not run",
            committed.display()
        );
    }
    let dir = scratch("m86-tamper-check");
    let names = assemble_to(&dir);
    assert!(
        names.iter().any(|name| name.as_str() == CANDIDATE_TABLE),
        "the candidate table the tamper check mutates is not one the assembly writes: {CANDIDATE_TABLE}"
    );
    // Mutate one assembled row's safe_saving the way a hand-edit would, and show that the model's
    // own invariant function refuses it. The published file is not touched.
    let mut candidates = read_json(&dir.join(CANDIDATE_TABLE));
    let rows = candidates["rows"]
        .as_array_mut()
        .expect("the candidate table publishes rows");
    assert!(!rows.is_empty(), "no candidate to tamper with");
    let before = rows[0]["safe_saving"].clone();
    rows[0]["safe_saving"] = json!(99u64);
    let errors = candidate_invariant_errors(&rows[0]);
    assert!(
        !errors.is_empty(),
        "a candidate row whose safe_saving was rewritten to 99 passed every invariant — the \
         invariant function does not actually read the number it is supposed to guard"
    );
    rows[0]["safe_saving"] = before;
    assert!(
        candidate_invariant_errors(&rows[0]).is_empty(),
        "the unmutated row does not pass its own invariants either, which makes the control above \
         meaningless: {:?}",
        candidate_invariant_errors(&rows[0])
    );

    // The other half of the rule: the label is a function of the counts, and the counts are the
    // records'. Rewriting a verdict word cannot survive the fold that produced it.
    let pairs = recorded_pairs();
    let safe: u64 = pairs
        .iter()
        .filter(|pair| pair_marked_safe(pair) || conditions_all_met(pair))
        .count() as u64;
    let verdict = published(VERDICT_TABLE);
    let expected_label = if safe > 0 {
        VERDICT_CANDIDATE_FOUND
    } else {
        VERDICT_NONE_FOUND
    };
    assert_eq!(
        verdict["label"].as_str(),
        Some(expected_label),
        "the verdict says {:?} while the record credits {safe} pairs",
        verdict["label"]
    );
    assert_eq!(
        verdict["verdict_block"]["safe_reducible_rpc"].as_u64(),
        Some(safe),
        "the verdict block's safe figure is not the record's"
    );
    assert_eq!(
        verdict["verdict_block"]["current_hot_path_rpc_count"].as_u64(),
        Some(raw_call_rows().len() as u64),
        "the verdict block's hot-path count is not the record count"
    );
    assert_eq!(
        verdict["verdict_block"]["new_rpc"].as_u64(),
        Some(0),
        "§26's own accounting says otherwise"
    );
    assert!(
        verdict["candidates_found"]
            .as_array()
            .is_some_and(|list| list.is_empty() == (safe == 0)),
        "the list of candidates that found a saving does not track the saving itself"
    );

    // The container half of the same rule: the committed bytes are produced by the model, so an
    // edit to a published table shows up as a byte difference before it can change a verdict.
    let dir = scratch("m86-tamper-check");
    let names = assemble_to(&dir);
    assert!(
        names.iter().any(|name| name.as_str() == CANDIDATE_TABLE),
        "the candidate table the tamper check mutates is not one the assembly writes: {CANDIDATE_TABLE}"
    );
    let bytes = std::fs::read(dir.join(CANDIDATE_TABLE)).unwrap_or_default();
    let committed_bytes = std::fs::read(committed.join(CANDIDATE_TABLE)).unwrap_or_default();
    assert_eq!(
        bytes, committed_bytes,
        "the candidate table's bytes are not reproducible, so the byte comparison that makes a \
         hand-edit visible is not a comparison of the same file"
    );
}

#[test]
fn the_readme_is_part_of_the_file_set() {
    assert!(
        generated_files().contains(&README.to_string()),
        "the directory's front page is not in the generated file set"
    );
}
