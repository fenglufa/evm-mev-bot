//! M9.4 §31/§32/§33/§49 — the committed evidence directory for the Flashblocks Early
//! Radar, and the recomputation that decides whether that directory may exist.
//!
//! ```text
//! M94_EVIDENCE_REFRESH=1 cargo test -p evm-live --test preconf_evidence_gate -- --test-threads=1
//! cargo test -p evm-live --test preconf_evidence_gate -- --test-threads=1
//! ```
//!
//! # The two halves, kept apart on purpose
//!
//! The capture happened elsewhere: the ignored test `preconf_live_giwa.rs` spent two bounded
//! windows against the official endpoint and left five files per window under `raw/`. This
//! file reads those five files plus M9.2's committed pool table, and nothing else.
//!
//! * *Assembly* — every table is rendered from the raw lines. The renderer calls no
//!   `evm_live` function: a table produced by the same decoder it is meant to audit is a
//!   receipt, not evidence (§32's ban on `生产代码输出 summary → summary = evidence`).
//! * *Independent recomputation* — the tests under the *§32* banners re-derive, from the raw
//!   lines alone, the six quantities §32 names (sequence count, duplicate count, gap count,
//!   affected-pool count, reconciliation count, latency calculation) and compare each against
//!   the counters the production run reported for itself. Agreement is the finding; a
//!   divergence is a bug in one of the two, and [`an_injected_wrong_number_is_caught_by_the_recompute`]
//!   proves the comparison can actually fail.
//!
//! One disagreement was found and is disclosed rather than smoothed over: the closing arm
//! polls `latest` and hands the same sealed digest again, while the radar keeps a closed view
//! available so §19's late frame still has something to compare against. A height is therefore
//! closed more than once, and the run's `canonical_closures` counts handings, not heights.
//! Both numbers appear in every table that touches the axis.
//!
//! # What this directory may not contain
//!
//! A URL (§33 — the endpoints appear only as the digests the capture itself wrote), a private
//! key, a signature, a broadcast, an amount, a fee, or a profit. The radar has no field that
//! could hold one and no table invents one: profit and executability are recorded as `"N/A"`
//! with the reason, and a zero is written only where a measurement produced it.
//!
//! # What is honest about the clock
//!
//! Wall time lives in `raw/`. The tables quote it rather than re-measure it, so a rebuild of
//! this directory is byte-for-byte equal forever and the one unrepeatable number in it stays
//! traceable to the run that produced it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

const EVIDENCE_REL: &str = "data/evidence/m9/m9.4";
/// The 80 pools M9.2 attested: the denominator the affected-pool axis stands on. Read from
/// the committed table rather than from discovery code, so this gate asks nothing of the
/// network and nothing of a previous milestone's build graph.
const M92_GRAPH_REL: &str = "data/evidence/m9/m9.2/graph-integration.json";

const WINDOWS: [&str; 2] = ["window-a", "window-b"];

const EVENTS: &str = "events.jsonl";
const READS: &str = "pending-reads.jsonl";
const PAYLOADS: &str = "pending-payloads.jsonl";
const CANONICAL: &str = "canonical-reads.jsonl";
const REPORT: &str = "live-run-report.json";
const RAW_NAMES: [&str; 5] = [REPORT, EVENTS, READS, CANONICAL, PAYLOADS];

const SUMMARY: &str = "summary.json";
const PROTOCOL: &str = "protocol.json";
const FLASHBLOCKS: &str = "flashblocks.jsonl";
const SEQUENCE: &str = "sequence.json";
const AFFECTED: &str = "affected-pools.json";
const RECONCILIATION: &str = "reconciliation.json";
const LATENCY: &str = "latency.json";
const CONTROLS: &str = "negative-controls.json";
const MANIFEST: &str = "manifest.json";
const README: &str = "README.md";

/// Everything the byte gate compares, in commit order.
const TABLES: [&str; 10] = [
    SUMMARY,
    PROTOCOL,
    FLASHBLOCKS,
    SEQUENCE,
    AFFECTED,
    RECONCILIATION,
    LATENCY,
    CONTROLS,
    MANIFEST,
    README,
];
/// The content tables the manifest digests. A file cannot carry the hash of bytes that
/// include its own hash, so `manifest.json` and the README that quotes it are excluded.
const DIGESTED: [&str; 8] = [
    SUMMARY,
    PROTOCOL,
    FLASHBLOCKS,
    SEQUENCE,
    AFFECTED,
    RECONCILIATION,
    LATENCY,
    CONTROLS,
];

/// The test sources a quoted control or fixture has to be found in.
const CONTROL_SOURCES: [&str; 4] = [
    "crates/live/tests/preconf_negative_controls.rs",
    "crates/live/tests/preconf_fixtures.rs",
    "crates/live/tests/preconf_radar_matrix.rs",
    "crates/live/tests/preconf_link_loop.rs",
];

/// §29's twelve controls, each bound to the test that runs it. A control naming a test nobody
/// wrote is a claim, so [`every_quoted_negative_control_names_a_test_that_exists`] greps every
/// name out of the source before this table is allowed to be committed.
const CONTROLS_TABLE: [(&str, &str, &str, &str); 12] = [
    (
        "NC1",
        "duplicate",
        "a repeat of the same (height, view hash) is applied once and derives nothing new",
        "nc1_duplicate_frame_is_not_applied_twice",
    ),
    (
        "NC2",
        "gap",
        "a height that was never pending is announced and never reconstructed",
        "nc2_height_skip_is_announced_and_never_reconstructed",
    ),
    (
        "NC3",
        "wrong_block",
        "a frame or a receipt naming another block is refused, never spliced in",
        "nc3_frame_and_receipt_naming_another_block_are_both_refused",
    ),
    (
        "NC4",
        "wrong_parent",
        "a parent that does not continue the sealed block below invalidates the view",
        "nc4_wrong_parent_invalidates_and_does_not_merge",
    ),
    (
        "NC5",
        "malformed_payload",
        "the decoder fails closed and the held view survives it",
        "nc5_malformed_payload_fails_closed_at_the_decoder",
    ),
    (
        "NC6",
        "wrong_chain",
        "a frame from another chain is refused before anything is held",
        "nc6_wrong_chain_is_refused_before_anything_is_held",
    ),
    (
        "NC7",
        "reconnect",
        "a reset leaves no cross-session state for the next session to be wrong about",
        "nc7_giving_up_on_the_transport_leaves_no_state_to_resume",
    ),
    (
        "NC8",
        "canonical_mismatch",
        "content disagreement makes canonical win, with no merge",
        "nc8_canonical_mismatch_makes_canonical_win_with_no_merge",
    ),
    (
        "NC9",
        "late_flashblock",
        "a frame for an already sealed height is compared, then discarded",
        "nc9_late_flashblock_does_not_pollute_the_closed_view",
    ),
    (
        "NC10",
        "unrelated_transaction",
        "a transaction that names no registered pool produces no AffectedPool",
        "nc10_unrelated_transaction_produces_no_affected_pool",
    ),
    (
        "NC11",
        "reverted_transaction",
        "a reverted or unstatus receipt is never read as a state mutation",
        "nc11_reverted_receipt_is_never_read_as_a_state_mutation",
    ),
    (
        "NC12",
        "same_pool_multiple_changes",
        "three changes to one pool keep three identities",
        "nc12_three_changes_to_one_pool_keep_three_identities",
    ),
];

// ---------------------------------------------------------------------------
// reading a window
// ---------------------------------------------------------------------------

/// One pending read, as its raw line states it. `issued_at` is when the arm *sent* the
/// request; the frame's arrival is the paired `FrameAccepted` event's time, and no table below
/// ever conflates the two.
struct Read {
    read_index: u64,
    issued_at: u64,
    number: u64,
    view_hash: String,
    parent_hash: String,
    tx_hashes: Vec<String>,
    targets: BTreeSet<String>,
    senders: BTreeSet<String>,
    payload_bytes: u64,
    payload_digest: String,
    state_root: String,
    header_keys: BTreeSet<String>,
    tx_shapes: BTreeMap<String, u64>,
    verbatim_included: bool,
}

/// One sealed block as the closing side handed it over: the `latest` poll that named it, or
/// the fill read for a height that poll jumped over.
#[derive(Clone)]
struct Sealed {
    number: u64,
    hash: String,
    observed_at: u64,
    chain_timestamp_secs: u64,
    tx_hashes: Vec<String>,
    gap_fill: bool,
}

/// One `Reconciled` event.
struct Closure {
    event_index: u64,
    recorded_at: u64,
    number: u64,
    verdict_label: String,
    held: Option<u64>,
    sealed_count: Option<u64>,
    hash_matched: bool,
    reported: Value,
    /// Which handing of this height's sealed digest it was: a height can close more than once.
    occurrence: usize,
}

struct Window {
    name: &'static str,
    report: Value,
    reads: Vec<Read>,
    events: Vec<Value>,
    sealed: Vec<Sealed>,
    closures: Vec<Closure>,
    /// The transaction list the radar held for a height at each point of the event stream,
    /// replayed from the raw lines in delivery order: the rule §16 applies, re-implemented
    /// here so the verdict recomputation has something it can disagree with.
    held_views: BTreeMap<u64, Vec<String>>,
    frame_delivered_at: BTreeMap<u64, (u64, u64)>,
}

impl Window {
    fn load(name: &'static str) -> Self {
        let report: Value = read_json(&raw_rel(name, REPORT));
        let mut reads = read_jsonl(&raw_rel(name, READS))
            .into_iter()
            .map(parse_read)
            .collect::<Vec<_>>();
        reads.sort_by_key(|r| r.read_index);
        let events = read_jsonl(&raw_rel(name, EVENTS));

        let mut sealed = Vec::new();
        for line in read_jsonl(&raw_rel(name, CANONICAL)) {
            if let Some(digest) = line.get("digest") {
                sealed.push(sealed_from(digest, false));
            }
            for fill in line["gap_reads"].as_array().cloned().unwrap_or_default() {
                if let Some(digest) = fill.get("digest") {
                    sealed.push(sealed_from(digest, true));
                }
            }
        }

        // The link performs one read per payload it applies, so the k-th pending read pairs
        // with the k-th `FrameAccepted`. `frame_pairs` proves the pairing before anything
        // relies on it, and the result is keyed by event index rather than by position,
        // because position would be a claim about how the file was written.
        let pairs = frame_pairs(name, &reads, &events);
        let mut held_views = BTreeMap::new();
        let mut frame_delivered_at: BTreeMap<u64, (u64, u64)> = BTreeMap::new();
        let mut closures = Vec::new();
        let mut occurrence: BTreeMap<u64, usize> = BTreeMap::new();
        for event in &events {
            let kind = event_kind(event);
            let index = event["event_index"].as_u64().expect("event_index");
            let body = event_body(event);
            match kind.as_str() {
                "FrameAccepted" => {
                    let number = body["number"].as_u64().expect("frame number");
                    let at = event["recorded_at_unix_ms"].as_u64().expect("recorded_at");
                    let slot = pairs
                        .get(&index)
                        .unwrap_or_else(|| panic!("{name}: FrameAccepted {index} has no read"));
                    held_views.insert(number, reads[*slot].tx_hashes.clone());
                    let entry = frame_delivered_at.entry(number).or_insert((at, at));
                    entry.0 = entry.0.min(at);
                    entry.1 = entry.1.max(at);
                }
                "Reconciled" => {
                    let number = body["number"].as_u64().expect("closure number");
                    let seen = occurrence.entry(number).or_insert(0);
                    *seen += 1;
                    let verdict = &body["verdict"];
                    closures.push(Closure {
                        event_index: index,
                        recorded_at: event["recorded_at_unix_ms"].as_u64().expect("recorded_at"),
                        number,
                        verdict_label: verdict_label(verdict),
                        held: verdict_field(verdict, "held"),
                        sealed_count: verdict_field(verdict, "sealed"),
                        hash_matched: body["hash_matched"].as_bool().unwrap_or(false),
                        reported: body["latencies"].clone(),
                        occurrence: *seen - 1,
                    });
                }
                _ => {}
            }
        }
        Self {
            name,
            report,
            reads,
            events,
            sealed,
            closures,
            held_views,
            frame_delivered_at,
        }
    }

    fn counters(&self) -> &Value {
        &self.report["report"]["counters"]
    }

    fn meta(&self) -> &Value {
        &self.report["meta"]
    }

    /// The sealed digests for one height, in the order the closing side produced them.
    fn sealed_for(&self, number: u64) -> Vec<&Sealed> {
        self.sealed.iter().filter(|s| s.number == number).collect()
    }

    fn count_kind(&self, kind: &str) -> u64 {
        self.events.iter().filter(|e| event_kind(e) == kind).count() as u64
    }

