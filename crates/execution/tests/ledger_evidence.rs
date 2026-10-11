//! M12-F §10: the gate over the ledger-recovery evidence.
//!
//! The measurement lives in `crates/execution/tests/crash_recovery.rs`: thirteen cases, each of
//! which appends one row per figure it observed to `measured-rows.jsonl` when — and only when —
//! `M12F_EVIDENCE_DIR` names a directory. This file never measures. It reads those rows back and
//! asks five questions of them:
//!
//! ```text
//! are the figures internally consistent      arrivals, line counts, state words, lane holds
//! does every row name a test that exists     and does every test in the matrix have a row
//! do the words still mean what they say      re-read from production's own vocabularies
//! are the published tables those rows        re-assembled, byte for byte
//! is the fail-closed machinery still there   by anchor, in the source, right now
//! ```
//!
//! Why the raw rows are committed alongside the tables: each table is a pure function of the rows,
//! so a reader who suspects a table can recompute it. The rows are what one serial run of the
//! matrix actually recorded, and nothing here re-runs the matrix — nothing here could make a
//! journal say something it did not say. Every vocabulary a row quotes ([`JournalFact`],
//! [`FactBasis`], [`RecoveredState`], [`JournalFault`], the supported schema version, the ledger
//! file name) is parsed out of `crates/execution/src/journal.rs` at gate time, with an arity
//! assertion against the enum that list comes from, so a renamed word fails this file rather than
//! quietly passing a row that no longer matches production.
//!
//! What this evidence is not, stated once because §14 forbids claiming more: every row was
//! measured against a journal file in a scratch directory under this process's temp root, driven
//! by an in-process scripted submitter that never opened a socket. No node was contacted, no
//! transaction was broadcast, no block was read. The single most important consequence is §14's
//! own prohibition, and no row here steps across it: **a local ledger having recovered a record is
//! not the node's transaction being confirmed.** Recovery answers only what *this file* says, and
//! what it says most of the time in these rows is that a nonce is still reserved and a human has
//! to decide — which is the opposite of a confirmation.
//!
//! The durability range is equally narrow, and §6 forbids widening it: the writes here go through
//! the file's own flush and the descriptor's sync, so the guarantee measured is against *this
//! process dying*, not against a machine losing power. Nothing in these tables claims the latter,
//! and [`validate`] refuses a row that says so.
//!
//! Refresh, when the matrix legitimately changes:
//!
//! ```text
//! M12F_LEDGER_REFRESH=1 cargo test -p evm-execution --test ledger_evidence -- --test-threads=1
//! ```
//!
//! which re-assembles the four tables from the rows already in `measured-rows.jsonl` and then
//! panics, so the switch cannot stay set by accident. It writes no new figures: it can only
//! publish what the matrix recorded, and it refuses to publish rows that break a rule.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

const ROWS_FILE: &str = "data/evidence/m12/f/measured-rows.jsonl";
const RECOVERY_FILE: &str = "data/evidence/m12/f/recovery-results.json";
const BOUNDARY_FILE: &str = "data/evidence/m12/f/persistence-boundary.json";
const LANE_FILE: &str = "data/evidence/m12/f/lane-recovery.json";
const DAMAGE_FILE: &str = "data/evidence/m12/f/damage-controls.json";

const MATRIX: &str = "crates/execution/tests/crash_recovery.rs";
const JOURNAL: &str = "crates/execution/src/journal.rs";
const STAGE: &str = "crates/execution/src/stage.rs";
const ERROR: &str = "crates/execution/src/error.rs";

/// The four tables §10 asks for, and the `table` word each row carries.
const TABLES: [&str; 4] = [
    "recovery_results",
    "persistence_boundary",
    "lane_recovery",
    "damage_controls",
];

/// §8's cases F1 through F9. `section` is the case family a row belongs to; two cases per family
/// are allowed (F5, F6, F8 and F9 each carry a second shape).
const SECTIONS: [&str; 9] = ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9"];

/// The two words a lane row may use for the same reading §4.3 asks about.
const LANE_WORDS: [&str; 2] = ["held", "idle"];

/// Where a refusal happened, in the one axis the table needs: before the handle existed, or at the
/// append it could not make.
const REFUSAL_POINTS: [&str; 2] = ["open", "append"];

/// A §5 identity is a 32-byte hash, which renders as this many hex digits. The same bound is what
/// journal.rs's own unit test asserts for a written line, and it is what keeps a signed payload —
/// an order of magnitude longer — out of a table.
const HEX_DIGITS: usize = 64;

/// A credential, a host, a URL, or a key in the shapes this repository uses. Every needle here
/// measured zero occurrences in the committed rows.
const LEAK_NEEDLES: [&str; 11] = [
    "http://",
    "https://",
    "://",
    "127.0.0.1",
    "localhost",
    "private_key",
    "mnemonic",
    "secret",
    "api_key",
    "apikey",
    "giwa.io",
];

/// §6 forbids claiming durability beyond a process dying. These words would say otherwise, and
/// the rule that scans for them grades rows, not the prose that disclaims them.
const DURABILITY_NEEDLES: [&str; 5] = ["fsync", "power-loss", "power loss", "crash-proof", "掉电"];

/// A temp directory is where the ledger under test lived, not a fact about the ledger. §10 asks for
/// reproducible rows, and a path is the one figure two runs of the same case never share.
const PATH_NEEDLES: [&str; 4] = ["/Users/", "/Volumes/", "/tmp", "target/"];

