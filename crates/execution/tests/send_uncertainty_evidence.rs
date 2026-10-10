//! M12-E §11: the gate over the send-uncertainty evidence.
//!
//! The measurement itself lives in `send_uncertainty.rs`: sixteen cases against a stub socket,
//! each of which appends one row per thing it observed to `measured-rows.jsonl` when — and only
//! when — `M12E_EVIDENCE_DIR` names a directory. This file never measures. It reads those rows
//! back and asks five questions of them:
//!
//! ```text
//! are the figures internally consistent      arrivals, counts, status words, lane releases
//! does every row name a test that exists     and does every test in the matrix have a row
//! does every classification still hold       recomputed against production's own refusal list
//! are the published tables those rows        re-assembled, byte for byte
//! is the send path still the one-shot entry  by anchor, in the source, right now
//! ```
//!
//! Why the raw rows are committed alongside the tables: the tables are a pure function of the
//! rows, so a reader who suspects the tables can recompute them. The rows are what the stub
//! sockets actually reported on one serial run of the matrix; nothing here re-runs the matrix,
//! and nothing here could make a socket say something it did not say. Each row's `case` is the
//! name of the `#[test]` function that wrote it, which is re-resolved against the matrix's
//! source, so a row that outlives its test — or a test that quietly stopped recording — fails.
//!
//! What this evidence is not, stated once here because §7 forbids claiming more: every row was
//! measured against a `std::net::TcpListener` on loopback that this test process spawned. No
//! node was deployed, no real RPC or Flashblocks endpoint was contacted, no transaction was
//! signed with a real key or broadcast, and no row says anything about a block. `submitted`
//! means the endpoint acknowledged the bytes, which is not `Mined`, not `Confirmed`, and not
//! profit; and the local hash a row tracks is this process's own, which does not survive a
//! process restart (M12-D §13.2's ledger gap stays open).
//!
//! Refresh, when the matrix legitimately changes:
//!
//! ```text
//! M12E_SEND_UNCERTAINTY_REFRESH=1 cargo test -p evm-execution --test send_uncertainty_evidence
//! ```
//!
//! which re-assembles `request-counts.json` and `classification.json` from the rows that are
//! already in `measured-rows.jsonl`. It writes no new figures: it can only publish what the
//! matrix recorded.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

const ROWS_FILE: &str = "data/evidence/m12/e/measured-rows.jsonl";
const COUNTS_FILE: &str = "data/evidence/m12/e/request-counts.json";
const CLASSIFICATION_FILE: &str = "data/evidence/m12/e/classification.json";
const MATRIX: &str = "crates/execution/tests/send_uncertainty.rs";
const RPC: &str = "crates/chain/src/rpc.rs";
const DIRECT: &str = "crates/execution/src/giwa/sequencer_direct.rs";
const SUBMITTER: &str = "crates/execution/src/submitter.rs";

const SEND_METHOD: &str = "eth_sendRawTransaction";

const TABLES: [&str; 3] = ["request_counts", "classification", "trace_accounting"];
const SECTIONS: [&str; 6] = ["7A", "7B", "7C", "7D", "9.6", "9.7"];
const STATUS_WORDS: [&str; 3] = ["submitted", "rejected", "unknown"];
const GRADED_AT: [&str; 3] = ["wire", "classifier", "payload reader"];

/// §4 names these four answers as the ones that must not be read as "definitely not sent".
/// They are the task book's words, quoted here so the table has to contain them; the rule that
/// does the actual work is [`SourceFacts::is_definite_refusal`], which asks production.
const NEVER_ABSOLUTION: [&str; 4] = [
    "already known",
    "known transaction",
    "nonce too low",
    "replacement transaction underpriced",
];

/// The endpoint digest is the only shape in which a row may name a socket.
const DIGEST_PREFIX: &str = "rpc-";
const DIGEST_HEX: usize = 16;

/// A credential, a host, or a port, in any of the shapes this repository's stubs use. Every
/// needle here measured zero occurrences in the committed rows; the two words that do appear
/// (`nonce`, `sender`) are the node's own vocabulary, not secrets, and are not needles.
const LEAK_NEEDLES: [&str; 11] = [
    "http://",
    "https://",
    "://",
    "127.0.0.1",
    "localhost",
    "abc123",
    "Bearer",
    "api_key",
    "apikey",
    "private_key",
    "giwa.io",
];

// ---------------------------------------------------------------------------
// reading the artifact and the source it is measured against
// ---------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_text(relative: &str) -> String {
    let path = workspace_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The production region of a source file: everything above its inline test module, which is
/// the cut every other gate in this repository uses so an anchor cannot be satisfied by a line
/// that only exists inside a test.
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
             M12E_EVIDENCE_DIR=<absolute path to {}/data/evidence/m12/e> \
             cargo test -p evm-execution --test send_uncertainty -- --test-threads=1",
            workspace_root().display()
        );
    }
    measured_rows()
}

/// The integers production declares for the two wire policies, read off the source rather than
/// typed: `Once` is what a send must cost, `RetryOnce` is what a read still costs.
fn policy_attempts(arm: &str) -> usize {
    let lines = production_lines(RPC);
    let hits = lines
        .iter()
        .filter(|line| line.contains(arm))
        .collect::<Vec<_>>();
    let [one] = hits.as_slice() else {
        panic!(
            "{RPC}: {} lines contain `{arm}`; the attempt loop must be one place",
            hits.len()
        );
    };
    one.split("=>")
        .last()
        .and_then(|tail| tail.trim().trim_end_matches(',').parse::<usize>().ok())
        .unwrap_or_else(|| panic!("{RPC}: no attempt count after `{arm}` in `{one}`"))
}