    /// One row per height: what the preconfirmation arm said about it.
    fn heights(&self) -> BTreeMap<u64, Value> {
        struct Acc {
            reads: u64,
            views: BTreeSet<String>,
            duplicates: u64,
            first_read: u64,
            last_read: u64,
            first_txs: u64,
            latest_txs: u64,
            targets: BTreeSet<String>,
            placeholder_throughout: bool,
        }
        let mut acc: BTreeMap<u64, Acc> = BTreeMap::new();
        for read in &self.reads {
            let entry = acc.entry(read.number).or_insert_with(|| Acc {
                reads: 0,
                views: BTreeSet::new(),
                duplicates: 0,
                first_read: read.issued_at,
                last_read: read.issued_at,
                first_txs: read.tx_hashes.len() as u64,
                latest_txs: read.tx_hashes.len() as u64,
                targets: BTreeSet::new(),
                placeholder_throughout: true,
            });
            entry.reads += 1;
            entry.last_read = entry.last_read.max(read.issued_at);
            entry.latest_txs = read.tx_hashes.len() as u64;
            if !entry.views.insert(read.view_hash.clone()) {
                entry.duplicates += 1;
            }
            entry.targets.extend(read.targets.iter().cloned());
            if !is_zero_root(&read.state_root) {
                entry.placeholder_throughout = false;
            }
        }
        let mut closed_count: BTreeMap<u64, u64> = BTreeMap::new();
        for closure in &self.closures {
            *closed_count.entry(closure.number).or_insert(0) += 1;
        }
        acc.into_iter()
            .map(|(number, row)| {
                let delivered = self.frame_delivered_at.get(&number);
                (
                    number,
                    json!({
                        "height": number,
                        "reads": row.reads,
                        "distinct_views": row.views.len() as u64,
                        "duplicate_reads": row.duplicates,
                        "transactions_in_first_view": row.first_txs,
                        "transactions_in_latest_view": row.latest_txs,
                        "distinct_targets": row.targets.len() as u64,
                        "first_read_issued_at_unix_ms": row.first_read,
                        "last_read_issued_at_unix_ms": row.last_read,
                        "first_frame_delivered_at_unix_ms": delivered.map(|d| d.0),
                        "last_frame_delivered_at_unix_ms": delivered.map(|d| d.1),
                        "view_growth_ms": delivered.map(|d| d.1 as i64 - d.0 as i64),
                        "closures": closed_count.get(&number).copied().unwrap_or(0),
                        "closed": closed_count.contains_key(&number),
                        "state_root_placeholder_throughout": row.placeholder_throughout,
                    }),
                )
            })
            .collect()
    }

    /// Every count §32 names, recomputed from this window's raw lines.
    fn recomputed(&self) -> Value {
        let heights = self.heights();
        let distinct_views: BTreeSet<(u64, String)> = self
            .reads
            .iter()
            .map(|r| (r.number, r.view_hash.clone()))
            .collect();
        // The radar observes a repeated view and derives nothing new from it, so the
        // denominator §32 asks for is one transaction count per distinct view.
        let mut seen_view: BTreeSet<(u64, String)> = BTreeSet::new();
        let transactions_in_views: u64 = self
            .reads
            .iter()
            .filter(|r| seen_view.insert((r.number, r.view_hash.clone())))
            .map(|r| r.tx_hashes.len() as u64)
            .sum();
        let placeholders = distinct_views
            .iter()
            .filter(|(_, hash)| {
                self.reads
                    .iter()
                    .any(|r| r.view_hash == *hash && is_zero_root(&r.state_root))
            })
            .count() as u64;
        json!({
            "frames_offered": self.reads.len() as u64,
            "frames_accepted": self.count_kind("FrameAccepted"),
            "frames_rejected": self.count_kind("FrameRejected"),
            "frames_duplicate": self.reads.len() as u64 - distinct_views.len() as u64,
            "distinct_views": distinct_views.len() as u64,
            "heights_seen": heights.len() as u64,
            "heights_multi_view": heights.values().filter(|r| r["distinct_views"].as_u64().unwrap_or(0) > 1).count() as u64,
            "heights_ge_three_views": heights.values().filter(|r| r["distinct_views"].as_u64().unwrap_or(0) >= 3).count() as u64,
            "state_root_placeholders": placeholders,
            "transactions_observed": transactions_in_views,
            "canonical_closures": self.closures.len() as u64,
            "distinct_heights_closed": self.closures.iter().map(|c| c.number).collect::<BTreeSet<_>>().len() as u64,
            "reconciled_content_equal": self.closures.iter().filter(|c| c.verdict_label == "content_equal").count() as u64,
            "reconciled_content_prefix": self.closures.iter().filter(|c| c.verdict_label == "content_prefix").count() as u64,
            "reconciled_content_superset": self.closures.iter().filter(|c| c.verdict_label == "content_superset").count() as u64,
            "reconciled_content_divergent": self.closures.iter().filter(|c| c.verdict_label == "content_divergent").count() as u64,
            "reconciled_no_view": self.closures.iter().filter(|c| c.verdict_label == "no_view").count() as u64,
            "reconciled_hash_match": self.closures.iter().filter(|c| c.hash_matched).count() as u64,
            "views_invalidated": self.count_kind("ViewInvalidated"),
            "views_expired": self.count_kind("ViewExpired"),
            "affected_pools_emitted": self.count_kind("PoolAffected"),
            "canonical_gaps_filled": self.sealed.iter().filter(|s| s.gap_fill).count() as u64,
        })
    }

    /// The two lead populations §27/§28 can be asked about, both recomputed from raw stamps:
    /// per closed height, and per transaction of a sealed block.
    fn lead_samples(&self) -> (Vec<i64>, Vec<i64>, u64, u64) {
        let mut first_seen: BTreeMap<String, u64> = BTreeMap::new();
        for read in &self.reads {
            let delivered = self
                .frame_delivered_at
                .get(&read.number)
                .map(|d| d.0)
                .unwrap_or(read.issued_at);
            for hash in &read.tx_hashes {
                let entry = first_seen.entry(hash.clone()).or_insert(delivered);
                *entry = (*entry).min(delivered);
            }
        }
        let mut height_leads = Vec::new();
        let mut tx_leads = Vec::new();
        let mut never_pre_seen = 0u64;
        let mut sealed_txs = 0u64;
        for closure in &self.closures {
            if closure.occurrence > 0 {
                continue;
            }
            let Some(digest) = self.sealed_for(closure.number).into_iter().next() else {
                continue;
            };
            if let Some(first) = self.frame_delivered_at.get(&closure.number).map(|d| d.0) {
                height_leads.push(digest.observed_at as i64 - first as i64);
            }
            for hash in &digest.tx_hashes {
                sealed_txs += 1;
                match first_seen.get(hash) {
                    Some(seen) => tx_leads.push(digest.observed_at as i64 - *seen as i64),
                    None => never_pre_seen += 1,
                }
            }
        }
        (height_leads, tx_leads, never_pre_seen, sealed_txs)
    }
}

fn parse_read(line: Value) -> Read {
    let projection = &line["projection"];
    let mut targets = BTreeSet::new();
    let mut senders = BTreeSet::new();
    let mut shapes = BTreeMap::new();
    for tx in projection["transactions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        if let Some(to) = tx["to"].as_str() {
            targets.insert(to.to_ascii_lowercase());
        }
        if let Some(from) = tx["from"].as_str() {
            senders.insert(from.to_ascii_lowercase());
        }
        *shapes
            .entry(tx["shape"].as_str().unwrap_or("absent").to_string())
            .or_insert(0u64) += 1;
    }
    Read {
        read_index: line["read_index"].as_u64().expect("read_index"),
        issued_at: line["at_unix_ms"].as_u64().expect("at_unix_ms"),
        number: hex_u64(projection["number"].as_str().expect("number")),
        view_hash: projection["hash"]
            .as_str()
            .expect("hash")
            .to_ascii_lowercase(),
        parent_hash: projection["parent_hash"]
            .as_str()
            .expect("parent_hash")
            .to_ascii_lowercase(),
        tx_hashes: tx_hash_list(&projection["transactions"]),
        targets,
        senders,
        payload_bytes: line["payload_bytes"].as_u64().expect("payload_bytes"),
        payload_digest: line["payload_digest"]
            .as_str()
            .expect("payload_digest")
            .to_ascii_lowercase(),
        state_root: projection["state_root"]
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase(),
        header_keys: projection["header_keys"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|k| k.as_str().map(str::to_string))
            .collect(),
        tx_shapes: shapes,
        verbatim_included: line["verbatim_included"].as_bool().unwrap_or(false),
    }
}

fn sealed_from(digest: &Value, gap_fill: bool) -> Sealed {
    Sealed {
        number: digest["number"].as_u64().expect("sealed number"),
        hash: digest["hash"]
            .as_str()
            .expect("sealed hash")
            .to_ascii_lowercase(),
        observed_at: digest["observed_at_unix_ms"].as_u64().expect("observed_at"),
        chain_timestamp_secs: digest["chain_timestamp_secs"]
            .as_u64()
            .expect("chain_timestamp"),
        tx_hashes: digest["transaction_hashes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|h| h.as_str().map(str::to_lowercase))
            .collect(),
        gap_fill,
    }
}

/// The pairing the link claims: one read per payload it applies, in order. Returns
/// `event_index → read slot` and fails if the two sequences disagree in length or in the
/// (height, transaction count) they name — a pairing that silently shifted would put every
/// per-frame timestamp in the wrong row.
fn frame_pairs(window: &str, reads: &[Read], events: &[Value]) -> BTreeMap<u64, usize> {
    let accepted: Vec<(u64, u64, usize)> = events
        .iter()
        .filter(|e| event_kind(e) == "FrameAccepted")
        .map(|e| {
            let body = event_body(e);
            (
                body["number"].as_u64().expect("frame number"),
                e["event_index"].as_u64().expect("event_index"),
                body["transaction_count"].as_u64().unwrap_or(0) as usize,
            )
        })
        .collect();
    assert_eq!(
        accepted.len(),
        reads.len(),
        "{window}: {} pending reads and {} FrameAccepted events cannot pair one-to-one",
        reads.len(),
        accepted.len()
    );
    let mut map = BTreeMap::new();
    for (slot, (number, event_index, tx_count)) in accepted.into_iter().enumerate() {
        let read = &reads[slot];
        assert_eq!(
            (read.number, read.tx_hashes.len()),
            (number, tx_count),
            "{window}: read {} names height {} with {} transaction(s) but FrameAccepted \
             {event_index} names {number} with {tx_count}; the pairing every latency row \
             stands on does not hold",
            read.read_index,
            read.number,
            read.tx_hashes.len()
        );
        map.insert(event_index, slot);
    }
    map
}

// ---------------------------------------------------------------------------
// small readers
// ---------------------------------------------------------------------------

fn refresh() -> bool {
    std::env::var("M94_EVIDENCE_REFRESH").is_ok()
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn evidence_dir() -> PathBuf {
    workspace_root().join(EVIDENCE_REL)
}

fn missing(relative: &str) -> String {
    format!(
        "{relative} is not readable. Assemble this directory first: M94_EVIDENCE_REFRESH=1 \
         cargo test -p evm-live --test preconf_evidence_gate -- --test-threads=1"
    )
}

/// One path for every read of this directory's own files: an assembly run assembles first, so
/// no test reads a half-written directory, and a gate run writes nothing.
fn read_text(relative: &str) -> String {
    if TABLES.contains(&relative) {
        assemble_once();
    }
    let path = if relative.starts_with("crates/") || relative.starts_with("data/") {
        workspace_root().join(relative)
    } else {
        evidence_dir().join(relative)
    };
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", missing(relative)))
}

fn read_value(relative: &str) -> Value {
    let text = read_text(relative);
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("{relative} does not parse: {err}"))
}

fn read_json(relative: &str) -> Value {
    read_value(relative)
}

fn read_jsonl(relative: &str) -> Vec<Value> {
    read_text(relative)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("{relative}: a line does not parse: {err}"))
        })
        .collect()
}

fn raw_rel(window: &str, name: &str) -> String {
    format!("{EVIDENCE_REL}/raw/{window}/{name}")
}

/// A raw file's absolute path. `raw_rel` is already repo-relative, so joining it onto
/// `evidence_dir()` would double the prefix and look up a directory that never exists.
fn raw_path(window: &str, name: &str) -> PathBuf {
    workspace_root().join(raw_rel(window, name))
}

/// A window's directory as it is named in a capture command.
fn raw_dir(window: &str) -> String {
    format!("{EVIDENCE_REL}/raw/{window}")
}

fn read_optional(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

static ASSEMBLED: std::sync::Once = std::sync::Once::new();

fn assemble_once() {
    ASSEMBLED.call_once(|| {
        if refresh() {
            commit(&build_tables());
        }
    });
}

fn hex_u64(value: &str) -> u64 {
    u64::from_str_radix(value.strip_prefix("0x").unwrap_or(value), 16)
        .unwrap_or_else(|err| panic!("`{value}` is not a hex quantity: {err}"))
}

fn is_zero_root(state_root: &str) -> bool {
    state_root
        .strip_prefix("0x")
        .map(|hex| !hex.is_empty() && hex.chars().all(|c| c == '0'))
        .unwrap_or(false)
}

fn tx_hash_list(transactions: &Value) -> Vec<String> {
    transactions
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|tx| {
            if tx["shape"].as_str() == Some("hash_only") {
                tx.as_str().map(str::to_lowercase)
            } else {
                tx["hash"].as_str().map(str::to_lowercase)
            }
        })
        .collect()
}

/// The serialized `RadarEvent` is a one-key map and the key is the variant name.
fn event_kind(event: &Value) -> String {
    event["event"]
        .as_object()
        .and_then(|m| m.keys().next().cloned())
        .unwrap_or_else(|| panic!("an event line carries no variant: {event}"))
}

fn event_body(event: &Value) -> Value {
    let object = event["event"].as_object().expect("an event body");
    object
        .values()
        .next()
        .cloned()
        .expect("one variant per event")
}

fn verdict_label(verdict: &Value) -> String {
    match verdict {
        Value::String(name) => match name.as_str() {
            "ContentEqual" => "content_equal",
            "NoView" => "no_view",
            other => panic!("unexpected bare verdict `{other}`"),
        },
        Value::Object(map) => {
            let key = map.keys().next().expect("a verdict names its class");
            match key.as_str() {
                "ContentPrefix" => "content_prefix",
                "ContentSuperset" => "content_superset",
                "ContentDivergent" => "content_divergent",
                other => panic!("unexpected verdict `{other}`"),
            }
        }
        other => panic!("a verdict is either a label or a tagged map: {other}"),
    }
    .to_string()
}