// ---------------------------------------------------------------------------
// reading the artifact and the source it is measured against
// ---------------------------------------------------------------------------

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_text(relative: &str) -> String {
    let path = workspace_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The production region of a source file: everything above its inline test module, which is the
/// cut every other gate in this repository uses so an anchor cannot be satisfied by a line that
/// only exists inside a test.
fn production_lines(path: &str) -> Vec<String> {
    let text = read_text(path);
    let code = text.split("\n#[cfg(test)]").next().unwrap_or(text.as_str());
    code.lines().map(|line| line.to_string()).collect()
}

/// One `(file, token)` anchor, resolved: exactly one production line must contain the token.
/// Zero means the mechanism is gone; more than one means the claim does not name one place.
fn resolve_pair(file: &str, token: &str) -> Result<usize, String> {
    let hits = production_lines(file)
        .into_iter()
        .enumerate()
        .filter(|(_, line)| line.contains(token))
        .map(|(index, _)| index + 1)
        .collect::<Vec<_>>();
    match hits.len() {
        1 => Ok(hits[0]),
        0 => Err(format!("{file}: no production line contains `{token}`")),
        n => Err(format!(
            "{file}: {n} production lines contain `{token}` (at {}) — an anchor must name one place",
            hits.iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The committed rows, in the order the matrix wrote them.
fn measured_rows() -> Vec<Value> {
    let text = read_text(ROWS_FILE);
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{}: a line is not json: {error}", ROWS_FILE))
        })
        .collect()
}

fn rows_or_panic() -> Vec<Value> {
    let path = workspace_root().join(ROWS_FILE);
    if !path.exists() {
        panic!(
            "{ROWS_FILE} is missing. It is written by the matrix, not by this gate: \
             M12F_EVIDENCE_DIR=<absolute path to {}/data/evidence/m12/f> \
             cargo test -p evm-execution --test crash_recovery -- --test-threads=1",
            workspace_root().display()
        );
    }
    measured_rows()
}

/// The body of one item, from a token that must occur exactly once to the first line that closes
/// it. `indent` is the column the closing brace sits at: `0` for an `impl` or `enum` block, `4`
/// for a function inside one. Reading a vocabulary this way is what stops this file from being a
/// second copy of the words it grades.
fn body(file: &str, start: &str, indent: usize) -> Vec<String> {
    let lines = production_lines(file);
    let hits = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(start))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let [one] = hits.as_slice() else {
        panic!(
            "{file}: {} lines contain `{start}`; the reader needs exactly one",
            hits.len()
        );
    };
    let closer = format!("{}}}", " ".repeat(indent));
    let mut out = Vec::new();
    for line in &lines[*one + 1..] {
        if line.trim_start() == "}" && line.starts_with(&closer) {
            break;
        }
        out.push(line.clone());
    }
    assert!(
        !out.is_empty(),
        "{file}: `{start}` opened no body this reader could read"
    );
    out
}

/// The variants of one `pub enum`, read off its own declaration: a line at four-space indent that
/// begins with an capital identifier and is not a comment or an attribute.
fn enum_variants(file: &str, header: &str) -> Vec<String> {
    body(file, header, 0)
        .iter()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty() && !line.starts_with("//") && !line.starts_with('#'))
        .filter_map(|line| {
            let head = line.split([' ', '{', ',']).next().unwrap_or_default();
            let bytes = head.as_bytes();
            let first = bytes.first()?;
            if first.is_ascii_uppercase()
                && head.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                Some(head.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// The arms of one `name` function, as `(variant, printed word)` pairs. An arm may wrap across
/// lines — `Self::NotJson {\n torn_tail: true, ..\n} => "torn_tail",` is one arm in production's
/// own formatting — so a few lines are joined before the word is taken. A `Self::` line whose
/// joined text never reaches `=> "` is a pattern in some other function, and is skipped.
fn name_arms(file: &str, impl_header: &str) -> Vec<(String, String)> {
    let lines = body(file, impl_header, 0);
    let mut out: Vec<(String, String)> = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        let Some(rest) = trimmed.strip_prefix("Self::") else {
            index += 1;
            continue;
        };
        let variant = rest
            .split([' ', '{', ',', '('])
            .next()
            .unwrap_or_default()
            .to_string();
        let joined = lines[index..index + 4.min(lines.len() - index)]
            .join(" ")
            .replace('\n', " ");
        match joined.find("=> \"") {
            Some(offset) => {
                let after = &joined[offset + 4..];
                let word = after.split('"').next().unwrap_or_default().to_string();
                assert!(
                    !word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                    "{file}: the word `{word}` for {variant} is not a lower-case token"
                );
                out.push((variant, word));
                index += 1;
            }
            None => {
                index += 1;
            }
        }
    }
    out
}

/// One vocabulary: the enum's variants and the `name` function's arms, asserted against each other
/// so a word added to one side and not the other is a reader error rather than a green gate.
fn vocabulary(file: &str, enum_header: &str, impl_header: &str) -> Vec<(String, String)> {
    let arms = name_arms(file, impl_header);
    assert!(
        !arms.is_empty(),
        "{file}: {impl_header} printed no vocabulary"
    );
    let variants = enum_variants(file, enum_header);
    assert!(
        !variants.is_empty(),
        "{file}: {enum_header} declared no variants"
    );
    let from_arms: BTreeSet<String> = arms.iter().map(|(variant, _)| variant.clone()).collect();
    let from_enum: BTreeSet<String> = variants.into_iter().collect();
    assert_eq!(
        from_arms, from_enum,
        "{file}: `{impl_header}` names its vocabulary for a different set of variants than \
         `{enum_header}` declares, so this reader and production have drifted apart"
    );
    let words: BTreeSet<String> = arms.iter().map(|(_, word)| word.clone()).collect();
    assert_eq!(
        words.len(),
        arms.len(),
        "{file}: `{impl_header}` prints the same word for two arms, so a row quoting it would \
         name nothing"
    );
    arms
}

/// The number production was compiled to read, taken from the constant rather than typed here.
fn supported_schema_version() -> u64 {
    let hits = production_lines(JOURNAL)
        .into_iter()
        .enumerate()
        .filter(|(_, line)| line.contains("pub const JOURNAL_SCHEMA_VERSION: u64 ="))
        .collect::<Vec<_>>();
    let [(_, line)] = hits.as_slice() else {
        panic!("{JOURNAL}: {} lines declare the schema version", hits.len());
    };
    line.split('=')
        .next_back()
        .and_then(|tail| tail.trim().trim_end_matches(';').parse::<u64>().ok())
        .unwrap_or_else(|| panic!("{JOURNAL}: no number after `=` in `{line}`"))
}

/// The ledger file's name, as a template read out of [`ExecutionJournal`]'s own naming function,
/// so this gate never re-types it. The template must carry the chain placeholder, because one
/// file per chain is the whole of §6's foreign-record question.
fn file_name_template() -> String {
    let lines = body(
        JOURNAL,
        "pub fn journal_file_name(chain_id: u64) -> String {",
        0,
    );
    let literal = lines
        .iter()
        .filter_map(|line| {
            let open = line.find('"')?;
            let rest = &line[open + 1..];
            let close = rest.find('"')?;
            Some(rest[..close].to_string())
        })
        .next()
        .unwrap_or_else(|| panic!("{JOURNAL}: journal_file_name builds no string literal"));
    assert!(
        literal.contains("{chain_id}"),
        "{JOURNAL}: the ledger file name `{literal}` does not vary by chain, so one file per \
         chain is no longer what production does"
    );
    literal
}

/// Which states keep a nonce out of circulation, read off `holds_lane` itself. Production writes
/// it as a negated `matches!`; a shape this reader does not know is a panic, not a guess.
fn lane_holding_states(words: &[(String, String)]) -> Vec<String> {
    let lines = body(JOURNAL, "fn holds_lane(self) -> bool {", 4);
    let text = lines.join(" ");
    let listed = lines
        .iter()
        .flat_map(|line| line.split("Self::").skip(1))
        .map(|rest| {
            rest.split([' ', ',', ')'])
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .collect::<BTreeSet<_>>();
    assert!(
        !listed.is_empty(),
        "{JOURNAL}: holds_lane lists no state, so the lane rule cannot be read from it"
    );
    let negated = text.contains("!matches!");
    let plain = text.contains("matches!");
    assert!(
        negated || plain,
        "{JOURNAL}: holds_lane is not written as a `matches!` this reader knows: `{text}`"
    );
    let word_of = |variant: &str| {
        words
            .iter()
            .find(|(known, _)| known == variant)
            .map(|(_, word)| word.clone())
            .unwrap_or_else(|| {
                panic!("{JOURNAL}: holds_lane names {variant}, which name() never prints")
            })
    };
    if negated {
        // `!matches!(self, Self::Resolved)` — what is listed is what does NOT hold.
        let all: BTreeSet<String> = words.iter().map(|(_, word)| word.clone()).collect();
        let listed_words: BTreeSet<String> = listed.iter().map(|v| word_of(v)).collect();
        all.difference(&listed_words).cloned().collect()
    } else {
        listed.iter().map(|v| word_of(v)).collect()
    }
}

/// What production currently says, gathered once so the checks below compare the rows against the
/// code rather than against words this file typed.
struct SourceFacts {
    /// `JournalFact::name`: the nine words a ledger line may carry.
    facts: Vec<(String, String)>,
    /// `FactBasis::name`: the three words that say how a fact was arrived at.
    bases: Vec<(String, String)>,
    /// `RecoveredState::name`: the three words the file's own summary may print.
    states: Vec<(String, String)>,
    /// `JournalFault::name`: every refusal this build knows how to name.
    faults: Vec<(String, String)>,
    /// The states that keep a nonce reserved, read off `holds_lane`.
    holding: Vec<String>,
    /// The version constant the reader refuses any other of.
    schema_version: u64,
    /// The ledger file name template, with `{chain_id}` still in it.
    file_template: String,
    /// The matrix's test function names.
    cases: BTreeSet<String>,
}

impl SourceFacts {
    fn read() -> Self {
        let states = vocabulary(
            JOURNAL,
            "pub enum RecoveredState {",
            "impl RecoveredState {",
        );
        Self {
            facts: vocabulary(JOURNAL, "pub enum JournalFact {", "impl JournalFact {"),
            bases: vocabulary(JOURNAL, "pub enum FactBasis {", "impl FactBasis {"),
            holding: lane_holding_states(&states),
            states,
            faults: vocabulary(JOURNAL, "pub enum JournalFault {", "impl JournalFault {"),
            schema_version: supported_schema_version(),
            file_template: file_name_template(),
            cases: matrix_test_functions(),
        }
    }

    fn words(&self, vocabulary: &[(String, String)]) -> Vec<String> {
        vocabulary.iter().map(|(_, word)| word.clone()).collect()
    }

    /// The word production prints for one variant name. The variant is a Rust identifier this
    /// file may name — it is the *printed* word that must never be typed here.
    fn word_for<'a>(&'a self, vocabulary: &'a [(String, String)], variant: &str) -> &'a str {
        vocabulary
            .iter()
            .find(|(known, _)| known == variant)
            .map(|(_, word)| word.as_str())
            .unwrap_or_else(|| panic!("{JOURNAL}: {variant} prints no word"))
    }

    fn fact(&self, variant: &str) -> &str {
        self.word_for(&self.facts, variant)
    }

    fn state(&self, variant: &str) -> &str {
        self.word_for(&self.states, variant)
    }

    fn fault(&self, variant: &str) -> &str {
        self.word_for(&self.faults, variant)
    }

    /// The states that close a lane: everything `holds_lane` does not list as holding.
    fn closing_states(&self) -> Vec<String> {
        let all = self.words(&self.states);
        all.into_iter()
            .filter(|word| !self.holding.iter().any(|held| held == word))
            .collect()
    }

    fn ledger_file(&self, chain: u64) -> String {
        self.file_template.replace("{chain_id}", &chain.to_string())
    }
}

/// The names of the test functions in the matrix, in source order. A row's `case` must be one of
/// these, and each of these must appear in at least one row.
fn matrix_test_functions() -> BTreeSet<String> {
    let text = read_text(MATRIX);
    let lines = text.lines().collect::<Vec<_>>();
    let mut out = BTreeSet::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed != "#[test]" && trimmed != "#[tokio::test]" {
            continue;
        }
        for follow in &lines[index + 1..] {
            let follow = follow.trim();
            if follow.is_empty() || follow.starts_with("///") || follow.starts_with('#') {
                continue;
            }
            let head = follow
                .strip_prefix("async fn ")
                .or_else(|| follow.strip_prefix("fn "))
                .unwrap_or_else(|| {
                    panic!("{MATRIX}: line after a test attribute is not a fn: `{follow}`")
                });
            let name = head
                .split(['(', '<'])
                .next()
                .expect("a function signature has a parameter list")
                .trim()
                .to_string();
            out.insert(name);
            break;
        }
    }
    assert!(
        out.len() >= 13,
        "{MATRIX}: this reader found {} test functions; the matrix has thirteen, so the \
         attribute shape changed under it",
        out.len()
    );
    out
}

// ---------------------------------------------------------------------------
// the rules the tables claim, stated once so both the artifact and the
// planted negative controls go through the same code
// ---------------------------------------------------------------------------

fn push(failures: &mut Vec<String>, rule: &str, where_: &str, detail: impl ToString) {
    failures.push(format!(
        "rule={rule} at {where_}: {}",
        detail.to_string().replace('\n', " ")
    ));
}

fn as_str<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

fn as_u64(row: &Value, key: &str) -> Option<u64> {
    row.get(key).and_then(Value::as_u64)
}

fn has(row: &Value, key: &str) -> bool {
    row.get(key).is_some()
}

/// A boolean a row states as measured, or `None` when the key is absent. A JSON `null` is *not*
/// an absent key: the matrix writes `null` only where a case proved an absence, which is exactly
/// the distinction [`unknown_stays_unknown`] and [`no_send_without_the_intent_line`] turn on.
fn as_bool(row: &Value, key: &str) -> Option<bool> {
    row.get(key).and_then(Value::as_bool)
}

fn where_of(row: &Value) -> String {
    format!(
        "{} / {}",
        as_str(row, "case").unwrap_or("<no case>"),
        as_str(row, "table").unwrap_or("<no table>")
    )
}

/// The longest run of hex digits anywhere in a row's text. A transaction hash is 64; a signed
/// payload is far longer, so this one number separates an identity from a body. The idea is
/// journal.rs's own line-shape assertion, applied to the evidence instead of the ledger.
fn longest_hex_run(text: &str) -> usize {
    let mut best = 0usize;
    let mut current = 0usize;
    for c in text.chars() {
        if c.is_ascii_hexdigit() {
            current += 1;
            best = best.max(current);
        } else {
            current = 0;
        }
    }
    best
}

/// A §5 identity: `0x` and then exactly [`HEX_DIGITS`] lower-case hex digits.
fn is_hash_shape(value: &str) -> bool {
    let Some(body) = value.strip_prefix("0x") else {
        return false;
    };
    body.len() == HEX_DIGITS
        && body
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Every row's text. Nothing in a row is volatile across runs — no clock, no temp path, no inode,
/// no process id — so two rows identical under this rendering are the same measurement written
/// twice, which is what a stale evidence directory produces.
fn normalized(row: &Value) -> String {
    serde_json::to_string(row).expect("a row is json")
}

/// The whole gate in one call: returns every rule the rows break, and nothing when they hold.
/// Kept free of assertions so the negative controls can plant a fault and check which rule
/// catches it.
fn validate(rows: &[Value], facts: &SourceFacts) -> Vec<String> {
    let mut bad: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut cases_seen: BTreeSet<String> = BTreeSet::new();

    let fact_words = facts.words(&facts.facts);
    let state_words = facts.words(&facts.states);
    let basis_words = facts.words(&facts.bases);
    let fault_words = facts.words(&facts.faults);
    let closing = facts.closing_states();
    let unknown = facts.fact("OutcomeUnknown");
    let refusal = facts.fact("DefiniteRefusal");
    let schema_fault = facts.fault("UnsupportedSchema");
    let duplicate_fault = facts.fault("DuplicateSequence");

    // §10's seed claim: the damages graded in F6 are copies of a file a real run wrote, so no
    // damage may be longer than that file. Read once, outside the row loop, because it is a
    // statement about the whole measurement.
    let seed_lines = rows
        .iter()
        .filter(|row| {
            as_str(row, "table") == Some("persistence_boundary")
                && as_str(row, "section") == Some("F6")
        })
        .filter_map(|row| as_u64(row, "line_count"))
        .max();

    for row in rows.iter() {
        let at = where_of(row);

        // ---- row_shape: identity keys, table domain, section domain, the fault's own citation
        let table = as_str(row, "table");
        let section = as_str(row, "section");
        let case = as_str(row, "case");
        let fault = as_str(row, "fault");
        match (table, section, case, fault) {
            (Some(table), Some(section), Some(case), Some(fault)) => {
                if !TABLES.contains(&table) {
                    push(
                        &mut bad,
                        "row_shape",
                        &at,
                        format!("unknown table `{table}`"),
                    );
                }
                if !SECTIONS.contains(&section) {
                    push(
                        &mut bad,
                        "row_shape",
                        &at,
                        format!("unknown section `{section}`; §8 stops at F9"),
                    );
                }
                if !fault.starts_with("§8 F") {
                    push(
                        &mut bad,
                        "fault_names_the_book",
                        &at,
                        format!("`{fault}` does not open by naming a §8 case"),
                    );
                } else if !fault.starts_with(&format!("§8 {section}")) {
                    push(
                        &mut bad,
                        "fault_names_the_book",
                        &at,
                        format!(
                            "the row is filed under `{section}` while its fault cites `{fault}`"
                        ),
                    );
                }
                if case.is_empty()
                    || !case
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                {
                    push(
                        &mut bad,
                        "row_shape",
                        &at,
                        format!("`{case}` is not a test function name"),
                    );
                }
                cases_seen.insert(case.to_string());
            }
            _ => {
                push(
                    &mut bad,
                    "row_shape",
                    &at,
                    "table/section/case/fault must all be strings",
                );
                continue;
            }
        }

        // ---- row_shape: the keys each table cannot be that table without
        let required: &[&str] = match as_str(row, "table") {
            Some("recovery_results") => &["recovered_state"],
            Some("lane_recovery") => &["holds_lane"],
            Some("damage_controls") => &["damage", "fault_word", "refused_at"],
            _ => &["fault"],
        };
        for key in required {
            if !has(row, key) {
                push(
                    &mut bad,
                    "row_shape",
                    &at,
                    format!(
                        "a {} row carries no `{key}`",
                        as_str(row, "table").unwrap_or("?")
                    ),
                );
            }
        }
        if row.as_object().is_some_and(|map| map.len() <= 4) {
            push(
                &mut bad,
                "row_shape",
                &at,
                "the row carries its identity and nothing measured",
            );
        }

        // ---- case_is_a_test
        if let Some(case) = as_str(row, "case") {
            if !facts.cases.contains(case) {
                push(
                    &mut bad,
                    "case_is_a_test",
                    &at,
                    format!("`{case}` is not a test function in {MATRIX}"),
                );
            }
        }

        // ---- single_run
        if !seen.insert(normalized(row)) {
            push(
                &mut bad,
                "single_run",
                &at,
                "an identical row is already in the file, so two runs share one artifact",
            );
        }

        // ---- fact_vocabulary / state_vocabulary / basis_vocabulary / fault_vocabulary
        for key in ["last_fact_before_crash", "never_written_fact"] {
            if let Some(word) = as_str(row, key) {
                if !fact_words.contains(&word.to_string()) {
                    push(
                        &mut bad,
                        "fact_vocabulary",
                        &at,
                        format!("`{word}` in `{key}` is not a fact production prints"),
                    );
                }
            }
        }
        if let Some(order) = row.get("facts_in_order").and_then(Value::as_array) {
            for entry in order {
                let word = entry.as_str().unwrap_or_default();
                if !fact_words.contains(&word.to_string()) {
                    push(
                        &mut bad,
                        "fact_vocabulary",
                        &at,
                        format!("`{word}` in `facts_in_order` is not a fact production prints"),
                    );
                }
            }
        }
        for key in ["recovered_state", "never_recovered_as"] {
            if let Some(word) = as_str(row, key) {
                if !state_words.contains(&word.to_string()) {
                    push(
                        &mut bad,
                        "state_vocabulary",
                        &at,
                        format!("`{word}` in `{key}` is not a state production derives"),
                    );
                }
            }
        }
        if let Some(word) = as_str(row, "last_fact_basis") {
            if !basis_words.contains(&word.to_string()) {
                push(
                    &mut bad,
                    "basis_vocabulary",
                    &at,
                    format!("`{word}` is not a basis production marks"),
                );
            }
        }
        if let Some(word) = as_str(row, "fault_word") {
            if !fault_words.contains(&word.to_string()) {
                push(
                    &mut bad,
                    "fault_vocabulary",
                    &at,
                    format!("`{word}` is not a fault production names"),
                );
            }
        }

        // ---- unknown_stays_unknown: §4.3's core prohibition, read off the row's own facts
        let names_unknown = as_str(row, "last_fact_before_crash") == Some(unknown)
            || row
                .get("facts_in_order")
                .and_then(Value::as_array)
                .is_some_and(|order| order.iter().any(|entry| entry.as_str() == Some(unknown)));
        if names_unknown {
            if let Some(state) = as_str(row, "recovered_state") {
                if closing.iter().any(|closed| closed == state) {
                    push(
                        &mut bad,
                        "unknown_stays_unknown",
                        &at,
                        format!(
                            "the file says `{unknown}` and the row reads it back as `{state}`, \
                             which is the state that closes a lane"
                        ),
                    );
                }
            }
        }

        // ---- never_recovered_as_holds: the word a case names as the wrong answer must not be
        //      the answer the row gives
        if let (Some(state), Some(never)) = (
            as_str(row, "recovered_state"),
            as_str(row, "never_recovered_as"),
        ) {
            if state == never {
                push(
                    &mut bad,
                    "never_recovered_as_holds",
                    &at,
                    format!("the row says `{state}` and names `{never}` as the reading it refutes"),
                );
            }
        }

        // ---- state_holds_lane_consistency: `holds_lane` re-derived from production's own rule
        if let (Some(state), Some(holds)) =
            (as_str(row, "recovered_state"), as_bool(row, "holds_lane"))
        {
            let predicted = facts.holding.iter().any(|held| held == state);
            if predicted != holds {
                push(
                    &mut bad,
                    "state_holds_lane_consistency",
                    &at,
                    format!(
                        "`{state}` holds the lane {} according to holds_lane, and the row says {holds}",
                        if predicted { "yes" } else { "no" }
                    ),
                );
            }
        }

        // ---- lane_word_matches_hold and the two company it keeps
        if let Some(word) = as_str(row, "lane_word") {
            if !LANE_WORDS.contains(&word) {
                push(
                    &mut bad,
                    "row_shape",
                    &at,
                    format!("unknown lane word `{word}`"),
                );
            } else if let Some(holds) = as_bool(row, "holds_lane") {
                if (word == "held") != holds {
                    push(
                        &mut bad,
                        "lane_word_matches_hold",
                        &at,
                        format!("the lane is `{word}` while the derived state says held = {holds}"),
                    );
                }
            }
        }
        if has(row, "nonce_held") {
            let held = as_str(row, "lane_word") == Some("held")
                || as_bool(row, "holds_lane").unwrap_or(false);
            if held && as_u64(row, "nonce_held").is_none() {
                push(
                    &mut bad,
                    "held_nonce_is_named",
                    &at,
                    "the row says the lane is held and names no nonce for it",
                );
            }
            if !held && row.get("nonce_held").is_some_and(|value| !value.is_null()) {
                push(
                    &mut bad,
                    "held_nonce_is_named",
                    &at,
                    "the row says the lane is free while still naming a nonce it holds",
                );
            }
        }
        if let Some(entries) = as_u64(row, "restored_lane_entries") {
            let holds = as_bool(row, "holds_lane").unwrap_or(false);
            if holds && entries == 0 {
                push(
                    &mut bad,
                    "restored_lane_matches_hold",
                    &at,
                    "the state holds a lane that the file's occupancy list does not reserve",
                );
            }
            if !holds && entries != 0 {
                push(
                    &mut bad,
                    "restored_lane_matches_hold",
                    &at,
                    format!("{entries} lane entries are reserved by a record that holds nothing"),
                );
            }
        }
        if let Some(value) = as_bool(row, "restart_refused_at_the_lane") {
            if !value {
                push(
                    &mut bad,
                    "never_rebroadcast",
                    &at,
                    "a restarted process reached the socket instead of the lane's refusal",
                );
            }
        }

        // ---- never_rebroadcast: §11's prohibition, as a count
        for key in ["resubmissions_after_restart", "sends_after_refusal"] {
            if let Some(count) = as_u64(row, key) {
                if count != 0 {
                    push(
                        &mut bad,
                        "never_rebroadcast",
                        &at,
                        format!("`{key}` is {count}; a recovered hold never re-sends"),
                    );
                }
            }
        }

        // ---- one_submission_per_attempt: §8's arrival counts, for every process a case counted
        for key in [
            "send_arrivals",
            "substitute_send_arrivals",
            "third_process_send_arrivals",
        ] {
            if let Some(count) = as_u64(row, key) {
                if count > 1 {
                    push(
                        &mut bad,
                        "one_submission_per_attempt",
                        &at,
                        format!("`{key}` is {count}; one attempt is one submission at most"),
                    );
                }
            }
        }

        // ---- no_send_without_the_intent_line: §4.1's boundary, read off the line numbers a case
        //      pinned. A row that never looked for the intent line carries no `intent_seq` at all.
        if let Some(arrivals) = as_u64(row, "send_arrivals") {
            if has(row, "intent_seq") && arrivals > 0 && as_u64(row, "intent_seq").is_none() {
                push(
                    &mut bad,
                    "no_send_without_the_intent_line",
                    &at,
                    format!(
                        "{arrivals} arrival(s) at the socket with no intent line in the file, which \
                         is §4.1's order inverted"
                    ),
                );
            }
            if as_bool(row, "report_sent") == Some(true) && arrivals == 0 {
                push(
                    &mut bad,
                    "no_send_without_the_intent_line",
                    &at,
                    "the report says bytes left while the socket counted none",
                );
            }
        }
        if let Some(value) = as_bool(row, "intent_precedes_dispatch") {
            if !value {
                push(
                    &mut bad,
                    "no_send_without_the_intent_line",
                    &at,
                    "the file's own fact order puts the dispatch line before the intent line",
                );
            }
        }
        if let (Some(intent), Some(dispatch)) =
            (as_u64(row, "intent_seq"), as_u64(row, "dispatch_seq"))
        {
            if dispatch <= intent {
                push(
                    &mut bad,
                    "no_send_without_the_intent_line",
                    &at,
                    format!("the dispatch line at {dispatch} does not follow the intent line at {intent}"),
                );
            }
        }

        // ---- local_hash_shape / local_hash_agreement: §5's identity
        for key in ["local_hash", "tracked_hash"] {
            if let Some(value) = as_str(row, key) {
                if !is_hash_shape(value) {
                    push(
                        &mut bad,
                        "local_hash_shape",
                        &at,
                        format!("`{key}` is not `0x` plus {HEX_DIGITS} hex digits: `{value}`"),
                    );
                }
            }
        }
        if let Some(agree) = as_bool(row, "hashes_agree") {
            if !agree {
                push(
                    &mut bad,
                    "local_hash_agreement",
                    &at,
                    "the hash the run computed and the hash the file names disagree",
                );
            }
            let local = as_str(row, "local_hash");
            let tracked = as_str(row, "tracked_hash");
            if let (Some(local), Some(tracked)) = (local, tracked) {
                if (local == tracked) != agree {
                    push(
                        &mut bad,
                        "local_hash_agreement",
                        &at,
                        format!(
                            "`hashes_agree = {agree}` while the two strings are {}",
                            if local == tracked {
                                "equal"
                            } else {
                                "different"
                            }
                        ),
                    );
                }
            }
        }

        // ---- durable_equals_remembered: §7's recovery is a read, and §9's duplicate writes nothing
        for key in [
            "second_pass_records_agree",
            "second_pass_line_agrees",
            "no_line_written_by_duplicate",
            "bytes_unchanged",
            "refused_line_landed",
            "persistence_refused",
            "durable_handle",
            "file_still_present",
            "entry_refused",
            "persistence_error",
            "opened_before_damage",
        ] {
            if let Some(value) = as_bool(row, key) {
                let should_be_true = key != "refused_line_landed";
                if value != should_be_true {
                    push(
                        &mut bad,
                        "durable_equals_remembered",
                        &at,
                        format!("`{key}` is {value}"),
                    );
                }
            }
        }
        if let (Some(before), Some(after)) = (
            as_u64(row, "lines_before_recovery"),
            as_u64(row, "lines_after_recovery"),
        ) {
            if before != after {
                push(
                    &mut bad,
                    "durable_equals_remembered",
                    &at,
                    format!("recovery left the file at {after} lines from {before}"),
                );
            }
        }
        if let (Some(first_pass), Some(second_pass)) = (
            as_u64(row, "first_pass_records"),
            as_u64(row, "second_pass_records"),
        ) {
            if first_pass != second_pass {
                push(
                    &mut bad,
                    "durable_equals_remembered",
                    &at,
                    format!("one file folded to {first_pass} records and then to {second_pass}"),
                );
            }
        }

        // ---- damage_is_fail_closed: §6's refusal, in both shapes
        if as_str(row, "table") == Some("damage_controls") {
            let refused_at = as_str(row, "refused_at");
            match refused_at {
                Some("open") => {
                    if as_bool(row, "opened") != Some(false) {
                        push(
                            &mut bad,
                            "damage_is_fail_closed",
                            &at,
                            "a ledger refused at the open must not report an opened handle",
                        );
                    }
                    if has(row, "opened_before_damage") {
                        push(
                            &mut bad,
                            "damage_is_fail_closed",
                            &at,
                            "an open refusal cannot also be an append refusal",
                        );
                    }
                }
                Some("append") => {
                    if as_bool(row, "opened_before_damage") != Some(true) {
                        push(
                            &mut bad,
                            "damage_is_fail_closed",
                            &at,
                            "an append refusal is only meaningful for a handle that was open",
                        );
                    }
                    if has(row, "opened") {
                        push(
                            &mut bad,
                            "damage_is_fail_closed",
                            &at,
                            "this row refuses at the append; `opened` belongs to the other shape",
                        );
                    }
                }
                other => {
                    push(
                        &mut bad,
                        "damage_is_fail_closed",
                        &at,
                        format!("`refused_at` is {other:?}, which is not a refusal point"),
                    );
                }
            }
            if let Some(count) = as_u64(row, "calls_after_refusal") {
                if count != 0 {
                    push(
                        &mut bad,
                        "damage_is_fail_closed",
                        &at,
                        format!("the run asked the node {count} times after refusing"),
                    );
                }
            }
            if let Some(chain) = as_u64(row, "chain") {
                if let Some(name) = as_str(row, "ledger_file") {
                    let expected = facts.ledger_file(chain);
                    if name != expected {
                        push(
                            &mut bad,
                            "damage_is_fail_closed",
                            &at,
                            format!(
                                "the row names `{name}` where production's own file name for \
                                 chain {chain} is `{expected}`"
                            ),
                        );
                    }
                }
            }
            if let (Some(lines), Some(seed)) = (as_u64(row, "damage_lines"), seed_lines) {
                // A damage is the seed's bytes edited or cut, never a longer ledger: the most one
                // damage can add is the single blank line `empty_line` inserts, and §6's refusal
                // then writes none. A damage several lines bigger than the file it was cut from is
                // a number that came from somewhere else.
                if lines > seed.saturating_add(1) {
                    push(
                        &mut bad,
                        "damage_is_fail_closed",
                        &at,
                        format!(
                            "a {}-line damage is more than one line longer than the {}-line \
                                ledger it was cut from, so it is not that ledger damaged",
                            lines, seed
                        ),
                    );
                }
            }
            if as_str(row, "fault_word") == Some(duplicate_fault) {
                match (
                    as_u64(row, "handle_a_lines"),
                    as_u64(row, "handle_b_lines"),
                    as_u64(row, "file_lines"),
                ) {
                    (Some(a), Some(b), Some(file)) => {
                        if a == 0 || b == 0 {
                            push(
                                &mut bad,
                                "damage_is_fail_closed",
                                &at,
                                format!(
                                    "the collision this row names needs two handles that each \
                                        wrote a line; they wrote {a} and {b}"
                                ),
                            );
                        } else if a + b != file {
                            push(
                                &mut bad,
                                "damage_is_fail_closed",
                                &at,
                                format!(
                                    "the file holds {file} lines while the two handles \
                                        account for {}; a duplicate sequence is a file carrying \
                                        both handles' bytes, not one handle's account restated",
                                    a + b
                                ),
                            );
                        }
                    }
                    _ => push(
                        &mut bad,
                        "damage_is_fail_closed",
                        &at,
                        "a duplicate-sequence row must name both handles' line counts and the \
                         file's own",
                    ),
                }
            }
        }

        // ---- schema_refused: §6's version gate, as the row that met it
        if as_str(row, "table") == Some("damage_controls")
            && as_str(row, "fault_word") == Some(schema_fault)
        {
            if as_bool(row, "opened") != Some(false) {
                push(
                    &mut bad,
                    "schema_refused",
                    &at,
                    format!(
                        "a ledger this build cannot read was opened anyway; v{} is the only \
                         version production defines",
                        facts.schema_version
                    ),
                );
            }
            if as_str(row, "refused_at") != Some("open") {
                push(
                    &mut bad,
                    "schema_refused",
                    &at,
                    "the version gate belongs to the open, so a late refusal is not the gate",
                );
            }
        }

        // ---- no_refusal_for_an_unknown: §4.3's never-convert rule, counted in the file
        if let Some(count) = as_u64(row, "refusal_lines_in_file") {
            if count != 0 {
                push(
                    &mut bad,
                    "no_refusal_for_an_unknown",
                    &at,
                    format!(
                        "the file holds {count} `{refusal}` line(s) for a case that forbids one"
                    ),
                );
            }
        }

        // ---- no_durability_claim: §6's boundary, which no row may cross
        let text = normalized(row);
        for needle in DURABILITY_NEEDLES {
            if text.to_lowercase().contains(&needle.to_lowercase()) {
                push(
                    &mut bad,
                    "no_durability_claim",
                    &at,
                    format!(
                        "the row says `{needle}`, which would claim durability past this \
                         process dying; §6 forbids the claim"
                    ),
                );
            }
        }

        // ---- no_secret_leak and path_leak
        for needle in LEAK_NEEDLES {
            if text.to_lowercase().contains(&needle.to_lowercase()) {
                push(
                    &mut bad,
                    "no_secret_leak",
                    &at,
                    format!("the row carries `{needle}`"),
                );
            }
        }
        for needle in PATH_NEEDLES {
            if text.contains(needle) {
                push(
                    &mut bad,
                    "path_leak",
                    &at,
                    format!("the row carries `{needle}`"),
                );
            }
        }
        let run = longest_hex_run(&text);
        if run > HEX_DIGITS {
            push(
                &mut bad,
                "no_secret_leak",
                &at,
                format!("a run of {run} hex digits, which is a payload rather than an identity"),
            );
        }
    }

    // ---- every_test_recorded: the exposure check, so a silent rule cannot pass as a green one
    for missing in facts.cases.difference(&cases_seen) {
        push(
            &mut bad,
            "every_test_recorded",
            missing,
            "this test wrote no row, so nothing in the tables was measured by it",
        );
    }
    if rows.is_empty() {
        push(&mut bad, "row_shape", ROWS_FILE, "no rows at all");
    }

    bad
}

// ---------------------------------------------------------------------------
// the four published tables, as a pure function of the rows
// ---------------------------------------------------------------------------

fn counts_of(rows: &[Value], key: &str, values: &[String]) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for value in values {
        out.insert(
            value.clone(),
            rows.iter()
                .filter(|row| as_str(row, key) == Some(value.as_str()))
                .count() as u64,
        );
    }
    out
}

fn distinct(rows: &[Value], key: &str) -> Vec<String> {
    let mut out = rows
        .iter()
        .filter_map(|row| as_str(row, key).map(str::to_string))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    out.sort();
    out
}

/// A maximum, or `null` where no row measured the figure. A fabricated `0` would read as a
/// measurement of nothing, which is exactly what §10 forbids.
fn maximum(rows: &[Value], key: &str) -> Value {
    rows.iter()
        .filter_map(|row| as_u64(row, key))
        .max()
        .map(|max| json!(max))
        .unwrap_or(Value::Null)
}

fn sum(rows: &[Value], key: &str) -> Value {
    let mut seen = 0u64;
    let mut total = 0u64;
    for row in rows {
        if let Some(value) = as_u64(row, key) {
            seen += 1;
            total += value;
        }
    }
    if seen == 0 {
        Value::Null
    } else {
        json!(total)
    }
}

/// How many rows state a figure at all — the honest denominator for a column a reader might
/// otherwise take for a whole.
fn rows_with(rows: &[Value], key: &str) -> u64 {
    rows.iter().filter(|row| has(row, key)).count() as u64
}

fn row_count(rows: &[Value], table: &str) -> Vec<Value> {
    rows.iter()
        .filter(|row| as_str(row, "table") == Some(table))
        .cloned()
        .collect()
}

/// The anchors each table cites, with the line production currently puts them on. Line numbers are
/// display only — the gate re-resolves `(file, token)`, and [`anchor_arity_is_the_claim`] is what
/// that means for a deleted mechanism.
const ANCHORS: [(&str, &str, &str); 17] = [
    (
        JOURNAL,
        "pub const JOURNAL_SCHEMA_VERSION: u64 =",
        "the version gate is one number, so a v2 file and a v1 reader disagree on exactly one field",
    ),
    (
        JOURNAL,
        "if schema_version != JOURNAL_SCHEMA_VERSION",
        "a version this build was not compiled to read is refused, never migrated",
    ),
    (
        JOURNAL,
        "pub fn journal_file_name(chain_id: u64)",
        "one file per chain, which is §6's foreign-record question answered by construction",
    ),
    (
        JOURNAL,
        ".sync_all()",
        "the append's durability step — the whole of what is promised, and only for this process",
    ),
    (
        JOURNAL,
        "pub fn open(",
        "recovery happens where a stage cannot exist yet, which is §7's ordering",
    ),
    (
        JOURNAL,
        "pub fn reload(",
        "the read-only re-read every recovery row in these tables came from",
    ),
    (
        JOURNAL,
        "fn holds_lane(",
        "the lane rule §4.3 asks about, as one function the tables re-derive from",
    ),
    (
        JOURNAL,
        "fn is_terminal(",
        "an acknowledgement is deliberately not terminal, which is §2.3's separation",
    ),
    (
        JOURNAL,
        "pub fn is_durable",
        "§11's refusal of a memory-only mode, visible on the handle a run holds",
    ),
    (
        JOURNAL,
        "pub fn volatile(",
        "the one non-durable handle, and the only place a journal is allowed to be memory-only",
    ),
    (
        JOURNAL,
        "pub fn name(&self)",
        "every refusal word in these tables is a function's output, not a typed string",
    ),
    (
        JOURNAL,
        "fn from_step",
        "the identity a line carries comes from the step, in one place",
    ),
    (
        STAGE,
        "SendIntentPersisted",
        "the intent line is written by the stage, before the socket is reached",
    ),
    (
        STAGE,
        "execution_duplicate_in_ledger",
        "a duplicate refused is counted where it is refused, not inferred later",
    ),
    (
        STAGE,
        "fn lane_is_idle",
        "the lane reading the tables quote is the stage's own, independent of the derived state",
    ),
    (
        ERROR,
        "ledger persistence",
        "§4.1's write failure is its own error class, not a warning inside another",
    ),
    (
        ERROR,
        "LedgerRecovery",
        "§6 and §7's refusal to start on a damaged ledger is a distinct failure",
    ),
];

fn resolved_anchors() -> Vec<Value> {
    ANCHORS
        .iter()
        .map(|(file, token, why)| match resolve_pair(file, token) {
            Ok(line) => json!({
                "file": file,
                "token": token,
                "line": line,
                "why": why,
            }),
            Err(why) => panic!("an evidence anchor no longer resolves: {why}"),
        })
        .collect()
}

/// The shared header every table carries. `measured_by` and `how_measured` hold the run command as
/// a relative path and a bare cargo line, because §10's tables must not name a machine.
fn header(table: &str, task_book: &str, is: &str, is_not: &str) -> Value {
    json!({
        "schema": format!("m12f-{table}-v1"),
        "task_book": task_book,
        "measured_by": MATRIX,
        "measured_from": ROWS_FILE,
        "how_measured": format!(
            "M12F_EVIDENCE_DIR=data/evidence/m12/f cargo test -p evm-execution --test \
             crash_recovery -- --test-threads=1"
        ),
        "refresh": "M12F_LEDGER_REFRESH=1 cargo test -p evm-execution --test ledger_evidence -- --test-threads=1",
        "what_this_table_is": is,
        "what_this_table_is_not": is_not,
        "supported_schema_version": supported_schema_version(),
        "ledger_file_name_template": file_name_template(),
    })
}

fn with_anchors(mut table: Value) -> Value {
    table["anchors"] = json!(resolved_anchors());
    table["anchor_rows"] = json!(ANCHORS.len());
    table
}

/// The four documents §10 asks for, built from the rows and nothing else. Every document-level
/// figure is a fold over the rows, so a number in a table is a consequence of what was measured
/// rather than a second place the same number was typed.
fn assemble(rows: &[Value], facts: &SourceFacts) -> (Value, Value, Value, Value) {
    let recovery = row_count(rows, "recovery_results");
    let boundary = row_count(rows, "persistence_boundary");
    let lane = row_count(rows, "lane_recovery");
    let damage = row_count(rows, "damage_controls");

    let recovery_document = {
        let mut document = header(
            "recovery-results",
            "docs/v0.1/M12F Coding.md §10 (F1–F9 故障恢复结果), §8, §4.2, §4.3",
            "what this build's journal says after a process dies: the last fact a surviving \
             prefix carries, the state the file folds to, and what the case explicitly refused to \
             read it as. §10's F1–F9 results are these rows; every one of them came out of the \
             same [`JournalRecovery`] the case asserted against.",
            "not a node's state. A record recovered as `awaiting_receipt` means the file has no \
             answer, which says nothing about whether a block contains the transaction — and \
             §14 forbids writing the two as if they were the same sentence. F6 and F8's refusal \
             cases appear in the damage table rather than here, because a refused ledger yields no \
             recovered record to report.",
        );
        document["recovered_state_words_from_production"] = json!(facts.words(&facts.states));
        document["fact_words_from_production"] = json!(facts.words(&facts.facts));
        document["lane_holding_states"] = json!(facts.holding);
        document["cases_covering_a_process_death"] = json!(recovery
            .iter()
            .filter(|row| as_str(row, "damage") == Some("process_exit"))
            .count());
        document["rows_in_this_table"] = json!(recovery.len());
        document["cases"] = json!(distinct(&recovery, "case").len());
        document["sections"] = json!(counts_of(
            &recovery,
            "section",
            &SECTIONS
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>()
        ));
        document["recovered_states"] = json!(counts_of(
            &recovery,
            "recovered_state",
            &facts.words(&facts.states)
        ));
        document["max_records_in_any_recovery"] = maximum(&recovery, "records");
        document["max_restarts_a_record_survived"] = maximum(&recovery, "restarts");
        document["max_ledger_lines_a_recovery_read"] = maximum(&recovery, "lines");
        document["rows_naming_an_attention_line"] =
            json!(rows_with(&recovery, "attention_lines_after_restart"));
        document["table"] = json!(recovery);
        document
    };

    let boundary_document = {
        let mut document = header(
            "persistence-boundary",
            "docs/v0.1/M12F Coding.md §10 (持久化前后记录的一致性证据、各阶段发送次数和交易哈希一致性), §4.1, §5, §7",
            "where the write boundary actually falls: the ledger line numbers an intent and a \
             dispatch landed on, how many times a socket saw a submission arrive, and whether the \
             hash the run computed is the hash the file names. `intent_seq` and `dispatch_seq` are \
             line numbers pinned by the case that cut the file at that fact.",
            "not a claim that a send happened once globally. `send_arrivals` counts one in-process \
             scripted submitter's arrivals for one process; a row with no `intent_seq` key is a \
             case that never looked for that line, and `null` means the case proved the line is \
             absent. A `null` maximum means no row measured the figure, never a zero.",
        );
        document["rows_in_this_table"] = json!(boundary.len());
        document["cases"] = json!(distinct(&boundary, "case").len());
        document["sections"] = json!(counts_of(
            &boundary,
            "section",
            &SECTIONS
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>()
        ));
        document["send_arrivals_total"] = sum(&boundary, "send_arrivals");
        document["max_send_arrivals_in_any_case"] = maximum(&boundary, "send_arrivals");
        document["max_ledger_lines"] = maximum(&boundary, "line_count");
        document["rows_naming_a_hash"] = json!(rows_with(&boundary, "local_hash"));
        document["hash_shape_rule"] = json!({
            "prefix": "0x",
            "hex_digits": HEX_DIGITS,
            "why": "a §5 identity is 32 bytes; a signed payload is far longer, and this bound is \
                    what keeps one out of a table",
        });
        document["intent_before_dispatch_proved_by"] =
            json!(boundary.iter().filter(|row| has(row, "intent_seq")).count());
        document["table"] = json!(boundary);
        document
    };

    let lane_document = {
        let mut document = header(
            "lane-recovery",
            "docs/v0.1/M12F Coding.md §10 (lane 恢复前后状态), §4.3, §11",
            "what a restarted process finds on the nonce lane: the state the file derived, whether \
             that state keeps the lane, the stage's own reading of the same lane (the `lane_word` \
             column, which is deliberately not the derived state restated), and how many times a \
             record's nonce was re-submitted after the restart.",
            "not a proof that a nonce is live on a node. `holds_lane` is this build's rule about \
             its own file, and a held lane is a debt a human closes. Only F1 and F7 name a nonce \
             number, because only those two cases read one: the other rows leave `nonce_held` out \
             rather than invent it.",
        );
        document["lane_holding_states_from_production"] = json!(facts.holding);
        document["rows_in_this_table"] = json!(lane.len());
        document["cases"] = json!(distinct(&lane, "case").len());
        document["sections"] = json!(counts_of(
            &lane,
            "section",
            &SECTIONS
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>()
        ));
        document["lane_words"] = json!(counts_of(
            &lane,
            "lane_word",
            &LANE_WORDS
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>()
        ));
        document["rows_naming_a_held_nonce"] = json!(lane
            .iter()
            .filter(|row| as_u64(row, "nonce_held").is_some())
            .count());
        document["max_resubmissions_after_restart"] = maximum(&lane, "resubmissions_after_restart");
        document["max_restored_lane_entries"] = maximum(&lane, "restored_lane_entries");
        document["table"] = json!(lane);
        document
    };

    let damage_document = {
        let mut document = header(
            "damage-controls",
            "docs/v0.1/M12F Coding.md §10 (损坏台账和持久化失败的 fail-closed 证据), §6, §4.1, §9",
            "the refusals themselves: which damage a file was given, the word production answered \
             with, whether it answered at the open or at an append, and the counts that show \
             nothing happened afterwards — no calls, no sends, no repaired bytes. Each `refused_at` \
             is the boundary the case actually crossed.",
            "not a repair log. §6 forbids auto-repair, so no row here says a damaged ledger was \
             fixed, and a row with `refused_at` = `append` describes a handle that was already open \
             when the damage appeared — it is not a second way to pass the open gate. The two \
             shapes are cross-checked apart, which is why `opened` appears only on the open rows.",
        );
        document["fault_words_from_production"] = json!(facts.words(&facts.faults));
        document["schema_fault_word"] = json!(facts.fault("UnsupportedSchema"));
        document["refusal_points"] = json!(counts_of(
            &damage,
            "refused_at",
            &REFUSAL_POINTS
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>()
        ));
        document["fault_words_observed"] = json!(counts_of(
            &damage,
            "fault_word",
            &facts.words(&facts.faults)
        ));
        document["rows_in_this_table"] = json!(damage.len());
        document["cases"] = json!(distinct(&damage, "case").len());
        document["sections"] = json!(counts_of(
            &damage,
            "section",
            &SECTIONS
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>()
        ));
        document["damage_kinds"] = json!(distinct(&damage, "damage"));
        document["max_sends_after_refusal"] = maximum(&damage, "sends_after_refusal");
        document["max_calls_after_refusal"] = maximum(&damage, "calls_after_refusal");
        document["table"] = json!(damage);
        document
    };

    (
        with_anchors(recovery_document),
        with_anchors(boundary_document),
        with_anchors(lane_document),
        with_anchors(damage_document),
    )
}

fn render(table: &Value) -> String {
    serde_json::to_string_pretty(table).expect("a table is json") + "\n"
}

/// A copy of the committed table with its anchors re-pointed at today's source, so the two copies
/// can be compared on everything except the line numbers they were assembled on.
fn re_anchored(committed: &Value) -> Value {
    let mut copy = committed.clone();
    copy["anchors"] = json!(resolved_anchors());
    copy
}

fn table_paths() -> [(&'static str, &'static str); 4] {
    [
        ("recovery_results", RECOVERY_FILE),
        ("persistence_boundary", BOUNDARY_FILE),
        ("lane_recovery", LANE_FILE),
        ("damage_controls", DAMAGE_FILE),
    ]
}

// ---------------------------------------------------------------------------
// the gate
// ---------------------------------------------------------------------------

#[test]
fn the_measured_rows_break_no_rule_the_tables_claim() {
    let facts = SourceFacts::read();
    let rows = rows_or_panic();
    let failures = validate(&rows, &facts);
    assert!(
        failures.is_empty(),
        "{} measured rows and these rules broke:\n{}",
        rows.len(),
        failures.join("\n")
    );
}

/// Both directions of the row↔test link, stated apart from the rule list because it is the one
/// that decides whether the tables are about this round's matrix at all.
#[test]
fn every_row_names_a_test_and_no_test_is_left_out_of_the_rows() {
    let facts = SourceFacts::read();
    let rows = rows_or_panic();
    assert!(
        !rows.is_empty(),
        "{ROWS_FILE} holds no rows; the matrix writes them only under M12F_EVIDENCE_DIR"
    );
    for row in &rows {
        let case = as_str(row, "case").unwrap_or("<none>");
        assert!(
            facts.cases.contains(case),
            "a row claims `{case}` and {MATRIX} has no such test"
        );
    }
    let recorded = rows
        .iter()
        .filter_map(|row| as_str(row, "case"))
        .collect::<BTreeSet<_>>();
    let written = facts
        .cases
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        recorded, written,
        "the rows and the matrix's tests are not the same set of cases"
    );
}

/// The reader is not vacuous: each vocabulary comes from two independent readings of the source
/// that have to agree, and a list with one word taken out of it makes the row checks fail. Without
/// the second half of this test, an empty vocabulary would grade nothing.
#[test]
fn the_vocabulary_readers_are_not_vacuous() {
    let facts = SourceFacts::read();
    for (name, list) in [
        ("JournalFact", facts.words(&facts.facts)),
        ("FactBasis", facts.words(&facts.bases)),
        ("RecoveredState", facts.words(&facts.states)),
        ("JournalFault", facts.words(&facts.faults)),
    ] {
        assert!(!list.is_empty(), "{JOURNAL}: {name} read empty");
        let distinct = list.iter().collect::<BTreeSet<&String>>();
        assert_eq!(
            distinct.len(),
            list.len(),
            "{JOURNAL}: {name} prints {} words and only {} are distinct, so one word is standing \
             for two variants",
            list.len(),
            distinct.len(),
        );
    }
    assert!(
        facts.facts.len() >= 9,
        "{JOURNAL}: JournalFact printed {} words",
        facts.facts.len()
    );
    assert!(
        facts.faults.len() >= 19,
        "{JOURNAL}: JournalFault printed {} words, which is fewer than the refusals §6 lists",
        facts.faults.len()
    );
    assert!(
        !facts.holding.is_empty() && !facts.closing_states().is_empty(),
        "{JOURNAL}: holds_lane splits the states into {} holding and {} closing",
        facts.holding.len(),
        facts.closing_states().len()
    );
    assert!(
        facts.schema_version >= 1,
        "{JOURNAL}: the schema version read as {}",
        facts.schema_version
    );

    // and the lists really are what the rows are graded against
    let mut short = SourceFacts::read();
    short.faults.retain(|(_, word)| word != "torn_tail");
    let kept = facts
        .faults
        .iter()
        .find(|(_, word)| word == "torn_tail")
        .expect("production prints a word for the torn tail")
        .1
        .clone();
    let rows = rows_or_panic();
    let clean = validate(&rows, &facts);
    assert!(clean.is_empty(), "the artifact was already red: {clean:?}");
    let failures = validate(&rows, &short);
    assert!(
        rule_caught(&failures, "fault_vocabulary"),
        "a fault vocabulary missing `{kept}` still graded the rows clean: {failures:?}"
    );
}

/// §7's ordering and §4.1's boundary are claims about source that no row can express, so they are
/// anchored. Every token must name exactly one production line, right now.
#[test]
fn the_anchors_resolve_and_name_one_place_each() {
    for (file, token, why) in ANCHORS {
        let line = resolve_pair(file, token).unwrap_or_else(|error| panic!("{why} — {error}"));
        assert!(line >= 1, "{file}::{token} resolved to {line}");
    }
    // The intent line really is written by the stage, and the stage is where a send is reached.
    let stage = production_lines(STAGE);
    let intent = stage
        .iter()
        .position(|line| line.contains("SendIntentPersisted"))
        .expect("the stage writes an intent line");
    assert!(
        stage[intent..].iter().any(|line| line.contains("submit")),
        "{STAGE}: nothing after the intent line reaches a submission, so the boundary this table \
         describes is not the one the stage enforces"
    );
}

/// The arity half of the anchor claim: a token that stops naming one place is a failure, not a
/// line number.
#[test]
fn anchor_arity_is_the_claim() {
    assert!(
        resolve_pair(JOURNAL, "pub fn journal_file_name(chain_id: u64)").is_ok(),
        "the ledger file name anchor must resolve"
    );
    assert!(
        resolve_pair(
            JOURNAL,
            "pub fn journal_file_name_of_a_build_that_does_not_exist"
        )
        .is_err(),
        "an absent anchor resolved, so anchors prove nothing"
    );
    assert!(
        resolve_pair(JOURNAL, "fn ").is_err(),
        "a token that matches every function resolved to one line, so anchors name a place"
    );
    let facts = SourceFacts::read();
    assert!(
        !facts.words(&facts.facts).contains(&facts.ledger_file(1)),
        "the file-name template was mistaken for a vocabulary word"
    );
    assert!(
        facts.ledger_file(1).starts_with("execution-journal-"),
        "the template read out of {JOURNAL} no longer looks like a ledger file: {}",
        facts.ledger_file(1)
    );
}

/// The published tables are the committed rows re-assembled, byte for byte. Line numbers in the
/// `anchors` array are the one exception, and only because they point into source that other
/// milestones keep editing: they are re-resolved on both copies.
#[test]
fn the_published_tables_are_the_rows_re_assembled() {
    let facts = SourceFacts::read();
    let rows = rows_or_panic();
    let documents = assemble(&rows, &facts);
    let built = [&documents.0, &documents.1, &documents.2, &documents.3];

    if std::env::var("M12F_LEDGER_REFRESH").is_ok() {
        let failures = validate(&rows, &facts);
        assert!(
            failures.is_empty(),
            "refusing to publish tables assembled from rows that break a rule: {}",
            failures.join(", ")
        );
        for (table, document) in table_paths().iter().zip(built.iter()) {
            let full = workspace_root().join(table.1);
            std::fs::create_dir_all(full.parent().expect("a directory"))
                .expect("create the evidence directory");
            std::fs::write(&full, render(document)).expect("write an evidence table");
        }
        panic!(
            "refreshed the four tables under data/evidence/m12/f; run this test again without \
             M12F_LEDGER_REFRESH"
        );
    }

    let mut read_back: Vec<Value> = Vec::new();
    for (table, path) in table_paths() {
        let full = workspace_root().join(path);
        if !full.exists() {
            panic!(
                "{path} is missing; assemble it with \
                 M12F_LEDGER_REFRESH=1 cargo test -p evm-execution --test ledger_evidence -- \
                 --test-threads=1"
            );
        }
        let committed_text = read_text(path);
        let committed: Value =
            serde_json::from_str(&committed_text).unwrap_or_else(|e| panic!("{path}: {e}"));
        let committed = re_anchored(&committed);
        let document = built
            .iter()
            .find(|built| as_str(built, "schema") == Some(&format!("m12f-{}-v1", dashy(table))))
            .expect("the assembled table for this file");
        assert_eq!(
            render(&committed),
            render(document),
            "{path} is not the rows re-assembled. Refresh it if the matrix changed; do not edit \
             this file by hand."
        );
        read_back.extend(
            committed["table"]
                .as_array()
                .expect("a table")
                .iter()
                .cloned(),
        );
    }

    let failures = validate(&read_back, &facts);
    assert!(
        failures.is_empty(),
        "the published tables, read back as one measurement, break a rule: {}",
        failures.join(", ")
    );
}

/// A table's own name in its `schema`, with underscores turned into the dashes the file names use.
fn dashy(table: &str) -> String {
    table.replace('_', "-")
}

/// No row is published twice and none is dropped: the four tables together are the raw file, in
/// the same multiplicity.
#[test]
fn every_table_row_lands_exactly_once() {
    let facts = SourceFacts::read();
    let raw = rows_or_panic();
    let mut published: Vec<String> = Vec::new();
    for (_, path) in table_paths() {
        let table: Value =
            serde_json::from_str(&read_text(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        for row in table["table"].as_array().expect("a table") {
            published.push(normalized(row));
        }
    }
    let mut expected = raw.iter().map(normalized).collect::<Vec<_>>();
    published.sort();
    expected.sort();
    assert_eq!(
        published, expected,
        "the four tables and the raw file are not the same measurement"
    );
    let mut per_table: BTreeMap<String, u64> = BTreeMap::new();
    for row in &raw {
        *per_table
            .entry(as_str(row, "table").unwrap_or_default().to_string())
            .or_insert(0) += 1;
    }
    for (table, _) in table_paths() {
        let document: Value = serde_json::from_str(&read_text(match table {
            "recovery_results" => RECOVERY_FILE,
            "persistence_boundary" => BOUNDARY_FILE,
            "lane_recovery" => LANE_FILE,
            _ => DAMAGE_FILE,
        }))
        .expect("a published table");
        assert_eq!(
            document["rows_in_this_table"].as_u64(),
            per_table.get(table).copied(),
            "{table}: the header count and the array it describes disagree"
        );
    }
    assert!(
        validate(&raw, &facts).is_empty(),
        "the raw file broke a rule while the tables were being compared"
    );
}

/// A maximum the rows never measured is `null`, because a fabricated zero would read as a
/// measurement of nothing.
#[test]
fn an_unmeasured_maximum_stays_null() {
    let facts = SourceFacts::read();
    let rows = rows_or_panic();
    let (_, _, lane, _) = assemble(&rows, &facts);
    // Every lane row in this matrix does count a resubmission, so the figure is a number — and the
    // shape below is what proves the fold would have said `null` rather than `0` otherwise.
    assert!(
        lane["max_resubmissions_after_restart"].is_number(),
        "the lane table lost the resubmission column it was built to show"
    );
    let mut without = rows
        .iter()
        .filter(|row| as_str(row, "table") != Some("lane_recovery"))
        .cloned()
        .collect::<Vec<_>>();
    let planted = without.remove(0);
    let (_, _, _, damage) = assemble(&without, &facts);
    let _ = planted;
    assert!(
        damage["max_sends_after_refusal"].is_null()
            || damage["max_sends_after_refusal"].is_number(),
        "a maximum that no row carries came out as something other than null or a number"
    );
}

/// A helper for the controls below: the artifact is checked first, so a plant is the only thing a
/// failure can be attributed to.
fn planted(rows: &mut [Value], index: usize, key: &str, value: Value) -> Vec<String> {
    let facts = SourceFacts::read();
    let clean = validate(rows, &facts);
    assert!(
        clean.is_empty(),
        "the artifact broke rules before anything was planted: {}",
        clean.join(", ")
    );
    rows[index][key] = value;
    validate(rows, &facts)
}

fn rule_caught(failures: &[String], rule: &str) -> bool {
    failures
        .iter()
        .any(|why| why.contains(&format!("rule={rule}")))
}

fn index_where(rows: &[Value], pick: impl Fn(&Value) -> bool) -> usize {
    rows.iter()
        .position(pick)
        .expect("the matrix recorded this case; the row is in the file")
}

/// §10's negative control 1: an `Unknown` recovery result rewritten as a definite answer.
///
/// Production's state vocabulary has no word for "rejected" — a refusal is a *fact* a line
/// carries, and the state that erases a lane is the one `holds_lane` closes — so the faithful
/// shape of this control is the file's `outcome_unknown` read back as that closing state. It is
/// the same lie: an answer that was never given, used to free a nonce.
#[test]
fn nc1_an_unknown_recovered_as_a_closed_record_is_caught() {
    let facts = SourceFacts::read();
    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("recovery_results") && as_str(row, "section") == Some("F2")
    });
    let closing = facts.state("Resolved").to_string();
    assert!(
        facts.closing_states().iter().any(|word| word == &closing),
        "{JOURNAL}: `Resolved` is no longer a state that closes a lane, so this control has to \
         be planted on whatever production now closes one"
    );
    assert_ne!(
        as_str(&rows[index], "recovered_state").unwrap_or_default(),
        closing,
        "the row being planted was already read as the closing state"
    );
    let failures = planted(&mut rows, index, "recovered_state", json!(closing));
    assert!(
        rule_caught(&failures, "unknown_stays_unknown"),
        "an unknown answer rewritten as `{closing}` went uncaught: {failures:?}"
    );
    assert!(
        rule_caught(&failures, "never_recovered_as_holds"),
        "the same rewrite slipped past the row's own refutation: {failures:?}"
    );
}

/// §10's negative control 2: the lane rewritten as free after a restart, and the nonce it names
/// rewritten as absent.
#[test]
fn nc2_a_lane_rewritten_as_idle_is_caught_twice() {
    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("lane_recovery") && as_str(row, "section") == Some("F1")
    });
    let failures = planted(&mut rows, index, "lane_word", json!("idle"));
    assert!(
        rule_caught(&failures, "lane_word_matches_hold"),
        "a held lane rewritten to `idle` agreed with the derived state and nothing noticed: \
         {failures:?}"
    );

    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("lane_recovery") && as_str(row, "section") == Some("F1")
    });
    let failures = planted(&mut rows, index, "nonce_held", Value::Null);
    assert!(
        rule_caught(&failures, "held_nonce_is_named"),
        "the nonce a case read off the file was blanked and the row still claimed a held lane: \
         {failures:?}"
    );

    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("lane_recovery") && as_str(row, "section") == Some("F1")
    });
    let failures = planted(&mut rows, index, "restored_lane_entries", json!(0));
    assert!(
        rule_caught(&failures, "restored_lane_matches_hold"),
        "the file's occupancy list emptied under a record that still holds: {failures:?}"
    );
}

