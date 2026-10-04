//! M8.5.1 §39's other half: the seven published tables re-derived from the run records.
//!
//! `eth_call_semantics_evidence.rs` proves the committed directory is what the model and the
//! records assemble to right now. That is reproducibility, not correctness: it would still pass
//! if the fold that reads a record row filed the ask under the wrong selector, if an aggregate
//! counted groups instead of rows, or if a distance were typed rather than subtracted. This file
//! therefore reads the same bytes on its own terms — its own field paths, its own `|`-splitting of
//! the records' `dedup_key`, its own grouping and arithmetic — and compares field by field against
//! the published tables. It imports nothing from the model and calls nothing in the assembly.
//!
//! A failure here means one of two different things, and the messages say which: the records moved
//! (then the tables are stale and must be refreshed), or the assembly's fold disagrees with a
//! second reading of the same records (then one of the two is wrong and the milestone may publish
//! neither).

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The seven tables plus the README, in the order [`ETH_CALL_FILES`] publishes them; spelled out
/// here because this file must not import the constant it is checking.
const SURFACE: &str = "call-surface.json";
const IDENTITIES: &str = "normalized-identities.json";
const OWNERSHIP: &str = "ownership-matrix.json";
const LIFECYCLE: &str = "lifecycle-contracts.json";
const DEPENDENCY: &str = "dependency-matrix.json";
const CONTROLS: &str = "negative-controls.json";
const VERDICTS: &str = "reuse-verdicts.json";
const README: &str = "README.md";

const EVIDENCE_DIR: &str = "data/evidence/m8/m8.5.1";
const RUNS_DIR: &str = "data/evidence/m8/cross-stage/runs";
const ROUTE_RUNS_DIR: &str = "data/evidence/m8/cross-stage/route-runs";
const CALLS_FILE: &str = "pipeline-calls.json";
const ROUTE_RUN_FILE: &str = "route-run.json";
const PREFLIGHT_FILE: &str = "preflight.json";
const CANDIDATES_RECORD: &str = "data/evidence/m8/cross-stage/reuse-candidates.json";
const SUMMARY_RECORD: &str = "data/evidence/m8/cross-stage/duplicate-summary.json";
const FEE_MEASUREMENT_RECORD: &str = "data/evidence/m7/candidate-fee-measurement.json";
const ACTIVITY_RECORD: &str = "data/evidence/m7/live-pool-activity.json";

/// The four non-identity fields §36 refuses to let an absent value share with a null.
const NON_IDENTITY_FIELDS: [&str; 4] = ["from", "value", "state_override", "gas"];

/// The terms a semantic identity is made of, in the spelling the records' own `dedup_key` uses.
const IDENTITY_TERMS: [&str; 4] = ["chain", "block", "to", "data"];

/// One recorded `eth_call` ask, read straight off the run record. `calldata` is the whole data
/// term of the `dedup_key`, not its first four bytes, so a selector collision cannot hide an ask.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Ask {
    run: String,
    rpc_id: u64,
    sink: String,
    stage: String,
    caller: String,
    chain: String,
    block: String,
    to: String,
    calldata: String,
    logical_request_id: String,
}

impl Ask {
    /// The with-block identity, rebuilt from the terms this file split out of the record. The
    /// assembly writes the same string through `EthCallIdentity`; whether the two agree is one of
    /// the questions §51 asks, so neither reading is allowed to borrow from the other.
    fn with_block(&self) -> String {
        format!(
            "eth_call|chain={}|block={}|to={}|data={}",
            self.chain, self.block, self.to, self.calldata
        )
    }

    fn without_block(&self) -> String {
        format!(
            "eth_call|chain={}|to={}|data={}",
            self.chain, self.to, self.calldata
        )
    }

    fn selector(&self) -> String {
        self.calldata.chars().take(8).collect()
    }

    /// `number` when the block term is a decimal height, `tag` when the node resolved a word.
    fn block_form(&self) -> String {
        if !self.block.is_empty() && self.block.chars().all(|c| c.is_ascii_digit()) {
            "number".to_string()
        } else {
            "tag".to_string()
        }
    }