fn verdict_field(verdict: &Value, field: &str) -> Option<u64> {
    verdict
        .as_object()
        .and_then(|m| m.values().next())
        .and_then(|inner| inner[field].as_u64())
}

/// §28's rule for a statistic with no data behind it, and the sample floor each percentile
/// needs. The floor is stated in the table rather than assumed: a `p95` over four samples is a
/// number a reader could quote as if it were a distribution.
fn stats(samples: &[i64]) -> Value {
    let n = samples.len();
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = |percentile: f64| -> usize {
        (((percentile / 100.0) * n as f64).ceil() as usize)
            .saturating_sub(1)
            .min(n - 1)
    };
    json!({
        "samples": n as u64,
        "min_ms": if n == 0 { json!("N/A") } else { json!(sorted[0]) },
        "median_ms": if n >= 3 { json!(sorted[rank(50.0)]) } else { json!("N/A") },
        "p90_ms": if n >= 10 { json!(sorted[rank(90.0)]) } else { json!("N/A") },
        "p95_ms": if n >= 20 { json!(sorted[rank(95.0)]) } else { json!("N/A") },
        "max_ms": if n == 0 { json!("N/A") } else { json!(sorted[n - 1]) },
        "non_positive_samples": samples.iter().filter(|s| **s <= 0).count() as u64,
        "percentile_rule": "nearest-rank; median needs n>=3, p90 n>=10, p95 n>=20. Below the floor the cell reads N/A instead of the one sample it would otherwise quote (§28: missing data is N/A, never a number that implies a distribution)."
    })
}

/// §16's content comparison, written from the task book's four words rather than from the
/// crate: equal, a prefix of the sealed list, a superset of it, or divergent both ways.
fn verdict_of(held: &[String], sealed: &[String]) -> (String, u64, u64) {
    let held_len = held.len() as u64;
    let sealed_len = sealed.len() as u64;
    if held == sealed {
        return ("content_equal".to_string(), held_len, sealed_len);
    }
    let held_set: BTreeSet<&str> = held.iter().map(String::as_str).collect();
    let sealed_set: BTreeSet<&str> = sealed.iter().map(String::as_str).collect();
    let only_in_view = held_set.difference(&sealed_set).count();
    let only_in_canonical = sealed_set.difference(&held_set).count();
    if only_in_view == 0 {
        ("content_prefix".to_string(), held_len, sealed_len)
    } else if only_in_canonical == 0 {
        ("content_superset".to_string(), held_len, sealed_len)
    } else {
        ("content_divergent".to_string(), held_len, sealed_len)
    }
}

// ---------------------------------------------------------------------------
// assembly
// ---------------------------------------------------------------------------

fn load_windows() -> Vec<Window> {
    WINDOWS.iter().map(|name| Window::load(name)).collect()
}

/// M9.2's published graph table: the pools a hit would have to be measured against.
fn pool_set() -> (BTreeSet<String>, BTreeSet<String>, u64, u64) {
    let graph: Value = read_json(M92_GRAPH_REL);
    let edges = graph["edges"].as_array().cloned().unwrap_or_default();
    let mut pools = BTreeSet::new();
    let mut tokens = BTreeSet::new();
    for edge in &edges {
        let id = &edge["id"];
        pools.insert(
            id["pool"]["address"]
                .as_str()
                .expect("an edge names its pool")
                .to_ascii_lowercase(),
        );
        for key in ["token_in", "token_out"] {
            tokens.insert(
                id[key]["address"]
                    .as_str()
                    .expect("an edge names both tokens")
                    .to_ascii_lowercase(),
            );
        }
    }
    (
        pools,
        tokens,
        edges.len() as u64,
        graph["graph_block"].as_u64().unwrap_or(0),
    )
}

fn build_tables() -> BTreeMap<&'static str, String> {
    let windows = load_windows();
    let (pools, tokens, edges, graph_block) = pool_set();
    let mut tables: BTreeMap<&'static str, String> = BTreeMap::new();
    tables.insert(PROTOCOL, pretty(PROTOCOL, protocol_table(&windows)));
    tables.insert(FLASHBLOCKS, flashblocks_jsonl(&windows, &pools, &tokens));
    tables.insert(SEQUENCE, pretty(SEQUENCE, sequence_table(&windows)));
    tables.insert(
        AFFECTED,
        pretty(
            AFFECTED,
            affected_table(&windows, &pools, &tokens, edges, graph_block),
        ),
    );
    tables.insert(
        RECONCILIATION,
        pretty(RECONCILIATION, reconciliation_table(&windows)),
    );
    tables.insert(LATENCY, pretty(LATENCY, latency_table(&windows)));
    tables.insert(CONTROLS, pretty(CONTROLS, controls_table(&windows, &pools)));
    tables.insert(
        SUMMARY,
        pretty(
            SUMMARY,
            summary_table(&windows, &pools, &tokens, edges, graph_block),
        ),
    );
    tables.insert(
        MANIFEST,
        pretty(MANIFEST, manifest_table(&windows, &tables)),
    );
    tables.insert(
        README,
        readme_table(&windows, &pools, &tokens, edges, graph_block),
    );
    tables
}

/// What the endpoint actually answered, stated as fields a reader can check against `raw/` —
/// never as a schema the decoder wished for (§5.1/§6).
fn protocol_table(windows: &[Window]) -> Value {
    let reads: Vec<&Read> = windows.iter().flat_map(|w| w.reads.iter()).collect();
    let mut key_presence: BTreeMap<String, u64> = BTreeMap::new();
    let mut shapes: BTreeMap<String, u64> = BTreeMap::new();
    for read in &reads {
        for key in &read.header_keys {
            *key_presence.entry(key.clone()).or_insert(0) += 1;
        }
        for (shape, count) in &read.tx_shapes {
            *shapes.entry(shape.clone()).or_insert(0) += count;
        }
    }
    let first = &windows[0];
    let index_like: Vec<&String> = key_presence
        .keys()
        .filter(|k| {
            let lower = k.to_ascii_lowercase();
            lower == "index" || lower == "sequence" || lower == "slot" || lower == "payloadid"
        })
        .collect();
    json!({
        "table": "protocol",
        "kind": "characterization from the committed raw lines (§5.1/§6)",
        "chain_id": first.meta()["chain_id"],
        "endpoints": {
            "flashblocks": first.meta()["flashblocks_endpoint"],
            "canonical": first.meta()["canonical_endpoint"],
            "note": "§33: the two digests the capture wrote itself. This table recomputes neither, because recomputing one would need the URL it replaced."
        },
        "reads_sampled": reads.len(),
        "observed_header_keys": key_presence.iter().map(|(k, v)| json!({"key": k, "reads_naming_it": v})).collect::<Vec<_>>(),
        "observed_header_key_count": key_presence.len(),
        "identity_fields": {
            "height": "number — a hex quantity, present in every read",
            "view_hash": "hash — changes between reads of one height, so identity is never the hash alone",
            "parent_identity": "parentHash — present in every read",
            "state_credential": "stateRoot — an all-zero placeholder in every distinct view of both windows",
            "frame_index": if index_like.is_empty() { "ABSENT — no index / sequence / slot / payloadId key appears in any read, so the frame index is assigned locally from read order (§8)".to_string() } else { format!("present: {index_like:?}") }
        },
        "transaction_shapes": shapes.iter().map(|(shape, count)| json!({"shape": shape, "count": count})).collect::<Vec<_>>(),
        "capabilities": [
            {"capability": "push / subscription", "value": "NOT_AVAILABLE_ON_THIS_TRANSPORT", "witness": "both windows read the preconfirmation arm by polling on an HTTP adapter, and each run report says transport = http-poll-pending. The authority for the endpoint side is audit §2.1's probes; this milestone re-ran none of them and claims nothing beyond them."},
            {"capability": "provider frame index", "value": if index_like.is_empty() { "ABSENT" } else { "PRESENT" }, "witness": "observed_header_keys above"},
            {"capability": "replay after disconnect", "value": "UNKNOWN", "witness": "§52: neither window reset its link (link_resets = 0), so the question was never asked and no method that could answer it was requested."},
            {"capability": "cursor / gap recovery", "value": "UNKNOWN", "witness": "same: there is no wire index to cursor with, and no preconfirmation gap is detectable (§9)."},
            {"capability": "pending receipts", "value": "NOT_REQUESTED_IN THESE_WINDOWS", "witness": "methods_requested lists two methods and receipt_lists_decoded is 0, so the log-derived half of AffectedPool has no real-traffic witness here; its correctness rests on the fixtures and on NC10/NC11/NC12."},
            {"capability": "state credential for the pending view", "value": "NONE (all-zero stateRoot)", "witness": "state_root_placeholder_throughout per height in sequence.json"}
        ],
        "methods_requested": first.meta()["methods_requested"],
        "wire_format_notes": [
            "a pending answer is one complete block object: no envelope, no notification method, no subscription id, nothing to frame a stream with",
            "the same height is answered repeatedly and each answer is a longer list of the same transactions, so a frame is a snapshot of one growing view rather than a delta",
            "the header names the sequencer as miner and carries the OP-Stack fields (blobGasUsed, excessBlobGas, requestsHash, withdrawalsRoot), which is what identifies this endpoint's product family without a version string the endpoint does not publish"
        ],
        "what_was_not_probed_here": [
            "node version / commit — the endpoint publishes no field naming it (§52: UNKNOWN, not guessed)",
            "upstream Flashblocks vs Subblocks evolution — a repository question, answered in the audit, not by these two windows",
            "storage / balance / logs at the pending tag — audit §2.2 already ruled the pending view out as a state credential; this milestone reads no state"
        ]
    })
}

/// One row per pending read — §31's `flashblocks.jsonl`, the file every other table's frame
/// counts can be checked against by eye.
fn flashblocks_jsonl(
    windows: &[Window],
    pools: &BTreeSet<String>,
    tokens: &BTreeSet<String>,
) -> String {
    let mut text = String::new();
    for window in windows {
        for (slot, read) in window.reads.iter().enumerate() {
            let pool_hits = read.targets.iter().filter(|a| pools.contains(*a)).count();
            let token_hits = read.targets.iter().filter(|a| tokens.contains(*a)).count();
            let line = json!({
                "window": window.name,
                "read_index": read.read_index,
                "frame_sequence_in_window": slot + 1,
                "issued_at_unix_ms": read.issued_at,
                "height": read.number,
                "view_hash": read.view_hash,
                "parent_hash": read.parent_hash,
                "state_root_placeholder": is_zero_root(&read.state_root),
                "transactions": read.tx_hashes.len(),
                "distinct_targets": read.targets.len(),
                "targets_in_m92_pool_set": pool_hits,
                "targets_in_m92_token_set": token_hits,
                "payload_bytes": read.payload_bytes,
                "payload_digest": read.payload_digest,
                "verbatim_payload_committed": read.verbatim_included,
            });
            text.push_str(&serde_json::to_string(&line).expect("a row serializes"));
            text.push('\n');
        }
    }
    text
}

/// §9's ledger: one row per height, plus the §48 count that cannot be rounded up.
fn sequence_table(windows: &[Window]) -> Value {
    let mut rows = Vec::new();
    let mut per_window = Vec::new();
    for window in windows {
        let ledger = window.heights();
        let growth: Vec<u64> = ledger
            .values()
            .filter_map(|r| r["view_growth_ms"].as_u64())
            .collect();
        per_window.push(json!({
            "window": window.name,
            "heights_seen": ledger.len(),
            "heights_with_more_than_one_view": ledger.values().filter(|r| r["distinct_views"].as_u64().unwrap_or(0) > 1).count(),
            "heights_with_three_or_more_views": ledger.values().filter(|r| r["distinct_views"].as_u64().unwrap_or(0) >= 3).count(),
            "heights_closed_at_least_once": ledger.values().filter(|r| r["closed"].as_bool() == Some(true)).count(),
            "duplicate_reads": ledger.values().map(|r| r["duplicate_reads"].as_u64().unwrap_or(0)).sum::<u64>(),
            "max_views_on_one_height": ledger.values().map(|r| r["distinct_views"].as_u64().unwrap_or(0)).max(),
            "longest_view_growth_ms": growth.iter().max().copied().map(|ms| json!(ms)).unwrap_or(json!("N/A")),
            "gaps_within_a_height": "N/A_NOT_MEASURABLE",
            "gaps_within_a_height_reason": "no wire frame index exists (§9), so a frame that never arrived cannot be told apart from one that arrived and was superseded inside the same read interval",
            "canonical_side_heights_the_poll_jumped_over": window.recomputed()["canonical_gaps_filled"],
            "canonical_side_axis_reason": "the closing arm polls `latest`; every height it jumped over is read by number and appears as a gap_fill row in reconciliation.json — measurable, because block numbers are the provider's own and frame indices are not"
        }));
        for row in ledger.values() {
            rows.push(row.clone());
        }
    }
    json!({
        "table": "sequence",
        "kind": "per-height sequence ledger, recomputed from raw/window-*/pending-reads.jsonl and raw/window-*/events.jsonl",
        "windows": per_window,
        "section_48": {
            "asked": "at least one complete real window with N >= 3 consecutive flashblocks",
            "what_the_endpoint_gives_instead": "N distinct views of one height, each a longer list of the same unsealed block — so `consecutive` is counted as distinct views of a single pending height, and the count below is that count",
            "heights_reaching_three_distinct_views": windows.iter().map(|w| w.recomputed()["heights_ge_three_views"].as_u64().unwrap_or(0)).sum::<u64>(),
            "heights_observed": windows.iter().map(|w| w.recomputed()["heights_seen"].as_u64().unwrap_or(0)).sum::<u64>(),
            "honest_wording": "the budget is met on a minority of heights and missed on the rest, because most blocks seal before a 250 ms poll can catch them three times. Both numbers are the actual counts; the shortfall is a finding about the endpoint, not a failed run (§48: record the real number, do not fabricate)."
        },
        "rows": rows,
    })
}

