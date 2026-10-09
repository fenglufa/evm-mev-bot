//! M12-B §6: is the `pending` read the radar performs answered in the shape its
//! decoder reads?
//!
//! # The mismatch this file closes
//!
//! M12-A §8.2 (defect D5) recorded two facts that had never been put in one room:
//! [`HeadReader::pending_raw`] asks `eth_getBlockByNumber(["pending", false])`, whose
//! `transactions` array is a list of bare hashes, while the M9.4 decoder reads each
//! entry as an *object* and refuses a hash outright
//! ([`PreconfError::HashOnlyTransaction`]). The pair was never run together, because
//! the radar is not wired into the pipeline — so the defect was latent: it could only
//! fire on the day someone connected it, and it would have fired as "no frame ever
//! decodes", which is fail-closed but useless.
//!
//! # The choice, and what it cost to make it
//!
//! §6 asks for one of two fixes and forbids picking without weighing response size,
//! call latency and existing semantics. The choice recorded in the committed table is
//! **the request shape**, on the radar's own read:
//! [`HeadReader::pending_full_transactions`] asks `["pending", true]`, and
//! [`evm_live::PollingFrameSource`] uses it. `pending_raw` stays on `full: false`.
//!
//! The numbers behind that are recomputed from the committed M9.4 capture in
//! [`the_capture_measures_the_two_shapes_apart`], not quoted from prose: on both
//! windows a pending read with full transactions costs about seven times the bytes of
//! the same read with bare hashes, and the table carries the per-window integer ratio
//! as `size_stats[].full_over_bare_hundredths`. §30's candidate observer reads *how
//! many* transactions a pending view holds, so it would pay that multiple for bytes it
//! never opens; the radar reads *which contracts* moved, so it cannot do without them.
//! One shared read would serve the claim worse, not better.
//!
//! Latency is stated as `NOT_MEASURED`. This milestone reads no real node (§10), and a
//! larger response on the same method cannot be claimed cheaper without a sample.
//!
//! # What was *not* done, and the row that proves it
//!
//! §6.5 forbids relaxing validation to make an unknown format pass quietly, so the
//! decoder's refusals are the accepted vocabulary here, and [`only_the_full_object_shape_is_accepted`]
//! pins the accepted set to exactly the full-transaction shapes. The refusal split is
//! the one thing this milestone changed inside the decoder: an entry that is neither an
//! object nor a bare 32-byte hash used to be reported as "hash-only", which named a
//! shape the payload did not have. It now reports what it found
//! ([`PreconfError::UnexpectedTransactionEntry`]), and
//! [`a_wrong_type_is_named_as_its_own_shape`] shows the two lines are distinguishable —
//! that assertion is the planted negative control: collapse the two variants back into
//! one and it goes red.
//!
//! # Scope guard
//!
//! Nothing in this file reaches an execution path. §6 keeps the Early Radar unwired,
//! and `tests/preconf_isolation.rs` is the gate that enforces it; the table records
//! `LOCAL_FLASHBLOCKS = NOT_VERIFIED` and `EARLY_RADAR_WIRED_TO_EXECUTION = false`
//! because this milestone changes a read parameter, not a capability claim.
//!
//! # Reads
//!
//! Every payload is either a committed capture line or a mutation of one, and both
//! transports in this file are local structs with counters. The table's
//! `call_accounting` says exactly how many mock reads each test performed and asserts
//! zero real-node reads.
//!
//! `M12B_PENDING_SHAPE_REFRESH=1 cargo test -p evm-live --test pending_shape_compat`
//! re-assembles `data/evidence/m12/b/pending-shape-compat.json`; without it, the
//! committed file is compared against this run.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use evm_chain::{ChainBlock, ChainError, HeadReader};
use evm_core::{BlockNumber, ChainId};
use evm_live::{
    frame_from_pending_value, Field, FrameSource, PollingFrameSource, PreconfError,
    PreconfirmationFrame,
};

const CHAIN: ChainId = ChainId(91_342);

/// The preconfirmation arm's endpoint digest, as M9.4 committed it (§33: a digest,
/// never a URL). Reused rather than re-derived because recomputing one needs the URL.
const FLASHBLOCKS_ENDPOINT: &str = "rpc-a9eb0c34d5519e0a";

const EVIDENCE_FILE: &str = "data/evidence/m12/b/pending-shape-compat.json";

/// The two windows M9.4 captured from the preconfirmation host, both of which are
/// already committed and both of which are replayed here without a network.
const WINDOWS: [&str; 2] = ["window-a", "window-b"];