/// JSON-RPC's `method not found`, as the production constant spells it. Parsing the number out
/// of the source is what keeps this gate from hard-coding a code the classifier no longer uses.
fn method_not_found_code() -> i64 {
    let lines = production_lines(DIRECT);
    let hits = lines
        .iter()
        .filter(|line| line.contains("const METHOD_NOT_FOUND: i64 ="))
        .collect::<Vec<_>>();
    let [one] = hits.as_slice() else {
        panic!("{DIRECT}: {} lines declare METHOD_NOT_FOUND", hits.len());
    };
    one.split('=')
        .next_back()
        .and_then(|tail| tail.trim().trim_end_matches(';').parse::<i64>().ok())
        .unwrap_or_else(|| panic!("{DIRECT}: no number after `=` in `{one}`"))
}

/// The messages production reads as a definite refusal, extracted from `DEFINITE_REFUSALS` as
/// the first string literal of each tuple. The declared arity in the array's type is parsed too
/// and asserted against the count, so a list that changed shape cannot pass this quietly.
fn definite_refusal_messages() -> Vec<String> {
    let text = read_text(DIRECT);
    let start = text
        .find("const DEFINITE_REFUSALS")
        .unwrap_or_else(|| panic!("{DIRECT}: DEFINITE_REFUSALS is gone"));
    let block = &text[start..];
    let open = block
        .find("= [")
        .unwrap_or_else(|| panic!("{DIRECT}: DEFINITE_REFUSALS does not open with `= [`"));
    let header = &block[..open];
    let body = &block[open + 3..];
    let end = body
        .find("];")
        .unwrap_or_else(|| panic!("{DIRECT}: DEFINITE_REFUSALS does not close"));
    let body = &body[..end];

    let declared = header
        .split(';')
        .next_back()
        .and_then(|tail| tail.split(']').next())
        .and_then(|inner| inner.trim().parse::<usize>().ok())
        .unwrap_or_else(|| panic!("{DIRECT}: cannot read the declared length from `{header}`"));

    // Only the literals inside a tuple count, and only the first of each tuple: the second is a
    // reason, and it is production's prose rather than a message the node sends. Braces inside a
    // string are ignored because the scanner only counts depth outside one.
    let mut out: Vec<String> = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut current = String::new();
    let mut tuple_taken = false;
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if in_string {
            if c == '\\' {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
                continue;
            }
            if c == '"' {
                if depth == 1 && !tuple_taken {
                    out.push(std::mem::take(&mut current));
                    tuple_taken = true;
                } else {
                    current.clear();
                }
                in_string = false;
                continue;
            }
            current.push(c);
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                current.clear();
            }
            '(' => {
                depth += 1;
                if depth == 1 {
                    tuple_taken = false;
                }
            }
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    assert_eq!(
        out.len(),
        declared,
        "{DIRECT}: DEFINITE_REFUSALS declares {declared} entries and this reader found {}; the \
         reader and the list have drifted apart",
        out.len()
    );
    for message in &out {
        assert!(
            !message.contains('\n'),
            "{DIRECT}: `{message}` spans lines; the reader takes single-line messages"
        );
    }
    out
}

/// The names of the test functions in the matrix, in source order. A row's `case` must be one
/// of these, and each of these must appear in at least one row.
fn matrix_test_functions() -> BTreeSet<String> {
    let lines = read_text(MATRIX);
    let lines = lines.lines().collect::<Vec<_>>();
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
                .split('(')
                .next()
                .expect("a function signature has a parameter list");
            out.insert(name.to_string());
            break;
        }
    }
    out
}

/// What production currently says, gathered once so the row checks below compare the rows
/// against the code rather than against numbers this file typed.
struct SourceFacts {
    once_attempts: usize,
    retry_attempts: usize,
    method_not_found: i64,
    definite: Vec<String>,
    cases: BTreeSet<String>,
}

impl SourceFacts {
    fn read() -> Self {
        Self {
            once_attempts: policy_attempts("Self::Once =>"),
            retry_attempts: policy_attempts("Self::RetryOnce =>"),
            method_not_found: method_not_found_code(),
            definite: definite_refusal_messages(),
            cases: matrix_test_functions(),
        }
    }