    /// The group key the call surface is built on: two asks are the same shape of question only
    /// when the stage, the caller stamp, the selector and the target all say the same word.
    fn shape(&self) -> (String, String, String, String) {
        (
            self.stage.clone(),
            self.caller.clone(),
            self.selector(),
            self.to.clone(),
        )
    }
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_json(absolute: &Path) -> Value {
    let text = std::fs::read_to_string(absolute)
        .unwrap_or_else(|error| panic!("{}: {error}", absolute.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", absolute.display()))
}

fn record(relative: &str) -> Value {
    read_json(&repo().join(relative))
}

fn published(name: &str) -> Value {
    read_json(&repo().join(EVIDENCE_DIR).join(name))
}

fn published_text(name: &str) -> String {
    std::fs::read_to_string(repo().join(EVIDENCE_DIR).join(name))
        .unwrap_or_else(|error| panic!("{name}: {error}"))
}

fn subdirectories(relative: &str) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(repo().join(relative))
        .unwrap_or_else(|error| panic!("{relative}: {error}"))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

fn rows_of(table: &Value) -> Vec<Value> {
    table["rows"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("a table with no `rows` array: {}", table["file"]))
}

fn array_of(table: &Value, key: &str) -> Vec<Value> {
    table[key]
        .as_array()
        .unwrap_or_else(|| panic!("{}.{} is not an array", table["file"], key))
        .clone()
}

/// A field that must be there and must be a string.
fn text(row: &Value, path: &[&str]) -> String {
    optional(row, path).unwrap_or_else(|| panic!("{} is not a string", path.join(".")))
}

/// A field that may legitimately be absent or null.
fn optional(row: &Value, path: &[&str]) -> Option<String> {
    let mut node = row;
    for key in path {
        node = &node[key];
    }
    match node {
        Value::Null => None,
        value => value.as_str().map(str::to_string),
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

/// A block height, which the older M7 record carries as a decimal string and the M8 records carry
/// as a number. Both spellings mean the same height, so the helper accepts either and refuses a
/// third — a word like `latest` in a height column would otherwise silently become zero.
fn height(row: &Value, path: &[&str]) -> u64 {
    let mut node = row;
    for key in path {
        node = &node[key];
    }
    match node {
        Value::Number(_) => node.as_u64().unwrap_or_else(|| {
            panic!(
                "{} is a negative or fractional height: {node}",
                path.join(".")
            )
        }),
        Value::String(word) => word.parse::<u64>().unwrap_or_else(|error| {
            panic!(
                "{} is not a decimal height ({error}): {word:?}",
                path.join(".")
            )
        }),
        other => panic!("{} names no height: {other}", path.join(".")),
    }
}

/// A distance, which is signed: a consumer that ran before its producer reads as a negative
/// number of blocks, and reading that as an unsigned count would clamp it to garbage.
fn signed(row: &Value, path: &[&str]) -> i64 {
    let mut node = row;
    for key in path {
        node = &node[key];
    }
    node.as_i64()
        .unwrap_or_else(|| panic!("{} is not a signed distance: {node}", path.join(".")))
}

/// A published `{key: count}` object, read as a map so ordering cannot make a false equal.
fn count_map(value: &Value) -> BTreeMap<String, usize> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("expected a count object, found {value}"))
        .iter()
        .map(|(key, cell)| (key.clone(), cell.as_u64().unwrap_or_default() as usize))
        .collect()
}

fn string_set(value: &Value) -> BTreeSet<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("expected a list of strings, found {value}"))
        .iter()
        .map(|item| item.as_str().unwrap_or_default().to_string())
        .collect()
}

fn tally(values: &[String]) -> BTreeMap<String, usize> {
    let mut map: BTreeMap<String, usize> = BTreeMap::new();
    for value in values {
        *map.entry(value.clone()).or_insert(0) += 1;
    }
    map
}

/// One `key=value` term out of a published or recorded identity string.
fn term_of(identity: &str, term: &str) -> String {
    let needle = format!("{term}=");
    identity
        .split('|')
        .find(|part| part.starts_with(&needle))
        .and_then(|part| part.split_once('='))
        .map(|(_, value)| value.to_string())
        .unwrap_or_else(|| panic!("{identity:?} carries no {term:?} term"))
}

/// Every `eth_call` row in the three committed runs, with the identity terms taken out of the
/// record's own `dedup_key` and cross-checked against the row's separate columns. A `dedup_key`
/// that disagrees with `block_tag` or `target` is a record problem, and this is where it surfaces.
fn raw_asks() -> Vec<Ask> {
    let mut found = Vec::new();
    for run in subdirectories(RUNS_DIR) {
        let path = repo().join(RUNS_DIR).join(&run).join(CALLS_FILE);
        let record_file = read_json(&path);
        let rows = record_file["rows"]
            .as_array()
            .unwrap_or_else(|| panic!("{}: no `rows` array", path.display()));
        let chain_in_dir = run
            .split('-')
            .nth(1)
            .unwrap_or_else(|| panic!("{run}: the run directory does not name a chain"))
            .to_string();
        for row in rows {
            if row["method"].as_str() != Some("eth_call") {
                continue;
            }
            let key = row["dedup_key"]
                .as_str()
                .unwrap_or_else(|| panic!("{run}: an eth_call row with no dedup_key"));
            let parts: Vec<&str> = key.split('|').collect();
            assert_eq!(
                parts.len(),
                5,
                "{run}: the eth_call dedup_key {key:?} is not chain|block|to|data"
            );
            assert_eq!(
                parts[0], "call",
                "{run}: a row whose method is eth_call carries the key class {:?}",
                parts[0]
            );
            assert_eq!(
                parts[1], chain_in_dir,
                "{run}: the key names chain {} while the directory names {chain_in_dir}",
                parts[1]
            );
            let block_tag = optional(row, &["block_tag"]).unwrap_or_else(|| {
                panic!("{run}: the eth_call row records no block_tag to compare with its key")
            });
            assert_eq!(
                parts[2], block_tag,
                "{run}: the key's block {} is not the row's block_tag {block_tag}",
                parts[2]
            );
            let target = optional(row, &["target"])
                .unwrap_or_default()
                .to_lowercase();
            assert_eq!(
                parts[3], target,
                "{run}: the key's target {} is not the row's target {target}",
                parts[2]
            );
            assert!(
                parts[4].len() >= 8,
                "{run}: a calldata shorter than a selector cannot carry one: {:?}",
                parts[4]
            );
            found.push(Ask {
                run: run.clone(),
                rpc_id: row["rpc_id"].as_u64().unwrap_or_default(),
                sink: optional(row, &["sink"]).unwrap_or_default(),
                stage: optional(row, &["stage"]).unwrap_or_default(),
                caller: optional(row, &["caller"]).unwrap_or_default(),
                chain: parts[1].to_string(),
                block: parts[2].to_string(),
                to: parts[3].to_string(),
                calldata: parts[4].to_string(),
                logical_request_id: optional(row, &["logical_request_id"]).unwrap_or_default(),
            });
        }
    }
    found.sort();
    found
}

/// Every recorded ask of every method, counted the same way §39's denominator has to be: the runs'
/// whole traffic, so an `eth_call` figure is never mistaken for it.
fn total_recorded_asks() -> usize {
    subdirectories(RUNS_DIR)
        .iter()
        .map(|run| {
            let file = read_json(&repo().join(RUNS_DIR).join(run).join(CALLS_FILE));
            file["rows"].as_array().map(Vec::len).unwrap_or_default()
        })
        .sum()
}

/// The runs' own per-run ask counts, in the order the run directories sort.
fn per_run_ask_counts() -> Vec<(String, usize)> {
    subdirectories(RUNS_DIR)
        .into_iter()
        .map(|run| {
            let file = read_json(&repo().join(RUNS_DIR).join(&run).join(CALLS_FILE));
            let rows = file["rows"].as_array().map(Vec::len).unwrap_or_default();
            (run, rows)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// §39 — the surface and the identity layers
// ---------------------------------------------------------------------------

/// §39's first question: the call-surface rows are the run records grouped by the key the table
/// says it uses, and every count in the row is that group's own arithmetic. Rows are matched to
/// groups by the four terms of the key — never by sorted position, which M8.4.2 already learned
/// cannot be an identity.
#[test]
fn the_call_surface_is_the_run_records_grouped_by_the_same_key() {
    let asks = raw_asks();
    let surface = published(SURFACE);
    let rows = rows_of(&surface);

    let mut groups: BTreeMap<(String, String, String, String), Vec<&Ask>> = BTreeMap::new();
    for ask in &asks {
        groups.entry(ask.shape()).or_default().push(ask);
    }
    assert_eq!(
        rows.len(),
        groups.len(),
        "the table publishes {} surface rows while the records form {} groups of (stage, caller, \
         selector, to)",
        rows.len(),
        groups.len()
    );

    let published_keys: BTreeSet<(String, String, String, String)> = rows
        .iter()
        .map(|row| {
            (
                text(row, &["stage"]),
                text(row, &["recorded_caller"]),
                text(row, &["selector"]),
                text(row, &["to"]),
            )
        })
        .collect();
    for key in groups.keys() {
        assert!(
            published_keys.contains(key),
            "the records form the group {key:?} and no published row names it, so an ask is \
             missing from the surface"
        );
    }

    for row in &rows {
        let key = (
            text(row, &["stage"]),
            text(row, &["recorded_caller"]),
            text(row, &["selector"]),
            text(row, &["to"]),
        );
        let members = groups
            .get(&key)
            .unwrap_or_else(|| panic!("no group of raw asks answers the published row {key:?}"));
        assert_eq!(
            number(row, &["asks"]) as usize,
            members.len(),
            "{key:?}: the row publishes {} asks for a group the records hold {} wide",
            number(row, &["asks"]),
            members.len()
        );
        assert_eq!(
            string_set(&row["heights"]),
            members.iter().map(|ask| ask.block.clone()).collect(),
            "{key:?}: the heights the row lists are not the heights its asks were sent at"
        );
        assert_eq!(
            string_set(&row["runs"]),
            members.iter().map(|ask| ask.run.clone()).collect(),
            "{key:?}: the runs the row lists are not the runs that asked"
        );
        assert_eq!(
            count_map(&row["block_forms"]),
            tally(
                &members
                    .iter()
                    .map(|ask| ask.block_form())
                    .collect::<Vec<_>>()
            ),
            "{key:?}: the block forms are not the forms the records hold"
        );
        assert_eq!(
            number(row, &["distinct_identities_with_block"]) as usize,
            members
                .iter()
                .map(|ask| ask.with_block())
                .collect::<BTreeSet<_>>()
                .len(),
            "{key:?}: the with-block identity count is not that group's own distinct keys"
        );
        assert_eq!(
            number(row, &["distinct_identities_without_block"]) as usize,
            members
                .iter()
                .map(|ask| ask.without_block())
                .collect::<BTreeSet<_>>()
                .len(),
            "{key:?}: the block-free identity count is not that group's own distinct keys"
        );
        let selectors = members
            .iter()
            .map(|ask| ask.selector())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            selectors,
            BTreeSet::from([text(row, &["selector"])]),
            "{key:?}: the row's selector is not the first four bytes of the calldata its own \
             members carry, so the grouping key and the printed key disagree"
        );
        assert_ne!(
            text(row, &["selector_knowledge", "signature"]),
            "UNKNOWN_SELECTOR",
            "{key:?}: a recorded selector has no entry in the knowledge table, so its artifact \
             class would be a guess"
        );
    }

    let totals = &surface["totals"];
    assert_eq!(
        number(totals, &["eth_call_asks"]) as usize,
        asks.len(),
        "the published eth_call count is not the record count"
    );
    assert_eq!(
        number(totals, &["asks_of_every_method_in_the_same_records"]) as usize,
        total_recorded_asks(),
        "the denominator is not the records' own total"
    );
    assert_eq!(
        count_map(&totals["by_stage"]),
        tally(&asks.iter().map(|ask| ask.stage.clone()).collect::<Vec<_>>()),
        "by_stage is not the records' stage stamps counted"
    );
    assert_eq!(
        count_map(&totals["by_selector"]),
        tally(&asks.iter().map(Ask::selector).collect::<Vec<_>>()),
        "by_selector is not the records' selectors counted"
    );
    assert_eq!(
        count_map(&totals["by_caller"]),
        tally(
            &asks
                .iter()
                .map(|ask| ask.caller.clone())
                .collect::<Vec<_>>()
        ),
        "by_caller is not the records' caller stamps counted"
    );
    assert_eq!(
        count_map(&totals["block_forms"]),
        tally(&asks.iter().map(Ask::block_form).collect::<Vec<_>>()),
        "the block forms of the whole surface are not the records' own forms"
    );
    assert_eq!(
        number(totals, &["distinct_identities_with_block"]) as usize,
        asks.iter()
            .map(Ask::with_block)
            .collect::<BTreeSet<_>>()
            .len(),
        "the with-block identity total is not the corpus's own distinct keys"
    );
    assert_eq!(
        number(totals, &["distinct_identities_without_block"]) as usize,
        asks.iter()
            .map(Ask::without_block)
            .collect::<BTreeSet<_>>()
            .len(),
        "the block-free identity total is not the corpus's own distinct keys"
    );
    assert_eq!(
        number(totals, &["distinct_selectors"]) as usize,
        asks.iter()
            .map(Ask::selector)
            .collect::<BTreeSet<_>>()
            .len()
    );
    assert_eq!(
        number(totals, &["distinct_targets"]) as usize,
        asks.iter()
            .map(|ask| ask.to.clone())
            .collect::<BTreeSet<_>>()
            .len()
    );
    assert_eq!(
        number(totals, &["selectors_not_in_the_table"]),
        0,
        "a recorded selector fell outside the knowledge table"
    );

    // The site count is checked against the other tables that name the same sites rather than
    // against a number, so a fifth site entering one table without the others cannot pass.
    let sites_in_surface = rows
        .iter()
        .map(|row| text(row, &["site"]))
        .collect::<BTreeSet<_>>();
    let sites_in_ownership = array_of(&published(OWNERSHIP), "rows")
        .iter()
        .map(|row| text(row, &["site"]))
        .collect::<BTreeSet<_>>();
    let sites_in_lifecycle = array_of(&published(LIFECYCLE), "sites")
        .iter()
        .map(|row| text(row, &["site"]))
        .collect::<BTreeSet<_>>();
    let sites_in_verdicts = array_of(&published(VERDICTS), "sites")
        .iter()
        .map(|row| text(row, &["site"]))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        sites_in_surface, sites_in_ownership,
        "the surface and the ownership matrix do not name the same sites"
    );
    assert_eq!(
        sites_in_ownership, sites_in_lifecycle,
        "the ownership matrix and the lifecycle table do not name the same sites"
    );
    assert_eq!(
        sites_in_lifecycle, sites_in_verdicts,
        "the lifecycle table and the reuse verdicts do not name the same sites"
    );
    assert_eq!(
        number(totals, &["call_sites_observed_in_the_records"]) as usize,
        sites_in_surface.len(),
        "the observed site count is not the distinct sites the rows carry"
    );
    assert_eq!(
        number(totals, &["call_sites_declared_by_the_model"]) as usize,
        sites_in_ownership.len(),
        "the declared site count is not the number of sites the other tables are built on"
    );
}

/// §39/§51's second question: is a published semantic identity exactly the terms the record
/// already carried? Both the identity strings and the block-free groups are rebuilt here from the
/// `dedup_key`, so an identity that quietly gained or lost a term fails rather than reads well.
#[test]
fn the_identity_strings_are_the_records_own_dedup_keys() {
    let asks = raw_asks();
    let identities = published(IDENTITIES);
    let rows = rows_of(&identities);
    assert_eq!(
        rows.len(),
        asks.len(),
        "the identity table publishes {} rows for the {} asks the records hold",
        rows.len(),
        asks.len()
    );

    let mut by_call_site: BTreeMap<(String, u64, String, String), &Ask> = BTreeMap::new();
    for ask in &asks {
        let previous = by_call_site.insert(
            (ask.run.clone(), ask.rpc_id, ask.to.clone(), ask.selector()),
            ask,
        );
        assert!(
            previous.is_none(),
            "{}: two eth_call rows share the identity the tables use to name a row",
            ask.run
        );
    }

    for row in &rows {
        let key = (
            text(row, &["run"]),
            number(row, &["rpc_id"]),
            text(row, &["to"]),
            text(row, &["selector"]),
        );
        let ask = by_call_site
            .get(&key)
            .unwrap_or_else(|| panic!("no recorded ask answers the published row {key:?}"));
        assert_eq!(
            text(row, &["identity_with_block"]),
            ask.with_block(),
            "{key:?}: the published identity is not the terms the record's dedup_key carries"
        );
        assert_eq!(
            text(row, &["identity_without_block"]),
            ask.without_block(),
            "{key:?}: dropping the block term here does not match the same drop in the record"
        );
        assert_eq!(text(row, &["block_term"]), ask.block);
        assert_eq!(text(row, &["calldata"]), ask.calldata);
        assert_eq!(text(row, &["chain_id"]), ask.chain);
        assert_eq!(text(row, &["stage"]), ask.stage);
        assert_eq!(text(row, &["caller"]), ask.caller);
        assert_eq!(text(row, &["sink"]), ask.sink);
        assert_eq!(
            text(row, &["block_form"]),
            ask.block_form(),
            "{key:?}: the block form is not what the record's block term says"
        );
        // §36 from the record's side: the four non-identity fields are absent because the row holds
        // no such key at all, and the table says proven_absent rather than printing a null.
        for field in NON_IDENTITY_FIELDS {
            let cell = &row[field];
            assert_ne!(
                cell,
                &Value::Null,
                "{key:?}: {field} is a bare null, which §36 forbids"
            );
            assert_eq!(
                text(cell, &["state"]),
                "proven_absent",
                "{key:?}: {field} is published as {:?}",
                text(cell, &["state"])
            );
            assert!(
                cell["detail"].is_string(),
                "{key:?}: {field} claims an absence without naming the rule"
            );
        }
    }

    let totals = &identities["totals"];
    assert_eq!(number(totals, &["asks"]) as usize, asks.len());
    assert_eq!(
        number(totals, &["distinct_identities_with_block"]) as usize,
        asks.iter()
            .map(Ask::with_block)
            .collect::<BTreeSet<_>>()
            .len()
    );
    assert_eq!(
        number(totals, &["distinct_identities_without_block"]) as usize,
        asks.iter()
            .map(Ask::without_block)
            .collect::<BTreeSet<_>>()
            .len()
    );
    assert_eq!(
        number(totals, &["rows_with_a_tag_block_term"]) as usize,
        asks.iter().filter(|ask| ask.block_form() == "tag").count(),
        "not one tag-blocked row was published without the records holding it"
    );

    // The block-term histogram is the records' own heights counted.
    let measured_heights = tally(&asks.iter().map(|ask| ask.block.clone()).collect::<Vec<_>>());
    assert_eq!(
        count_map(&identities["block_term"]["heights"]),
        measured_heights,
        "the published height histogram is not the records' heights"
    );
    assert_eq!(
        count_map(&identities["block_term"]["forms"]),
        tally(&asks.iter().map(Ask::block_form).collect::<Vec<_>>()),
        "the published form histogram is not the records' own forms"
    );

    // The block-free groups, rebuilt and matched on the identity string itself.
    let mut groups: BTreeMap<String, Vec<&Ask>> = BTreeMap::new();
    for ask in &asks {
        groups.entry(ask.without_block()).or_default().push(ask);
    }
    let published_groups = array_of(&identities, "block_free_groups");
    assert_eq!(
        published_groups.len(),
        groups.len(),
        "the table publishes {} block-free groups while the records form {}",
        published_groups.len(),
        groups.len()
    );
    let mut spanning = 0;
    for group in published_groups {
        let identity = text(&group, &["identity_without_block"]);
        let members = groups
            .get(&identity)
            .unwrap_or_else(|| panic!("{identity}: a published group no record answers"));
        assert_eq!(
            number(&group, &["asks"]) as usize,
            members.len(),
            "{identity}: the group's ask count is not its own members"
        );
        assert_eq!(
            string_set(&group["heights"]),
            members
                .iter()
                .map(|ask| ask.block.clone())
                .collect::<BTreeSet<_>>(),
            "{identity}: the heights listed are not the heights this group was asked at"
        );
        assert_eq!(
            number(&group, &["distinct_identities_with_block"]) as usize,
            members
                .iter()
                .map(|ask| ask.with_block())
                .collect::<BTreeSet<_>>()
                .len(),
            "{identity}: one block-free group spanning more than one with-block identity is \
             exactly what cross-block reuse would silently merge"
        );
        let spans = members
            .iter()
            .map(|ask| ask.block.clone())
            .collect::<BTreeSet<_>>()
            .len()
            > 1;
        assert_eq!(
            group["spans_more_than_one_height"]
                .as_bool()
                .unwrap_or(false),
            spans,
            "{identity}: the spanning flag disagrees with the heights the records hold"
        );
        if spans {
            spanning += 1;
        }
    }
    assert_eq!(
        number(totals, &["groups_spanning_more_than_one_height"]) as usize,
        spanning,
        "the spanning total is not the groups the records actually span"
    );
}

/// §40's counterfactuals, re-derived: how many identities the same corpus forms when one term is
/// dropped. These are the numbers that say what a wrong key would have hidden, so they are
/// recomputed here from the raw terms rather than trusted from the assembly's grouping.
#[test]
fn the_identity_collapse_counterfactuals_recompute_from_the_raw_terms() {
    let asks = raw_asks();
    let controls = published(CONTROLS);
    let collapse = &controls["key_control_same_target_same_calldata_different_block"]
        ["identity_collapse_counterfactuals"];

    let distinct = |drop: Option<&str>| -> usize {
        asks.iter()
            .map(|ask| {
                (
                    if drop == Some("chain") {
                        String::new()
                    } else {
                        ask.chain.clone()
                    },
                    if drop == Some("block") {
                        String::new()
                    } else {
                        ask.block.clone()
                    },
                    if drop == Some("to") {
                        String::new()
                    } else {
                        ask.to.clone()
                    },
                    if drop == Some("data") {
                        String::new()
                    } else {
                        ask.calldata.clone()
                    },
                )
            })
            .collect::<BTreeSet<_>>()
            .len()
    };

    assert_eq!(
        number(collapse, &["asks_in_the_corpus"]) as usize,
        asks.len()
    );
    assert_eq!(
        number(collapse, &["distinct_with_every_term"]) as usize,
        distinct(None)
    );
    assert_eq!(
        number(collapse, &["distinct_if_chain_dropped"]) as usize,
        distinct(Some("chain"))
    );
    assert_eq!(
        number(collapse, &["distinct_if_block_dropped"]) as usize,
        distinct(Some("block"))
    );
    assert_eq!(
        number(collapse, &["distinct_if_to_dropped"]) as usize,
        distinct(Some("to"))
    );
    assert_eq!(
        number(collapse, &["distinct_if_calldata_dropped"]) as usize,
        distinct(Some("data"))
    );
    assert_eq!(
        number(collapse, &["chains_in_the_corpus"]) as usize,
        asks.iter()
            .map(|ask| ask.chain.clone())
            .collect::<BTreeSet<_>>()
            .len()
    );

    // The pair side, rebuilt from the candidate record rather than from the matrix rows.
    let pairs = eth_call_candidates();
    let group_pairs = |keep: &[&str]| -> usize {
        pairs
            .iter()
            .map(|row| {
                let free = text(&row.value, &["identity", "identity_without_block"]);
                keep.iter()
                    .map(|term| term_of(&free, term))
                    .collect::<Vec<_>>()
            })
            .collect::<BTreeSet<_>>()
            .len()
    };
    assert_eq!(
        number(collapse, &["pairs_in_the_corpus"]) as usize,
        pairs.len()
    );
    assert_eq!(
        number(collapse, &["pair_groups_by_to_and_calldata"]) as usize,
        group_pairs(&["to", "data"])
    );
    assert_eq!(
        number(collapse, &["pair_groups_by_calldata_alone"]) as usize,
        group_pairs(&["data"])
    );
    assert_eq!(
        number(collapse, &["pair_groups_by_to_alone"]) as usize,
        group_pairs(&["to"])
    );

    // The three collapses a wrong key would cause are all real collapses: each one merges at least
    // one pair the correct key keeps apart, and the chain term is the one this corpus cannot test —
    // published as untested rather than as passed.
    assert!(
        distinct(Some("to")) < distinct(None),
        "dropping `to` changed nothing, so the control that exists to catch it would be vacuous"
    );
    assert!(
        distinct(Some("data")) < distinct(None),
        "dropping calldata changed nothing, so the control that exists to catch it would be vacuous"
    );
    assert!(
        distinct(Some("block")) < distinct(None),
        "dropping the block changed nothing, which would erase §41's whole point"
    );
    assert_eq!(
        distinct(Some("chain")),
        distinct(None),
        "{} recorded asks on {} chain value: a chain-blind key would collapse nothing here, so \
         the published table must keep saying the corpus does not test that term",
        asks.len(),
        number(collapse, &["chains_in_the_corpus"])
    );
}

/// A small wrapper so the candidate rows can be sorted and indexed without carrying `Value`'s
/// ordering ambiguity through the file.
struct Candidate {
    value: Value,
}

impl Candidate {
    fn id(&self) -> String {
        text(&self.value, &["candidate_id"])
    }
}

/// M8.4.2's directed candidates, filtered to `eth_call` here rather than in the assembly.
fn eth_call_candidates() -> Vec<Candidate> {
    let record_file = record(CANDIDATES_RECORD);
    let mut rows: Vec<Candidate> = rows_of(&record_file)
        .into_iter()
        .filter(|row| row["identity"]["method"].as_str() == Some("eth_call"))
        .map(|value| Candidate { value })
        .collect();
    rows.sort_by_key(|row| row.id());
    rows
}

// ---------------------------------------------------------------------------
// §40 — the six controls on raw ids
// ---------------------------------------------------------------------------

/// §40: each control has to hold on the parsed records, not on the assembly's selection of them.
/// NC1 and NC2 are found here by their own predicates, NC3's count is recomputed, and NC4–NC6 are
/// checked against the record's key set — the only place an absence can actually be observed.
#[test]
fn the_six_negative_controls_hold_on_the_raw_records_not_on_the_assembly() {
    let asks = raw_asks();
    let controls = published(CONTROLS);
    let rows = array_of(&controls, "controls");
    let published_by_id: BTreeMap<String, &Value> =
        rows.iter().map(|row| (text(row, &["id"]), row)).collect();
    assert_eq!(
        rows.len(),
        6,
        "§40 asks for six controls and the table publishes {}",
        rows.len()
    );

    // The identity terms are the four the records' own key carries, spelled the way the table
    // spells them: the key says `chain` and `data`, the published vocabulary says `chain_id` and
    // `calldata`. The translation is written out here so a renamed term fails instead of matching
    // a leftover synonym.
    let published_terms = string_set(&published(IDENTITIES)["identity_terms"]);
    let measured_terms = IDENTITY_TERMS
        .iter()
        .map(|term| match *term {
            "chain" => "chain_id",
            "data" => "calldata",
            other => other,
        })
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        published_terms, measured_terms,
        "the identity terms published are not the four terms the records' dedup keys carry"
    );
    for row in &rows {
        let field = text(row, &["field"]);
        let participates = row["participates_in_identity"].as_bool().unwrap_or(false);
        assert_eq!(
            participates,
            !NON_IDENTITY_FIELDS.contains(&field.as_str()),
            "{} says it {field:?} participates={participates}, which the four non-identity \
             fields contradict",
            text(row, &["id"])
        );
    }

    // NC1 — two asks differing only in `to`, found here.
    let nc1 = published_by_id.get("NC1").expect("NC1 is published");
    let found_nc1 = asks.iter().find(|a| {
        asks.iter().any(|b| {
            a.block == b.block && a.calldata == b.calldata && a.to != b.to && a.run == b.run
        })
    });
    assert_eq!(
        nc1["run_against_the_corpus"].as_bool().unwrap_or(false),
        found_nc1.is_some(),
        "NC1's published result does not match the corpus read directly"
    );
    if found_nc1.is_some() {
        let pair = &nc1["measured"];
        assert_eq!(
            pair["pair_found"].as_bool(),
            Some(true),
            "NC1 reports no pair for a corpus that contains one"
        );
        let a = text(pair, &["ask_a", "identity"]);
        let b = text(pair, &["ask_b", "identity"]);
        assert_ne!(term_of(&a, "to"), term_of(&b, "to"));
        assert_eq!(term_of(&a, "block"), term_of(&b, "block"));
        assert_eq!(term_of(&a, "data"), term_of(&b, "data"));
        assert!(
            pair["identity_with_block_moved"].as_bool().unwrap_or(false),
            "NC1: two different pools share one identity, which is the error it exists to catch"
        );
        // The two asks the table names are in the corpus and say what the table says they say.
        for side in ["ask_a", "ask_b"] {
            let run = text(pair, &[side, "run"]);
            let rpc_id = number(pair, &[side, "rpc_id"]);
            let identity = text(pair, &[side, "identity"]);
            let matching = asks
                .iter()
                .find(|ask| ask.run == run && ask.rpc_id == rpc_id)
                .unwrap_or_else(|| panic!("NC1 names {run} rpc{rpc_id}, which no record holds"));
            assert_eq!(
                matching.with_block(),
                identity,
                "NC1 names {run} rpc{rpc_id} with an identity the record does not carry"
            );
        }
    }

    // NC2 — two asks differing only in calldata.
    let nc2 = published_by_id.get("NC2").expect("NC2 is published");
    let found_nc2 = asks.iter().find(|a| {
        asks.iter().any(|b| {
            a.block == b.block && a.to == b.to && a.calldata != b.calldata && a.run == b.run
        })
    });
    assert_eq!(
        nc2["run_against_the_corpus"].as_bool().unwrap_or(false),
        found_nc2.is_some(),
        "NC2's published result does not match the corpus read directly"
    );
    if found_nc2.is_some() {
        let pair = &nc2["measured"];
        let a = text(pair, &["ask_a", "identity"]);
        let b = text(pair, &["ask_b", "identity"]);
        assert_eq!(term_of(&a, "to"), term_of(&b, "to"));
        assert_ne!(term_of(&a, "data"), term_of(&b, "data"));
        assert!(
            pair["identity_with_block_moved"].as_bool().unwrap_or(false),
            "NC2: token0() and getReserves() share an identity"
        );
    }

    // NC3 — the same target and calldata at more than one height.
    let nc3 = published_by_id.get("NC3").expect("NC3 is published");
    let nc3_measured: Vec<&Ask> = asks
        .iter()
        .filter(|a| {
            asks.iter().any(|b| {
                a.block != b.block && a.to == b.to && a.calldata == b.calldata && a.chain == b.chain
            })
        })
        .collect();
    assert_eq!(
        number(&nc3["measured"], &["asks_with_a_sibling_at_another_height"]) as usize,
        nc3_measured.len(),
        "NC3's count is not the corpus's own cross-height asks"
    );
    assert!(
        nc3["run_against_the_corpus"].as_bool().unwrap_or(false),
        "NC3 says the corpus holds no cross-height pair, which would leave §41 unmeasured"
    );
    if let Some(ask) = nc3_measured.first() {
        let sibling = asks
            .iter()
            .find(|b| b.block != ask.block && b.to == ask.to && b.calldata == ask.calldata)
            .expect("the filter above promised a sibling");
        assert_eq!(
            ask.without_block(),
            sibling.without_block(),
            "the pair that §41 is about is not the same target and calldata after all"
        );
        assert_ne!(
            ask.with_block(),
            sibling.with_block(),
            "the pair that §41 is about keeps the same identity across a height change"
        );
    }
    assert_eq!(
        number(&nc3["measured"], &["directed_pairs_in_the_record"]) as usize,
        eth_call_candidates().len(),
        "NC3's pair count is not the candidate record's own eth_call rows"
    );

    // NC4–NC6: the record itself carries no such key, so proven_absent is a property of the
    // request shape rather than a sentence about it.
    let mut missing_key_evidence = 0;
    for run in subdirectories(RUNS_DIR) {
        let file = read_json(&repo().join(RUNS_DIR).join(&run).join(CALLS_FILE));
        for row in file["rows"].as_array().expect("rows") {
            if row["method"].as_str() != Some("eth_call") {
                continue;
            }
            for field in NON_IDENTITY_FIELDS {
                assert!(
                    row.get(field).is_none(),
                    "{run}: the record does hold a {field:?} key, so proven_absent is false"
                );
            }
            missing_key_evidence += 1;
        }
    }
    assert_eq!(
        missing_key_evidence,
        asks.len(),
        "the scan of record keys covered {} rows while the corpus holds {}",
        missing_key_evidence,
        asks.len()
    );
    for id in ["NC4", "NC5", "NC6"] {
        let control = published_by_id.get(id).unwrap_or_else(|| {
            panic!("§40 asks for {id} and the table does not publish it");
        });
        assert!(
            control["run_against_the_corpus"].as_bool().unwrap_or(false),
            "{id} did not run against the corpus"
        );
        assert_eq!(
            text(control, &["measured", "state"]),
            "proven_absent",
            "{id}: §36 allows proven_absent / recorded / not_applicable / not_recorded / unknown, \
             and anything but proven_absent here would mean the control did not prove the field \
             absent"
        );
        assert!(
            control["measured"]["settled"].as_bool().unwrap_or(false),
            "{id}: an unsettled absence is an open question, not a control that passed"
        );
        assert_eq!(
            number(control, &["measured", "asks"]) as usize,
            asks.len(),
            "{id}: the absence was checked over {} asks while the corpus holds {}",
            number(control, &["measured", "asks"]),
            asks.len()
        );
    }
}

// ---------------------------------------------------------------------------
// §41/§42 — the block term measured from two recorded readings
// ---------------------------------------------------------------------------

/// One pin reading and one head reading per route run, subtracted and compared here. Every
/// equality in the dependency matrix has to be exactly what this says, because this is the only
/// comparison §42 permits: two recorded answers of the same kind, field by field.
struct WithinRun {
    run: String,
    pool: String,
    leg: String,
    pinned_height: u64,
    head_height: u64,
    pinned_reserve0: String,
    pinned_reserve1: String,
    head_reserve0: String,
    head_reserve1: String,
}

impl WithinRun {
    fn blocks_apart(&self) -> u64 {
        self.head_height - self.pinned_height
    }