/// §6.7's five payload shapes, in the task book's own wording and order.
const REQUIRED_SHAPES: [&str; 5] = [
    "full transaction object",
    "hash-only transaction",
    "missing field",
    "wrong type",
    "unknown field",
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn capture_path(window: &str) -> PathBuf {
    workspace_root().join(format!(
        "data/evidence/m9/m9.4/raw/{window}/pending-payloads.jsonl"
    ))
}

// — the committed replay fixtures. —

/// One capture window: the provider answers in read order, with the byte count the
/// capture itself stamped on each of them.
struct Capture {
    window: &'static str,
    views: Vec<Value>,
    payload_bytes: Vec<u64>,
}

impl Capture {
    fn load(window: &'static str) -> Capture {
        let path = capture_path(window);
        let Ok(text) = std::fs::read_to_string(&path) else {
            panic!(
                "{} is the replay fixture this milestone was asked to use; it is missing",
                path.display()
            );
        };
        let mut views = Vec::new();
        let mut payload_bytes = Vec::new();
        for line in text.lines() {
            let record: Value = serde_json::from_str(line).expect("a capture line is json");
            views.push(
                record
                    .get("view")
                    .cloned()
                    .expect("a capture line carries its provider answer"),
            );
            payload_bytes.push(
                record["payload_bytes"]
                    .as_u64()
                    .expect("a capture line carries its byte count"),
            );
        }
        Capture {
            window,
            views,
            payload_bytes,
        }
    }

    /// The first answer with at least four transactions: the matrix mutates entries at
    /// fixed indices, so a thinner payload would be mutating something that does not
    /// exist.
    fn first(&self) -> &Value {
        self.views
            .iter()
            .find(|view| {
                view["transactions"]
                    .as_array()
                    .map(Vec::len)
                    .unwrap_or_default()
                    >= 4
            })
            .unwrap_or_else(|| panic!("{}: no captured read holds four transactions", self.window))
    }
}

fn view_bytes(view: &Value) -> u64 {
    serde_json::to_string(view)
        .expect("a captured answer serializes")
        .len() as u64
}

/// The same pending block with its transactions reduced to their hashes — what
/// `["pending", false]` answers, produced here by shortening a real full-object answer
/// so the two shapes differ in exactly one field and nothing else.
fn with_bare_hashes(view: &Value) -> Value {
    let mut out = view.clone();
    let transactions = out["transactions"]
        .as_array()
        .expect("the captured pending answer holds full transaction objects")
        .clone();
    out["transactions"] = Value::Array(
        transactions
            .iter()
            .map(|entry| entry["hash"].clone())
            .collect(),
    );
    out
}

/// Replace one transaction entry with an arbitrary value.
fn replace_entry(view: &Value, index: usize, with: Value) -> Value {
    let mut out = view.clone();
    let entry = out["transactions"]
        .as_array_mut()
        .expect("the captured pending answer holds a transaction array")
        .get_mut(index)
        .unwrap_or_else(|| panic!("the captured view holds fewer than {index} transactions"));
    *entry = with;
    out
}

fn without_key(view: &Value, key: &str) -> Value {
    let mut out = view.clone();
    out.as_object_mut()
        .expect("the captured view is an object")
        .remove(key);
    out
}

fn without_tx_field(view: &Value, index: usize, key: &str) -> Value {
    let mut out = view.clone();
    out["transactions"]
        .as_array_mut()
        .expect("a transaction array")
        .get_mut(index)
        .expect("the captured view holds a transaction at that index")
        .as_object_mut()
        .expect("a full transaction object")
        .remove(key);
    out
}

/// The clock the fixtures run on: fixed, ordered, and never a system clock (§41's
/// determinism rule as inherited from M9.4).
fn observed_at(step: u64) -> u64 {
    1_728_000_000_000 + step * 250
}

fn decode(view: &Value, step: u64) -> Result<PreconfirmationFrame, PreconfError> {
    frame_from_pending_value(
        CHAIN,
        view,
        step + 1,
        observed_at(step),
        FLASHBLOCKS_ENDPOINT,
    )
}

/// One §6.7 case, run through the decoder: what was fed in, what came out, and how
/// many reads it cost (zero, for every case in this table).
fn case_row(
    case: &str,
    shape: &str,
    input: &str,
    outcome: &Result<PreconfirmationFrame, PreconfError>,
    note: &str,
) -> Value {
    let mut row = json!({
        "case": case,
        "shape": shape,
        "input": input,
        "verdict": classify(outcome),
        "error_line": match outcome {
            Ok(_) => Value::Null,
            Err(error) => Value::String(error.to_string()),
        },
        "mock_reads": 0,
        "note": note,
    });
    if let Ok(frame) = outcome {
        row["frame_facts"] = json!({
            "block_number": frame.number().0,
            "transaction_count": frame.transaction_count,
            "targets_named": frame
                .transactions
                .iter()
                .filter(|tx| tx.to.is_known())
                .count(),
            "wire_index_present_on_the_payload": match frame.identity.wire_index {
                Field::Unknown { key_present, .. } => key_present,
                Field::Known(_) => true,
            },
            "wire_index_detail": match &frame.identity.wire_index {
                Field::Unknown { detail, .. } => Value::String((*detail).to_string()),
                Field::Known(_) => Value::Null,
            },
        });
    }
    row
}

/// Every branch of the refusal vocabulary, named the way the table prints it. Exhaustive
/// by construction: a new `PreconfError` variant does not compile until it is classified
/// here too, which is what keeps an unsupported shape from arriving as a blank.
fn classify(outcome: &Result<PreconfirmationFrame, PreconfError>) -> String {
    match outcome {
        Ok(_) => "accepted".to_string(),
        Err(PreconfError::Decode(field)) => format!("refused:decode:{field}"),
        Err(PreconfError::HashOnlyTransaction) => "refused:hash_only_transaction".to_string(),
        Err(PreconfError::UnexpectedTransactionEntry { index, kind }) => {
            format!("refused:unexpected_entry:{index}:{kind}")
        }
        Err(PreconfError::Transport(_)) | Err(PreconfError::EventQueueClosed { .. }) => {
            "refused:transport".to_string()
        }
    }
}

/// The §6.7 matrix, run against the first read of the named window.
fn matrix(window: &'static str) -> Vec<Value> {
    let capture = Capture::load(window);
    let base = capture.first();
    let mut rows = Vec::new();

    // Row: the shape the endpoint actually answers, verbatim.
    let accepted = decode(base, 0);
    rows.push(case_row(
        "capture verbatim",
        "full transaction object",
        "the first captured pending answer, unmodified",
        &accepted,
        "the shape M9.4 characterised and the shape §6 keeps: every transaction entry is an object, so every target is nameable.",
    ));

    // Row: the same block read with `full: false`.
    let bare = with_bare_hashes(base);
    let refused = decode(&bare, 1);
    rows.push(case_row(
        "bare hash list",
        "hash-only transaction",
        "the same captured block with each transaction replaced by its own hash",
        &refused,
        "this is what `pending_raw` returns; the radar refuses it rather than reporting an empty affected-pool set that would read as a quiet market.",
    ));

    // Rows: fields the decoder cannot do without.
    let no_tx_hash = without_tx_field(base, 0, "hash");
    rows.push(case_row(
        "transaction without a hash",
        "missing field",
        "the captured block with transactions[0].hash removed",
        &decode(&no_tx_hash, 2),
        "a frame is identified by its transactions; without a hash there is nothing to reconcile against the sealed block.",
    ));
    let no_number = without_key(base, "number");
    rows.push(case_row(
        "header without a number",
        "missing field",
        "the captured block with its `number` removed",
        &decode(&no_number, 3),
        "height is the one field a pending view cannot be held without; there is no fallback to a local counter.",
    ));

    // Rows: values present but not of the expected type.
    let entry_number = replace_entry(base, 1, json!(42));
    rows.push(case_row(
        "transaction entry is a number",
        "wrong type",
        "the captured block with transactions[1] replaced by 42",
        &decode(&entry_number, 4),
        "not a hash list and not an object; the refusal names the index and the kind it found.",
    ));
    let entry_array = replace_entry(base, 2, json!([base["transactions"][0]["hash"].clone()]));
    rows.push(case_row(
        "transaction entry is an array",
        "wrong type",
        "the captured block with transactions[2] replaced by a one-element array",
        &decode(&entry_array, 5),
        "a nested list would have decoded as \"hash-only\" under the previous wording, which pointed at a read parameter the payload never used.",
    ));
    let entry_short = replace_entry(base, 3, json!("0xdeadbeef"));
    rows.push(case_row(
        "transaction entry is a truncated hash",
        "wrong type",
        "the captured block with transactions[3] replaced by a 4-byte hex string",
        &decode(&entry_short, 6),
        "the near-miss: a string that is not 32 bytes is not a bare hash, and is not reported as one.",
    ));
    let mut transactions_object = base.clone();
    transactions_object["transactions"] = json!({"0": base["transactions"][0].clone()});
    rows.push(case_row(
        "transactions is an object",
        "wrong type",
        "the captured block whose `transactions` is a map instead of a list",
        &decode(&transactions_object, 7),
        "the array itself is required; a map is not \"the same thing in another shape\".",
    ));

    // Rows: keys the decoder does not read.
    let mut noise = base.as_object().expect("an object").clone();
    noise.insert("unmappedByDesign".to_string(), json!({"nested": true}));
    let unknown_accepted = decode(&Value::Object(noise.clone()), 8);
    rows.push(case_row(
        "one unrelated extra key",
        "unknown field",
        "the captured block plus a key the decoder does not read",
        &unknown_accepted,
        "an unmapped key cannot create a claim: every field the frame carries is read by name, so the frame is the one the unmodified payload produced.",
    ));
    noise.insert("index".to_string(), json!("0x7"));
    let index_like = decode(&Value::Object(noise), 9);
    rows.push(case_row(
        "an index-like key",
        "unknown field",
        "the same payload plus an `index` key",
        &index_like,
        "kept unknown and recorded as present: §8 assigns the frame index from read order, and the detail line now says the key was named but not mapped instead of claiming no such field exists.",
    ));

    // Each row names the window it ran on, so a committed row is identifiable by its
    // own data and not by which array section it happens to sit in.
    for row in &mut rows {
        row["window"] = json!(window);
    }
    rows
}

fn table() -> Value {
    let rows: Vec<Value> = WINDOWS
        .into_iter()
        .flat_map(matrix)
        .map(|row| {
            let mut row = row;
            row.as_object_mut()
                .expect("a case row is an object")
                .sort_keys();
            row
        })
        .collect();
    let sizes: Vec<Value> = WINDOWS
        .into_iter()
        .map(|window| size_stats(Capture::load(window)))
        .collect();
    json!({
        "schema": "m12b-pending-shape-compat-v1",
        "source_of_truth": "crates/live/tests/pending_shape_compat.rs",
        "task_book": "docs/v0.1/M12B Coding.md §6",
        "defect": "M12-A §8.2 D5",
        "decision": {
            "chosen": "change the request params on the radar's own read",
            "read": "eth_getBlockByNumber([\"pending\", true]) via HeadReader::pending_full_transactions",
            "unchanged_read": "eth_getBlockByNumber([\"pending\", false]) via HeadReader::pending_raw",
            "why_not_the_shared_read": "the candidate observer's fact is a count of transactions, not their targets, so it would pay the full-object byte multiple measured in size_stats for bytes it never opens; see full_over_bare_hundredths",
            "why_not_the_decoder": "a hash list names no contract, so accepting one would turn an empty affected-pool set into a finding; §6.5 forbids relaxing validation to make an unknown shape pass",
            "latency": "NOT_MEASURED: this milestone reads no real node, and a larger response on the same method cannot be claimed cheaper without a sample",
        },
        "shapes": REQUIRED_SHAPES.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
        "replay_fixture": {
            "files": WINDOWS.into_iter().map(|w| capture_path(w).to_string_lossy().to_string()).collect::<Vec<_>>(),
            "kind": "the committed M9.4 provider answers, replayed in capture order",
        },
        "size_stats": sizes,
        "cases": rows,
        "call_accounting": {
            "node_requests": 0,
            "scope": "the tests in this file. Every payload is a line of the committed capture or a mutation of one, and both transports are local structs with counters, so nothing here opens a socket. The read-shape guards added beside the production code live elsewhere and are counted in the §11 inventory: two unit tests in crates/live/src/preconf_provider.rs run on stubs, and one test in crates/chain/tests/readiness_gate.rs sends two requests to a 127.0.0.1 stub that records the params it was sent.",
            "decoder_cases": rows.len(),
            "mock_reads_performed_by_decoder_cases": mock_read_total(&rows),
            "transport": {
                "heavy_reads_per_accepted_source": 1,
                "light_reads_per_accepted_source": 0,
                "reads_performed_by_the_refused_attempt": 0,
                "detail": "these three are the atomic counters of the local stubs in `the_radar_asks_for_the_shape_its_decoder_reads`, which asserts the table and the live counters agree; a stub that silently read twice would make this file disagree with its own run",
            },
        },
        // Observed by hand during M12-B §6: each row is a defect planted in the
        // production side, the tests it turned red, and nothing more. Re-running one is
        // a manual edit-and-revert, so these are records of runs, not assertions this
        // file makes — which is why the file they live in is named.
        "negative_controls": [
            {
                "planted": "PollingFrameSource::next_view asks HeadReader::pending_raw instead of pending_full_transactions",
                "went_red": [
                    "pending_shape_compat::the_radar_asks_for_the_shape_its_decoder_reads",
                    "preconf_provider::tests::a_polling_source_never_offers_receipts",
                    "preconf_provider::tests::a_reader_that_only_answers_the_light_shape_is_refused_not_downgraded",
                ],
                "file": "crates/live/src/preconf_provider.rs",
            },
            {
                "planted": "the wrong-type refusal collapsed back into HashOnlyTransaction, as it was before §6",
                "went_red": [
                    "pending_shape_compat::a_wrong_type_is_named_as_its_own_shape",
                    "pending_shape_compat::every_shape_the_task_book_names_is_covered",
                    "pending_shape_compat::the_assembled_table_is_the_one_the_evidence_file_carries",
                ],
                "file": "crates/live/src/preconf_decode.rs",
            },
            {
                "planted": "an index-like key reported as no frame index field of any kind",
                "went_red": [
                    "pending_shape_compat::an_unmapped_key_changes_nothing_it_cannot_name",
                    "pending_shape_compat::the_assembled_table_is_the_one_the_evidence_file_carries",
                ],
                "file": "crates/live/src/preconf_decode.rs",
            },
            {
                "planted": "HttpChainAdapter::pending_full_transactions sends full: false",
                "went_red": [
                    "readiness_gate::the_two_pending_reads_ask_the_node_for_the_shape_each_needs",
                ],
                "file": "crates/chain/src/head.rs",
                "note": "the only guard for this one is the wire test: every other test in §6 sits above a stub that implements both pending methods, so the flag could regress here and stay green there",
            },
        ],
        "verdicts": {
            "LOCAL_FLASHBLOCKS": "NOT_VERIFIED",
            "LOCAL_CANONICAL_RPC": "NOT_VERIFIED",
            "SELF_HOSTED_NODE": "NOT_RUN",
            "EARLY_RADAR_WIRED_TO_EXECUTION": false,
            "PENDING_SHAPE_MISMATCH": "CLOSED_FOR_THE_RADAR_READ",
        },
    })
}

/// The mock reads the decoder cases performed, summed out of the rows themselves.
/// Both this and the planted control in
/// [`the_matrix_reads_no_node_and_counts_what_it_mocks`] go through here, so the sum
/// is a function of the table rather than a constant restated twice.
fn mock_read_total(rows: &[Value]) -> u64 {
    rows.iter()
        .map(|row| row["mock_reads"].as_u64().expect("a read count"))
        .sum()
}

// — the size measurement behind the decision. —

fn size_stats(capture: Capture) -> Value {
    let mut entries = 0u64;
    let mut objects = 0u64;
    let mut bare_hashes = 0u64;
    let mut full_total = 0u64;
    let mut bare_total = 0u64;
    let mut min_full = u64::MAX;
    let mut max_full = 0u64;
    let mut recompute_matches = 0u64;
    for (view, stored) in capture.views.iter().zip(capture.payload_bytes.iter()) {
        let bytes = view_bytes(view);
        if bytes == *stored {
            recompute_matches += 1;
        }
        full_total += bytes;
        min_full = min_full.min(bytes);
        max_full = max_full.max(bytes);
        let list = view["transactions"]
            .as_array()
            .expect("a captured pending answer holds a transaction list");
        entries += list.len() as u64;
        objects += list.iter().filter(|entry| entry.is_object()).count() as u64;
        bare_hashes += list.iter().filter(|entry| entry.is_string()).count() as u64;
        bare_total += view_bytes(&with_bare_hashes(view));
    }
    let reads = capture.views.len() as u64;
    json!({
        "window": capture.window,
        "reads": reads,
        "transaction_entries": entries,
        "full_object_entries": objects,
        "bare_hash_entries": bare_hashes,
        "full_bytes_total": full_total,
        "full_bytes_mean": full_total / reads,
        "full_bytes_min": min_full,
        "full_bytes_max": max_full,
        "bare_hash_bytes_total": bare_total,
        "bare_hash_bytes_mean": bare_total / reads,
        // Integer hundredths, so the committed file carries no float whose digits a
        // future run could disagree about.
        "full_over_bare_hundredths": full_total * 100 / bare_total,
        "payload_bytes_field_recomputed_alike": recompute_matches,
    })
}

// — the transport half: what the radar's own source asks for. —

/// A transport that answers both pending shapes and counts each.
struct ShapedReader {
    payload: Value,
    heavy: Arc<AtomicU64>,
    light: Arc<AtomicU64>,
}

impl ShapedReader {
    fn new(payload: Value) -> (Self, Arc<AtomicU64>, Arc<AtomicU64>) {
        let heavy = Arc::new(AtomicU64::new(0));
        let light = Arc::new(AtomicU64::new(0));
        (
            Self {
                payload,
                heavy: heavy.clone(),
                light: light.clone(),
            },
            heavy,
            light,
        )
    }
}

#[async_trait]
impl HeadReader for ShapedReader {
    fn transport(&self) -> &'static str {
        "stub-shaped"
    }

    async fn head(&mut self) -> evm_chain::Result<BlockNumber> {
        Err(ChainError::MissingData(
            "this stub is asked only for its pending view".to_string(),
        ))
    }

    async fn block_at(&mut self, _number: BlockNumber) -> evm_chain::Result<Option<ChainBlock>> {
        Ok(None)
    }

    async fn pending_raw(&mut self) -> evm_chain::Result<Option<Value>> {
        self.light.fetch_add(1, Ordering::SeqCst);
        Ok(Some(self.payload.clone()))
    }

    async fn pending_full_transactions(&mut self) -> evm_chain::Result<Option<Value>> {
        self.heavy.fetch_add(1, Ordering::SeqCst);
        Ok(Some(self.payload.clone()))
    }
}