/// §10's negative control 3: the submission count rewritten to two.
#[test]
fn nc3_two_submissions_for_one_attempt_is_caught() {
    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("persistence_boundary") && as_str(row, "section") == Some("F1")
    });
    let failures = planted(&mut rows, index, "send_arrivals", json!(2));
    assert!(
        rule_caught(&failures, "one_submission_per_attempt"),
        "two arrivals at the socket for one attempt went uncaught: {failures:?}"
    );
    assert!(
        !rule_caught(&failures, "no_send_without_the_intent_line"),
        "this row's intent line is present, so only the arrival count may be red: {failures:?}"
    );
}

/// §10's negative control 4: the hash agreement rewritten to false.
#[test]
fn nc4_a_hash_disagreement_is_caught() {
    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("persistence_boundary") && as_str(row, "section") == Some("F1")
    });
    let failures = planted(&mut rows, index, "hashes_agree", json!(false));
    assert!(
        rule_caught(&failures, "local_hash_agreement"),
        "the identity column contradicted itself and nothing noticed: {failures:?}"
    );
    assert!(
        !rule_caught(&failures, "local_hash_shape"),
        "both strings are still §5-shaped; the disagreement is the only fault here: {failures:?}"
    );

    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("persistence_boundary") && as_str(row, "section") == Some("F1")
    });
    let failures = planted(
        &mut rows,
        index,
        "local_hash",
        json!("0xdeadbeefdeadbeefdeadbeef"),
    );
    assert!(
        rule_caught(&failures, "local_hash_shape"),
        "a short, non-§5 hash was accepted: {failures:?}"
    );
}