    /// The classifier's decision, recomputed here: production calls a send refused outright
    /// when the code is its `method not found`, or when the message begins with one of its own
    /// payload-fact strings. Everything else is an answer about state, and says nothing.
    fn is_definite_refusal(&self, code: Option<i64>, message: &str) -> bool {
        code == Some(self.method_not_found)
            || self.definite.iter().any(|entry| {
                message
                    .to_ascii_lowercase()
                    .starts_with(&entry.to_ascii_lowercase())
            })
    }
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

/// A row's `code` may be JSON `null` (a payload that carried no number), which is not the same
/// as the field being absent: both mean "no code", and only one of them is a measured answer.
fn row_code(row: &Value) -> Option<i64> {
    match row.get("code") {
        Some(Value::Null) | None => None,
        Some(value) => value.as_i64(),
    }
}

fn where_of(row: &Value) -> String {
    format!(
        "{} / {}",
        as_str(row, "case").unwrap_or("<no case>"),
        as_str(row, "fault").unwrap_or("<no fault>")
    )
}

/// Every row's text, with each endpoint digest replaced by a fixed placeholder. Two rows that
/// are identical under this replacement are the same measurement written twice — which is what
/// happens if the matrix is run into a directory that already holds a run.
fn normalized(row: &Value) -> String {
    let text = serde_json::to_string(row).expect("a row is json");
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    loop {
        let Some(offset) = rest.find(DIGEST_PREFIX) else {
            out.push_str(rest);
            return out;
        };
        let (before, after_prefix) = (&rest[..offset], &rest[offset + DIGEST_PREFIX.len()..]);
        out.push_str(before);
        let hex_len = after_prefix
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .count();
        if hex_len == DIGEST_HEX {
            out.push_str(DIGEST_PREFIX);
            out.push_str("DIGEST");
            // hex digits are ASCII, so this slice lands on a character boundary
            rest = &after_prefix[hex_len..];
        } else {
            out.push_str(DIGEST_PREFIX);
            rest = after_prefix;
        }
    }
}

/// A digest-shaped token is the only way a row may name a socket, so every `rpc-` in the text
/// has to be followed by exactly [`DIGEST_HEX`] hex digits and then a non-hex character.
fn digests_are_exactly_hex(text: &str) -> Vec<String> {
    let mut bad = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while let Some(offset) = text[index..].find(DIGEST_PREFIX).map(|found| index + found) {
        let start = offset + DIGEST_PREFIX.len();
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
            end += 1;
        }
        if end - start != DIGEST_HEX {
            bad.push(format!(
                "`{}` ({} hex digits)",
                &text[offset..end.min(text.len())],
                end - start
            ));
        }
        index = end;
    }
    bad
}