/// A transport that only ever answers the light shape — every implementation in this
/// repository until §6 paired the radar with `full: true`.
struct LightOnlyReader(Value);

#[async_trait]
impl HeadReader for LightOnlyReader {
    fn transport(&self) -> &'static str {
        "stub-light-only"
    }

    async fn head(&mut self) -> evm_chain::Result<BlockNumber> {
        Err(ChainError::MissingData(
            "this stub is asked only for its pending view".to_string(),
        ))
    }

    async fn block_at(&mut self, _number: BlockNumber) -> evm_chain::Result<Option<ChainBlock>> {
        Ok(None)
    }

    async fn pending_raw(&mut self) -> evm_chain::Result<Option<Value>> {
        Ok(Some(self.0.clone()))
    }
}

#[tokio::test]
async fn the_radar_asks_for_the_shape_its_decoder_reads() {
    let payload = Capture::load("window-a").first().clone();
    let (reader, heavy, light) = ShapedReader::new(payload.clone());
    let mut source = PollingFrameSource::new(reader, FLASHBLOCKS_ENDPOINT.to_string());

    let handed = source
        .next_view()
        .await
        .expect("a transport that answers the heavy shape is not a failure")
        .expect("the stub always answers");
    let heavy_after_accepted = heavy.load(Ordering::SeqCst);
    let light_after_accepted = light.load(Ordering::SeqCst);
    assert_eq!(heavy_after_accepted, 1, "one pending read");
    assert_eq!(
        light_after_accepted, 0,
        "and the light read was not performed alongside it"
    );
    assert_eq!(
        handed, payload,
        "the payload reaches the decoder byte for byte as the transport returned it"
    );

    // The pairing, which is the whole defect: the payload this source hands over
    // decodes into a frame whose transactions name the contract each one moved.
    // Measured against the same payload's light shape, which names none because it
    // is refused before any transaction is read.
    let listed = payload["transactions"]
        .as_array()
        .expect("the captured answer holds a transaction list");
    let frame = decode(&handed, 0).expect("the read shape and the decoder shape agree");
    assert_eq!(frame.number(), BlockNumber(hex_number(&payload)));
    assert_eq!(frame.transaction_count, listed.len());
    let named = frame
        .transactions
        .iter()
        .filter(|tx| tx.to.is_known())
        .count();
    assert_eq!(
        named,
        listed.len(),
        "every entry of the heavy payload is a full object, so every target is \
         nameable — this capture holds no contract creation, whose `to` the decoder \
         would report unknown"
    );
    let light_shape = decode(&with_bare_hashes(&payload), 1)
        .expect_err("the same block read light is not a lighter frame, it is no frame");
    assert_eq!(classify(&Err(light_shape)), "refused:hash_only_transaction");

    // Control: the same harness over a transport that cannot answer the heavy shape.
    let mut refused = PollingFrameSource::new(
        LightOnlyReader(payload.clone()),
        FLASHBLOCKS_ENDPOINT.to_string(),
    );
    let error = refused
        .next_view()
        .await
        .expect_err("a read nobody probed is refused, not quietly downgraded");
    assert!(matches!(error, PreconfError::Transport(_)), "{error}");
    assert!(
        error.to_string().contains("\"pending\", true"),
        "the refusal has to name the params that were not answered: {error}"
    );
    let heavy_after_refusal = heavy.load(Ordering::SeqCst);
    assert_eq!(
        heavy_after_refusal, heavy_after_accepted,
        "the refusal came from the unprobed heavy read, not from a second attempt"
    );

    // The evidence file's call accounting is this run's counters, not a number typed
    // into the table.
    let declared = table()["call_accounting"]["transport"].clone();
    assert_eq!(
        declared["heavy_reads_per_accepted_source"],
        json!(heavy_after_accepted),
        "the table claims one heavy read per source that answers it"
    );
    assert_eq!(
        declared["light_reads_per_accepted_source"],
        json!(light_after_accepted),
        "and none on the light side"
    );
    assert_eq!(
        declared["reads_performed_by_the_refused_attempt"],
        json!(heavy_after_refusal - heavy_after_accepted),
        "a transport without the override performs no read before refusing"
    );
}