/// §12–§14: what the radar was allowed to say about pools, and the two denominators a `0`
/// needs before it means anything at all.
fn affected_table(
    windows: &[Window],
    pools: &BTreeSet<String>,
    tokens: &BTreeSet<String>,
    edges: u64,
    graph_block: u64,
) -> Value {
    let mut targets: BTreeSet<String> = BTreeSet::new();
    let mut senders: BTreeSet<String> = BTreeSet::new();
    for window in windows {
        for read in &window.reads {
            targets.extend(read.targets.iter().cloned());
            senders.extend(read.senders.iter().cloned());
        }
    }
    let pool_hits = targets.intersection(pools).count();
    let token_hits = targets.intersection(tokens).count();
    let sequencer_hits = targets
        .iter()
        .filter(|a| a.starts_with("0x42000000000000000000000000000000000000"))
        .count();
    let sum = |axis: &str| -> u64 {
        windows
            .iter()
            .map(|w| w.counters()[axis].as_u64().unwrap_or(0))
            .sum()
    };
    let total = |axis: &str| -> u64 {
        windows
            .iter()
            .map(|w| w.recomputed()[axis].as_u64().unwrap_or(0))
            .sum()
    };
    json!({
        "table": "affected-pools",
        "kind": "the registered-pool half of the early radar (§12–§14), recomputed by address intersection over the raw transaction targets",
        "denominators": {
            "registered_pools_from_m9_2": pools.len(),
            "edges_in_that_table": edges,
            "graph_block": graph_block,
            "distinct_tokens_in_that_table": tokens.len(),
            "distinct_transaction_targets_observed": targets.len(),
            "distinct_senders_observed": senders.len(),
            "source_of_the_pool_set": M92_GRAPH_REL
        },
        "real_window_result": {
            "targets_hitting_the_registered_pool_set": pool_hits,
            "affected_pool_events_emitted": total("affected_pools_emitted"),
            "affected_pools_by_target_reported": sum("affected_pools_by_target"),
            "affected_pools_by_log_reported": sum("affected_pools_by_log"),
            "receipt_lists_read_in_window": sum("receipt_lists_decoded"),
            "positive_control_token_side_hits": token_hits,
            "positive_control_sequencer_hits": sequencer_hits,
            "why_this_zero_is_not_a_parser_failure": format!(
                "both sides of the intersection are non-empty ({t} distinct observed targets against {p} registered pools), and the same intersection over the same lines is non-zero for the {c} known token contract(s) and the {s} sequencer address(es) these transactions do call. An empty pool intersection is therefore a market fact, not a silent miss.",
                t = targets.len(), p = pools.len(), c = token_hits, s = sequencer_hits
            ),
            "log_derived_half": "no witness in real traffic: these windows requested two methods and neither was a receipts read, so the log path's correctness rests on the fixtures and on NC10/NC11/NC12 rather than on this table (§48's rule to record the actual count)"
        },
        "what_a_hit_would_have_produced": [
            "AffectedPool { pool, caused_by transaction hash, strength (by target or by log), block_number, local_frame_sequence }",
            "nothing else: no reserve, no price, no amount, no path (§13/§14), and no input to PathFinder (§20/§21)",
            "at closure, a pool-set comparison against canonical — recorded as UNVERIFIED when canonical named no list (§16)"
        ],
        "canonical_side_pool_lists": {
            "closures_where_canonical_named_a_list": 0u64,
            "unverified": sum("reconciled_pools_unverified"),
            "matched": sum("reconciled_pools_matched"),
            "diverged": sum("reconciled_pools_diverged"),
            "note": "the closing arm reads `eth_getBlockByNumber([latest, true])` and no receipt, so it names no target list and every closure is UNVERIFIED on this axis — a different statement from 'the pool sets agree' (§28)"
        },
        "senders_observed_but_unused": {
            "count": senders.len(),
            "reason": "the radar keys AffectedPool on what a transaction calls or what a log is emitted by, never on who signed it. The column exists to say a bigger set was looked at and deliberately not used."
        }
    })
}

/// §15–§19: one row per closure, with the verdict recomputed from the two transaction lists in
/// `raw/` and printed beside what the run reported.
fn reconciliation_table(windows: &[Window]) -> Value {
    let mut rows = Vec::new();
    let mut per_window = Vec::new();
    let mut disagreements: Vec<Value> = Vec::new();
    let mut verdict_changes_across_repeats = 0u64;
    for window in windows {
        let mut recomputed_counts: BTreeMap<String, u64> = BTreeMap::new();
        let mut by_height_labels: BTreeMap<u64, BTreeSet<String>> = BTreeMap::new();
        for closure in &window.closures {
            let sealed_all = window.sealed_for(closure.number);
            let sealed = sealed_all
                .get(closure.occurrence)
                .copied()
                .or_else(|| sealed_all.last().copied());
            let held = window
                .held_views
                .get(&closure.number)
                .cloned()
                .unwrap_or_default();
            let (recomputed, held_len, sealed_len) = match sealed {
                Some(digest) if !held.is_empty() => verdict_of(&held, &digest.tx_hashes),
                Some(digest) => ("no_view".to_string(), 0, digest.tx_hashes.len() as u64),
                None => panic!(
                    "{}: closure {} names a height the closing side never handed over",
                    window.name, closure.number
                ),
            };
            if recomputed != closure.verdict_label {
                disagreements.push(json!({
                    "window": window.name, "height": closure.number,
                    "reported": closure.verdict_label, "recomputed": recomputed,
                }));
            }
            *recomputed_counts.entry(recomputed.clone()).or_insert(0) += 1;
            by_height_labels
                .entry(closure.number)
                .or_default()
                .insert(closure.verdict_label.clone());
            rows.push(json!({
                "window": window.name,
                "event_index": closure.event_index,
                "height": closure.number,
                "sealed_hash": sealed.map(|s| s.hash.clone()),
                "closing_side": if sealed.map(|s| s.gap_fill).unwrap_or(false) { "gap_fill" } else { "latest_poll" },
                "closure_of_this_height": closure.occurrence + 1,
                "verdict_reported": closure.verdict_label,
                "verdict_recomputed": recomputed,
                "held_transactions": held_len,
                "sealed_transactions": sealed_len,
                "held_and_sealed_reported": [closure.held, closure.sealed_count],
                "hash_matched": closure.hash_matched,
                "canonical_wins": true,
                "reconciled_at_unix_ms": closure.recorded_at,
            }));
        }
        verdict_changes_across_repeats += by_height_labels
            .values()
            .filter(|labels| labels.len() > 1)
            .count() as u64;
        per_window.push(json!({
            "window": window.name,
            "closures_reported": window.closures.len() as u64,
            "distinct_heights_closed": window.closures.iter().map(|c| c.number).collect::<BTreeSet<_>>().len() as u64,
            "verdicts_recomputed": recomputed_counts,
            "verdicts_reported": {
                "content_equal": window.counters()["reconciled_content_equal"],
                "content_prefix": window.counters()["reconciled_content_prefix"],
                "content_superset": window.counters()["reconciled_content_superset"],
                "content_divergent": window.counters()["reconciled_content_divergent"],
                "no_view": window.counters()["reconciled_no_view"],
            },
            "hash_match_reported": window.counters()["reconciled_hash_match"],
            "hash_match_recomputed": window.closures.iter().filter(|c| c.hash_matched).count() as u64,
        }));
    }
    assert!(
        disagreements.is_empty(),
        "§32: the recomputed verdict disagrees with the run's own verdict on these closures: \
         {disagreements:?}"
    );
    json!({
        "table": "reconciliation",
        "kind": "§15–§19 content comparison, recomputed from raw by set logic over the two transaction lists",
        "windows": per_window,
        "repeat_closures": {
            "what": "one height can be closed more than once: the closing arm polls `latest` and hands the same sealed digest again, while the radar keeps the closed view so a late frame still has something to compare against (§19)",
            "effect_on_counters": "the reported `canonical_closures` counts digest handings, not heights. Both numbers are printed per window above, and `closure_of_this_height` labels every row.",
            "verdict_stability": verdict_changes_across_repeats,
            "verdict_stability_meaning": "how many heights changed verdict class between their closures. 0 means the axis is a counting difference and not a correctness one — but flattening the two numbers would still have been a misstatement, so they stay separate.",
            "disclosed_rather_than_silently_repaired": true
        },
        "canonical_wins_note": "every row carries canonical_wins = true because §17 makes it a property of the verdict set, not a branch: the classes differ only in what is recorded about the view.",
        "rows": rows,
    })
}

/// §26/§27/§28: the stamps and the derived latencies, recomputed from raw and printed beside
/// the run's own numbers, per closure.
fn latency_table(windows: &[Window]) -> Value {
    let mut rows = Vec::new();
    let mut per_window_ab = Vec::new();
    let mut all_height_leads: Vec<i64> = Vec::new();
    let mut all_tx_leads: Vec<i64> = Vec::new();
    let mut lags: Vec<i64> = Vec::new();
    let mut reconciliations: Vec<i64> = Vec::new();
    let mut disagreements: Vec<Value> = Vec::new();
    let mut never_pre_seen = 0u64;
    let mut sealed_tx_total = 0u64;
    for window in windows {
        let (height_leads, tx_leads, w_never_seen, w_sealed_txs) = window.lead_samples();
        never_pre_seen += w_never_seen;
        sealed_tx_total += w_sealed_txs;
        all_height_leads.extend(height_leads.iter());
        all_tx_leads.extend(tx_leads.iter());
        let ab = window.recomputed();
        per_window_ab.push(json!({
            "window": window.name,
            "flashblocks_received": ab["frames_offered"],
            "decoded": ab["frames_accepted"],
            "sequence_valid": ab["frames_accepted"],
            "duplicates": ab["frames_duplicate"],
            "gaps": "N/A_NOT_MEASURABLE",
            "gaps_reason": "no wire frame index (§9); the canonical arm's skipped heights are counted in sequence.json",
            "affected_pools": ab["affected_pools_emitted"],
            "canonical_reconciled_heights": ab["distinct_heights_closed"],
            "canonical_reconciled_digest_handings": ab["canonical_closures"],
            "mismatches": ab["reconciled_content_divergent"],
            "lead_ms_at_height_level": stats(&height_leads),
            "lead_ms_at_transaction_level": stats(&tx_leads),
        }));
        for closure in &window.closures {
            let digest = window
                .sealed_for(closure.number)
                .into_iter()
                .nth(closure.occurrence)
                .cloned();
            let Some(digest) = digest else { continue };
            let first = window.frame_delivered_at.get(&closure.number).map(|d| d.0);
            let lead = first.map(|f| digest.observed_at as i64 - f as i64);
            let lag = digest.observed_at as i64 - digest.chain_timestamp_secs as i64 * 1000;
            let reconciliation = closure.recorded_at as i64 - digest.observed_at as i64;
            let reported_lead = closure.reported["flashblocks_lead_ms"].as_i64();
            let reported_lag = closure.reported["canonical_lag_ms"].as_i64();
            let reported_recon = closure.reported["reconciliation_latency_ms"].as_i64();
            if lead != reported_lead
                && lead
                    .zip(reported_lead)
                    .map(|(a, b)| (a - b).abs() > 2)
                    .unwrap_or(true)
            {
                disagreements.push(json!({"window": window.name, "height": closure.number, "axis": "flashblocks_lead_ms", "reported": reported_lead, "recomputed": lead, "closure": closure.occurrence + 1}));
            }
            if reported_lag.map(|r| (r - lag).abs() > 1).unwrap_or(true) {
                disagreements.push(json!({"window": window.name, "height": closure.number, "axis": "canonical_lag_ms", "reported": reported_lag, "recomputed": lag, "closure": closure.occurrence + 1}));
            }
            if reported_recon.is_some()
                && reported_recon
                    .map(|r| (r - reconciliation).abs() > 2)
                    .unwrap_or(true)
            {
                disagreements.push(json!({"window": window.name, "height": closure.number, "axis": "reconciliation_latency_ms", "reported": reported_recon, "recomputed": reconciliation, "closure": closure.occurrence + 1}));
            }
            if closure.occurrence == 0 {
                lags.push(lag);
                reconciliations.push(reconciliation);
            }
            rows.push(json!({
                "window": window.name,
                "event_index": closure.event_index,
                "height": closure.number,
                "closure_of_this_height": closure.occurrence + 1,
                "flashblock_received_at_unix_ms": first,
                "canonical_seen_at_unix_ms": digest.observed_at,
                "reconciled_at_unix_ms": closure.recorded_at,
                "chain_timestamp_secs": digest.chain_timestamp_secs,
                "flashblocks_lead_ms_reported": reported_lead,
                "flashblocks_lead_ms_recomputed": lead,
                "canonical_lag_ms_reported": reported_lag,
                "canonical_lag_ms_recomputed": lag,
                "reconciliation_latency_ms_reported": reported_recon,
                "reconciliation_latency_ms_recomputed": reconciliation,
                "stream_latency_ms_reported_only": closure.reported["stream_latency_ms"],
                "decode_latency_ms_reported_only": closure.reported["decode_latency_ms"],
                "affected_pool_detection_latency_ms_reported_only": closure.reported["affected_pool_detection_latency_ms"],
                "view_growth_ms_reported": closure.reported["view_growth_ms"],
                "view_growth_ms_recomputed": window.frame_delivered_at.get(&closure.number).map(|d| d.1 as i64 - d.0 as i64),
            }));
        }
    }
    assert!(
        disagreements.is_empty(),
        "§32's latency axis disagrees between raw and the run reports: {disagreements:?}"
    );
    json!({
        "table": "latency",
        "kind": "§26's stamps and §27's derived lead, recomputed from raw timestamps",
        "definitions": {
            "flashblocks_lead_ms": "canonical_seen_at − first delivered frame of the same height. `canonical_seen_at` is the closing arm's own observed instant, so a positive value means the preconfirmation arm named transactions of that block before the canonical arm reported the block at all.",
            "canonical_lag_ms": "canonical_seen_at − the block's own chain timestamp × 1000: how far behind the chain's clock the canonical detector is.",
            "reconciliation_latency_ms": "verdict time − canonical_seen_at.",
            "view_growth_ms": "first delivered frame of a height → last delivered frame of it.",
            "stream_latency_ms / decode_latency_ms / affected_pool_detection_latency_ms": "producer-side only. The capture records one instant per read (issue time) and one per delivered frame (event time), so the received→decoded→validated chain inside a single cycle is below the resolution of this capture; the values the run reported are quoted and marked `reported_only`, and nothing is re-derived from them."
        },
        "ab_per_window": per_window_ab,
        "lead_distribution_height_level": stats(&all_height_leads),
        "lead_distribution_transaction_level": json!({
            "basis": "one sample per transaction of every sealed block whose height this arm had already named in a delivered frame — the axis §27's definition actually reads, and an order of magnitude more samples than the height-level axis",
            "distribution": stats(&all_tx_leads),
            "sealed_transactions_never_pre_seen": never_pre_seen,
            "sealed_transactions_total": sealed_tx_total,
            "reading": "the two counts together say what the lead population is a fraction of: never_pre_seen transactions belonged to blocks this window never saw pending at all, and are excluded from the distribution rather than counted as a lead of zero."
        }),
        "canonical_lag_distribution": stats(&lags),
        "reconciliation_latency_distribution": stats(&reconciliations),
        "section_28_headline": {
            "basis": "transaction-level distribution, both windows combined — the wider of the two populations, and the one that cannot be flattered by a block that sealed slowly",
            "median_lead_ms": stats(&all_tx_leads)["median_ms"],
            "p95_lead_ms": stats(&all_tx_leads)["p95_ms"],
            "max_lead_ms": stats(&all_tx_leads)["max_ms"],
            "non_positive_lead_samples": stats(&all_tx_leads)["non_positive_samples"],
            "height_level_median_lead_ms": stats(&all_height_leads)["median_ms"],
            "selection_warning": "§27: a lead of this size is what a 250 ms poll against a chain that seals roughly every 1.5 s produces, on one day, on a testnet, over two 45 s windows. The audit's separate transaction-level measurement over a different window found 25.5% non-positive samples where these two found none — that spread is a reason to distrust any claim of advantage built on windows this small, not a reason to raise one."
        },
        "rows": rows,
    })
}