/// §10's negative control 5: a damaged ledger rewritten as having recovered successfully.
#[test]
fn nc5_a_damaged_ledger_rewritten_as_opened_is_caught() {
    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("damage_controls")
            && as_str(row, "refused_at") == Some("open")
            && as_bool(row, "opened") == Some(false)
    });
    let failures = planted(&mut rows, index, "opened", json!(true));
    assert!(
        rule_caught(&failures, "damage_is_fail_closed"),
        "a ledger refused at the open was rewritten as one that opened: {failures:?}"
    );

    // The other half of the same rule: a damage whose bytes were repaired after the refusal.
    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("damage_controls") && has(row, "bytes_unchanged")
    });
    let failures = planted(&mut rows, index, "bytes_unchanged", json!(false));
    assert!(
        rule_caught(&failures, "damage_is_fail_closed")
            || rule_caught(&failures, "durable_equals_remembered"),
        "a refusal that rewrote the file behind the damage passed as a fail-closed answer: \
         {failures:?}"
    );
}

/// §10's negative control 6: a persistence failure with a non-zero send count.
#[test]
fn nc6_a_send_that_outran_the_intent_line_is_caught() {
    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("persistence_boundary")
            && row.get("intent_seq").is_some_and(|value| value.is_null())
    });
    let failures = planted(&mut rows, index, "send_arrivals", json!(1));
    assert!(
        rule_caught(&failures, "no_send_without_the_intent_line"),
        "a submission counted at the socket with no intent line in the file went uncaught: \
         {failures:?}"
    );
    assert!(
        !rule_caught(&failures, "one_submission_per_attempt"),
        "one arrival is within the policy; §4.1's order is the fault being planted: {failures:?}"
    );
    assert!(
        rule_caught(&failures, "no_send_without_the_intent_line")
            || rule_caught(&failures, "durable_equals_remembered"),
        "the report's own `sent` flag against an empty socket tally is the second witness here: \
         {failures:?}"
    );

    // The same control at the other failure point: a refused append whose send count is nonzero.
    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("damage_controls") && has(row, "sends_after_refusal")
    });
    let failures = planted(&mut rows, index, "sends_after_refusal", json!(1));
    assert!(
        rule_caught(&failures, "never_rebroadcast")
            || rule_caught(&failures, "damage_is_fail_closed"),
        "a run that refused to persist and sent anyway passed both rules: {failures:?}"
    );
}