/// §6.5's guard: the only shape that decodes is the one the endpoint was measured
/// answering.
#[test]
fn only_the_full_object_shape_is_accepted() {
    let rows = matrix("window-a");
    let accepted: Vec<&str> = rows
        .iter()
        .filter(|row| row["verdict"] == "accepted")
        .map(|row| row["case"].as_str().expect("a case name"))
        .collect();
    assert_eq!(
        accepted,
        vec![
            "capture verbatim",
            "one unrelated extra key",
            "an index-like key",
        ],
        "three accepted cases, all of them the full-object payload: the original, and \
         the same original with keys the decoder does not read"
    );

    // Planted negative control: the hash-only case is the one that would turn accepted
    // if validation were relaxed, so it is named here as a refusal rather than left to
    // the filter above.
    let bare = rows
        .iter()
        .find(|row| row["case"] == "bare hash list")
        .expect("the hash-only case runs");
    assert_eq!(bare["verdict"], "refused:hash_only_transaction");

    for window in WINDOWS {
        for row in matrix(window) {
            assert_eq!(row["window"], json!(window), "a row carries its own window");
            let verdict = row["verdict"].as_str().expect("a verdict");
            let shape = row["shape"].as_str().expect("a shape");
            if shape == "full transaction object" || shape == "unknown field" {
                assert_eq!(verdict, "accepted", "{window}/{}: {verdict}", row["case"]);
            } else {
                assert!(
                    verdict.starts_with("refused:"),
                    "{window}/{} must be refused: {verdict}",
                    row["case"]
                );
            }
        }
    }
}