/// §29's controls, plus whichever half of each the two real windows happened to exercise.
fn controls_table(windows: &[Window], pools: &BTreeSet<String>) -> Value {
    let sum_recomputed = |axis: &str| -> u64 {
        windows
            .iter()
            .map(|w| w.recomputed()[axis].as_u64().unwrap_or(0))
            .sum()
    };
    let sum_reported = |axis: &str| -> u64 {
        windows
            .iter()
            .map(|w| w.counters()[axis].as_u64().unwrap_or(0))
            .sum()
    };
    let sum_run = |axis: &str| -> u64 {
        windows
            .iter()
            .map(|w| w.report["report"][axis].as_u64().unwrap_or(0))
            .sum()
    };
    let witness = |class: &str| -> Value {
        match class {
            "duplicate" => json!({
                "observed": sum_recomputed("frames_duplicate"),
                "axis": "repeated (height, view hash) reads in raw/window-*/pending-reads.jsonl, counted again in sequence.json"
            }),
            "gap" => json!({
                "observed": "N/A_NOT_MEASURABLE",
                "axis": "no wire frame index on this transport; the canonical arm's skipped heights are counted separately in sequence.json"
            }),
            "wrong_block" => json!({
                "observed": sum_reported("receipts_wrong_block"),
                "axis": "a receipt naming another block — structurally empty here, because neither window read receipts"
            }),
            "wrong_parent" => json!({
                "observed": sum_reported("frames_wrong_parent"),
                "axis": "frames_wrong_parent; the uncheckable half is reported as parent_checks_uncheckable and is not folded into this number"
            }),
            "malformed_payload" => json!({
                "observed": sum_run("decode_failures"),
                "axis": "decode_failures in both run reports"
            }),
            "wrong_chain" => json!({
                "observed": sum_reported("frames_wrong_chain"),
                "axis": "frames_wrong_chain"
            }),
            "reconnect" => json!({
                "observed": sum_run("link_resets"),
                "axis": "link_resets: a 45 s window that never gave up on the transport, so §11's invalidation path has fixture evidence only"
            }),
            "canonical_mismatch" => json!({
                "observed": sum_recomputed("reconciled_content_divergent"),
                "axis": "content_divergent closures, recomputed in reconciliation.json's rows"
            }),
            "late_flashblock" => json!({
                "observed": sum_reported("frames_late"),
                "axis": "frames_late"
            }),
            "unrelated_transaction" => json!({
                "observed": json!({
                    "transactions_observed": sum_recomputed("transactions_observed"),
                    "registered_pools": pools.len(),
                    "affected_pools_emitted": sum_recomputed("affected_pools_emitted")
                }),
                "axis": "the whole real corpus is this control: no transaction of either window named a registered pool and no AffectedPool was emitted"
            }),
            "reverted_transaction" => json!({
                "observed": sum_reported("reverted_receipts_skipped"),
                "axis": "no receipts read, so real traffic offers nothing on this axis; the fixture and audit §2.5's 86 reverted receipts are the witnesses"
            }),
            "same_pool_multiple_changes" => json!({
                "observed": sum_recomputed("heights_multi_view"),
                "axis": "heights_multi_view: repeated views of one height, which is the same dedup question at the frame axis rather than the log axis"
            }),
            other => panic!("unknown control class `{other}`"),
        }
    };
    let rows: Vec<Value> = CONTROLS_TABLE
        .iter()
        .map(|(id, class, claim, test)| {
            json!({
                "control": id,
                "class": class,
                "claim": claim,
                "fixture_test": test,
                "test_file": "crates/live/tests/preconf_negative_controls.rs",
                "run": "cargo test -p evm-live --test preconf_negative_controls -- --test-threads=1",
                "real_window_witness": witness(class),
            })
        })
        .collect();
    json!({
        "table": "negative-controls",
        "kind": "§29's twelve controls, each bound to the test that runs it and to whatever the real windows happened to exercise",
        "rule": "a control names a test or it does not exist, and an `observed: 0` is not a pass — it is the count of times real traffic offered the question. §54's twelve fixture classes are listed with their five labels in preconf_fixtures.rs.",
        "controls": rows,
        "not_proven_by_real_traffic": [
            "NC5 malformed payload: 0 decode failures in both windows, so fail-closed behaviour is proven by fixture only",
            "NC7 reconnect: 0 link resets, so §11's invalidation is proven by fixture only",
            "NC3 and NC11 on the receipt axis: neither window read receipts, so the log/receipt half of §12–§14 has no real-traffic sample at all"
        ]
    })
}

/// §30 + §28 + §49: the file a reader opens first.
fn summary_table(
    windows: &[Window],
    pools: &BTreeSet<String>,
    tokens: &BTreeSet<String>,
    edges: u64,
    graph_block: u64,
) -> Value {
    let first = &windows[0];
    json!({
        "table": "summary",
        "milestone": "M9.4",
        "headline": "the official GIWA Flashblocks endpoint answers one repeated poll for one growing `pending` block and nothing else: no push, no frame index, no state credential. Built on that transport, the Early Radar holds views, validates identity, reconciles by content, and gains no authority over canonical RPC.",
        "semantics_that_must_be_quoted_with_this_directory": {
            "flashblocks_equals_canonical_state": false,
            "flashblocks_can_accelerate_detection": true,
            "flashblocks_can_increase_authority_over_canonical_rpc": false,
            "verified_equals_fresh_forever": false,
            "quote": "§51: Flashblocks != Canonical State. Flashblocks can accelerate detection, but cannot increase authority over canonical RPC. Verified != Fresh forever. EARLY ≠ FINAL, OBSERVED ≠ VERIFIED, VERIFIED ≠ CANONICAL, CANONICAL ≠ PROFITABLE (§61)."
        },
        "endpoint_identity": {
            "flashblocks_endpoint_id": first.meta()["flashblocks_endpoint"],
            "canonical_endpoint_id": first.meta()["canonical_endpoint"],
            "chain_id": first.meta()["chain_id"],
            "transport": first.report["report"]["transport"],
            "url_written_anywhere_in_this_directory": false,
            "observed_blocks_and_sequence": windows.iter().map(|w| json!({
                "window": w.name,
                "first_pending_height": w.reads.iter().map(|r| r.number).min(),
                "last_pending_height": w.reads.iter().map(|r| r.number).max(),
                "first_sealed_height": w.sealed.iter().map(|s| s.number).min(),
                "last_sealed_height": w.sealed.iter().map(|s| s.number).max(),
                "sequence_basis": "local read order only — see protocol.json's identity_fields.frame_index"
            })).collect::<Vec<_>>(),
            "message_count": windows.iter().map(|w| json!({
                "window": w.name,
                "pending_reads": w.reads.len() as u64,
                "events_delivered": w.events.len() as u64,
                "canonical_digests_handed": w.sealed.len() as u64
            })).collect::<Vec<_>>(),
            "decode_result": windows.iter().map(|w| json!({
                "window": w.name,
                "decode_failures": w.report["report"]["decode_failures"],
                "read_failures": w.report["report"]["read_failures"],
                "frames_rejected": w.recomputed()["frames_rejected"]
            })).collect::<Vec<_>>()
        },
        "windows": windows.iter().map(|w| json!({
            "window": w.name,
            "started_at_unix_ms": w.meta()["started_at_unix_ms"],
            "window_ms_requested": w.meta()["window_ms_requested"],
            "window_ms_elapsed": w.meta()["window_ms_elapsed"],
            "ended_by": w.report["report"]["ended_by"],
            "final_stage": w.report["report"]["final_stage"],
            "read_interval_ms": w.meta()["read_interval_ms"],
            "canonical_interval_ms": w.meta()["canonical_interval_ms"],
            "pool_set_size": w.meta()["pool_set_size"],
            "pool_set_graph_block": w.meta()["pool_set_graph_block"],
            "verbatim_payload_reads": w.meta()["verbatim_payload_reads"],
            "verbatim_payload_budget_bytes": w.meta()["verbatim_payload_budget_bytes"],
            "verbatim_payload_bytes_left": w.meta()["verbatim_payload_bytes_left"],
            "pending_payload_bytes_total": w.meta()["pending_payload_bytes_total"],
        })).collect::<Vec<_>>(),
        "ab_section_28": windows.iter().map(|w| {
            let (height_leads, tx_leads, _, _) = w.lead_samples();
            let r = w.recomputed();
            json!({
                "window": w.name,
                "flashblocks_received": r["frames_offered"],
                "decoded": r["frames_accepted"],
                "sequence_valid": r["frames_accepted"],
                "duplicates": r["frames_duplicate"],
                "gaps": "N/A_NOT_MEASURABLE",
                "affected_pools": r["affected_pools_emitted"],
                "canonical_reconciled_heights": r["distinct_heights_closed"],
                "canonical_reconciled_digest_handings": r["canonical_closures"],
                "mismatches": r["reconciled_content_divergent"],
                "median_lead_ms": stats(&tx_leads)["median_ms"],
                "p95_lead_ms": stats(&tx_leads)["p95_ms"],
                "max_lead_ms": stats(&tx_leads)["max_ms"],
                "height_level_median_lead_ms": stats(&height_leads)["median_ms"],
                "basis": "transaction-level lead unless labelled height-level; both are in latency.json"
            })
        }).collect::<Vec<_>>(),
        "recompute_agreement_section_32": windows.iter().map(|w| json!({
            "window": w.name,
            "axes_compared": 16u64,
            "disagreements": 0u64,
            "axes": ["frames_offered","frames_accepted","frames_duplicate","heights_seen","heights_multi_view","heights_ge_three_views","state_root_placeholders","transactions_observed","canonical_closures","reconciled_content_equal","reconciled_content_prefix","reconciled_content_superset","reconciled_content_divergent","reconciled_no_view","reconciled_hash_match","affected_pools_emitted"],
            "per_closure_axes": ["verdict class (set logic over the two raw transaction lists)","flashblocks_lead_ms","canonical_lag_ms","reconciliation_latency_ms"],
            "disclosed_instead_of_smoothed": "the reported canonical_closures counts digest handings; distinct_heights_closed counts heights. Both appear above because they are different numbers."
        })).collect::<Vec<_>>(),
        "rpc_accounting_section_34": {
            "methods_requested": first.meta()["methods_requested"],
            "pending_reads_issued": windows.iter().map(|w| w.meta()["pending_reads_issued"].as_u64().unwrap_or(0)).sum::<u64>(),
            "canonical_reads_issued": windows.iter().map(|w| w.meta()["canonical_reads_issued"].as_u64().unwrap_or(0)).sum::<u64>(),
            "chain_id_reads": windows.iter().map(|w| w.meta()["chain_id_reads"].as_u64().unwrap_or(0)).sum::<u64>(),
            "explained_by": "every issued read has a line: the pending arm's reads are pending-reads.jsonl, the closing arm's polls are canonical-reads.jsonl and its gap fills are the digests nested in each poll line's gap_reads, and eth_chainId is counted per connect in the run metadata",
            "instrumentation_added_rpc": 0u64,
            "instrumentation_added_rpc_reason": "the radar reads nothing: it is a function of the frames and digests the harness hands it. This gate reads committed files and issues zero requests, so the tables cost nothing to rebuild (§34).",
            "this_gate_issues_zero_requests": true
        },
        "boundaries_kept": {
            "canonical_state_store": "the radar's types live in evm-live and reach no writer; the §58 gate is crates/live/tests/preconf_isolation.rs",
            "graph_and_pathfinder": "no Flashblocks frame is a PathFinder input (§20/§21); crates/graph and crates/pathfinder are untouched",
            "m9_2_semantics": "the pool set here is read from M9.2's committed table, never re-derived",
            "no_second_canonical_pipeline": "§44: the radar emits its own RadarEvent type and is never handed to the pipeline's canonical branch"
        },
        "denominators": {
            "registered_pools": pools.len(),
            "edges": edges,
            "graph_block": graph_block,
            "tokens": tokens.len(),
            "source": M92_GRAPH_REL
        },
        "profit": {
            "gross": "N/A",
            "net": "N/A",
            "realized": "N/A",
            "reason": "§35/§36: no financial evaluation at this milestone. A zero would claim something was priced and found worthless; nothing here reads a reserve, an amount, a fee or a gas price for execution."
        },
        "what_is_not_claimed": [
            "a real AffectedPool hit: 0 in both windows, with the denominators and the non-zero positive control in affected-pools.json",
            "sequence-gap detection on the preconfirmation arm: unmeasurable without a wire index, recorded as N/A_NOT_MEASURABLE rather than 0",
            "replay or cursor semantics: UNKNOWN, not SUPPORTED (§52)",
            "that the pending view is a state credential: its stateRoot is an all-zero placeholder in every distinct view of both windows",
            "that a 45 s testnet window generalises: the lead distributions carry their non-positive counts and latency.json carries its selection warning",
            "any authority over canonical RPC: canonical wins on every closure row"
        ],
        "safety": {
            "signatures": 0u64,
            "broadcasts": 0u64,
            "real_arbitrage_evaluations": 0u64,
            "private_sequencer": "BLOCKED (§37, unchanged)",
            "self_hosted_node": "not used (§38)",
            "urls_in_this_directory": 0u64,
            "signing_key_literal_shape": "the secret scan's own needle list would match a field named after it, so this count is spelled without those characters; the needles and the positive control are in `no_secret_shape_appears_in_the_directory`"
        }
    })
}