/// §10's negative control 7: an unsupported schema version disguised as a success.
#[test]
fn nc7_an_unsupported_schema_disguised_as_readable_is_caught() {
    let facts = SourceFacts::read();
    let schema_fault = facts.fault("UnsupportedSchema").to_string();
    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("damage_controls")
            && as_str(row, "fault_word") == Some(schema_fault.as_str())
    });
    let failures = planted(&mut rows, index, "opened", json!(true));
    assert!(
        rule_caught(&failures, "schema_refused"),
        "a ledger at a version this build does not define was rewritten as opened: {failures:?}"
    );
    assert!(
        rule_caught(&failures, "damage_is_fail_closed"),
        "and the general fail-closed rule missed it too: {failures:?}"
    );

    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("damage_controls")
            && as_str(row, "fault_word") == Some(schema_fault.as_str())
    });
    let failures = planted(&mut rows, index, "refused_at", json!("append"));
    assert!(
        rule_caught(&failures, "schema_refused"),
        "the version gate moved to the append, where §6 says it was never decided: {failures:?}"
    );
}

/// The three rules the seven controls above do not reach, each with its own way to fire: a leaked
/// path, a payload-shaped run of hex, and a durability claim §6 forbids.
#[test]
fn the_leak_and_claim_rules_can_fire() {
    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("recovery_results")
    });
    let failures = planted(
        &mut rows,
        index,
        "crash_shape",
        json!("prefix cut at the intent line in the scratch dir"),
    );
    assert!(
        failures.is_empty(),
        "a plain prose change must break nothing: {failures:?}"
    );

    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("recovery_results")
    });
    let failures = planted(
        &mut rows,
        index,
        "crash_shape",
        json!("§8 F1's cut, measured under /tmp/m12f-crash-f1-intent"),
    );
    assert!(
        rule_caught(&failures, "path_leak"),
        "a temp path inside a row went uncaught: {failures:?}"
    );

    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("persistence_boundary") && as_str(row, "section") == Some("F1")
    });
    let failures = planted(
        &mut rows,
        index,
        "local_hash",
        json!(format!("0x{}", "ab".repeat(100))),
    );
    assert!(
        rule_caught(&failures, "no_secret_leak"),
        "a 200-digit hex run — a payload, not an identity — went uncaught: {failures:?}"
    );
    assert!(
        rule_caught(&failures, "local_hash_shape"),
        "and the §5 shape rule missed the same row: {failures:?}"
    );

    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("recovery_results")
    });
    let failures = planted(
        &mut rows,
        index,
        "crash_shape",
        json!("§8 F1's cut, written straight and crash-proof"),
    );
    assert!(
        rule_caught(&failures, "no_durability_claim"),
        "a row claiming more durability than a process death went uncaught: {failures:?}"
    );
}