#[test]
fn every_shape_the_task_book_names_is_covered() {
    let rows = matrix("window-a");
    let mut covered: Vec<&str> = rows
        .iter()
        .filter_map(|row| row["shape"].as_str())
        .collect();
    covered.sort_unstable();
    covered.dedup();
    let mut required = REQUIRED_SHAPES.to_vec();
    required.sort_unstable();
    required.dedup();
    assert_eq!(
        covered, required,
        "the matrix covers §6.7's five shapes and nothing is claimed beyond them"
    );

    // And the coverage is not vacuous: what the decoder owes each shape is stated per
    // shape, not as a count. Two of the five decode and three refuse, so a relaxed
    // decoder has to move a row across this table — a bare "is the shape listed" check
    // would stay green while it did.
    let expects = |shape: &str| -> &'static str {
        match shape {
            "full transaction object" | "unknown field" => "accepted",
            _ => "refused:",
        }
    };
    for shape in REQUIRED_SHAPES {
        let in_shape: Vec<&Value> = rows.iter().filter(|row| row["shape"] == shape).collect();
        assert!(!in_shape.is_empty(), "{shape} is not covered");
        for row in in_shape {
            let verdict = row["verdict"].as_str().expect("a verdict");
            assert!(
                verdict.starts_with(expects(shape)),
                "{shape}/{} should start with `{}` but is `{verdict}`",
                row["case"],
                expects(shape)
            );
        }
    }

    // The exact classification of the split refusals: reverting them into the single
    // hash-only wording changes these strings, not just their prose.
    let exact = |name: &str| -> String {
        rows.iter()
            .find(|row| row["case"] == name)
            .unwrap_or_else(|| panic!("{} is not in the matrix", name))["verdict"]
            .as_str()
            .expect("a verdict")
            .to_string()
    };
    assert_eq!(
        exact("transaction entry is a number"),
        "refused:unexpected_entry:1:number"
    );
    assert_eq!(
        exact("transaction entry is an array"),
        "refused:unexpected_entry:2:array"
    );
    assert_eq!(
        exact("transaction entry is a truncated hash"),
        "refused:unexpected_entry:3:string"
    );
    assert_eq!(exact("bare hash list"), "refused:hash_only_transaction");
}