    fn answer_equal(&self) -> bool {
        self.pinned_reserve0 == self.head_reserve0 && self.pinned_reserve1 == self.head_reserve1
    }
}

fn within_run_readings() -> Vec<WithinRun> {
    let mut found = Vec::new();
    for run in subdirectories(ROUTE_RUNS_DIR) {
        let dir = repo().join(ROUTE_RUNS_DIR).join(&run);
        let route = read_json(&dir.join(ROUTE_RUN_FILE));
        let preflight = read_json(&dir.join(PREFLIGHT_FILE));
        let pinned_height = route["pinned_block"]
            .as_u64()
            .unwrap_or_else(|| panic!("{run}: the route records no pinned height"));
        let head_height = preflight["head"]["block_number"]
            .as_u64()
            .unwrap_or_else(|| panic!("{run}: the gate records no head height"));
        let mut head_pools: BTreeMap<String, &Value> = BTreeMap::new();
        for pool in preflight["pools"]
            .as_array()
            .expect("the gate records its pools")
        {
            head_pools.insert(
                pool["pool"].as_str().unwrap_or_default().to_lowercase(),
                pool,
            );
        }
        for side in ["buy", "sell"] {
            let leg = &route["route"][side];
            let pool = leg["pool"].as_str().unwrap_or_default().to_lowercase();
            let head = *head_pools
                .get(&pool)
                .unwrap_or_else(|| panic!("{run}: the gate recorded no head reading for {pool}"));
            found.push(WithinRun {
                run: run.clone(),
                pool,
                leg: side.to_string(),
                pinned_height,
                head_height,
                pinned_reserve0: leg["reserve0"].as_str().unwrap_or_default().to_string(),
                pinned_reserve1: leg["reserve1"].as_str().unwrap_or_default().to_string(),
                head_reserve0: head["reserve0"].as_str().unwrap_or_default().to_string(),
                head_reserve1: head["reserve1"].as_str().unwrap_or_default().to_string(),
            });
        }
    }
    found.sort_by(|a, b| (&a.run, &a.pool, &a.leg).cmp(&(&b.run, &b.pool, &b.leg)));
    found
}

/// §41 and §42: the within-run equalities and the cross-height difference are both recomputed, and
/// the asymmetry between them is what the tables are built on.
#[test]
fn the_block_term_is_measured_load_bearing_in_the_records_themselves() {
    let readings = within_run_readings();
    let dependency = published(DEPENDENCY);
    let published_answers = array_of(&dependency, "decoded_answers_within_a_run");
    assert_eq!(
        published_answers.len(),
        readings.len(),
        "the table publishes {} within-run comparisons while the route runs hold {}",
        published_answers.len(),
        readings.len()
    );

    for row in &published_answers {
        let key = (
            text(row, &["run"]),
            text(row, &["pool"]).to_lowercase(),
            text(row, &["leg"]),
        );
        let reading = readings
            .iter()
            .find(|r| {
                (r.run.as_str(), r.pool.as_str(), r.leg.as_str())
                    == (key.0.as_str(), key.1.as_str(), key.2.as_str())
            })
            .unwrap_or_else(|| panic!("no route-run pair answers the published row {key:?}"));
        assert_eq!(
            number(row, &["producer", "height"]),
            reading.pinned_height,
            "{key:?}: the producer height is not the route's pinned block"
        );
        assert_eq!(
            number(row, &["consumer", "height"]),
            reading.head_height,
            "{key:?}: the consumer height is not the gate's head block"
        );
        assert_eq!(
            number(row, &["blocks_apart"]),
            reading.blocks_apart(),
            "{key:?}: the published distance is not the subtraction of the two recorded heights"
        );
        assert_eq!(
            text(row, &["producer", "reserve0"]),
            reading.pinned_reserve0,
            "{key:?}: the pin's reserve0 is not the route's own field"
        );
        assert_eq!(
            text(row, &["consumer", "reserve1"]),
            reading.head_reserve1,
            "{key:?}: the head's reserve1 is not the gate's own field"
        );
        assert_eq!(
            row["answer_equal"]
                .as_bool()
                .unwrap_or(!reading.answer_equal()),
            reading.answer_equal(),
            "{key:?}: the equality claim is not the two recorded reserve pairs compared"
        );
    }

    // The within-run side: a short gap and an equal answer for every reading, which is exactly why
    // it cannot be read as independence.
    let equal_within = readings.iter().filter(|r| r.answer_equal()).count();
    assert_eq!(
        number(
            &dependency["totals"],
            &["pairs_with_equal_answers_within_the_run"]
        ) as usize,
        equal_within
    );
    assert_eq!(
        number(&dependency["totals"], &["pairs_compared_within_the_run"]) as usize,
        readings.len()
    );
    let max_short = readings
        .iter()
        .map(WithinRun::blocks_apart)
        .max()
        .unwrap_or_default();
    assert!(
        max_short < 100,
        "the within-run readings sit up to {max_short} blocks apart, so they are not the short-gap \
         case the table claims them to be"
    );

    // The cross-height side, rebuilt from the M7 measurement: same pool, same getReserves(),
    // a height thousands of blocks away, and an answer that moved.
    let fee = record(FEE_MEASUREMENT_RECORD);
    let early_height = height(&fee["_provenance"], &["read_at_head"]);
    let published_control = array_of(&dependency, "cross_height_control");
    assert!(
        !published_control.is_empty(),
        "§41's control is published empty, which would leave the block term unmeasured"
    );
    for row in &published_control {
        let pool = text(row, &["pool"]).to_lowercase();

        // The earlier side is the fee measurement's own reading of this pool, at the head that
        // record names. `address` is checksum-cased there, so the match is made on lowercases.
        assert_eq!(
            text(row, &["earlier", "record"]),
            FEE_MEASUREMENT_RECORD,
            "{pool}: the row names {}/earlier.record, which is not the record this file re-reads",
            DEPENDENCY
        );
        let mut from_the_record: Option<&Value> = None;
        for candidate in fee["candidates"].as_array().expect("candidates") {
            for side in ["buying_pool", "selling_pool"] {
                let reading = &candidate[side];
                if reading["address"]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase()
                    != pool
                {
                    continue;
                }
                if let Some(seen) = from_the_record {
                    for field in ["reserve0", "reserve1"] {
                        assert_eq!(
                            seen[field].as_str().unwrap_or_default(),
                            reading[field].as_str().unwrap_or_default(),
                            "{pool}: the fee measurement holds two different {field} values for \
                             the same pool, so the control would depend on which candidate is read"
                        );
                    }
                }
                from_the_record = Some(reading);
            }
        }
        let earlier_reading =
            from_the_record.expect("the control's pool is read in the fee record");
        assert_eq!(
            text(row, &["earlier", "reserve0"]),
            earlier_reading["reserve0"].as_str().unwrap_or_default(),
            "{pool}: the early reserve0 published is not the fee measurement's own field"
        );
        assert_eq!(
            text(row, &["earlier", "reserve1"]),
            earlier_reading["reserve1"].as_str().unwrap_or_default(),
            "{pool}: the early reserve1 published is not the fee measurement's own field"
        );
        assert_eq!(
            text(row, &["earlier", "block_timestamp_last"]),
            earlier_reading["getReserves_blockTimestampLast"]
                .as_str()
                .unwrap_or_default(),
            "{pool}: the early reserve timestamp is not the fee measurement's own field"
        );
        assert_eq!(
            height(row, &["earlier", "height"]),
            early_height,
            "{pool}: the early height is not the head the fee measurement was taken at"
        );
        assert_eq!(
            text(row, &["earlier", "block_hash"]),
            text(&fee["_provenance"], &["block_hash"]),
            "{pool}: the early block hash is not the head hash the fee measurement records"
        );

        // The later side is whichever record the row itself names, so a stale or swapped pointer
        // fails here rather than printing a number that happens to still exist somewhere.
        let later_record = record(&text(row, &["later", "record"]));
        let pinned_height = height(&later_record, &["pinned_block"]);
        let mut legs: Vec<&Value> = Vec::new();
        for side in ["buy", "sell"] {
            let leg = &later_record["route"][side];
            if leg["pool"].as_str().unwrap_or_default().to_lowercase() == pool {
                legs.push(leg);
            }
        }
        assert!(
            !legs.is_empty(),
            "{pool}: the row's later.record carries no route leg for this pool"
        );
        let leg = legs[0];
        for other in &legs {
            for field in ["reserve0", "reserve1"] {
                assert_eq!(
                    leg[field].as_str().unwrap_or_default(),
                    other[field].as_str().unwrap_or_default(),
                    "{pool}: the named record reads this pool with two different {field} values \
                     across its legs, so the control would depend on which leg is read"
                );
            }
        }
        assert_eq!(
            text(row, &["later", "reserve0"]),
            leg["reserve0"].as_str().unwrap_or_default(),
            "{pool}: the late reserve0 published is not the named record's own field"
        );
        assert_eq!(
            text(row, &["later", "reserve1"]),
            leg["reserve1"].as_str().unwrap_or_default(),
            "{pool}: the late reserve1 published is not the named record's own field"
        );
        assert_eq!(
            text(row, &["later", "block_timestamp_last"]),
            leg["getReserves_blockTimestampLast"]
                .as_str()
                .unwrap_or_default(),
            "{pool}: the late reserve timestamp is not the named record's own field"
        );
        let later_height = height(row, &["later", "height"]);
        assert_eq!(
            later_height, pinned_height,
            "{pool}: the late height is not the pinned block of the record the row names"
        );
        assert_eq!(
            later_height,
            height(leg, &["read_at_block"]),
            "{pool}: the late height is not the block the named record read this pool at"
        );
        assert_eq!(
            number(row, &["blocks_apart"]),
            later_height - early_height,
            "{pool}: the distance is not the subtraction of the two recorded heights"
        );
        assert!(
            later_height - early_height > 1_000,
            "{pool}: the control sits {} blocks apart, which is not far enough to expect a move",
            later_height - early_height
        );
        // §41's decisive half, recomputed from the four reserve fields rather than read off the
        // published flag: at this distance the answer moved.
        let moved = text(row, &["earlier", "reserve0"]) != text(row, &["later", "reserve0"])
            || text(row, &["earlier", "reserve1"]) != text(row, &["later", "reserve1"]);
        assert!(
            moved,
            "{pool}: the two records answer the same reserves, so nothing here measures a block \
             dependency"
        );
        assert_eq!(
            row["answer_equal"].as_bool(),
            Some(!moved),
            "{pool}: the published equality flag contradicts the reserves the same row prints"
        );
    }
    let published_differing = published_control
        .iter()
        .filter(|row| row["answer_equal"] == json_false())
        .count();
    assert_eq!(
        number(
            &dependency["totals"],
            &["cross_height_controls_answering_differently"]
        ) as usize,
        published_differing
    );

    // The cold-pool caveat has to be backed by the census it cites.
    let census = record(ACTIVITY_RECORD);
    let activity = census["activity"]
        .as_object()
        .expect("the census records its pools");
    let published_census = &dependency["activity_census"];
    assert_eq!(
        number(published_census, &["window_blocks"]),
        number(&census, &["window_blocks"]),
        "the census window published is not the census's own window"
    );
    for row in array_of(published_census, "candidate_pools") {
        let pool = text(&row, &["pool"]).to_lowercase();
        assert_eq!(
            row["in_census"].as_bool().unwrap_or(true),
            activity.contains_key(&pool),
            "{pool}: the census membership published is not the census's own key set"
        );
    }

    // A control only measures a block dependency if the question it prints is a question the
    // corpus actually sent — otherwise the two readings are of a call this build never makes.
    let asks = raw_asks();
    for row in &published_control {
        let pool = text(row, &["pool"]).to_lowercase();
        let calldata = text(row, &["calldata"]);
        assert!(
            asks.iter()
                .any(|ask| ask.to == pool && ask.calldata == calldata),
            "the control reads {pool} with calldata {calldata}, but no run record sent that ask \
             to that pool"
        );
    }

    let bounds = text(published_census, &["what_it_bounds"]);
    let min_short = readings
        .iter()
        .map(WithinRun::blocks_apart)
        .min()
        .unwrap_or_default();
    assert!(
        bounds.contains(&format!("{min_short}\u{2013}{max_short}")),
        "the caveat says {bounds:?}, which does not name the measured short gap {min_short}–{max_short}"
    );
    let long_gap = number(&published_control[0], &["blocks_apart"]);
    assert!(
        bounds.contains(&long_gap.to_string()),
        "the caveat says {bounds:?}, which does not name the measured far gap {long_gap}"
    );
}

fn json_false() -> Value {
    Value::Bool(false)
}

// ---------------------------------------------------------------------------
// §39 — the pairs are the candidate record's own rows
// ---------------------------------------------------------------------------

/// The 18 directed pairs are M8.4.2's rows, not a new measurement: every field the two published
/// copies carry has to be the record's own, including the distance, which is a subtraction of the
/// two block terms the record holds.
#[test]
fn the_measured_pairs_are_the_candidate_record_s_own_eth_call_rows() {
    let candidates = eth_call_candidates();
    let dependency = published(DEPENDENCY);
    let verdicts = published(VERDICTS);
    let matrix_pairs = array_of(&dependency, "pair_rows");
    let verdict_pairs = array_of(&verdicts, "pairs");
    assert_eq!(
        matrix_pairs.len(),
        candidates.len(),
        "the dependency matrix publishes {} pairs while the candidate record holds {}",
        matrix_pairs.len(),
        candidates.len()
    );
    assert_eq!(
        verdict_pairs.len(),
        candidates.len(),
        "the verdict table publishes {} pairs while the candidate record holds {}",
        verdict_pairs.len(),
        candidates.len()
    );

    let by_id: BTreeMap<String, &Value> = candidates
        .iter()
        .map(|row| (row.id(), &row.value))
        .collect();
    let published_ids: BTreeSet<String> = matrix_pairs
        .iter()
        .map(|row| text(row, &["candidate_id"]))
        .collect();
    assert_eq!(
        published_ids,
        by_id.keys().cloned().collect::<BTreeSet<_>>(),
        "the pairs published are not the pairs the record names"
    );

    for matrix_row in &matrix_pairs {
        let id = text(matrix_row, &["candidate_id"]);
        let record_row = by_id
            .get(&id)
            .unwrap_or_else(|| panic!("{id}: no candidate row answers the published pair"));
        let verdict_row = verdict_pairs
            .iter()
            .find(|row| text(row, &["candidate_id"]) == id)
            .unwrap_or_else(|| panic!("{id}: the verdict table publishes no copy of this pair"));

        assert_eq!(text(matrix_row, &["run"]), text(record_row, &["run"]));
        assert_eq!(
            text(matrix_row, &["producer", "stage"]),
            text(record_row, &["producer", "stage"])
        );
        assert_eq!(
            text(matrix_row, &["consumer", "stage"]),
            text(record_row, &["consumer", "stage"])
        );
        assert_eq!(
            text(matrix_row, &["producer", "caller"]),
            text(record_row, &["producer", "caller"])
        );
        assert_eq!(
            text(matrix_row, &["consumer", "caller"]),
            text(record_row, &["consumer", "caller"])
        );
        assert_eq!(
            number(matrix_row, &["producer", "rpc_id"]),
            number(record_row, &["producer", "rpc_id"])
        );
        assert_eq!(
            number(matrix_row, &["consumer", "rpc_id"]),
            number(record_row, &["consumer", "rpc_id"])
        );
        assert_eq!(
            text(matrix_row, &["block_relation"]),
            text(record_row, &["block_relation"])
        );
        assert_eq!(
            text(matrix_row, &["same_block_free_identity"]),
            text(record_row, &["identity", "identity_without_block"]),
            "{id}: the block-free identity published is not the record's own"
        );
        assert_eq!(
            text(matrix_row, &["record_verdict"]),
            text(record_row, &["verdict"])
        );
        assert_eq!(
            matrix_row["record_safe_to_reuse"], record_row["safe_to_reuse"],
            "{id}: the record's own safe_to_reuse was rewritten on the way into the table"
        );

        let producer_block = text(record_row, &["producer_block"]).parse::<i64>();
        let consumer_block = text(record_row, &["consumer_block"]).parse::<i64>();
        let (Ok(producer_block), Ok(consumer_block)) = (producer_block, consumer_block) else {
            panic!("{id}: the candidate record's block terms are not heights this can subtract")
        };
        assert_eq!(
            signed(matrix_row, &["blocks_apart"]),
            consumer_block - producer_block,
            "{id}: the distance is not the record's own two heights subtracted"
        );
        assert_eq!(
            signed(verdict_row, &["blocks_apart"]),
            consumer_block - producer_block,
            "{id}: the verdict table's distance and the matrix's disagree"
        );

        // The two published copies of a pair have to say the same thing about it.
        for field in [
            "this_milestone_class",
            "block_relation",
            "selector",
            "record_verdict",
            "same_block_free_identity",
        ] {
            assert_eq!(
                matrix_row[field], verdict_row[field],
                "{id}: the matrix and the verdict table publish different {field:?} for one pair"
            );
        }
        assert_eq!(
            matrix_row["blockers"], verdict_row["blockers"],
            "{id}: the two tables name different blockers for one pair"
        );

        // §42's boundary: the selector published here is the record's own data term, and the
        // equality claim can only be between two recorded answers of the same kind.
        let free = text(record_row, &["identity", "identity_without_block"]);
        assert_eq!(
            text(matrix_row, &["selector"]),
            term_of(&free, "data").chars().take(8).collect::<String>()
        );
        let comparison = &matrix_row["artifact_layer_answer_comparison"];
        if comparison.is_object() {
            let detail = &comparison["detail"];
            assert_eq!(
                detail["answer_equal"], matrix_row["artifact_layer_answer_comparison"]["equal"],
                "{id}: the artifact comparison published is not a copy of the row that holds it"
            );
            // Recompute it against the route-run records rather than trusting the fold.
            let run = text(matrix_row, &["run"]);
            let pool = term_of(&free, "to");
            if let Some(reading) = within_run_readings()
                .into_iter()
                .find(|r| r.run == run && r.pool == pool)
            {
                assert_eq!(
                    comparison["equal"].as_bool(),
                    Some(reading.answer_equal()),
                    "{id}: the artifact-layer equality is not the two recorded reserve pairs"
                );
            }
        }
    }

    // Direction and distance are uniform across the measured set, and that is a finding, not an
    // assumption: every pair goes detection → preflight and every pair crosses a height.
    let detection_to_preflight = candidates
        .iter()
        .filter(|row| {
            text(&row.value, &["producer", "stage"]) == "opportunity_detection"
                && text(&row.value, &["consumer", "stage"]) == "preflight"
        })
        .count();
    assert_eq!(
        detection_to_preflight,
        candidates.len(),
        "the candidate record's eth_call pairs are not all detection → preflight, so the surface \
         figure quoted in the README is stale"
    );
    let cross_block = candidates
        .iter()
        .filter(|row| text(&row.value, &["block_relation"]) == "different_block")
        .count();
    assert_eq!(
        cross_block,
        candidates.len(),
        "a measured pair keeps the same block, so the block term cannot be its blocker"
    );
    let refused = candidates
        .iter()
        .filter(|row| row.value["safe_to_reuse"].as_bool().unwrap_or(true))
        .count();
    assert_eq!(
        refused, 0,
        "M8.4.2's own record already calls {refused} of these safe to reuse, which would make \
         this milestone's refusal a disagreement with the record rather than a reading of it"
    );
}

// ---------------------------------------------------------------------------
// §39 — the verdict arithmetic
// ---------------------------------------------------------------------------

/// Every total in the three verdict tables is the sum of the rows it publishes, recomputed here
/// with this file's own loops. A constant typed into a `totals` object cannot survive this.
#[test]
fn every_published_total_is_the_rows_it_prints_added_up() {
    // Ownership: 8 axes × 6 sites, and the axis-status map is those cells counted.
    let ownership = published(OWNERSHIP);
    let axes = string_set(&ownership["axes"]);
    let rows = array_of(&ownership, "rows");
    let mut axis_status: BTreeMap<String, usize> = BTreeMap::new();
    let mut safe = 0;
    let mut principle = 0;
    let mut read_is_check = 0;
    for row in &rows {
        let cells = array_of(row, "axes");
        assert_eq!(
            cells.len(),
            axes.len(),
            "{} carries {} axis cells while the table declares {}",
            text(row, &["site"]),
            cells.len(),
            axes.len()
        );
        for cell in &cells {
            *axis_status
                .entry(format!(
                    "{}={}",
                    text(cell, &["axis"]),
                    text(cell, &["status"])
                ))
                .or_insert(0) += 1;
        }
        if row["safe_to_reuse_now"].as_bool().unwrap_or(false) {
            safe += 1;
        }
        if row["reusable_in_principle"].as_bool().unwrap_or(false) {
            principle += 1;
        }
        if row["read_is_the_check"].as_bool().unwrap_or(false) {
            read_is_check += 1;
        }
    }
    assert_eq!(
        count_map(&ownership["totals"]["by_axis_status"]),
        axis_status,
        "the axis-status tally is not the cells the rows carry"
    );
    assert_eq!(
        number(&ownership["totals"], &["sites"]) as usize,
        rows.len()
    );
    assert_eq!(
        number(&ownership["totals"], &["safe_to_reuse_now"]) as usize,
        safe
    );
    assert_eq!(
        number(&ownership["totals"], &["reusable_in_principle"]) as usize,
        principle
    );
    assert_eq!(
        number(&ownership["totals"], &["read_is_the_check"]) as usize,
        read_is_check
    );

    // Lifecycle: sites × steps cells, and the status map is those cells counted.
    let lifecycle = published(LIFECYCLE);
    let steps = string_set(&lifecycle["steps"]);
    let sites = array_of(&lifecycle, "sites");
    let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
    let mut cells = 0;
    for site in &sites {
        let rows = array_of(site, "steps");
        assert_eq!(
            rows.len(),
            steps.len(),
            "{} answers {} lifecycle steps while the table declares {}",
            text(site, &["site"]),
            rows.len(),
            steps.len()
        );
        for cell in &rows {
            *by_status.entry(text(cell, &["status"])).or_insert(0) += 1;
            cells += 1;
        }
    }
    assert_eq!(number(&lifecycle["totals"], &["cells"]) as usize, cells);
    assert_eq!(
        count_map(&lifecycle["totals"]["by_status"]),
        by_status,
        "the lifecycle status tally is not the cells the sites carry"
    );
    assert_eq!(cells, sites.len() * steps.len());

    // Reuse verdicts: site rows, pair rows, blocker tally and the outcome's own arithmetic.
    let verdicts = published(VERDICTS);
    let sites = array_of(&verdicts, "sites");
    let pairs = array_of(&verdicts, "pairs");
    let mut blockers: BTreeMap<String, usize> = BTreeMap::new();
    let mut reuse_ready = 0;
    for pair in &pairs {
        if text(pair, &["this_milestone_class"]) == "REUSE_READY" {
            reuse_ready += 1;
        }
        for blocker in array_of(pair, "blockers") {
            *blockers.entry(blocker_word(&blocker)).or_insert(0) += 1;
        }
    }
    assert_eq!(
        count_map(&verdicts["totals"]["blockers"]),
        blockers,
        "the blocker tally is not the blockers the pair rows name"
    );
    assert_eq!(
        number(&verdicts["totals"], &["pairs"]) as usize,
        pairs.len()
    );
    assert_eq!(
        number(&verdicts["totals"], &["pairs_reuse_ready"]) as usize,
        reuse_ready
    );
    assert_eq!(
        number(&verdicts["totals"], &["sites"]) as usize,
        sites.len()
    );
    let mut safe_sites = 0;
    let mut principle_sites = 0;
    for site in &sites {
        if site["safe_to_reuse_now"].as_bool().unwrap_or(false) {
            safe_sites += 1;
        }
        if site["reusable_in_principle"].as_bool().unwrap_or(false) {
            principle_sites += 1;
        }
        let class = text(site, &["class"]);
        let names_blockers = !array_of(site, "blockers").is_empty();
        assert!(
            class == "REUSE_READY" || names_blockers,
            "{} is classed {class:?} without naming what blocks it",
            text(site, &["site"])
        );
        // §27's two questions stay two questions: a site may be reusable in principle while no
        // reuse is safe now, and the tables must not let one answer the other.
        if site["safe_to_reuse_now"].as_bool().unwrap_or(false) {
            assert!(
                site["reusable_in_principle"].as_bool().unwrap_or(false),
                "{} is safe now without being reusable in principle",
                text(site, &["site"])
            );
        }
    }
    assert_eq!(
        number(&verdicts["totals"], &["safe_to_reuse_now"]) as usize,
        safe_sites
    );
    assert_eq!(
        number(&verdicts["totals"], &["reusable_in_principle"]) as usize,
        principle_sites
    );

    // The outcome block is read as arithmetic: what was found, what survived, what that saves.
    let outcome = &verdicts["outcome"];
    assert_eq!(
        number(outcome, &["candidates_found"]) as usize,
        pairs.len(),
        "the outcome's candidate count is not the pairs measured"
    );
    assert_eq!(
        number(outcome, &["candidates_safe"]) as usize,
        reuse_ready,
        "the outcome's safe count is not the pairs classed REUSE_READY"
    );
    assert_eq!(
        number(outcome, &["net_rpc_saving"]) as usize,
        reuse_ready,
        "§31: the net saving can only be one ask removed per reuse-ready pair, and there are \
         {reuse_ready} of those"
    );
    let found = number(outcome, &["candidates_found"]) as usize;
    let label = text(outcome, &["label"]);
    let vocabulary = string_set(&verdicts["class_vocabulary"]);
    assert!(
        vocabulary.contains(&label)
            || matches!(
                label.as_str(),
                "REUSE_BLOCKED" | "SAFE_CANDIDATE_FOUND" | "NOT_ENOUGH_EVIDENCE"
            ),
        "{label:?} is not an outcome §58 recognises"
    );
    if found > 0 && reuse_ready == 0 {
        assert_eq!(
            label, "REUSE_BLOCKED",
            "{found} candidates measured and none safe has to be labelled REUSE_BLOCKED, not {label:?}"
        );
    }
    if found == 0 {
        assert_eq!(
            label, "NOT_ENOUGH_EVIDENCE",
            "no candidate was measured, so the honest label is NOT_ENOUGH_EVIDENCE"
        );
    }

    // §66's one-sentence answer has to exist, has to refuse, and has to be quoted verbatim.
    let sentence = text(outcome, &["one_sentence_answer_to_66"]);
    assert!(
        sentence.split_whitespace().count() > 12,
        "§66's answer is a fragment: {sentence:?}"
    );
    assert!(
        sentence.contains("Preflight"),
        "§66 asks about Preflight and the answer never names it: {sentence:?}"
    );
    assert!(
        sentence.contains("could not") || sentence.contains("cannot"),
        "the outcome is {label:?} while the §66 sentence does not refuse: {sentence:?}"
    );
    let readme = published_text(README);
    assert!(
        readme.contains(&sentence),
        "the README does not quote §66's sentence verbatim"
    );

    // The README's measured figures have to be the records' figures, not remembered ones. This is
    // the gate that would have caught a distance typed as 170,153.
    let readings = within_run_readings();
    let min_short = readings
        .iter()
        .map(WithinRun::blocks_apart)
        .min()
        .unwrap_or_default();
    let max_short = readings
        .iter()
        .map(WithinRun::blocks_apart)
        .max()
        .unwrap_or_default();
    let dependency = published(DEPENDENCY);
    let far = number(&dependency["cross_height_control"][0], &["blocks_apart"]);
    for figure in [
        format!("**{}**", asks_in_the_records()),
        format!(
            "{} 块 swap/sync",
            number(&record(ACTIVITY_RECORD), &["window_blocks"])
        ),
        format!("（相差 {}–{} 块）", min_short, max_short),
        format!("**相隔 {} 块**", far),
    ] {
        assert!(
            readme.contains(&figure),
            "the README no longer carries the measured figure {figure:?}, so its numbers were \
             written rather than read"
        );
    }
}

/// A blocker cell is a string; a non-string would be a publishing bug, and this says so.
fn blocker_word(cell: &Value) -> String {
    cell.as_str()
        .unwrap_or_else(|| panic!("a blocker cell is not a word: {cell}"))
        .to_string()
}

fn asks_in_the_records() -> usize {
    raw_asks().len()
}

// ---------------------------------------------------------------------------
// §49 — the diagnosis asked for nothing
// ---------------------------------------------------------------------------

/// §31/§32/§46/§49 in one place: the corpus the tables describe is the corpus M8.4.2 already
/// committed, the runs say their instrumentation asked for nothing, and no wall clock survived
/// into a published table.
#[test]
fn the_diagnosis_asked_for_nothing_and_kept_no_clock() {
    let run_dirs = subdirectories(RUNS_DIR);
    let route_dirs = subdirectories(ROUTE_RUNS_DIR);
    assert_eq!(
        run_dirs, route_dirs,
        "the runs whose RPC traffic is read and the runs whose route records are read are not the \
         same committed runs, so a table would be describing a run the other does not have"
    );

    // M8.4.2's summary is the witness that no run was added and no ask was inserted.
    let summary = record(SUMMARY_RECORD);
    let summarised: BTreeSet<String> = array_of(&summary, "per_run")
        .iter()
        .map(|row| text(row, &["run"]))
        .collect();
    assert_eq!(
        summarised,
        run_dirs.iter().cloned().collect::<BTreeSet<_>>(),
        "the runs this diagnosis reads are not the runs M8.4.2 summarised"
    );
    let measured_per_run = per_run_ask_counts();
    let claims: BTreeMap<String, usize> = array_of(&summary, "per_run")
        .iter()
        .map(|row| (text(row, &["run"]), number(row, &["asks"]) as usize))
        .collect();
    for (run, rows) in &measured_per_run {
        assert_eq!(
            claims.get(run),
            Some(rows),
            "{run}: the record holds {rows} asks while M8.4.2's summary counts {:?}",
            claims.get(run)
        );
    }
    assert_eq!(
        number(&summary, &["total_asks"]) as usize,
        total_recorded_asks(),
        "the corpus M8.4.2 counted is not the corpus read here"
    );

    // Each run states that the trace itself issued nothing, and the numbering shows it: the
    // per-sink rpc ids run 1..n with no gap, and every row has its own logical request id.
    for run in &run_dirs {
        let file = read_json(&repo().join(RUNS_DIR).join(run).join(CALLS_FILE));
        assert!(
            file["instrumentation_issues_no_requests"]
                .as_bool()
                .unwrap_or(false),
            "{run}: the record does not state that instrumentation issued no request"
        );
        let rows = array_of(&file, "rows");
        let logical: BTreeSet<String> = rows
            .iter()
            .map(|row| text(row, &["logical_request_id"]))
            .collect();
        assert_eq!(
            logical.len(),
            rows.len(),
            "{run}: two rows share a logical request id, so the trace would be counting one ask twice"
        );
        let mut by_sink: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        for row in &rows {
            by_sink
                .entry(text(row, &["sink"]))
                .or_default()
                .push(row["rpc_id"].as_u64().unwrap_or_default());
        }
        for (sink, mut ids) in by_sink {
            ids.sort_unstable();
            let expected: Vec<u64> = (1..=ids.len() as u64).collect();
            assert_eq!(
                ids, expected,
                "{run}/{sink}: the rpc ids are not a gapless 1..n run, which is what an inserted \
                 request would look like"
            );
        }
    }

    // No wall clock in a published table: an assembly timestamp would make red-or-green depend on
    // when the gate ran, which is the failure M8.4.2's byte gate was fixed for. The test is by
    // name shape, not by an enumeration of the clocks seen so far, so a clock that arrives under a
    // new spelling is still caught.
    let mut offenders: Vec<String> = Vec::new();
    for name in ETH_CALL_TABLES {
        scan_clock_keys(&published(name), name, &mut offenders);
    }
    assert!(
        offenders.is_empty(),
        "a clock field reached a published table: {offenders:?}"
    );

    // The committed directory holds exactly the seven tables and the README, so the diagnosis wrote
    // nothing else and left nothing behind.
    let mut committed: Vec<String> = std::fs::read_dir(repo().join(EVIDENCE_DIR))
        .unwrap_or_else(|error| panic!("{EVIDENCE_DIR}: {error}"))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    committed.sort();
    let mut expected = ETH_CALL_TABLES
        .iter()
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    expected.push(README.to_string());
    expected.sort();
    assert_eq!(
        committed, expected,
        "the evidence directory holds something other than the seven tables and their README"
    );

    // The tables name only runs that exist.
    let existing = run_dirs.iter().cloned().collect::<BTreeSet<_>>();
    let mut mentioned: BTreeSet<String> = BTreeSet::new();
    collect_run_names(&published(DEPENDENCY), &existing, &mut mentioned);
    collect_run_names(&published(VERDICTS), &existing, &mut mentioned);
    collect_run_names(&published(IDENTITIES), &existing, &mut mentioned);
    assert!(
        !mentioned.is_empty(),
        "no table names a committed run, which would mean nothing was measured at all"
    );
    assert_eq!(
        mentioned, existing,
        "the runs the tables name are not the runs the repository holds: {:?} vs {existing:?}",
        mentioned
    );
}

/// The seven tables, spelled out because this file does not import the model's list.
const ETH_CALL_TABLES: [&str; 7] = [
    SURFACE, IDENTITIES, OWNERSHIP, LIFECYCLE, DEPENDENCY, CONTROLS, VERDICTS,
];

/// Name shapes that mean "this field was read off a clock when the table was assembled". Matched
/// as substrings and case-sensitively: the bytecode table's `TIMESTAMP` key is an opcode name, and
/// a case-folding match would flag it as a wall clock and force the exception to be widened.
const CLOCK_PATTERNS: [&str; 12] = [
    "clock",
    "latency",
    "duration",
    "elapsed",
    "unix",
    "timestamp",
    "generated_at",
    "assembled_at",
    "started_at",
    "finished_at",
    "_ms",
    "_ns",
];

/// Keys that match a shape above and are still not a clock. `block_timestamp_last` is the
/// reserve-stamp field a `getReserves()` returns — chain state, from a header, identical on every
/// re-read of the same block — so it belongs in a byte-reproducible table.
const CLOCK_EXCEPTIONS: [&str; 1] = ["block_timestamp_last"];

fn clock_like(key: &str) -> bool {
    !CLOCK_EXCEPTIONS.contains(&key) && CLOCK_PATTERNS.iter().any(|part| key.contains(part))
}

fn scan_clock_keys(value: &Value, where_: &str, offenders: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if clock_like(key) {
                    offenders.push(format!("{where_}: {key}"));
                }
                scan_clock_keys(child, where_, offenders);
            }
        }
        Value::Array(items) => {
            for item in items {
                scan_clock_keys(item, where_, offenders);
            }
        }
        _ => {}
    }
}

/// Every string in a table that looks like a run directory name, kept only when it matches one.
fn collect_run_names(value: &Value, existing: &BTreeSet<String>, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (_, child) in map {
                collect_run_names(child, existing, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_run_names(item, existing, out);
            }
        }
        Value::String(text) if existing.contains(text) => {
            out.insert(text.clone());
        }
        _ => {}
    }
}