/// The whole gate in one call: returns every rule the rows break, and nothing when they hold.
/// Kept free of assertions so the negative controls can plant a fault and check which rule
/// catches it.
fn validate(rows: &[Value], facts: &SourceFacts) -> Vec<String> {
    let mut bad: Vec<String> = Vec::new();

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut cases_seen: BTreeSet<String> = BTreeSet::new();

    for row in rows.iter() {
        let at = where_of(row);

        // ---- row_shape, case_is_a_test, fault_names_the_book, section_domain
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
                        format!("unknown section `{section}`"),
                    );
                }
                if !facts.cases.contains(case) {
                    push(
                        &mut bad,
                        "case_is_a_test",
                        &at,
                        format!("`{case}` is not a test function in {MATRIX}"),
                    );
                }
                if !fault.starts_with('§') {
                    push(
                        &mut bad,
                        "fault_names_the_book",
                        &at,
                        format!("`{fault}` quotes no section of the task book"),
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

        if let Some(graded) = as_str(row, "graded_at") {
            if !GRADED_AT.contains(&graded) {
                push(
                    &mut bad,
                    "row_shape",
                    &at,
                    format!("unknown graded_at `{graded}`"),
                );
            }
        }

        // ---- arrival_arithmetic: the socket's own tally has to add up
        if let Some(methods) = row.get("methods").and_then(Value::as_array) {
            let send_count = methods
                .iter()
                .filter(|method| method.as_str() == Some(SEND_METHOD))
                .count() as u64;
            if as_u64(row, "total_arrivals") != Some(methods.len() as u64) {
                push(
                    &mut bad,
                    "arrival_arithmetic",
                    &at,
                    format!(
                        "total_arrivals {:?} while {} methods arrived",
                        row.get("total_arrivals"),
                        methods.len()
                    ),
                );
            }
            if as_u64(row, "send_arrivals") != Some(send_count) {
                push(
                    &mut bad,
                    "arrival_arithmetic",
                    &at,
                    format!(
                        "send_arrivals {:?} while {send_count} of the recorded methods are `{SEND_METHOD}`",
                        row.get("send_arrivals")
                    ),
                );
            }
        }
        if let Some(order) = row.get("arrivals_in_order").and_then(Value::as_array) {
            if as_u64(row, "total_arrivals") != Some(order.len() as u64) {
                push(
                    &mut bad,
                    "arrival_arithmetic",
                    &at,
                    format!("total_arrivals in an {}-arrival order", order.len()),
                );
            }
        }

        // ---- one_post_per_answer: an answered send costs exactly the Once policy
        let status = as_str(row, "status_word");
        if let Some(status) = status {
            if !STATUS_WORDS.contains(&status) {
                push(
                    &mut bad,
                    "row_shape",
                    &at,
                    format!("unknown status `{status}`"),
                );
            }
            if as_u64(row, "send_arrivals") != Some(facts.once_attempts as u64) {
                push(
                    &mut bad,
                    "one_post_per_answer",
                    &at,
                    format!(
                        "a send answered as `{status}` after {:?} POSTs; the Once policy says {}",
                        row.get("send_arrivals"),
                        facts.once_attempts
                    ),
                );
            }
        }

        // ---- read_attempts_follow_the_read_policy: §3 leaves the read loop alone
        if let Some(read) = as_u64(row, "read_arrivals") {
            if read != facts.retry_attempts as u64 {
                push(
                    &mut bad,
                    "read_attempts_follow_the_read_policy",
                    &at,
                    format!(
                        "a read answered {read} times; production's RetryOnce policy is {} attempts",
                        facts.retry_attempts
                    ),
                );
            }
        }

        // ---- in_flight_word, lane_release_word
        if let Some(proven) = row.get("proven_not_in_flight").and_then(Value::as_bool) {
            let expected = status == Some("rejected");
            if proven != expected {
                push(
                    &mut bad,
                    "in_flight_word",
                    &at,
                    format!("proven_not_in_flight `{proven}` on a `{status:?}` answer"),
                );
            }
        }
        if let Some(release) = as_str(row, "lane_release") {
            match (release, status) {
                ("released", Some("rejected")) => {}
                ("held", Some("unknown")) | ("held", Some("submitted")) => {}
                (other, word) => push(
                    &mut bad,
                    "lane_release_word",
                    &at,
                    format!("lane `{other}` on a `{word:?}` answer"),
                ),
            }
        }

        // ---- local_hash_shape: §5's rule that a mismatch never becomes a normal success
        if let Some(hash) = as_str(row, "local_hash") {
            let hex = hash.strip_prefix("0x").unwrap_or(hash);
            if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                push(
                    &mut bad,
                    "local_hash_shape",
                    &at,
                    format!("`{hash}` is not a 32-byte hash"),
                );
            }
        }
        if row.get("hashes_agree").and_then(Value::as_bool) == Some(false)
            && status == Some("submitted")
        {
            push(
                &mut bad,
                "local_hash_shape",
                &at,
                "a hash this process did not produce was answered as `submitted`",
            );
        }
        if row
            .get("tracked_hash_equals_local")
            .and_then(Value::as_bool)
            == Some(false)
        {
            push(
                &mut bad,
                "local_hash_shape",
                &at,
                "the outcome stopped tracking the local hash",
            );
        }

        // ---- classifier_recomputed, classification_reading
        let message = as_str(row, "message");
        let code = row_code(row);
        if let Some(message) = message {
            let definite = facts.is_definite_refusal(code, message);
            if let Some(status) = status {
                let expected = if definite { "rejected" } else { "unknown" };
                if status != expected {
                    push(
                        &mut bad,
                        "classifier_recomputed",
                        &at,
                        format!(
                            "code {:?} message `{message}` classifies as `{expected}`, not `{status}`",
                            code
                        ),
                    );
                }
            }
            if let Some(read) = as_str(row, "refusal_read") {
                let expected = if definite { "definite" } else { "uncertain" };
                if read != expected {
                    push(
                        &mut bad,
                        "classification_reading",
                        &at,
                        format!(
                            "code {:?} message `{message}` reads as `{expected}`, not `{read}`",
                            code
                        ),
                    );
                }
            }
            // ---- never_absolved: §4's four words must never be an acquittal
            if NEVER_ABSOLUTION.contains(&message)
                && (status == Some("rejected") || as_str(row, "lane_release") == Some("released"))
            {
                push(
                    &mut bad,
                    "never_absolved",
                    &at,
                    format!("`{message}` was read as proof the bytes are not in flight"),
                );
            }
        }

        // ---- trace_matches_socket: §9 item 7, the trace may not invent or drop an attempt
        if let Some(trace_send) = as_u64(row, "trace_send_attempts") {
            if Some(trace_send) != as_u64(row, "socket_send_arrivals") {
                push(
                    &mut bad,
                    "trace_matches_socket",
                    &at,
                    format!(
                        "the trace counted {trace_send} sends against {:?} at the socket",
                        row.get("socket_send_arrivals")
                    ),
                );
            }
        }
        if let Some(trace_receipt) = as_u64(row, "trace_receipt_attempts") {
            if Some(trace_receipt) != as_u64(row, "socket_receipt_arrivals") {
                push(
                    &mut bad,
                    "trace_matches_socket",
                    &at,
                    format!(
                        "the trace counted {trace_receipt} receipts against {:?} at the socket",
                        row.get("socket_receipt_arrivals")
                    ),
                );
            }
        }
        if let Some(trace_total) = as_u64(row, "trace_total_attempts") {
            if Some(trace_total) != as_u64(row, "total_arrivals") {
                push(
                    &mut bad,
                    "trace_matches_socket",
                    &at,
                    format!(
                        "the trace counted {trace_total} attempts against {:?} arrivals",
                        row.get("total_arrivals")
                    ),
                );
            }
        }
        if let Some(dropped) = as_u64(row, "dropped_events") {
            if dropped != 0 {
                push(
                    &mut bad,
                    "trace_matches_socket",
                    &at,
                    format!("{dropped} trace events were dropped, so the counts are not the run's"),
                );
            }
        }

        // ---- no_endpoint_leak, over the row's whole text
        let text = serde_json::to_string(row).expect("a row is json");
        let lower = text.to_lowercase();
        for needle in LEAK_NEEDLES {
            if lower.contains(&needle.to_lowercase()) {
                push(
                    &mut bad,
                    "no_endpoint_leak",
                    &at,
                    format!("the row carries `{needle}`"),
                );
            }
        }
        for wrong in digests_are_exactly_hex(&text) {
            push(
                &mut bad,
                "no_endpoint_leak",
                &at,
                format!("an endpoint name that is not a 16-hex digest: {wrong}"),
            );
        }

        // ---- single_run
        let key = normalized(row);
        if !seen.insert(key) {
            push(
                &mut bad,
                "single_run",
                &at,
                "an identical row is already in the file, so two runs share one artifact",
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

    // ---- definite_list_covered: production's list, entry by entry
    for entry in &facts.definite {
        let covered = rows.iter().any(|row| {
            as_str(row, "refusal_read") == Some("definite")
                && as_str(row, "message").is_some_and(|message| {
                    message
                        .to_ascii_lowercase()
                        .starts_with(&entry.to_ascii_lowercase())
                })
        });
        if !covered {
            push(
                &mut bad,
                "definite_list_covered",
                entry,
                "no row asked the classifier about this refusal, so the table does not show it",
            );
        }
    }

    bad
}

// ---------------------------------------------------------------------------
// the two published tables, as a pure function of the rows
// ---------------------------------------------------------------------------

fn counts_of(rows: &[Value], key: &str, values: &[&str]) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for value in values {
        let count = rows
            .iter()
            .filter(|row| as_str(row, key) == Some(value))
            .count() as u64;
        out.insert((*value).to_string(), count);
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

fn maximum(rows: &[Value], key: &str) -> Value {
    rows.iter()
        .filter_map(|row| as_u64(row, key))
        .max()
        .map(|max| json!(max))
        .unwrap_or(Value::Null)
}

fn row_count(rows: &[Value], table: &str) -> Vec<Value> {
    rows.iter()
        .filter(|row| as_str(row, "table") == Some(table))
        .cloned()
        .collect()
}

/// The anchors each table cites, with the line production currently puts them on. Line numbers
/// are display only — the gate re-resolves `(file, token)`, and [`anchor_arity_is_the_claim`]
/// is what that means for a deleted mechanism.
const ANCHORS: [(&str, &str, &str); 12] = [
    (
        RPC,
        "Self::Once =>",
        "the send policy allows exactly one attempt",
    ),
    (
        RPC,
        "Self::RetryOnce =>",
        "the read policy still allows two; §3 did not touch it",
    ),
    (
        RPC,
        "pub async fn request_raw_once",
        "the one-shot path is a named call, not a method-string match",
    ),
    (
        DIRECT,
        ".request_raw_once(\"eth_sendRawTransaction\", json!([payload]))",
        "the only production submission goes through it",
    ),
    (
        DIRECT,
        "const DEFINITE_REFUSALS",
        "the closed list of payload facts a refusal may be read from",
    ),
    (
        DIRECT,
        "const METHOD_NOT_FOUND",
        "the one code with its own arm in the classifier",
    ),
    (
        DIRECT,
        "pub fn read_send_refusal",
        "the classifier is a function a test can call without a socket",
    ),
    (
        DIRECT,
        "pub fn parse_rpc_error",
        "code and message come back out of the transport's own string",
    ),
    (
        DIRECT,
        "pub fn without_the_endpoint",
        "a transport line loses the URL before it reaches an answer",
    ),
    (
        DIRECT,
        "pub fn submission_provenance",
        "an answer names its socket by digest, never by URL",
    ),
    (
        SUBMITTER,
        "pub fn proven_not_in_flight",
        "the lane may go back only on a proof, which is `Rejected` alone",
    ),
    (
        SUBMITTER,
        "pub fn tracked_hash",
        "the local hash is what an outcome carries forward",
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

/// §9 item 2, in source rather than in a table: every production call site that passes the
/// submission method as a parameter must pass it to the one-shot entry. A line that starts
/// `("eth_sendRawTransaction"` is an argument list, so a doc comment or a `detail:` string
/// cannot be mistaken for one.
fn submission_call_sites() -> Vec<String> {
    let mut sites = Vec::new();
    for dir in std::fs::read_dir(workspace_root().join("crates"))
        .expect("the workspace has crates")
        .flatten()
    {
        let source = dir.path().join("src");
        if !source.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        collect_rs(&source, &mut files);
        for path in files {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            for line in text
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
            {
                if line.contains("(\"eth_sendRawTransaction\"") {
                    sites.push(format!("{} :: {}", path.display(), line.trim()));
                }
            }
        }
    }
    sites
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("a readable source directory") {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The two files §11 asks for, built from the rows and nothing else. Every document-level
/// figure is a fold over the rows, so a number in a table is a consequence of what was measured
/// rather than a second place the same number was typed.
fn assemble(rows: &[Value], facts: &SourceFacts) -> (Value, Value) {
    let counts_rows = {
        let mut out = row_count(rows, "request_counts");
        out.extend(row_count(rows, "trace_accounting"));
        out
    };
    let class_rows = row_count(rows, "classification");

    let answered = class_rows
        .iter()
        .filter(|row| row.get("status_word").is_some())
        .count() as u64;

    let counts = json!({
        "schema": "m12e-request-counts-v1",
        "task_book": "docs/v0.1/M12E Coding.md §7.A, §7.B, §7.D, §9 item 6, §9 item 7",
        "measured_by": MATRIX,
        "measured_from": ROWS_FILE,
        "what_this_table_is": "what a stub socket on loopback actually received, per case: the \
                              method of every arrival, how many of them were \
                              `eth_sendRawTransaction`, and what the client made of the answer \
                              it got. §7.A's rule is that the count comes from the socket, not \
                              from an assertion that a client function ran.",
        "what_this_table_is_not": "no row is a real endpoint. Nothing here says a transaction \
                                  entered a block, was confirmed, or made money, and the local \
                                  hash a row tracks lives in this process only — a restart \
                                  forgets it (M12-D §13.2).",
        "wire_policy": {
            "once_attempts": facts.once_attempts,
            "retry_once_attempts": facts.retry_attempts,
            "read_from": format!("{RPC} :: WirePolicy::attempts"),
        },
        "rows": counts_rows.len(),
        "cases": distinct(&counts_rows, "case").len(),
        "sections": counts_of(&counts_rows, "section", &SECTIONS),
        "rows_with_an_answered_send": answered_total(&counts_rows),
        "max_send_arrivals_in_any_case": maximum(&counts_rows, "send_arrivals"),
        "max_read_arrivals_in_any_case": maximum(&counts_rows, "read_arrivals"),
        "status_words": counts_of(&counts_rows, "status_word", &STATUS_WORDS),
        "table": counts_rows,
    });

    let classification = json!({
        "schema": "m12e-classification-v1",
        "task_book": "docs/v0.1/M12E Coding.md §4, §7.C, §9 item 4",
        "measured_by": MATRIX,
        "measured_from": ROWS_FILE,
        "what_this_table_is": "how this build reads a submission answer: which payloads it may \
                               call refused outright, which answers it must call uncertain, and \
                               what the lane did with each. Rows graded at the `wire` went \
                               through a stub socket; rows graded at the `classifier` are the \
                               same function asked directly, with no socket in the way.",
        "what_this_table_is_not": "a claim about any particular node's error strings. The six \
                                  messages graded definite are payload facts; the -32601 arm is \
                                  the whitelist refusal this repository's own M6 probe recorded. \
                                  Everything else is deliberately left unread, and the semantics \
                                  of those unconfirmed answers are recorded in the completion \
                                  report, not asserted here.",
        "definite_refusal_messages_from_production": facts.definite,
        "method_not_found_code": facts.method_not_found,
        "never_absolves": NEVER_ABSOLUTION,
        "rows": class_rows.len(),
        "cases": distinct(&class_rows, "case").len(),
        "sections": counts_of(&class_rows, "section", &SECTIONS),
        "graded_at": counts_of(&class_rows, "graded_at", &GRADED_AT),
        "refusal_reads": counts_of(&class_rows, "refusal_read", &["definite", "uncertain"]),
        "status_words": counts_of(&class_rows, "status_word", &STATUS_WORDS),
        "rows_with_an_answered_send": answered,
        "lane_releases": counts_of(&class_rows, "lane_release", &["released", "held"]),
        "max_send_arrivals_in_any_case": maximum(&class_rows, "send_arrivals"),
        "table": class_rows,
    });

    (
        with_anchors(counts, &counts_rows),
        with_anchors(classification, &class_rows),
    )
}

fn answered_total(rows: &[Value]) -> u64 {
    rows.iter()
        .filter(|row| row.get("status_word").is_some())
        .count() as u64
}

fn with_anchors(mut table: Value, rows: &[Value]) -> Value {
    let anchors = resolved_anchors();
    table["anchors"] = json!(anchors);
    table["anchor_rows"] = json!(anchors.len());
    table["rows_in_this_table"] = json!(rows.len());
    table
}

fn render(table: &Value) -> String {
    serde_json::to_string_pretty(table).expect("a table is json") + "\n"
}

/// A copy of the committed table with its anchors re-pointed at today's source, so the two
/// copies can be compared on everything except the line numbers they were assembled on.
fn re_anchored(committed: &Value) -> Value {
    let mut copy = committed.clone();
    copy["anchors"] = json!(resolved_anchors());
    copy
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
        "{ROWS_FILE} holds no rows; the matrix writes them only under M12E_EVIDENCE_DIR"
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

/// The classifier's list is production's, and the table has to say something about each entry —
/// otherwise a seventh definite refusal could join the code and the published table would keep
/// describing six.
#[test]
fn the_definite_refusal_list_is_production_s_and_the_rows_cover_it() {
    let facts = SourceFacts::read();
    assert!(
        !facts.definite.is_empty(),
        "{DIRECT}: the definite list read empty"
    );
    for entry in &facts.definite {
        assert!(
            !entry.contains("§") && !entry.starts_with(' '),
            "{DIRECT}: `{entry}` is not an error message the node would send"
        );
    }
    let failures = validate(&rows_or_panic(), &facts)
        .into_iter()
        .filter(|why| why.contains("rule=definite_list_covered"))
        .collect::<Vec<_>>();
    assert!(
        failures.is_empty(),
        "the rows do not cover production's list: {}",
        failures.join(", ")
    );
}

/// §9 items 1, 2 and 3 together, in source: the submission call sites, each of them the one-shot
/// entry, and the retrying entry never carrying the method.
#[test]
fn the_send_path_anchors_resolve_and_no_submission_takes_the_retrying_entry() {
    let sites = submission_call_sites();
    assert_eq!(
        sites.len(),
        1,
        "production passes `eth_sendRawTransaction` as a request parameter in {} places; §3 \
         allows exactly one, and these are they: {sites:?}",
        sites.len()
    );
    for site in &sites {
        assert!(
            site.contains("request_raw_once"),
            "a submission is on the retrying entry: {site}"
        );
    }
    for (file, token, why) in ANCHORS {
        let line = resolve_pair(file, token).unwrap_or_else(|error| panic!("{why} — {error}"));
        assert!(line >= 1, "{file}::{token} resolved to {line}");
    }
    // The retrying entry exists and is still what a read uses; §3's claim is the split, not the
    // removal, so this file must not be read as asking for one policy.
    assert!(
        production_lines(RPC)
            .iter()
            .any(|line| line.contains("WirePolicy::RetryOnce")),
        "{RPC}: no read path passes the retrying policy, so the split this milestone claimed \
         to make has one side missing"
    );
}

/// The published tables are the committed rows re-assembled, byte for byte. Line numbers in the
/// `anchors` array are the one exception, and only because they point into source that other
/// milestones keep editing: they are re-resolved on both copies by [`anchor_arity_is_the_claim`].
#[test]
fn the_published_tables_are_the_rows_re_assembled() {
    let facts = SourceFacts::read();
    let rows = rows_or_panic();
    let (counts, classification) = assemble(&rows, &facts);

    // Every row lands in exactly one table, and no table row came from outside the file.
    let counts_rows = counts["table"].as_array().expect("a table");
    let published = row_count(counts_rows, "request_counts").len()
        + row_count(counts_rows, "trace_accounting").len()
        + classification["table"].as_array().expect("a table").len();
    assert_eq!(
        published,
        rows.len(),
        "the two tables hold {published} of {} rows: a row would otherwise be unpublished",
        rows.len()
    );

    // Refreshing writes before anything is compared, so the missing-file case below is a real
    // failure rather than a instruction to run a different command. What it refuses to do is
    // publish rows that break a rule: a red measurement stays red on the page it is written to.
    if std::env::var("M12E_SEND_UNCERTAINTY_REFRESH").is_ok() {
        let failures = validate(&rows, &facts);
        assert!(
            failures.is_empty(),
            "refusing to publish tables assembled from rows that break a rule: {}",
            failures.join(", ")
        );
        for (path, table) in [
            (COUNTS_FILE, counts.clone()),
            (CLASSIFICATION_FILE, classification.clone()),
        ] {
            let full = workspace_root().join(path);
            std::fs::create_dir_all(full.parent().expect("a directory"))
                .expect("create the evidence directory");
            std::fs::write(&full, render(&table)).expect("write an evidence table");
        }
        panic!(
            "refreshed {COUNTS_FILE} and {CLASSIFICATION_FILE}; run this test again without \
             M12E_SEND_UNCERTAINTY_REFRESH"
        );
    }

    let mut read_back: Vec<Value> = Vec::new();
    for (path, table) in [
        (COUNTS_FILE, counts.clone()),
        (CLASSIFICATION_FILE, classification.clone()),
    ] {
        let full = workspace_root().join(path);
        if !full.exists() {
            panic!(
                "{path} is missing; assemble it with \
                 M12E_SEND_UNCERTAINTY_REFRESH=1 cargo test -p evm-execution \
                 --test send_uncertainty_evidence"
            );
        }
        let committed_text = read_text(path);
        let committed: Value =
            serde_json::from_str(&committed_text).unwrap_or_else(|e| panic!("{path}: {e}"));
        let committed = re_anchored(&committed);
        assert_eq!(
            render(&committed),
            render(&table),
            "{path} is not the rows re-assembled. Refresh it if the matrix changed; do not \
             edit this file by hand."
        );
        read_back.extend(
            committed["table"]
                .as_array()
                .expect("a table")
                .iter()
                .cloned(),
        );
    }

    // Two of the rules are claims about the *whole* measurement — every test wrote a row, and
    // every refusal on production's list was asked about — so they are checked against both
    // tables together. Either table on its own legitimately holds no row for the other's cases.
    let failures = validate(&read_back, &facts);
    assert!(
        failures.is_empty(),
        "the published tables, read back as one measurement, break a rule: {}",
        failures.join(", ")
    );
}

/// A table that no longer has a row for a rule it claims to demonstrate is a table about
/// nothing, so the controls below plant each fault in turn and name the rule that must catch it.
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

#[test]
fn nc1_a_second_post_at_the_socket_breaks_the_arrival_rules() {
    let mut rows = rows_or_panic();
    let index = rows
        .iter()
        .position(|row| as_str(row, "section") == Some("7B"))
        .expect("the §7.B case is in the rows");
    let failures = planted(&mut rows, index, "send_arrivals", json!(2));
    assert!(
        rule_caught(&failures, "arrival_arithmetic"),
        "two POSTs where the socket saw one went uncaught: {failures:?}"
    );
    assert!(
        rule_caught(&failures, "one_post_per_answer"),
        "an answered send costing more than the Once policy went uncaught: {failures:?}"
    );
}

#[test]
fn nc2_a_row_for_a_test_that_does_not_exist_is_rejected() {
    let mut rows = rows_or_panic();
    let failures = planted(
        &mut rows,
        0,
        "case",
        json!("a_test_someone_deleted_after_measuring"),
    );
    assert!(
        rule_caught(&failures, "case_is_a_test"),
        "a row naming a test that is not in the matrix went uncaught: {failures:?}"
    );
}

#[test]
fn nc3_a_test_that_stopped_recording_leaves_the_table_short() {
    let facts = SourceFacts::read();
    let rows = rows_or_panic();
    let kept = facts
        .cases
        .iter()
        .next()
        .expect("the matrix has at least one test");
    let dropped = rows
        .iter()
        .filter(|row| as_str(row, "case") != Some(kept.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let failures = validate(&dropped, &facts);
    assert!(
        rule_caught(&failures, "every_test_recorded"),
        "{kept} wrote no rows and the gate still passed: {failures:?}"
    );
}

/// §4's rule as a fault: a pool answer read as an acquittal. Both the status word and the lane's
/// copy of it are flipped, so the only rule left standing between this row and the artifact is
/// the classifier itself.
#[test]
fn nc4_a_pool_answer_marked_refused_is_caught_twice() {
    let mut rows = rows_or_panic();
    let index = rows
        .iter()
        .position(|row| {
            as_str(row, "message") == Some("nonce too low") && row.get("code").is_some()
        })
        .expect("a `nonce too low` row");
    rows[index]["status_word"] = json!("rejected");
    if rows[index].get("lane_release").is_some() {
        rows[index]["lane_release"] = json!("released");
    }
    rows[index]["proven_not_in_flight"] = json!(true);
    let failures = validate(&rows, &SourceFacts::read());
    assert!(
        rule_caught(&failures, "never_absolved"),
        "`nonce too low` was read as proof of nothing in flight: {failures:?}"
    );
    assert!(
        rule_caught(&failures, "classifier_recomputed"),
        "the row disagreed with production's classifier and nothing noticed: {failures:?}"
    );
}

#[test]
fn nc5_a_lane_released_on_an_unknown_answer_is_caught() {
    let mut rows = rows_or_panic();
    let index = rows
        .iter()
        .position(|row| as_str(row, "lane_release") == Some("held"))
        .expect("a held lane");
    let failures = planted(&mut rows, index, "lane_release", json!("released"));
    assert!(
        rule_caught(&failures, "lane_release_word"),
        "a held lane rewritten to released went uncaught: {failures:?}"
    );
}

#[test]
fn nc6_a_row_that_carries_the_endpoint_url_is_not_evidence() {
    let mut rows = rows_or_panic();
    let failures = planted(
        &mut rows,
        0,
        "fault",
        json!("§7.A case 4, against http://127.0.0.1:8545/token/abc123"),
    );
    assert!(
        rule_caught(&failures, "no_endpoint_leak"),
        "a URL and a path credential inside a row went uncaught: {failures:?}"
    );
}

#[test]
fn nc7_a_trace_that_disagrees_with_the_socket_is_caught() {
    let mut rows = rows_or_panic();
    let index = rows
        .iter()
        .position(|row| as_str(row, "table") == Some("trace_accounting"))
        .expect("the §9 item 7 trace row");
    let failures = planted(&mut rows, index, "trace_send_attempts", json!(2));
    assert!(
        rule_caught(&failures, "trace_matches_socket"),
        "a trace counting an attempt the socket never saw went uncaught: {failures:?}"
    );
    let mut rows = rows_or_panic();
    let failures = planted(&mut rows, index, "dropped_events", json!(1));
    assert!(
        rule_caught(&failures, "trace_matches_socket"),
        "a trace that dropped an event is not the run's account: {failures:?}"
    );
}

#[test]
fn nc8_the_same_run_recorded_twice_is_not_two_measurements() {
    let mut rows = rows_or_panic();
    let facts = SourceFacts::read();
    let clean = validate(&rows, &facts);
    assert!(clean.is_empty(), "the artifact was already red: {clean:?}");
    // A second run of the matrix writes the same figures and differs in one place only: the
    // port behind the endpoint digest. `record` appends, so that is exactly what a stale
    // evidence directory produces.
    let text = serde_json::to_string(&rows[0]).expect("json");
    let offset = text
        .find(DIGEST_PREFIX)
        .expect("the first row names its socket by digest");
    let start = offset + DIGEST_PREFIX.len();
    let mut swapped = text.clone();
    swapped.replace_range(start..start + DIGEST_HEX, "0123456789abcdef");
    assert_ne!(swapped, text, "the planted digest has to differ");
    let duplicate: Value = serde_json::from_str(&swapped).expect("the patched row is json");
    rows.push(duplicate);
    let failures = validate(&rows, &facts);
    assert!(
        rule_caught(&failures, "single_run"),
        "a second copy of a case, differing only in its socket's port, was accepted: {failures:?}",
    );
}

#[test]
fn nc9_a_row_read_back_out_of_a_table_is_still_a_row() {
    let facts = SourceFacts::read();
    let raw = rows_or_panic();
    let mut read_back: Vec<Value> = Vec::new();
    for path in [COUNTS_FILE, CLASSIFICATION_FILE] {
        let table: Value =
            serde_json::from_str(&read_text(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        read_back.extend(table["table"].as_array().expect("a table").iter().cloned());
    }
    // The tables together are the raw file: nothing added, nothing dropped.
    assert_eq!(
        read_back.len(),
        raw.len(),
        "the two tables hold {} rows against {} in the raw file",
        read_back.len(),
        raw.len()
    );
    let failures = validate(&read_back, &facts);
    assert!(
        failures.is_empty(),
        "the published tables hold rows the raw file does not: {}",
        failures.join(", ")
    );
    // and the honest shape of a figure the gate does not have
    let counts: Value = serde_json::from_str(&read_text(COUNTS_FILE)).expect("a json table");
    assert!(
        counts["max_read_arrivals_in_any_case"].is_number()
            || counts["max_read_arrivals_in_any_case"].is_null(),
        "an unsupported maximum must be null, never a number a reader takes for zero"
    );
    assert!(!read_back.is_empty(), "the published tables are empty");
}

/// The arity half of the anchor claim: a token that stops naming one place is a failure, not a
/// line number. This is the check that would go red if the send path were folded back into the
/// retry loop, or if the classifier's list were removed, without any row changing.
#[test]
fn anchor_arity_is_the_claim() {
    let facts = SourceFacts::read();
    // alive today
    assert!(
        resolve_pair(DIRECT, "const DEFINITE_REFUSALS").is_ok(),
        "the refusal list anchor must resolve"
    );
    // a name that does not exist, and a name that exists twice: both must be refused
    assert!(
        resolve_pair(
            DIRECT,
            "const DEFINITE_REFUSALS_OF_THIS_BUILD_DOES_NOT_EXIST"
        )
        .is_err(),
        "an absent anchor resolved, so anchors prove nothing"
    );
    assert!(
        resolve_pair(DIRECT, "fn ").is_err(),
        "a token that matches every function resolved to one line, so anchors name a place"
    );
    assert!(
        facts.once_attempts == 1,
        "the send policy is supposed to allow one attempt; production says {}",
        facts.once_attempts
    );
    assert!(
        facts.retry_attempts > facts.once_attempts,
        "the read policy no longer retries, so §3's claim that it was left alone needs \
         restating: {} attempts",
        facts.retry_attempts
    );
}