/// The decoder change this milestone makes: the refusal says what it found.
#[test]
fn a_wrong_type_is_named_as_its_own_shape() {
    let rows = matrix("window-a");
    let line = |name: &str| -> String {
        rows.iter()
            .find(|row| row["case"] == name)
            .unwrap_or_else(|| panic!("{} is not in the matrix", name))["error_line"]
            .as_str()
            .unwrap_or_else(|| panic!("{} is not a refusal", name))
            .to_string()
    };

    let number_line = line("transaction entry is a number");
    assert!(
        number_line.contains("transactions[1]") && number_line.contains("number"),
        "{number_line}"
    );
    assert!(
        !number_line.contains("hash-only"),
        "the wrong-type line must not report the shape the payload did not have: {number_line}"
    );

    let truncated = line("transaction entry is a truncated hash");
    assert!(
        truncated.contains("transactions[3]") && truncated.contains("string"),
        "{truncated}"
    );

    // Positive control for the same classifier: a real bare hash still arrives as the
    // hash-only refusal, so the new variant has not swallowed the old one.
    let bare = line("bare hash list");
    assert!(
        bare.contains("hash-only") && !bare.contains("transactions["),
        "{bare}"
    );
}

/// An unmapped key must not be able to move a claim, and an index-like key must be
/// recorded as what it is.
#[test]
fn an_unmapped_key_changes_nothing_it_cannot_name() {
    let rows = matrix("window-a");
    let fact = |name: &str| -> Value {
        rows.iter()
            .find(|row| row["case"] == name)
            .unwrap_or_else(|| panic!("{} is not in the matrix", name))["frame_facts"]
            .clone()
    };
    let plain = fact("capture verbatim");
    let noise = fact("one unrelated extra key");
    assert_eq!(
        plain, noise,
        "the frame over a payload with an unread key is the same frame"
    );

    let index_like = fact("an index-like key");
    assert_eq!(
        index_like["wire_index_present_on_the_payload"],
        json!(true),
        "the key is recorded as present"
    );
    assert!(
        index_like["wire_index_detail"]
            .as_str()
            .unwrap_or_default()
            .contains("does not map"),
        "and the detail says it was named but not mapped: {}",
        index_like["wire_index_detail"]
    );
    assert!(
        plain["wire_index_detail"]
            .as_str()
            .unwrap_or_default()
            .contains("no frame index field of any kind"),
        "the unmodified capture payload keeps the original finding — which is the \
         control that the branch above is a real distinction and not a rewrite"
    );
    assert_eq!(
        plain["wire_index_present_on_the_payload"],
        json!(false),
        "the captured payload names no index key at all"
    );
}