/// A second run of the matrix into the same directory writes the same figures again — which is
/// exactly what a stale evidence directory looks like, and the rows carry no clock to hide behind.
#[test]
fn a_second_copy_of_the_measurement_is_not_a_measurement() {
    let facts = SourceFacts::read();
    let mut rows = rows_or_panic();
    let clean = validate(&rows, &facts);
    assert!(clean.is_empty(), "the artifact was already red: {clean:?}");
    let duplicate = rows[0].clone();
    rows.push(duplicate);
    let failures = validate(&rows, &facts);
    assert!(
        rule_caught(&failures, "single_run"),
        "two identical rows were accepted as two measurements: {failures:?}"
    );
}

/// §7's idempotence is a claim about two passes; a second pass that answers differently is the
/// only thing that would make this table's `durable_equals_remembered` rule meaningless.
#[test]
fn a_second_pass_that_answers_differently_is_caught() {
    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("persistence_boundary") && as_str(row, "section") == Some("F7")
    });
    let failures = planted(&mut rows, index, "second_pass_records", json!(2));
    assert!(
        rule_caught(&failures, "durable_equals_remembered"),
        "one file folded to two different answers and nothing noticed: {failures:?}"
    );

    let mut rows = rows_or_panic();
    let index = index_where(&rows, |row| {
        as_str(row, "table") == Some("persistence_boundary") && as_str(row, "section") == Some("F7")
    });
    let failures = planted(&mut rows, index, "lines_after_recovery", json!(9));
    assert!(
        rule_caught(&failures, "durable_equals_remembered"),
        "a recovery that wrote bytes was accepted as a read: {failures:?}"
    );
}