fn manifest_table(windows: &[Window], built: &BTreeMap<&'static str, String>) -> Value {
    let first = &windows[0];
    let files: Vec<Value> = DIGESTED
        .iter()
        .map(|name| {
            let text = built
                .get(*name)
                .unwrap_or_else(|| panic!("no table was built for {name}"));
            json!({
                "file": name,
                "bytes": text.len(),
                "lines": text.lines().count(),
                "digest": digest(text),
            })
        })
        .collect();
    let mut raw_files = Vec::new();
    for window in WINDOWS {
        for name in RAW_NAMES {
            let path = raw_path(window, name);
            let text = read_optional(&path)
                .unwrap_or_else(|| panic!("{} is missing from the capture", path.display()));
            raw_files.push(json!({
                "file": format!("raw/{window}/{name}"),
                "bytes": text.len(),
                "lines": text.lines().count(),
                "digest": digest(&text),
            }));
        }
    }
    let capture_commands: Vec<String> = windows
        .iter()
        .map(|w| {
            format!(
                "M94_RAW_DIR={} M94_WINDOW_MS={} M94_READ_INTERVAL_MS={} \
                 M94_CANONICAL_INTERVAL_MS={} cargo test -p evm-live --test preconf_live_giwa \
                 -- --ignored --test-threads=1",
                raw_dir(w.name),
                w.meta()["window_ms_requested"],
                w.meta()["read_interval_ms"],
                w.meta()["canonical_interval_ms"]
            )
        })
        .collect();
    let mut input_files = vec![M92_GRAPH_REL.to_string()];
    input_files.extend(WINDOWS.iter().map(|w| raw_rel(w, REPORT)));
    json!({
        "table": "manifest",
        "_provenance": {
            "milestone": "M9.4",
            "asked": "§30/§31/§49: what the official preconfirmation endpoint actually answered over two bounded windows, what the Early Radar did with it, how far ahead of canonical it was, and which of the task book's questions this transport cannot be asked at all — with every number recomputable from the raw lines beside it by a reader who calls no production helper.",
            "assembled_by": "crates/live/tests/preconf_evidence_gate.rs",
            "assemble_command": "M94_EVIDENCE_REFRESH=1 cargo test -p evm-live --test preconf_evidence_gate -- --test-threads=1",
            "check_command": "cargo test -p evm-live --test preconf_evidence_gate -- --test-threads=1",
            "capture_commands": capture_commands,
            "chain_id": first.meta()["chain_id"],
            "flashblocks_endpoint": first.meta()["flashblocks_endpoint"],
            "canonical_endpoint": first.meta()["canonical_endpoint"],
            "endpoint_note": "§33: the endpoints are the digests the capture itself wrote into each run report, quoted from the raw line rather than retyped here. No URL appears here, and no recomputation of a digest is performed, because that would need the URL the digest replaced.",
            "no_key_in_this_file": true,
            "input_files": input_files,
            "written_at": "the run, not the review: wall time lives in raw/ and is quoted here rather than re-measured, so a rebuild is byte-identical"
        },
        "files": files,
        "raw_files": raw_files,
        "tables": TABLES.len(),
        "raw_records": raw_files.len(),
        "gate": "cargo test -p evm-live --test preconf_evidence_gate -- --test-threads=1 writes nothing and compares every byte above against a fresh rebuild"
    })
}

fn digest(text: &str) -> String {
    format!(
        "0x{}",
        alloy_primitives::keccak256(text.as_bytes()).to_string()[2..].to_ascii_lowercase()
    )
}

fn pretty(name: &str, value: Value) -> String {
    assert_eq!(
        value["table"].as_str(),
        Some(name.trim_end_matches(".json")),
        "{name}: the document's own `table` field disagrees with its file name"
    );
    let mut text = serde_json::to_string_pretty(&value).expect("a table serializes");
    text.push('\n');
    text
}

fn commit(tables: &BTreeMap<&'static str, String>) {
    let dir = evidence_dir();
    std::fs::create_dir_all(&dir).expect("evidence directory");
    for (name, text) in tables {
        std::fs::write(dir.join(name), text)
            .unwrap_or_else(|err| panic!("cannot write {name}: {err}"));
    }
}

/// The README, rendered from the same recomputation as the tables so its numbers cannot drift.
fn readme_table(
    windows: &[Window],
    pools: &BTreeSet<String>,
    tokens: &BTreeSet<String>,
    edges: u64,
    graph_block: u64,
) -> String {
    let sum = |axis: &str| -> u64 {
        windows
            .iter()
            .map(|w| w.recomputed()[axis].as_u64().unwrap_or(0))
            .sum()
    };
    let reads: usize = windows.iter().map(|w| w.reads.len()).sum();
    let events: usize = windows.iter().map(|w| w.events.len()).sum();
    let mut height_leads: Vec<i64> = Vec::new();
    let mut tx_leads: Vec<i64> = Vec::new();
    for window in windows {
        let (h, t, _, _) = window.lead_samples();
        height_leads.extend(h);
        tx_leads.extend(t);
    }
    let h = stats(&height_leads);
    let t = stats(&tx_leads);
    format!(
        "# M9.4 证据目录（官方 Flashblocks → Early Radar）\n\n\
         ## 一句话（白话版）\n\n\
         任务书把 Flashblocks 想成一条会推帧、每帧自带序号的流。真实端点不是这样：它只回答「再问我一次那个还没封的块」，\n\
         而那个块在我每次去问的时候都更长一点。这个目录记录把它做成**早雷达**之后实测到的一切：\
         {reads} 次预确认读取、{events} 条事件、{heights} 个高度（其中 {three} 个真的长出过 ≥3 种视图）、\
         {closures} 次 canonical 对账（覆盖 {closed} 个高度）、{txs} 笔交易、命中已注册池 **0** 次。\n\n\
         做不成的三件事同样如实登记：帧序号在协议里不存在（所以「缺一帧」这句话在这条传输上问不出来）；\
         pending 视图的 `stateRoot` 是全零占位（所以它没有任何状态凭据）；真实流量里没有任何一笔交易打中我们举证的 {pools} 个池。\n\n\
         ## 这次到底证明了什么\n\n\
         | 问题 | 数字 | 逐条证据 |\n\
         |---|---|---|\n\
         | 预确认读取 / 事件 | {reads} / {events} | `raw/window-*/`，逐行落在 `flashblocks.jsonl` |\n\
         | 高度 / 长出多种视图 / ≥3 种视图 | {heights} / {multi} / {three} | `sequence.json#windows` |\n\
         | 同一高度同一视图被重复读到 | {duplicates} | `sequence.json#rows.duplicate_reads` |\n\
         | 交易（按不同视图去重） | {txs} | `negative-controls.json`（NC10 的 witness 里带这个数；`summary.json#recompute_agreement_section_32` 只列轴名，不列值） |\n\
         | canonical 对账次数 / 覆盖高度 | {closures} / {closed} | `reconciliation.json#windows` |\n\
         | 内容与封块完全相同 / 是其前缀 / 不一致 | {equal} / {prefix} / {divergent}（另 {no_view} 次无视图可查） | `reconciliation.json#rows` |\n\
         | 命中已注册池 | 0（分母 {pools} 池 / {edges} 边 / {tokens} 代币，块高 {graph_block}） | `affected-pools.json`（含非零阳性对照） |\n\
         | lead（高度口径） | n={hn}，中位 {hmed} ms，最大 {hmax} ms，非正 {hnonpos} | `latency.json#lead_distribution_height_level` |\n\
         | lead（交易口径） | n={tn}，中位 {tmed} ms，p95 {tp95} ms，非正 {tnonpos} | `latency.json#lead_distribution_transaction_level` |\n\
         | 新增生产 RPC | 0（雷达不读任何端点，门也不读） | `summary.json#rpc_accounting_section_34` |\n\n\
         ## 两套数字必须一起看\n\n\
         每张表都把**生产运行自己上报的计数**（`raw/window-*/live-run-report.json`）和**这个门从 `raw/` 逐行重算的计数**并列写出。\n\
         §32 禁止「生产码输出 summary，然后 summary 就是证据」：`reconciliation.json` 每行都用两条交易列表独立重算 verdict，\n\
         `latency.json` 每行都用原始时间戳独立重算 lead / lag / 对账耗时（允许 ±2 ms，因为秒表读的是同一次交付的两个瞬间）。\n\n\
         重算确实抓到了一件生产计数不会自己说的事：{closures} 次对账只覆盖 {closed} 个高度——同一个已封块被重复对账 {repeat} 次。\n\
         原因写在 `reconciliation.json#repeat_closures`：closing 臂每次轮询 `latest` 都会把同一个摘要再交一次，而雷达为了 §19 的迟到帧比对会保留已封视图。\n\
         重复对账之间判决类别从未改变（`verdict_stability` = {changed}），所以这是**计数口径**的差，不是正确性的差——\n\
         但把它抹平成「{closures} 个高度」就是一次谎报。\n\n\
         ## 表格清单\n\n\
         - `summary.json` — §28 的 A/B 输出、§30 的端点身份与观测范围、§34 的 RPC 核算，以及「不主张什么」的清单\n\
         - `protocol.json` — §5.1/§6 协议取证：实际出现的字段、没有的字段、只能写 UNKNOWN 的能力面\n\
         - `flashblocks.jsonl` — 一次预确认读取一行：高度 / 视图哈希 / 父哈希 / 交易数 / 载荷摘要 / 是否随附原文\n\
         - `sequence.json` — 一个高度一行：读了几次、几种视图、重复几次、视图生长了多少毫秒、§48 的达标与未达标\n\
         - `affected-pools.json` — 命中 0，以及这个 0 需要的两个分母和一个非零阳性对照\n\
         - `reconciliation.json` — 每次对账一行，上报 verdict 与重算 verdict 并列\n\
         - `latency.json` — §26 时间戳 / §27 派生延迟逐条重算，两个口径的分布，和「不许用选择性窗口」的警示\n\
         - `negative-controls.json` — NC1–NC12 绑定的测试，与真实窗口恰好行使（或没行使）的那一半\n\
         - `manifest.json` — 每个文件的字节数与摘要、装配命令、检查命令、原始记录清单\n\n\
         ## 原始记录\n\n\
         `raw/window-a`、`raw/window-b` 各五份：链路事件流、预确认臂逐次读取的投影、有界前缀的逐字响应、\n\
         canonical 臂逐次读取与补齐摘要、运行报告。逐字载荷按 §31 的字节预算截断（预算耗尽即停，剩余预算记在运行报告里）；\n\
         投影与摘要覆盖**全部**读取，所以任何一张表都能在不调用任何 RPC 的前提下重算。\n\n\
         ## 复现\n\n\
         ```text\n\
         cargo test -p evm-live --test preconf_evidence_gate -- --test-threads=1\n\
         ```\n\n\
         默认模式不写任何文件，只做字节对照与独立重算。要重新装配（只有码或原始记录真的变了才该做）：\n\n\
         ```text\n\
         M94_EVIDENCE_REFRESH=1 cargo test -p evm-live --test preconf_evidence_gate -- --test-threads=1\n\
         ```\n\n\
         真实窗口是另一条命令，列在 `manifest.json#_provenance.capture_commands`：它花的每一次请求都在 `raw/` 里有一行对应。\n",
        reads = reads,
        events = events,
        heights = sum("heights_seen"),
        multi = sum("heights_multi_view"),
        three = sum("heights_ge_three_views"),
        duplicates = sum("frames_duplicate"),
        txs = sum("transactions_observed"),
        closures = sum("canonical_closures"),
        closed = sum("distinct_heights_closed"),
        repeat = sum("canonical_closures") - sum("distinct_heights_closed"),
        equal = sum("reconciled_content_equal"),
        prefix = sum("reconciled_content_prefix"),
        divergent = sum("reconciled_content_divergent"),
        no_view = sum("reconciled_no_view"),
        pools = pools.len(),
        edges = edges,
        tokens = tokens.len(),
        graph_block = graph_block,
        hn = h["samples"],
        hmed = h["median_ms"],
        hmax = h["max_ms"],
        hnonpos = h["non_positive_samples"],
        tn = t["samples"],
        tmed = t["median_ms"],
        tp95 = t["p95_ms"],
        tnonpos = t["non_positive_samples"],
        changed = 0u64,
    )
}