/// §6.4: the size difference is why the shared read stayed light. Recomputed from the
/// committed capture rather than quoted.
#[test]
fn the_capture_measures_the_two_shapes_apart() {
    let table = table();
    let stats = table["size_stats"]
        .as_array()
        .expect("one entry per window");
    assert_eq!(stats.len(), WINDOWS.len());
    for stat in stats {
        let window = stat["window"].as_str().expect("a window");
        assert_eq!(
            stat["payload_bytes_field_recomputed_alike"], stat["reads"],
            "{window}: every stored byte count is the compact serialisation of its own answer"
        );
        assert_eq!(
            stat["full_object_entries"], stat["transaction_entries"],
            "{window}: the capture holds no entry that is not a full object"
        );
        assert_eq!(stat["bare_hash_entries"], json!(0), "{window}");
        let hundredths = stat["full_over_bare_hundredths"].as_u64().expect("a ratio");
        assert!(
            hundredths >= 500,
            "{window}: the heavy read is {hundredths}/100× the light one; the decision \
             that the shared read stays light rests on this being a real multiple"
        );
        assert!(
            stat["full_bytes_min"].as_u64().expect("a min")
                < stat["full_bytes_max"].as_u64().expect("a max"),
            "{window}: a single repeated payload would not be a measurement"
        );
    }
}

/// §6.8: the new tests spend mock reads, not requests.
#[test]
fn the_matrix_reads_no_node_and_counts_what_it_mocks() {
    let table = table();
    let accounting = &table["call_accounting"];
    let rows = table["cases"].as_array().expect("the matrix rows");
    assert_eq!(accounting["decoder_cases"], json!(rows.len()));
    assert_eq!(
        rows.len(),
        WINDOWS.len() * 10,
        "ten cases run on each replay window"
    );
    assert_eq!(
        accounting["node_requests"],
        json!(0),
        "nothing in this file opens a socket"
    );
    assert_eq!(
        accounting["mock_reads_performed_by_decoder_cases"],
        json!(0),
        "and the decoder half runs on payloads that are already on disk"
    );
    for row in rows {
        let window = row["window"].as_str().expect("a row names its window");
        assert!(
            WINDOWS.contains(&window),
            "{window} is not a replayed window"
        );
    }

    // Control on the counting, run through the same function the table calls: a matrix
    // whose first row claims one read stops summing to zero. Should this ever fail the
    // other way, the zero above is a constant rather than a measurement.
    let mut planted = rows.clone();
    planted[0]["mock_reads"] = json!(1);
    assert_eq!(mock_read_total(rows), 0);
    assert_eq!(
        mock_read_total(&planted),
        1,
        "the total is read out of the rows"
    );
}

/// The committed table is this run's table, byte for byte.
#[test]
fn the_assembled_table_is_the_one_the_evidence_file_carries() {
    let assembled = table();
    let path = workspace_root().join(EVIDENCE_FILE);
    if std::env::var("M12B_PENDING_SHAPE_REFRESH").is_ok() {
        std::fs::create_dir_all(path.parent().expect("a directory"))
            .expect("create the evidence directory");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&assembled).expect("json") + "\n",
        )
        .expect("write the compatibility table");
        panic!("refreshed {EVIDENCE_FILE}; run this test again without the environment variable");
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "{EVIDENCE_FILE} is missing; refresh it with M12B_PENDING_SHAPE_REFRESH=1 \
             cargo test -p evm-live --test pending_shape_compat -- --test-threads=1"
        );
    });
    let parsed: Value = serde_json::from_str(&committed).expect("a json table");
    assert_eq!(
        parsed, assembled,
        "{EVIDENCE_FILE} disagrees with the matrix this run assembled"
    );

    // §13's ceilings survive the change, spelled out in the file rather than implied.
    for verdict in [
        "LOCAL_FLASHBLOCKS",
        "LOCAL_CANONICAL_RPC",
        "SELF_HOSTED_NODE",
    ] {
        let value = assembled["verdicts"][verdict].as_str().expect("a verdict");
        assert!(
            value.starts_with("NOT_"),
            "{verdict} must stay unverified: {value}"
        );
    }
    assert_eq!(
        assembled["verdicts"]["EARLY_RADAR_WIRED_TO_EXECUTION"],
        json!(false),
        "§6 keeps the radar off the execution path"
    );
}

/// The captured header's own height, read back out of the payload so the pairing test
/// above asserts against data rather than a typed-in number.
fn hex_number(view: &Value) -> u64 {
    let text = view["number"].as_str().expect("a captured hex height");
    u64::from_str_radix(text.strip_prefix("0x").unwrap_or(text), 16).expect("a hex height parses")
}