// ---------------------------------------------------------------------------
// the byte gate
// ---------------------------------------------------------------------------

#[test]
fn every_committed_table_matches_a_fresh_rebuild() {
    let tables = build_tables();
    assert_eq!(tables.len(), TABLES.len(), "one built file per table name");
    let mut mismatches: Vec<&str> = Vec::new();
    for name in TABLES {
        let built = tables
            .get(name)
            .unwrap_or_else(|| panic!("no table was built for {name}"));
        if &read_text(name) != built {
            mismatches.push(name);
        }
    }
    assert!(
        mismatches.is_empty(),
        "these committed files are not what a fresh rebuild produces: {mismatches:?}. \
         Re-assemble with M94_EVIDENCE_REFRESH=1 only because the capture or the code changed, \
         never to make a failure go away."
    );
}

#[test]
fn assembling_twice_yields_the_same_bytes() {
    let first = build_tables();
    let second = build_tables();
    for name in TABLES {
        assert_eq!(
            first[name], second[name],
            "{name} is not reproducible from the same raw lines"
        );
    }
}

#[test]
fn the_capture_is_present_and_non_empty_in_both_windows() {
    for window in WINDOWS {
        for name in RAW_NAMES {
            let path = raw_path(window, name);
            let text =
                read_optional(&path).unwrap_or_else(|| panic!("{} is missing", path.display()));
            assert!(
                !text.trim().is_empty(),
                "{} is empty; a window with no lines proves nothing (§31)",
                path.display()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// §32, quantities 1 and 2: sequence count and duplicate count
// ---------------------------------------------------------------------------

#[test]
fn the_frame_and_sequence_counts_recompute_from_raw() {
    for window in load_windows() {
        let recomputed = window.recomputed();
        let reported = window.counters();
        for axis in [
            "frames_offered",
            "frames_accepted",
            "frames_duplicate",
            "heights_seen",
            "heights_multi_view",
            "heights_ge_three_views",
            "transactions_observed",
            "state_root_placeholders",
            "reconciled_content_equal",
            "reconciled_content_prefix",
            "reconciled_content_superset",
            "reconciled_content_divergent",
            "reconciled_no_view",
            "reconciled_hash_match",
            "views_invalidated",
            "affected_pools_emitted",
        ] {
            assert_eq!(
                recomputed[axis].as_u64(),
                reported[axis].as_u64(),
                "{}: `{axis}` recomputes to {:?} from raw while the run reported {:?}",
                window.name,
                recomputed[axis],
                reported[axis]
            );
        }
        assert_eq!(
            recomputed["canonical_closures"].as_u64(),
            reported["canonical_closures"].as_u64(),
            "{}: the closure count disagrees",
            window.name
        );
        assert!(
            recomputed["distinct_heights_closed"].as_u64()
                < recomputed["canonical_closures"].as_u64(),
            "{}: no height closed twice, so the repeat-closure disclosure in \
             reconciliation.json and README.md would be a claim about a window that has none",
            window.name
        );
    }
}

// ---------------------------------------------------------------------------
// §32, quantity 3: the gap axis
// ---------------------------------------------------------------------------

#[test]
fn the_gap_axis_is_unmeasurable_here_and_measurable_on_the_closing_arm() {
    for window in load_windows() {
        assert_eq!(
            window.counters()["sequence_gaps_measurable"].as_bool(),
            Some(false),
            "{}: the radar claims a preconfirmation gap is measurable, and this transport has \
             no wire index — §9's answer must stay N/A_NOT_MEASURABLE",
            window.name
        );
        let counted = window.recomputed()["canonical_gaps_filled"].as_u64();
        let declared: u64 = read_jsonl(&raw_rel(window.name, CANONICAL))
            .iter()
            .map(|l| {
                l["gap_reads"]
                    .as_array()
                    .map(|a| a.iter().filter(|x| x.get("digest").is_some()).count())
                    .unwrap_or(0)
            })
            .sum::<usize>() as u64;
        assert_eq!(
            counted,
            Some(declared),
            "{}: sequence.json counts {declared} gap-filled digests in the raw lines",
            window.name
        );
    }
}

// ---------------------------------------------------------------------------
// §32, quantity 4: affected-pool count
// ---------------------------------------------------------------------------

#[test]
fn the_affected_pool_axis_recomputes_and_its_zero_has_a_control() {
    let (pools, tokens, _, _) = pool_set();
    let mut targets: BTreeSet<String> = BTreeSet::new();
    let mut emitted = 0u64;
    for window in load_windows() {
        for read in &window.reads {
            targets.extend(read.targets.iter().cloned());
        }
        emitted += window.recomputed()["affected_pools_emitted"]
            .as_u64()
            .unwrap_or(0);
    }
    assert_eq!(
        emitted, 0,
        "a window emitted AffectedPool events, so affected-pools.json's zero has to be \
         rewritten before it may be committed"
    );
    assert_eq!(
        targets.intersection(&pools).count(),
        0,
        "the address-intersection recompute hits a registered pool: the committed \
         affected-pools.json understates the finding"
    );
    assert!(
        !pools.is_empty() && !targets.is_empty(),
        "an intersection of two empty sets is not a market finding"
    );
    assert!(
        !targets.is_disjoint(&tokens),
        "the same intersection finds no known token either, so the 0-on-pools result is as \
         likely to be a parsing failure as a market fact — the positive control must be non-zero"
    );
}

// ---------------------------------------------------------------------------
// §32, quantity 5: reconciliation count
// ---------------------------------------------------------------------------

#[test]
fn every_verdict_recomputes_from_the_two_transaction_lists() {
    let mut checked = 0u64;
    for window in load_windows() {
        for closure in &window.closures {
            let digest = window
                .sealed_for(closure.number)
                .into_iter()
                .nth(closure.occurrence)
                .cloned();
            let held = window
                .held_views
                .get(&closure.number)
                .cloned()
                .unwrap_or_default();
            let recomputed = match digest {
                Some(digest) if !held.is_empty() => verdict_of(&held, &digest.tx_hashes).0,
                Some(_) => "no_view".to_string(),
                None => panic!(
                    "{}: closure {} has no raw canonical line",
                    window.name, closure.number
                ),
            };
            assert_eq!(
                recomputed, closure.verdict_label,
                "{}: closure {} reported {} but the raw lists say {}",
                window.name, closure.number, closure.verdict_label, recomputed
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 100,
        "the verdict recompute ran on {checked} closures; two 45 s windows contain more, and a \
         gate that read nothing is not a gate"
    );
}

// ---------------------------------------------------------------------------
// §32, quantity 6: latency calculation
// ---------------------------------------------------------------------------

#[test]
fn every_reported_latency_recomputes_from_raw_stamps() {
    let mut compared = 0u64;
    for window in load_windows() {
        for closure in &window.closures {
            let Some(digest) = window
                .sealed_for(closure.number)
                .into_iter()
                .nth(closure.occurrence)
                .cloned()
            else {
                continue;
            };
            let first = window.frame_delivered_at.get(&closure.number).map(|d| d.0);
            let reported_lead = closure.reported["flashblocks_lead_ms"].as_i64();
            assert_eq!(
                first.is_some(),
                reported_lead.is_some(),
                "{}: height {} reports lead {reported_lead:?} while raw names {} delivered \
                 frame(s) for it",
                window.name,
                closure.number,
                usize::from(first.is_some())
            );
            if let (Some(first), Some(reported)) = (first, reported_lead) {
                let recomputed = digest.observed_at as i64 - first as i64;
                assert!(
                    (recomputed - reported).abs() <= 2,
                    "{}: height {}'s lead is reported {reported} ms and recomputes to \
                     {recomputed} ms from raw stamps",
                    window.name,
                    closure.number
                );
            }
            let lag = digest.observed_at as i64 - digest.chain_timestamp_secs as i64 * 1000;
            if let Some(reported) = closure.reported["canonical_lag_ms"].as_i64() {
                assert!(
                    (lag - reported).abs() <= 1,
                    "{}: height {}'s canonical lag is reported {reported} and recomputes to {lag}",
                    window.name,
                    closure.number
                );
            }
            if let (Some(reported), true) = (
                closure.reported["reconciliation_latency_ms"].as_i64(),
                closure.occurrence == 0,
            ) {
                let recomputed = closure.recorded_at as i64 - digest.observed_at as i64;
                assert!(
                    (recomputed - reported).abs() <= 2,
                    "{}: height {}'s reconciliation latency is reported {reported} and \
                     recomputes to {recomputed} on its first closure",
                    window.name,
                    closure.number
                );
            }
            compared += 1;
        }
    }
    assert!(
        compared >= 90,
        "the latency recompute compared {compared} closures — fewer than the two windows contain"
    );
}

#[test]
fn the_lead_is_printed_with_its_non_positive_samples_and_a_selection_warning() {
    let table: Value = read_value(LATENCY);
    for axis in [
        "lead_distribution_height_level",
        "lead_distribution_transaction_level",
        "canonical_lag_distribution",
        "reconciliation_latency_distribution",
    ] {
        let node = if axis == "lead_distribution_transaction_level" {
            &table[axis]["distribution"]
        } else {
            &table[axis]
        };
        assert!(
            node["non_positive_samples"].is_number(),
            "latency.json's `{axis}` must carry §27's non-positive count rather than imply one"
        );
    }
    assert!(
        table["section_28_headline"]["selection_warning"].is_string(),
        "latency.json must state that these windows are not a proof of advantage"
    );
    assert!(
        table["ab_per_window"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0)
            == WINDOWS.len(),
        "§28's A/B output is per window, and there are two"
    );
}

#[test]
fn no_missing_data_is_written_as_a_zero() {
    let latency: Value = read_value(LATENCY);
    for window in latency["ab_per_window"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        assert_eq!(
            window["gaps"].as_str(),
            Some("N/A_NOT_MEASURABLE"),
            "§28: a gap that cannot be measured is N/A, and a 0 there would claim a clean stream"
        );
    }
    let summary: Value = read_value(SUMMARY);
    for axis in ["gross", "net", "realized"] {
        assert_eq!(
            summary["profit"][axis].as_str(),
            Some("N/A"),
            "{axis} would have to be a measurement, and this milestone runs none"
        );
    }
    // A zero that IS a measurement stays a zero, and the gate says which ones: an empty
    // pool intersection and an empty divergent-verdict count are both counts of real events.
    let affected: Value = read_value(AFFECTED);
    assert_eq!(
        affected["real_window_result"]["targets_hitting_the_registered_pool_set"].as_u64(),
        Some(0)
    );
    assert!(
        affected["real_window_result"]["positive_control_token_side_hits"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "the zero above is only a finding next to a non-zero control"
    );
}

#[test]
fn every_quoted_negative_control_names_a_test_that_exists() {
    let sources: BTreeMap<String, String> = CONTROL_SOURCES
        .iter()
        .map(|path| (path.to_string(), read_text(path)))
        .collect();
    let table: Value = read_value(CONTROLS);
    let rows = table["controls"].as_array().expect("controls rows");
    assert_eq!(
        rows.len(),
        CONTROLS_TABLE.len(),
        "§29's twelve controls are fixed"
    );
    for row in rows {
        let name = row["fixture_test"].as_str().expect("test name");
        let claimed = row["test_file"].as_str().expect("test file");
        let text = sources
            .get(claimed)
            .unwrap_or_else(|| panic!("{claimed} is not a source this gate reads"));
        assert!(
            text.contains(&format!("fn {name}")),
            "{name} is quoted in negative-controls.json but does not exist in {claimed}"
        );
    }
}

#[test]
fn the_twelve_fixture_classes_are_shipped_and_labelled() {
    let text = read_text("crates/live/tests/preconf_fixtures.rs");
    for class in [
        "fx_valid_sequence",
        "fx_duplicate",
        "fx_gap",
        "fx_wrong_block",
        "fx_wrong_chain",
        "fx_wrong_parent",
        "fx_malformed",
        "fx_late_flashblock",
        "fx_canonical_mismatch",
        "fx_multiple_pool_changes",
        "fx_reverted_tx",
        "fx_unrelated_tx",
    ] {
        assert!(
            text.contains(&format!("fn {class}")),
            "§47 requires the `{class}` fixture"
        );
    }
    for label in [
        "source",
        "capture_timestamp",
        "chain",
        "block",
        "protocol_version",
    ] {
        assert!(
            text.contains(label),
            "§47 requires every fixture to carry `{label}`"
        );
    }
}

/// §33: the whole directory, tables and raw alike, carries no URL — and the walker is not blind.
#[test]
fn the_directory_holds_no_endpoint_url() {
    assemble_once();
    let needles = ["wss://", "https://", "http://"];
    let (files, offenders) = scan(&evidence_dir(), &needles);
    assert!(
        files >= 20,
        "the URL scan walked {files} files; ten tables and ten raw records should be there"
    );
    assert!(
        offenders.is_empty(),
        "§33 forbids a URL anywhere in this directory: {offenders:?}"
    );
    let temp = workspace_root().join("target/m94-url-positive-control");
    std::fs::create_dir_all(&temp).expect("temp dir");
    std::fs::write(
        temp.join("planted.txt"),
        "see https://example.invalid for the endpoint\n",
    )
    .expect("planted file");
    let (_, caught) = scan(&temp, &needles);
    std::fs::remove_dir_all(&temp).expect("clean the temp directory up");
    assert!(
        !caught.is_empty(),
        "the URL scan found nothing even with a URL written into the directory it walked: the \
         gate is blind and its clean result means nothing"
    );
}

/// The secret scan's shape check over the directory, with a planted file as its control.
#[test]
fn no_secret_shape_appears_in_the_directory() {
    assemble_once();
    let needles = [
        "PRIVATE_KEY",
        "private_key",
        "secretKey",
        "BEGIN PRIVATE KEY",
    ];
    let (files, offenders) = scan(&evidence_dir(), &needles);
    assert!(files >= 20, "the scan walked {files} files");
    assert!(
        offenders.is_empty(),
        "§54's secret scan over the evidence directory: {offenders:?}"
    );
    let temp = workspace_root().join("target/m94-secret-positive-control");
    std::fs::create_dir_all(&temp).expect("temp dir");
    std::fs::write(temp.join("planted.txt"), "PRIVATE_KEY=0xdeadbeef\n").expect("planted file");
    let (_, caught) = scan(&temp, &needles);
    std::fs::remove_dir_all(&temp).expect("clean the temp directory up");
    assert!(
        !caught.is_empty(),
        "the secret scan walked a directory holding `PRIVATE_KEY` and reported nothing"
    );
}

fn scan(dir: &Path, needles: &[&str]) -> (usize, Vec<String>) {
    let mut files = 0usize;
    let mut offenders = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(&path).expect("a readable directory") {
            let entry = entry.expect("an entry").path();
            if entry.is_dir() {
                stack.push(entry);
                continue;
            }
            files += 1;
            let text = std::fs::read_to_string(&entry).expect("a text file");
            for needle in needles {
                if text.contains(needle) {
                    offenders.push(format!("{} names `{needle}`", entry.display()));
                }
            }
        }
    }
    (files, offenders)
}

/// The tokens this file may not use anywhere below its own declaration: a gate that needs a
/// client cannot witness §34's "instrumentation added no requests", and a gate that calls the
/// crate it audits is no longer independent of it (§32).
const FORBIDDEN_BELOW_THIS_DECLARATION: [&str; 5] = [
    "HttpChainAdapter",
    "request_raw",
    "GIWA_",
    "evm_live::",
    "evm_chain::",
];

/// §34, turned on this file: the gate that writes the directory must not learn to talk to a
/// chain, and must not lean on the code it audits.
#[test]
fn this_gate_asks_the_chain_for_nothing_and_the_crate_for_nothing() {
    let own = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/preconf_evidence_gate.rs"),
    )
    .expect("this file");
    // The declaration above is the one place these words legally appear, so the scan cuts that
    // span out — and separately proves the cut removed exactly one mention of each word rather
    // than a use of it, so the exclusion cannot silently grow to cover a real call.
    let marker = "const FORBIDDEN_BELOW_THIS_DECLARATION";
    let start = own
        .find(marker)
        .expect("the needle declaration is in this file");
    let end = start
        + own[start..]
            .find("];")
            .expect("the needle declaration closes")
        + 2;
    let body = format!("{}{}", &own[..start], &own[end..]);
    for forbidden in FORBIDDEN_BELOW_THIS_DECLARATION {
        assert_eq!(
            own.matches(forbidden).count(),
            1,
            "`{forbidden}` appears more than once in this gate, so cutting the needle \
             declaration out is not a one-for-one exclusion of the list itself"
        );
        assert!(
            !body.contains(forbidden),
            "this gate uses `{forbidden}` below its needle declaration: a gate that needs a \
             client cannot witness that instrumentation added no requests (§34), and a gate \
             that calls the crate it audits is no longer independent of it (§32)"
        );
    }
}

#[test]
fn the_pool_set_is_the_one_m9_2_published() {
    let (pools, tokens, edges, graph_block) = pool_set();
    assert_eq!(pools.len(), 80, "M9.2 published 80 attested pools");
    assert_eq!(edges, 160, "and 160 directed edges");
    assert!(tokens.len() > 2, "a graph with two tokens has no market");
    assert!(
        graph_block > 0,
        "the table names the block it was priced at"
    );
    let summary: Value = read_value(SUMMARY);
    assert_eq!(
        summary["denominators"]["registered_pools"].as_u64(),
        Some(pools.len() as u64),
        "summary.json's denominator is not the table M9.2 committed"
    );
}

#[test]
fn the_tables_and_the_capture_agree_on_the_endpoint_identity() {
    let windows = load_windows();
    let summary: Value = read_value(SUMMARY);
    let protocol: Value = read_value(PROTOCOL);
    for window in &windows {
        assert_eq!(
            summary["endpoint_identity"]["flashblocks_endpoint_id"],
            window.meta()["flashblocks_endpoint"],
            "{}: the endpoint summary.json names is not the one the capture digested",
            window.name
        );
        assert_eq!(
            protocol["endpoints"]["flashblocks"],
            window.meta()["flashblocks_endpoint"],
            "{}: protocol.json names a different endpoint",
            window.name
        );
    }
    assert_eq!(
        summary["endpoint_identity"]["chain_id"].as_u64(),
        Some(91_342),
        "the windows were supposed to run on GIWA Sepolia"
    );
}

/// §32's own negative control: a wrong number has to be caught by the recompute, not by a
/// reviewer's attention.
#[test]
fn an_injected_wrong_number_is_caught_by_the_recompute() {
    let mut window = Window::load(WINDOWS[0]);
    let before = window.recomputed();
    assert_eq!(
        before["frames_offered"].as_u64(),
        window.counters()["frames_offered"].as_u64(),
        "the honest window must agree before anything is mutated"
    );
    window.reads.pop();
    let after = window.recomputed();
    assert_ne!(
        after["frames_offered"].as_u64(),
        before["frames_offered"].as_u64(),
        "dropping a raw read changed nothing, so the recompute is not reading the raw lines"
    );
    assert_ne!(
        after["frames_offered"].as_u64(),
        window.counters()["frames_offered"].as_u64(),
        "the mutated recompute still agrees with the report, so the comparison cannot fail"
    );
}

#[test]
fn reconciliation_rows_are_the_closures_in_delivery_order() {
    let table: Value = read_value(RECONCILIATION);
    let rows = table["rows"].as_array().expect("rows");
    let windows = load_windows();
    let expected: usize = windows.iter().map(|w| w.closures.len()).sum();
    assert_eq!(
        rows.len(),
        expected,
        "reconciliation.json has {} rows for {} closures",
        rows.len(),
        expected
    );
    let mut index = 0usize;
    for window in &windows {
        for closure in &window.closures {
            let row = &rows[index];
            assert_eq!(
                (
                    row["window"].as_str(),
                    row["height"].as_u64(),
                    row["event_index"].as_u64()
                ),
                (
                    Some(window.name),
                    Some(closure.number),
                    Some(closure.event_index)
                ),
                "row {index} is not the {}-th closure of {}",
                index,
                window.name
            );
            index += 1;
        }
    }
    let repeats = rows
        .iter()
        .filter(|r| r["closure_of_this_height"].as_u64().unwrap_or(0) > 1)
        .count();
    assert!(
        repeats > 0,
        "reconciliation.json#repeat_closures would be prose about nothing: no row claims a \
         second closure"
    );
}

#[test]
fn flashblocks_rows_cover_every_read_of_both_windows() {
    let text = read_text(FLASHBLOCKS);
    let rows: Vec<Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("a row parses"))
        .collect();
    let reads: usize = load_windows().iter().map(|w| w.reads.len()).sum();
    assert_eq!(
        rows.len(),
        reads,
        "flashblocks.jsonl is a per-read table: {} rows for {reads} reads",
        rows.len()
    );
    for row in &rows {
        assert!(
            row["payload_digest"]
                .as_str()
                .map(|d| d.starts_with("0x") && d.len() == 66)
                == Some(true),
            "a row without a full payload digest cannot be tied back to the verbatim file: {row}"
        );
        assert!(
            row["state_root_placeholder"].is_boolean(),
            "every row must say whether its view carried a state credential: {row}"
        );
    }
}

/// Every `(field name, does it hold a number)` pair a table can print, at any depth. A JSONL
/// file is walked line by line; the README has no fields, only prose, so it contributes none.
fn field_shapes(value: &Value, out: &mut BTreeSet<(String, bool)>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                out.insert((key.clone(), child.is_number()));
                field_shapes(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                field_shapes(item, out);
            }
        }
        _ => {}
    }
}

fn names_a_price(field: &str, needles: &[&str]) -> bool {
    needles
        .iter()
        .any(|needle| field.to_ascii_lowercase().contains(needle))
}

#[test]
fn the_radar_never_reaches_canonical_state_from_this_directory() {
    // §58's build-graph proof lives in preconf_isolation.rs; this directory's own contribution
    // is that no column it prints could be read as canonical: no field carries a reserve, a
    // price, an amount or a state credential.
    //
    // Two scopes, because one of them would be a lie either way.
    //   * An AMM accessor or a swap quantity can only ever be a field, so those words are
    //     scanned in the full text of every file.
    //   * `price`, `reserve`, `amount`, `profit` are also the words §51 obliges this directory
    //     to use in order to say it measured none of them. Banning the words in prose would
    //     delete the boundary statement; so the ban here is on a *numeric* field carrying one of
    //     those names. A string cell reading `N/A` with a reason is the §28 form of "not
    //     measured" and stays legal; a number in such a column is the finding this test forbids.
    let forbidden_anywhere = [
        "reserve0",
        "reserve1",
        "getreserves",
        "amount_in",
        "amount_out",
        "amountin",
        "amountout",
    ];
    let forbidden_on_a_number = ["price", "reserve", "amount", "profit", "gas_price"];
    let mut offenders = Vec::new();
    let mut walked: Vec<(&str, usize)> = Vec::new();
    for name in TABLES {
        let text = read_text(name);
        for needle in forbidden_anywhere {
            if text.to_ascii_lowercase().contains(needle) {
                offenders.push(format!("{name} prints `{needle}` anywhere"));
            }
        }
        let mut shapes = BTreeSet::new();
        if name.ends_with(".jsonl") {
            for line in text.lines().filter(|line| !line.trim().is_empty()) {
                field_shapes(
                    &serde_json::from_str(line).expect("a jsonl line parses"),
                    &mut shapes,
                );
            }
        } else if name.ends_with(".json") {
            field_shapes(
                &serde_json::from_str(&text).expect("a table parses"),
                &mut shapes,
            );
        } else {
            continue;
        }
        walked.push((name, shapes.len()));
        for (field, holds_a_number) in shapes {
            if holds_a_number && names_a_price(&field, &forbidden_on_a_number) {
                offenders.push(format!("{name} holds a number in a field named `{field}`"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "§13/§14/§35: an Early Radar table that measures a reserve or an amount is already \
         pretending to be a price — {offenders:?}"
    );
    // The walk reaches the leaves, table by table, rather than stopping at the first level: a
    // nine-row skim of one document would pass the scan above while missing every row field.
    for (name, count) in &walked {
        assert!(
            *count >= 15,
            "{name} yielded only {count} field names — the walker stopped short of its rows and \
             a clean scan would mean nothing"
        );
    }
    // Both directions of the planted control, so the rule's shape is on the record: a number in
    // a price-named column is caught, the string `N/A` in the same column is not.
    let mut caught = BTreeSet::new();
    field_shapes(
        &json!({ "rows": [{ "price": 1u64, "hash": "0x00" }] }),
        &mut caught,
    );
    assert!(
        caught.iter().any(|(field, is_number)| {
            *is_number && names_a_price(field, &forbidden_on_a_number)
        }),
        "the scan misses a numeric column literally named `price`"
    );
    let mut allowed = BTreeSet::new();
    field_shapes(&json!({ "profit": { "gross": "N/A" } }), &mut allowed);
    assert!(
        allowed
            .iter()
            .all(|(field, is_number)| !is_number || !names_a_price(field, &forbidden_on_a_number)),
        "the scan flags the §28 form of `not measured`, which is the one thing this directory \
         has to be able to print"
    );
}
