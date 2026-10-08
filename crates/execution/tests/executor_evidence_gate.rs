//! M10 §48/§49/§50/§60/§61 — the committed evidence directory for the Arbitrage Executor
//! Contract, and the independent recomputation that decides whether that directory may exist.
//!
//! ```text
//! CC=clang CXX=clang++ CFLAGS="-include cstdint" CXXFLAGS="-include cstdint" \
//!   cargo test -p evm-execution --test executor_evidence_gate -- --test-threads=1
//! ```
//!
//! # Why this file is the second pair of hands
//!
//! [`executor_evidence.rs`][../../simulation/tests/executor_evidence.rs] assembled the 24 files
//! that were already in `data/evidence/m10/`: it owns the fixtures, runs them in REVM, and writes
//! what it observed. A gate that re-used that harness would be a receipt — the same code, the same
//! state, the same answer, quoted back. So this file is in a different crate, is compiled by a
//! different `cargo test` invocation, and reads **only the published JSON**. Nothing here imports
//! a fixture builder, a recipe constant, or an address literal from the simulation test:
//!
//! * §48's *rebuild* — the state provider is reconstructed from the published recipe file and the
//!   published recording path, and the transaction is reconstructed from the published
//!   [`ExecutorRun`] spec and the published calldata bytes, which are decoded rather than
//!   re-assembled from fields.
//! * §48's *simulate* — [`evm_simulation::executor::run`] executes that rebuild against the real
//!   deployed bytecode. This is the one call into production simulation code, and it is the thing
//!   under test, not a helper.
//! * §48's *compare* — the new outcome is serialized to JSON and must be **byte-identical** to the
//!   published `observed` block. §49's D3 is the cross-process, cross-crate form of that rule; the
//!   publisher could only claim two runs inside one process.
//!
//! # The five determinism gates, and where each is answered
//!
//! | §49 | question | this file |
//! |---|---|---|
//! | D1 | same plan → same plan hash | `plan_rebuild`: two constructions, and a sensitivity table that must change the hash for a changed field |
//! | D2 | same plan → same calldata | `plan_rebuild`: the execution crate's encoding must equal the bytes the simulation crate ran |
//! | D3 | same fixture → same simulation result | `d3_replay`: the whole `observed` block, JSON-for-JSON |
//! | D4 | same failure → same revert classification | `d4_rejudge`: `decode_revert` over the published revert bytes, and §27's residue judged out of published rows only |
//! | D5 | same route → same route id | `plan_rebuild`, plus a cross-check against the id the real run published |
//!
//! D2 is the load-bearing one. §47's ban is on `Rust calldata ≠ actual deployed contract ABI`, and
//! the only way to test it without a node is to have the crate that *writes* calldata reproduce the
//! bytes the crate that *ran* calldata executed. If those two disagree, every fixture in this
//! directory describes a transaction nobody could send.
//!
//! # What this file writes, and what it must not touch
//!
//! Eight files, all of them the ones §60 lists that were not already published:
//!
//! ```text
//! negative_controls/wrong_chain.json     §50's two plan-layer refusals
//! negative_controls/wrong_executor.json
//! contract/abi.json                      §47's ABI, tied to the selectors in the evidence
//! contract/runtime_bytecode.json         §47's runtime-code hash, tied to the fixture's executor code
//! execution/lifecycle.json               §23/§24's ladder over the published rows + §31/§32's real rows
//! recompute/deterministic.json           the five gates above, and this directory's own byte identity
//! manifest.json                          §61's eleven fields, null where the fact does not exist
//! README.md                              how to re-run it, and what may not be inferred from it
//! ```
//!
//! `contract/bytecode_hash.json` and `contract/deployment.json` are the live run's, and
//! [`executor_giwa_live.rs`] writes them — this gate reads them and never rewrites them. Same for
//! every other pre-existing file: the gate is append-only with respect to facts it did not produce.
//!
//! # What is not here, and stays not here
//!
//! No RPC. §51 forbids instrumentation from adding a read, and a hermetic gate is a stronger
//! answer: every number below comes off disk, so this test runs the same way with the network down.
//! No signing, no broadcast, no key — there is no [`Signer`][evm_execution::Signer] call in this
//! file and the private key is never read. No fabricated rows: §60's `giwa_success.json` does not
//! exist because §30's real profitable round trip was never proved, and `manifest.json`'s
//! `plan_hash` / `calldata_hash` / `real_tx_hash` quote the *real* run's published values rather
//! than the fixture's, because a fixture identity is not a transaction identity. A wall-clock value
//! appears nowhere, so a rebuild of this directory is byte-for-byte equal forever.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use alloy_primitives::{Address, Bytes, B256, U256};
use serde_json::{json, Map, Value};

use evm_core::{BlockNumber, ChainId};
use evm_execution::{
    AmountDerivation, ArbitrageExecutionPlan, ExecutablePlan, ExecutionBinding, ExecutionError,
    Freshness, MarketKind, PlanLeg, PlanValidity, ProfitDenomination, ProfitPolicy, SenderFunding,
    SimulationContext, SimulationOutcome,
};
use evm_protocol::executor::selector_from_signature;
use evm_protocol::{
    decode_calldata, decode_revert, execute_selector_hex, ExecutorCall, ExecutorLeg,
    ExecutorRevert, RevertPayload,
};
use evm_simulation::executor::{run as run_executor, ExecutorRun};
use evm_simulation::gas::GasPricing;
use evm_simulation::request::EvmRules;
use evm_simulation::state::{DumpStateProvider, StateDump, StateOverride, StateProvider};

const EVIDENCE_REL: &str = "data/evidence/m10";
const SCHEMA: &str = "m10-evidence-v1";
const MILESTONE: &str = "M10";
const ASSEMBLED_BY: &str = "crates/execution/tests/executor_evidence_gate.rs";
/// The command that assembles the 24 files this gate reads. Quoted, not run: a gate that can
/// regenerate its own inputs cannot report a divergence in them.
const ASSEMBLE_COMMAND: &str = "CC=clang CXX=clang++ CXXFLAGS=\"-include cstdint\" cargo test -p evm-simulation --test executor_evidence -- --test-threads=1";
/// The command that runs this file, as every published envelope's `check_command` already spells it.
const CHECK_COMMAND: &str = "CC=clang CXX=clang++ CXXFLAGS=\"-include cstdint\" cargo test -p evm-execution --test executor_evidence_gate -- --test-threads=1";

/// The eight files this gate owns.
const WRITTEN: [&str; 8] = [
    "negative_controls/wrong_chain.json",
    "negative_controls/wrong_executor.json",
    "contract/abi.json",
    "contract/runtime_bytecode.json",
    "execution/lifecycle.json",
    "recompute/deterministic.json",
    "manifest.json",
    "README.md",
];

/// §60's tree, minus the two files that are the live run's and the four this gate writes.
const PUBLISHED: [&str; 22] = [
    "contract/bytecode_hash.json",
    "contract/deployment.json",
    "fixtures/profit_guard.json",
    "fixtures/revert.json",
    "fixtures/slippage.json",
    "fixtures/success.json",
    "negative_controls/broken_route.json",
    "negative_controls/final_profit.json",
    "negative_controls/forced_second_leg_revert.json",
    "negative_controls/invalid_pair.json",
    "negative_controls/min_output.json",
    "negative_controls/token_not_allowed.json",
    "negative_controls/wrong_operator.json",
    "negative_controls/zero_amount.json",
    "real/giwa_execution.json",
    "real/giwa_failure.json",
    "real/giwa_ladder_steps.json",
    "real/preconditions.json",
    "simulation/failure.json",
    "simulation/success.json",
    "states/caller_is_not_the_stored_operator.json",
    "states/committed.json",
];
/// The two recipe variants, listed apart because they are only reachable from a control row.
const PUBLISHED_RECIPES: [&str; 2] = [
    "states/pair_not_in_the_allowlist.json",
    "states/token_not_in_the_allowlist.json",
];

/// The 10 §50 planted controls, each bound to the file that carries it and the refusal that file
/// has to show. `layer` is where the refusal lands: `contract` means REVM ran real bytecode and the
/// contract rejected it, `plan` means the execution crate refused before any bytes existed.
const CONTROLS: [(&str, &str, &str, &str); 10] = [
    (
        "wrong chain",
        "negative_controls/wrong_chain.json",
        "plan",
        "wrong_chain",
    ),
    (
        "wrong executor",
        "negative_controls/wrong_executor.json",
        "plan",
        "wrong_executor",
    ),
    (
        "wrong operator",
        "negative_controls/wrong_operator.json",
        "contract",
        "NotOperator",
    ),
    (
        "wrong token continuity",
        "negative_controls/broken_route.json",
        "contract",
        "BrokenContinuity",
    ),
    (
        "zero amount",
        "negative_controls/zero_amount.json",
        "contract",
        "ZeroAmount",
    ),
    (
        "min output violation",
        "negative_controls/min_output.json",
        "contract",
        "FinalShortfall",
    ),
    (
        "final profit violation",
        "negative_controls/final_profit.json",
        "contract",
        "AskBelowFloor",
    ),
    (
        "pair not allowed",
        "negative_controls/invalid_pair.json",
        "contract",
        "PairNotAllowed",
    ),
    (
        "token not allowed",
        "negative_controls/token_not_allowed.json",
        "contract",
        "TokenNotAllowed",
    ),
    (
        "forced second-leg revert",
        "negative_controls/forced_second_leg_revert.json",
        "contract",
        "Error(string)",
    ),
];

/// A state recipe's census as the scenario row declares it: recipe rows, account rows, storage
/// words. Each is `Option` because a publisher that omits a number is not publishing zero.
type RecipeCensus = (Option<u64>, Option<u64>, Option<u64>);

/// One plan-layer mutation for §49's D1 sensitivity table: the field's name and the edit itself.
type PlanMutation = (String, Box<dyn Fn(&mut ArbitrageExecutionPlan)>);

// ---------------------------------------------------------------------------
// Reading published JSON
// ---------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// A published field's string, or the reason this row cannot be judged.
fn field_str(v: &Value, key: &str) -> Result<String, String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("field {key} is absent or is not a string"))
}

fn field_u64(v: &Value, key: &str) -> Result<u64, String> {
    v.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("field {key} is absent or is not a number"))
}

fn field_bool(v: &Value, key: &str) -> Result<bool, String> {
    v.get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("field {key} is absent or is not a boolean"))
}

/// A wei word in either spelling this directory uses: the state recipes and the §31/§32 rows write
/// decimal strings, the lifecycle record and the observed amounts write `0x`-hex, and both are the
/// same number. Reading them with one function is what lets the two families be compared at all.
fn field_u256(v: &Value, key: &str) -> Result<U256, String> {
    let raw = v.get(key).ok_or_else(|| format!("field {key} is absent"))?;
    if let Some(text) = raw.as_str() {
        let (radix, body) = match text.strip_prefix("0x") {
            Some(hex_body) => (16, hex_body),
            None => (10, text),
        };
        return U256::from_str_radix(body, radix).map_err(|_| {
            format!("field {key} = {text:?} is neither a decimal word nor an 0x-prefixed hex word")
        });
    }
    raw.as_u64()
        .map(U256::from)
        .ok_or_else(|| format!("field {key} is neither a decimal string nor a number"))
}

/// An `0x`-prefixed address.
fn field_addr(v: &Value, key: &str) -> Result<Address, String> {
    let text = field_str(v, key)?;
    Address::parse_checksummed(&text, None)
        .or_else(|_| text.parse::<Address>())
        .map_err(|_| format!("field {key} = {text:?} is not a 20-byte address"))
}

fn field_hash(v: &Value, key: &str) -> Result<B256, String> {
    let text = field_str(v, key)?;
    text.parse::<B256>()
        .map_err(|_| format!("field {key} = {text:?} is not a 32-byte word"))
}

/// `0x`-prefixed bytes of any length.
fn field_bytes(v: &Value, key: &str) -> Result<Bytes, String> {
    let text = field_str(v, key)?;
    let hex = text.strip_prefix("0x").unwrap_or(&text);
    hex::decode(hex)
        .map(Bytes::from)
        .map_err(|_| format!("field {key} = {text:?} is not hex bytes"))
}

fn hex_of(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

fn read_json(root: &Path, rel: &str) -> Result<Value, String> {
    let path = root.join(rel);
    let bytes = std::fs::read(&path).map_err(|e| format!("reading {rel}: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parsing {rel}: {e}"))
}

fn read_text(root: &Path, rel: &str) -> Result<String, String> {
    std::fs::read_to_string(root.join(rel)).map_err(|e| format!("reading {rel}: {e}"))
}

fn file_digest(root: &Path, rel: &str) -> Result<Value, String> {
    let bytes = std::fs::read(root.join(rel)).map_err(|e| format!("reading {rel}: {e}"))?;
    Ok(json!({
        "bytes": bytes.len(),
        "keccak256": format!("{:#x}", alloy_primitives::keccak256(&bytes)),
    }))
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// One published scenario row, addressed by the semantic key this gate uses everywhere:
/// `<file>#<scenario>`. A row index is not an identity — re-ordering a directory would move it,
/// and a comparison that moves when nothing semantic changed is a comparison that proves nothing.
/// Clone exists so a phase can hold a row while also recording a drift note on the gate.
#[derive(Clone)]
struct Row {
    key: String,
    file: String,
    scenario: String,
    value: Value,
}

impl Row {
    fn state(&self) -> &Value {
        &self.value["state"]
    }
    fn run_spec(&self) -> &Value {
        &self.value["run_spec"]
    }
    fn expected(&self) -> &Value {
        &self.value["expected"]
    }
    fn observed(&self) -> &Value {
        &self.value["observed"]
    }
}

/// The directory's state: what has been read, what has been written, and every place a published
/// claim and a recomputed fact disagreed.
struct Gate {
    root: PathBuf,
    evidence: PathBuf,
    rows: Vec<Row>,
    /// Scenario rows deduped by `(recipe file, calldata hash, expected outcome)` — the identity of
    /// a REVM run. `success_round_trip` and `forced_second_leg_revert` are published in more than
    /// one file; replaying each copy would measure the same run twice.
    distinct: Vec<(String, Row)>,
    real: Value,
    failure: Value,
    deployment: Value,
    bytecode: Value,
    /// §61's numbers, filled in as the phases that produce them run.
    summary: Map<String, Value>,
    /// The artifact bytes this run produced, so the manifest can digest them without re-reading
    /// files a previous run may have left in a different shape.
    written: BTreeMap<String, Vec<u8>>,
    drift: Vec<String>,
}

impl Gate {
    fn new() -> Self {
        let root = workspace_root();
        Self {
            evidence: root.join(EVIDENCE_REL),
            rows: Vec::new(),
            root,
            distinct: Vec::new(),
            real: Value::Null,
            failure: Value::Null,
            deployment: Value::Null,
            bytecode: Value::Null,
            summary: Map::new(),
            written: BTreeMap::new(),
            drift: Vec::new(),
        }
    }

    /// Record a disagreement between a published claim and this run's own answer. Nothing is
    /// panicked on the spot: the directory is rewritten in full, the drift is published inside
    /// `recompute/deterministic.json`, and the single failing assertion at the end names every
    /// entry — so one run reports every defect rather than the first one.
    fn note(&mut self, phase: &str, detail: impl Into<String>) {
        self.drift.push(format!("{phase}: {}", detail.into()));
    }

    /// Store a live-run document in the slot its file owns, keyed by the same short name the
    /// reading loop uses.
    fn set_doc(&mut self, slot: &str, doc: &Value) {
        match slot {
            "real" => self.real = doc.clone(),
            "failure" => self.failure = doc.clone(),
            "deployment" => self.deployment = doc.clone(),
            "bytecode" => self.bytecode = doc.clone(),
            other => {
                self.summary.insert(other.to_string(), doc.clone());
            }
        }
    }

    fn check(&mut self, phase: &str, value: Value) -> Value {
        self.summary.insert(phase.to_string(), value.clone());
        value
    }

    /// Read a file under the evidence root. A bare relative path resolves against the workspace
    /// root here, so every in-directory read has to come through this.
    fn read_evidence(&self, rel: &str) -> Result<Value, String> {
        read_json(&self.root, &format!("{EVIDENCE_REL}/{rel}"))
    }

    /// A field every file in this directory carries, in the publisher's own spelling, so a reader
    /// can tell which command rebuilt it and which command judges it. `directory` and `file` are
    /// derived from the path this artifact is written to, so the envelope cannot name a place other
    /// than the one it is standing in.
    fn envelope(&self, rel: &str) -> Value {
        let directory = match Path::new(rel)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
        {
            Some(parent) if !parent.is_empty() && parent != "." => parent,
            _ => ".".to_string(),
        };
        json!({
            "schema": SCHEMA,
            "milestone": MILESTONE,
            "evidence_root": EVIDENCE_REL,
            "directory": directory,
            "file": format!("{EVIDENCE_REL}/{rel}"),
            "assembled_by": ASSEMBLED_BY,
            "assemble_command": CHECK_COMMAND,
            "check_command": CHECK_COMMAND,
            "published_by": {
                "the_rest_of_this_directory": ASSEMBLED_BY.replace("executor_evidence_gate", "executor_evidence"),
                "assemble_command": ASSEMBLE_COMMAND,
            },
        })
    }

    /// Write a JSON artifact with the directory's standard 2-space spelling and no trailing
    /// newline variance, so a rebuild is byte-comparable.
    fn write_json(&mut self, rel: &str, body: Value) {
        let mut envelope = self.envelope(rel);
        if let (Some(merge), Some(body)) = (envelope.as_object_mut(), body.as_object()) {
            for (k, v) in body {
                merge.insert(k.clone(), v.clone());
            }
        }
        self.write_bytes(rel, &to_bytes(&envelope));
    }

    fn write_text(&mut self, rel: &str, text: &str) {
        self.write_bytes(rel, text.as_bytes());
    }

    fn write_bytes(&mut self, rel: &str, bytes: &[u8]) {
        let path = self.evidence.join(rel);
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                self.note("write", format!("creating {}: {e}", parent.display()));
                return;
            }
        }
        let unchanged = match std::fs::read(&path) {
            Ok(previous) => Value::Bool(previous == bytes),
            Err(_) => Value::Null,
        };
        if let Err(e) = std::fs::write(&path, bytes) {
            self.note("write", format!("writing {rel}: {e}"));
            return;
        }
        self.written.insert(rel.to_string(), bytes.to_vec());
        if let Some(table) = self
            .summary
            .entry("byte_identity".to_string())
            .or_insert_with(|| json!({}))
            .as_object_mut()
        {
            table.insert(
                rel.to_string(),
                json!({
                    "bytes": bytes.len(),
                    "unchanged_from_previous_run": unchanged,
                }),
            );
        }
    }

    fn digest_of(&self, rel: &str) -> Option<Value> {
        let bytes = self.written.get(rel)?;
        Some(json!({
            "bytes": bytes.len(),
            "keccak256": format!("{:#x}", alloy_primitives::keccak256(bytes)),
        }))
    }
}

/// The canonical spelling: `serde_json`'s pretty writer, 2 spaces, plus a trailing newline so two
/// files concatenated the same way end the same way.
fn to_bytes(value: &Value) -> Vec<u8> {
    let mut text = serde_json::to_string_pretty(value).expect("artifact serializes");
    text.push('\n');
    text.into_bytes()
}

// ---------------------------------------------------------------------------
// Phase 1 — inventory: is the directory the gate reads the directory the task book asks for?
// ---------------------------------------------------------------------------

impl Gate {
    /// Census of the pre-existing files, the digests of the inputs every table below stands on,
    /// and the 14 published scenario rows. A missing file is drift with a name, not a panic: the
    /// report should say which of §60's items the directory does not have.
    fn inventory(&mut self) -> Result<Value, String> {
        let mut entries = Map::new();
        let mut digests = Map::new();
        for rel in PUBLISHED.iter().chain(PUBLISHED_RECIPES.iter()) {
            match file_digest(&self.root, &format!("{EVIDENCE_REL}/{rel}")) {
                Ok(d) => {
                    digests.insert(rel.to_string(), d);
                }
                Err(e) => self.note("inventory", e),
            }
            let path = self.evidence.join(rel);
            entries.insert(
                rel.to_string(),
                json!({ "exists": path.is_file(), "present_at_read_time": true }),
            );
        }
        // §60 lists these two and neither is this gate's to invent: a `giwa_success.json` would
        // claim a real profitable round trip, and §60's last line forbids creating one that does
        // not exist. Recording the absence is the finding.
        let forbidden = json!({
            "giwa_success.json": {
                "exists": self.evidence.join("real/giwa_success.json").is_file(),
                "rule": "§60: if no real transaction exists, do not create a fabricated \
                         giwa_success.json. §30's real profitable arbitrage is NOT_PROVEN and \
                         the rows that exist say so.",
            }
        });
        let mut listed: BTreeSet<String> = PUBLISHED
            .iter()
            .chain(PUBLISHED_RECIPES.iter())
            .chain(WRITTEN.iter())
            .map(|s| (*s).to_string())
            .collect();
        let extra: Vec<String> = {
            let mut found = Vec::new();
            for rel in sorted_dir(&self.evidence)? {
                if !listed.remove(&rel) {
                    found.push(rel);
                }
            }
            found
        };
        let mut unlisted: Vec<String> = listed.into_iter().collect();
        unlisted.sort();

        Ok(json!({
            "files_read": entries,
            "input_digests": digests,
            "files_this_gate_writes": WRITTEN,
            "listed_by_s60_but_absent": unlisted,
            "present_but_not_listed": extra,
            "absent_by_design": forbidden,
        }))
    }

    /// Every published scenario row, addressed by `<file>#<scenario>`, plus the finding that the
    /// rows sharing one REVM run agree. `success_round_trip` and `forced_second_leg_revert` are
    /// each published in more than one file; if those copies disagreed, this gate would be choosing
    /// which one to believe, and a gate that chooses is not a gate.
    fn load_rows(&mut self) -> Result<(), String> {
        let mut files: Vec<String> = Vec::new();
        for dir in ["fixtures", "negative_controls", "simulation"] {
            for rel in sorted_dir(&self.evidence)? {
                if rel.starts_with(&format!("{dir}/")) && rel.ends_with(".json") {
                    files.push(rel);
                }
            }
        }
        for rel in &files {
            if WRITTEN.contains(&rel.as_str()) {
                // The gate's own two plan-layer controls sit in the same directory as the
                // publisher's negative-control rows and carry no `rows` object on purpose: their
                // refusal happens in the execution crate before a REVM run exists to describe, so
                // the negative-control phase reads them directly and this loader, which is only for
                // publisher rows, skips them. A publisher file missing `rows` still reports.
                continue;
            }
            let doc = read_json(&self.root, &format!("{EVIDENCE_REL}/{rel}"))
                .map_err(|e| self.note("rows", e));
            let Ok(doc) = doc else { continue };
            if doc.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
                self.note("rows", format!("{rel}: schema is not {SCHEMA}"));
            }
            let Some(rows) = doc.get("rows").and_then(Value::as_object) else {
                self.note("rows", format!("{rel}: no rows object"));
                continue;
            };
            for (scenario, row) in rows {
                self.rows.push(Row {
                    key: format!("{rel}#{scenario}"),
                    file: rel.clone(),
                    scenario: scenario.clone(),
                    value: row.clone(),
                });
            }
        }
        self.rows.sort_by(|a, b| a.key.cmp(&b.key));

        // Group by the identity of a run: which state recipe, and which calldata bytes. The rows
        // are taken as a local value so a finding can be recorded while the grouping is being
        // walked.
        let rows = self.rows.clone();
        let mut groups: BTreeMap<(String, String), Vec<&Row>> = BTreeMap::new();
        for row in &rows {
            let recipe = field_str(row.state(), "recipe_keccak256").unwrap_or_default();
            let calldata = field_str(row.run_spec(), "calldata_keccak256").unwrap_or_default();
            groups.entry((recipe, calldata)).or_default().push(row);
        }
        let mut duplicate_agreement = Vec::new();
        for ((recipe, calldata), group) in &groups {
            let mut seen: Vec<(String, String)> = Vec::new();
            for row in group {
                seen.push((
                    row.key.clone(),
                    serde_json::to_string(&row.observed())
                        .map_err(|e| self.note("rows", format!("{}: {e}", row.key)))
                        .unwrap_or_default(),
                ));
            }
            let first = seen.first().map(|(_, s)| s.clone()).unwrap_or_default();
            let agreed = seen.iter().all(|(_, s)| *s == first);
            if !agreed {
                self.note(
                    "rows",
                    format!(
                        "rows over recipe {recipe} and calldata {calldata} publish different \
                            observations: {}",
                        seen.iter()
                            .map(|(k, _)| k.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
            if group.len() > 1 {
                duplicate_agreement.push(json!({
                    "recipe_keccak256": recipe,
                    "calldata_keccak256": calldata,
                    "rows": group.iter().map(|r| r.key.clone()).collect::<Vec<_>>(),
                    "observations_identical": agreed,
                }));
            }
            if self.distinct.iter().any(|(k, _)| k == recipe) {
                continue;
            }
            // One representative per run, chosen by the smallest key so the choice is not a
            // function of the order the files happened to be read in.
            let best = group
                .iter()
                .min_by_key(|r| r.key.clone())
                .expect("a group has at least one row");
            self.distinct
                .push((format!("{recipe}#{calldata}"), (*best).clone()));
        }
        self.distinct.sort_by(|a, b| a.0.cmp(&b.0));
        self.check(
            "row_inventory",
            json!({
                "files_with_rows": files.len(),
                "published_rows": self.rows.len(),
                "distinct_revm_runs": self.distinct.len(),
                "distinct_scenarios": self.distinct.iter().map(|(_, r)| r.scenario.clone()).collect::<Vec<_>>(),
                "duplicate_agreement": duplicate_agreement,
            }),
        );
        Ok(())
    }
}

/// Every file under `dir`, recursively, as `<subdir>/<name>`, sorted.
fn sorted_dir(dir: &Path) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let read =
            std::fs::read_dir(&path).map_err(|e| format!("listing {}: {e}", path.display()))?;
        for entry in read {
            let entry = entry.map_err(|e| format!("listing {}: {e}", path.display()))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(dir) {
                out.push(rel.display().to_string().replace('\\', "/"));
            }
        }
    }
    out.sort();
    Ok(out)
}

// ---------------------------------------------------------------------------
// Phase 2 — §48's rebuild: does the published recipe still produce the published state?
// ---------------------------------------------------------------------------

/// One recipe row → one [`StateOverride`], in the spelling the recipe's own `how_to_apply` states.
/// No field is defaulted: an absent `balance` stays absent, because an override that fills in a
/// zero is this gate writing state the publisher did not declare.
fn override_from_row(row: &Value) -> Result<StateOverride, String> {
    let reason = field_str(row, "reason").unwrap_or_default();
    match field_str(row, "kind").as_deref() {
        Ok("account") => {
            let address = field_addr(row, "address")?;
            let balance = field_u256(row, "balance")?;
            let nonce = field_u64(row, "nonce")?;
            let code = field_bytes(row, "code")?;
            Ok(StateOverride {
                address,
                balance: Some(balance),
                nonce: Some(nonce),
                code: Some(code),
                slots: Vec::new(),
                reason,
            })
        }
        Ok("word") => {
            let address = field_addr(row, "contract")?;
            let slot = field_bytes(row, "slot")?;
            if slot.len() != 32 {
                return Err(format!(
                    "word row at {address:#x} names a {}-byte slot, not 32",
                    slot.len()
                ));
            }
            let slot = U256::from_be_slice(&slot);
            let value = field_u256(row, "value")?;
            Ok(StateOverride {
                address,
                balance: None,
                nonce: None,
                code: None,
                slots: vec![(slot, value)],
                reason,
            })
        }
        Ok(other) => Err(format!("recipe row has unknown kind {other:?}")),
        Err(e) => Err(e.clone()),
    }
}

impl Gate {
    /// The recipe's own claims, checked against the two dumps on disk it says it reproduces.
    ///
    /// This is §48 before §48's simulation step: if the recipe does not rebuild the fixture the
    /// publisher says it rebuilds, then the replay below is running against a state nobody attested,
    /// and a byte-identical outcome would only prove the two files are mutually consistent.
    fn state_integrity(&mut self) -> Value {
        // Which recipes the publisher declares to be variants of the committed fixture. A variant
        // is *supposed* to disagree with it — that is the whole mechanism behind the three
        // allow-list and operator controls — so comparing a variant against the fixture would
        // report the control as a defect.
        let mut variant_by_recipe: BTreeMap<String, bool> = BTreeMap::new();
        let mut row_state_counts: BTreeMap<String, RecipeCensus> = BTreeMap::new();
        let distinct = self.distinct.clone();
        for (_, row) in &distinct {
            let rel = field_str(row.state(), "recipe_file").unwrap_or_default();
            let variant =
                field_bool(row.state(), "uses_a_variant_of_the_committed_fixture").unwrap_or(false);
            variant_by_recipe.entry(rel.clone()).or_insert(variant);
            let count = |key: &str| row.state().get(key).and_then(Value::as_u64);
            row_state_counts.entry(rel).or_insert((
                count("recipe_rows"),
                count("declared_accounts"),
                count("declared_words"),
            ));
        }
        let mut recipes: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for (_, row) in &distinct {
            let rel = field_str(row.state(), "recipe_file").unwrap_or_default();
            if recipes.contains_key(&rel) {
                continue;
            }
            match read_json(&self.root, &rel) {
                Ok(doc) => {
                    let rows = doc
                        .get("rows")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    recipes.insert(rel, rows);
                }
                Err(e) => self.note("state", e),
            }
        }

        let fixture_rel = "fixtures/simulation-m10/fixture-37530593-executor.json";
        let fixture = match StateDump::from_file(&self.root.join(fixture_rel)) {
            Ok(dump) => Some(dump),
            Err(e) => {
                self.note("state", format!("reading {fixture_rel}: {e}"));
                None
            }
        };

        let mut table = Vec::new();
        for (rel, rows) in &recipes {
            let doc = match read_json(&self.root, rel) {
                Ok(doc) => doc,
                Err(_) => continue,
            };
            let mut parse_errors = Vec::new();
            for row in rows {
                if let Err(e) = override_from_row(row) {
                    parse_errors.push(e);
                }
            }
            if !parse_errors.is_empty() {
                self.note("state", format!("{rel}: {}", parse_errors.join("; ")));
            }

            let is_variant = *variant_by_recipe.get(rel).unwrap_or(&false);
            let mut row_mismatches = Vec::new();
            let mut undeclared = Vec::new();
            if let Some(fixture) = &fixture {
                if !is_variant {
                    // Forward: every declared row lands on the fixture's value.
                    for row in rows {
                        match field_str(row, "kind").as_deref() {
                            Ok("account") => {
                                let Ok(address) = field_addr(row, "address") else {
                                    continue;
                                };
                                let Some(account) = fixture.account(address) else {
                                    row_mismatches.push(format!(
                                        "{address:#x} is declared but the \
                                                               fixture has no such account"
                                    ));
                                    continue;
                                };
                                let want = field_u256(row, "balance").unwrap_or(U256::ZERO);
                                if word_from_text(&account.balance) != Some(want) {
                                    row_mismatches.push(format!(
                                        "{address:#x} balance: row says \
                                            {want}, fixture says {}",
                                        account.balance
                                    ));
                                }
                                let nonce = field_u64(row, "nonce").unwrap_or(u64::MAX);
                                if account.nonce != nonce {
                                    row_mismatches.push(format!(
                                        "{address:#x} nonce: row says \
                                            {nonce}, fixture says {}",
                                        account.nonce
                                    ));
                                }
                                let Ok(code) = field_bytes(row, "code") else {
                                    continue;
                                };
                                if hex_of(&code) != account.code {
                                    row_mismatches.push(format!(
                                        "{address:#x} code: row says {} bytes, fixture says a \
                                         different {} bytes",
                                        code.len(),
                                        (account.code.len() - 2) / 2
                                    ));
                                }
                            }
                            Ok("word") => {
                                let Ok(address) = field_addr(row, "contract") else {
                                    continue;
                                };
                                let Ok(slot) = field_bytes(row, "slot") else {
                                    continue;
                                };
                                if slot.len() != 32 {
                                    continue;
                                }
                                let slot = U256::from_be_slice(&slot);
                                let Some(found) = fixture.storage(address, slot) else {
                                    row_mismatches.push(format!(
                                    "{address:#x} slot {} is declared but the fixture has no such word",
                                    hex_of(&slot.to_be_bytes::<32>())
                                ));
                                    continue;
                                };
                                let want = field_u256(row, "value").unwrap_or(U256::ZERO);
                                if found != want {
                                    row_mismatches.push(format!(
                                    "{address:#x} slot {}: row says {want}, fixture says {found}",
                                    hex_of(&slot.to_be_bytes::<32>())
                                ));
                                }
                            }
                            _ => {}
                        }
                    }

                    // Backward: the fixture is the recording plus these rows and nothing else. Any
                    // fixture fact the recording does not already carry has to be declared, or the
                    // recipe is incomplete and the rebuild below is not the published state. A fact
                    // a row declares is not undeclared by definition — the forward pass above has
                    // already checked it lands where the fixture says it lands.
                    let declared_accounts: BTreeSet<Address> = rows
                        .iter()
                        .filter(|row| field_str(row, "kind").as_deref() == Ok("account"))
                        .filter_map(|row| field_addr(row, "address").ok())
                        .collect();
                    let declared_words: BTreeSet<(Address, U256)> = rows
                        .iter()
                        .filter(|row| field_str(row, "kind").as_deref() == Ok("word"))
                        .filter_map(|row| {
                            let address = field_addr(row, "contract").ok()?;
                            let slot = field_bytes(row, "slot").ok()?;
                            (slot.len() == 32).then(|| (address, U256::from_be_slice(&slot)))
                        })
                        .collect();
                    let recording_rel = field_str(&doc["recording"], "file")
                        .unwrap_or_else(|_| "the recording".to_string());
                    match StateDump::from_file(&self.root.join(&recording_rel)) {
                        Ok(recording) => {
                            for (key, account) in &fixture.accounts {
                                let Ok(address) = key.parse::<Address>() else {
                                    undeclared
                                        .push(format!("{key} is not a parseable account key"));
                                    continue;
                                };
                                if declared_accounts.contains(&address) {
                                    continue;
                                }
                                let Some(found) = recording.account(address) else {
                                    undeclared.push(format!(
                                        "account {key} has no row and is not \
                                                             in the recording"
                                    ));
                                    continue;
                                };
                                if found.balance != account.balance
                                    || found.nonce != account.nonce
                                    || found.code != account.code
                                {
                                    undeclared.push(format!(
                                        "account {key} differs from the recording and has no row"
                                    ));
                                }
                            }
                            for (key, words) in &fixture.storage {
                                let Ok(address) = key.parse::<Address>() else {
                                    undeclared
                                        .push(format!("{key} is not a parseable storage key"));
                                    continue;
                                };
                                for (slot_key, value) in words {
                                    let Ok(slot) = slot_hex(slot_key) else {
                                        undeclared.push(format!("{key} {slot_key} is not a slot"));
                                        continue;
                                    };
                                    if declared_words.contains(&(address, slot)) {
                                        continue;
                                    }
                                    match recording.storage(address, slot) {
                                        Some(found) if word_from_text(value) == Some(found) => {}
                                        Some(_) => undeclared.push(format!(
                                            "{key} slot {slot_key} differs from the recording and \
                                             has no row"
                                        )),
                                        None => undeclared.push(format!(
                                            "{key} slot {slot_key} is not in the recording and has \
                                             no row"
                                        )),
                                    }
                                }
                            }
                        }
                        Err(e) => self.note("state", format!("{rel}: {e}")),
                    }
                }
            }

            let accounts = rows
                .iter()
                .filter(|row| field_str(row, "kind").as_deref() == Ok("account"))
                .count();
            let words = rows
                .iter()
                .filter(|row| field_str(row, "kind").as_deref() == Ok("word"))
                .count();
            // The row's own `state` block restates the recipe's census. If the census in the row
            // disagrees with the rows on disk, the file that names the run is describing a run that
            // is not there.
            let declared = row_state_counts.get(rel);
            let census_agrees =
                declared.is_some_and(|(state_rows, state_accounts, state_words)| {
                    (state_rows.is_none_or(|n| n as usize == rows.len()))
                        && (state_accounts.is_none_or(|n| n as usize == accounts))
                        && (state_words.is_none_or(|n| n as usize == words))
                        && doc["row_count"]["total"].as_u64().map(|n| n as usize)
                            == Some(rows.len())
                        && doc["row_count"]["accounts"].as_u64().map(|n| n as usize)
                            == Some(accounts)
                        && doc["row_count"]["words"].as_u64().map(|n| n as usize) == Some(words)
                }) && declared.is_some();

            table.push(json!({
                "recipe_file": rel,
                "recipe_name": doc.get("name").and_then(Value::as_str),
                "rows": rows.len(),
                "accounts": accounts,
                "words": words,
                "row_count_field": doc.get("row_count"),
                "declared_by_the_scenario_row": declared.map(|(rows, accounts, words)| json!({
                    "recipe_rows": rows,
                    "declared_accounts": accounts,
                    "declared_words": words,
                })),
                "census_agrees": census_agrees,
                "compared_against_the_committed_fixture": !is_variant,
                "is_a_variant_of_the_committed_fixture": is_variant,
                "variant_reason": if is_variant {
                    "the three allow-list and operator controls plant their refusal in a declared \
                     word, so their rows must differ from the committed fixture; §48's replay \
                     judges them by re-running them, not by equating them to the fixture"
                } else {
                    "not a variant"
                },
                "rows_match_the_fixture": row_mismatches.is_empty(),
                "fixture_mismatches": row_mismatches,
                "fixture_facts_with_no_row": undeclared,
                "fixture_facts_with_no_row_count": undeclared.len(),
                "recording": doc.get("recording"),
                "committed_fixture": doc.get("committed_fixture"),
                "state_source": doc.get("state_source"),
            }));
        }
        let value = self.check(
            "state_integrity",
            json!({
                "committed_fixture_file": fixture_rel,
                "committed_fixture_digest": file_digest(&self.root, fixture_rel).ok(),
                "recipes": table,
                "note": "a recipe row that disagrees with the committed fixture is a state nobody \
                         attested; the replay below would then be self-consistent and worthless. \
                         The reverse column is the same claim from the fixture's side: every fact \
                         the fixture holds that the recording does not already hold must be a row.",
            }),
        );
        for entry in &table {
            if entry["rows_match_the_fixture"] == Value::Bool(false) {
                self.note(
                    "state",
                    format!("{}: {}", entry["recipe_file"], entry["fixture_mismatches"]),
                );
            }
            if let Some(missing) = entry["fixture_facts_with_no_row"].as_array() {
                if !missing.is_empty() && entry["compared_against_the_committed_fixture"] == true {
                    self.note(
                        "state",
                        format!(
                            "{}: {} fixture facts have no row",
                            entry["recipe_file"],
                            missing.len()
                        ),
                    );
                }
            }
        }
        value
    }
}

/// A 256-bit word in whichever of the three spellings this directory uses: hex (what `serde`
/// writes for a `U256`), decimal (what the publishers write by hand), or a JSON number.
fn word_of(v: &Value) -> Option<U256> {
    if let Some(n) = v.as_u64() {
        return Some(U256::from(n));
    }
    word_from_text(v.as_str()?)
}

/// The same rule for a bare string, which is how the dumps spell their slot keys and values.
fn word_from_text(text: &str) -> Option<U256> {
    let text = text.trim();
    if let Some(hexed) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return U256::from_str_radix(hexed, 16).ok();
    }
    U256::from_str_radix(text, 10).ok()
}

/// A dump's storage key: `0x` and 64 hex digits, fixed width so ordering never depends on how many
/// leading zeros a printer happened to keep.
fn slot_hex(text: &str) -> Result<U256, ()> {
    word_from_text(text).ok_or(())
}

/// The one-word answer a `status` column carries: `Success`, `Reverted`, `OutOfGas`, `Halted`.
fn status_word(v: &Value) -> String {
    if let Some(text) = v.as_str() {
        return text.to_string();
    }
    if let Some(object) = v.as_object() {
        if let Some((key, _)) = object.iter().next() {
            return key.clone();
        }
    }
    String::new()
}

/// The revert payload a published row carries. `serde` writes a `Bytes` field as `0x` hex, which is
/// how every row here spells it; the Debug string the real evidence carries is handled by its own
/// caller, not by guessing at this one.
fn published_revert_bytes(row: &Value) -> Option<Bytes> {
    let text = row.get("status")?.get("Reverted")?.get("raw")?.as_str()?;
    hex::decode(text.strip_prefix("0x").unwrap_or(text))
        .map(Bytes::from)
        .ok()
}

/// The [`ExecutorRun`] the published spec describes, rebuilt from JSON alone.
///
/// The call is **decoded from the published calldata bytes** rather than re-assembled from the
/// published `call` object. That is deliberate: if this gate encoded the fields itself, a bug in
/// the field set would be reproduced by the bug's own author, and the run would agree with a
/// transaction nobody can send. The decoded call is then compared against the published fields, so
/// the two spellings still have to agree — in the other direction.
fn run_spec_from_published(row: &Row) -> Result<ExecutorRun, String> {
    let spec = row.run_spec();
    let calldata = field_bytes(spec, "calldata")?;
    let call =
        decode_calldata(&calldata).map_err(|e| format!("decoding the published calldata: {e}"))?;
    let rules = match field_str(spec, "rules")?.as_str() {
        "Shanghai" => EvmRules::Shanghai,
        "Cancun" => EvmRules::Cancun,
        "Prague" => EvmRules::Prague,
        "Osaka" => EvmRules::Osaka,
        other => {
            return Err(format!(
                "published rules {other:?} is not a ruleset this gate knows; refusing to run the \
                 fixture under a guessed one"
            ))
        }
    };
    let pricing = spec
        .get("pricing")
        .ok_or_else(|| "run_spec.pricing is absent".to_string())?;
    let pricing = match field_str(pricing, "kind")?.as_str() {
        "eip1559" => GasPricing::Eip1559 {
            priority_fee_per_gas: u128::from(field_u64(pricing, "priority_fee_per_gas")?),
            provenance: field_str(pricing, "provenance")?,
        },
        "legacy" => GasPricing::Legacy {
            gas_price: u128::from(field_u64(pricing, "gas_price")?),
            provenance: field_str(pricing, "provenance")?,
        },
        "unresolved" => GasPricing::Unresolved {
            reason: field_str(pricing, "reason")?,
        },
        other => {
            return Err(format!(
                "published pricing kind {other:?} is not a model this gate knows; §29 forbids \
                 guessing one"
            ))
        }
    };
    Ok(ExecutorRun {
        chain_id: ChainId(field_u64(spec, "chain_id")?),
        priced_at: BlockNumber(field_u64(spec, "priced_at_block")?),
        state_source: field_str(spec, "state_source")?,
        executor: field_addr(spec, "executor")?,
        operator: field_addr(spec, "operator")?,
        call,
        gas_limit: field_u64(spec, "gas_limit")?,
        rules,
        pricing,
        endowment: Some(field_u256(spec, "endowment_wei")?),
    })
}

/// The published `run_spec.call` object against the call decoded from the published bytes.
fn call_against_published(call: &ExecutorCall, published: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    let (legs, input_token, amount_in, min_final, recipient) = match call {
        ExecutorCall::Execute {
            legs,
            input_token,
            amount_in,
            min_final_amount,
            recipient,
        } => (
            legs.clone(),
            *input_token,
            *amount_in,
            *min_final_amount,
            *recipient,
        ),
        other => {
            problems.push(format!(
                "the published bytes decode as {} while the row's call object declares kind {:?}",
                other.signature(),
                published.get("kind").and_then(Value::as_str)
            ));
            return problems;
        }
    };
    if published.get("kind").and_then(Value::as_str) != Some("execute") {
        problems.push("the row's call object does not declare kind \"execute\"".to_string());
    }
    for (name, decoded, published_value) in [
        ("amount_in", amount_in, &published["amount_in"]),
        (
            "min_final_amount",
            min_final,
            &published["min_final_amount"],
        ),
    ] {
        match word_of(published_value) {
            Some(want) if want != decoded => problems.push(format!(
                "{name}: decoded {decoded}, published {published_value}"
            )),
            None => problems.push(format!(
                "{name}: the published row spells it {published_value}, which is not a word this \
                 gate can compare"
            )),
            _ => {}
        }
    }
    if field_addr(published, "input_token").ok() != Some(input_token) {
        problems.push(format!(
            "input_token: decoded {input_token:#x}, published elsewhere"
        ));
    }
    if field_addr(published, "recipient").ok() != Some(recipient) {
        problems.push(format!(
            "recipient: decoded {recipient:#x}, published elsewhere"
        ));
    }
    let published_legs = published["legs"].as_array().cloned().unwrap_or_default();
    if published_legs.len() != legs.len() {
        problems.push(format!(
            "{} legs decoded, {} published",
            legs.len(),
            published_legs.len()
        ));
        return problems;
    }
    for (index, (leg, published)) in legs.iter().zip(published_legs.iter()).enumerate() {
        let ExecutorLeg {
            pool,
            token_in,
            token_out,
            amount_in,
            amount_out,
            min_amount_out,
        } = *leg;
        for (name, want, found) in [
            ("pool", field_addr(published, "pool"), Ok::<_, String>(pool)),
            (
                "token_in",
                field_addr(published, "token_in"),
                Ok::<_, String>(token_in),
            ),
            (
                "token_out",
                field_addr(published, "token_out"),
                Ok::<_, String>(token_out),
            ),
        ] {
            if let (Ok(want), Ok(found)) = (want, found) {
                if want != found {
                    problems.push(format!(
                        "legs[{index}].{name}: decoded {found:#x}, published {want:#x}"
                    ));
                }
            }
        }
        for (name, decoded, published_value) in [
            ("amount_in", amount_in, &published["amount_in"]),
            ("amount_out", amount_out, &published["amount_out"]),
            (
                "min_amount_out",
                min_amount_out,
                &published["min_amount_out"],
            ),
        ] {
            match word_of(published_value) {
                Some(want) if want != decoded => problems.push(format!(
                    "legs[{index}].{name}: decoded {decoded}, published {}",
                    published_value
                )),
                None => problems.push(format!(
                    "legs[{index}].{name}: the published row spells it {published_value}, which is \
                     not a word this gate can compare"
                )),
                _ => {}
            }
        }
    }
    problems
}

impl Gate {
    /// §49's D3, in the only form that means something: a second process, a different crate, the
    /// published recipe and the published spec, and the same bytes back out.
    ///
    /// The comparison is the whole `observed` block, not a hand-picked subset. A gate that compares
    /// `gas_used` and `status` and skips `state_changes` is a gate that cannot see a provider that
    /// serves different storage while still reverting at the same place.
    async fn d3_replay(&mut self) {
        let mut table = Vec::new();
        let mut identical = 0usize;
        let distinct = self.distinct.clone();
        for (identity, row) in &distinct {
            let entry = self.replay_one(identity, row).await;
            if entry["observed_json_identical"] == Value::Bool(true) {
                identical += 1;
            } else {
                self.note("d3", format!("{identity}: {}", entry["differing_or_error"]));
            }
            table.push(entry);
        }
        let value = json!({
            "layer": "revm",
            "runs_replayed": table.len(),
            "runs_byte_identical": identical,
            "identity_of_a_run": "the recipe file's keccak256 plus the published calldata keccak256; \
                                  a row's position in a directory is not an identity",
            "comparison": "serde_json of the rebuilt ExecutorOutcome against the published observed \
                           block, key by key, with the differing keys named",
            "runs": table,
            "note": "the publisher could only claim two runs inside one process (§49's D3 is the \
                     cross-process form); this file is compiled by a different crate and started by \
                     a different cargo test invocation, and it never reads the publisher's Rust.",
        });
        self.check("d3_replay", value);
    }

    async fn replay_one(&self, identity: &str, row: &Row) -> Value {
        let attempted: Result<Value, String> = async {
            let recipe_rel = field_str(row.state(), "recipe_file")?;
            let recipe = read_json(&self.root, &recipe_rel)?;
            let rows = recipe
                .get("rows")
                .and_then(Value::as_array)
                .cloned()
                .ok_or_else(|| format!("{recipe_rel} has no rows array"))?;
            let mut overrides = Vec::with_capacity(rows.len());
            for entry in &rows {
                overrides.push(override_from_row(entry)?);
            }
            let recording_rel = field_str(&recipe["recording"], "file")?;
            let override_count = overrides.len();
            let dump = StateDump::from_file(&self.root.join(&recording_rel))
                .map_err(|e| format!("reading the recording {recording_rel}: {e}"))?;
            let provider: Arc<dyn StateProvider> = Arc::new(
                DumpStateProvider::new(dump, field_str(row.run_spec(), "state_source")?)
                    .with_overrides(overrides),
            );
            let run = run_spec_from_published(row)?;
            let call_problems = call_against_published(
                &run.call,
                row.run_spec().get("call").unwrap_or(&Value::Null),
            );
            let outcome = run_executor(provider, &run)
                .await
                .map_err(|e| format!("the rebuilt run refused: {e}"))?;
            let rebuilt = serde_json::to_value(&outcome)
                .map_err(|e| format!("serializing the rebuilt outcome: {e}"))?;
            let published = row.observed();
            let mut differing = Vec::new();
            let mut keys: BTreeSet<String> = BTreeSet::new();
            for key in rebuilt.as_object().into_iter().flat_map(|m| m.keys()) {
                keys.insert(key.clone());
            }
            for key in published.as_object().into_iter().flat_map(|m| m.keys()) {
                keys.insert(key.clone());
            }
            for key in keys {
                if rebuilt.get(&key) != published.get(&key) {
                    differing.push(key);
                }
            }
            Ok(json!({
                "identity": identity,
                "row": row.key,
                "scenario": row.scenario,
                "file": row.file,
                "recipe_file": recipe_rel,
                "recording_file": recording_rel,
                "overrides_rebuilt": override_count,
                "published_bytes_decode_to_the_published_call_fields": call_problems,
                "calldata_hash_rebuilt": format!("{:#x}", alloy_primitives::keccak256(outcome.calldata.as_ref())),
                "calldata_hash_published": row.run_spec()["calldata_keccak256"],
                "gas_used_rebuilt": outcome.gas_used,
                "status_rebuilt": status_word(&rebuilt["status"]),
                "observed_json_identical": differing.is_empty(),
                "differing_or_error": if differing.is_empty() { json!("") } else { json!(differing) },
            }))
        }
        .await;
        match attempted {
            Ok(value) => value,
            Err(e) => json!({
                "identity": identity,
                "row": row.key,
                "scenario": row.scenario,
                "observed_json_identical": false,
                "differing_or_error": e,
            }),
        }
    }

    /// §49's D4 — the same failure classified the same way, by a decoder run in a different crate
    /// over the bytes the publisher left in the row.
    ///
    /// The classification is only half of it. The row also claims what the run *did*: whether the
    /// market moved, how many logs came out, whether anything was returned, and whether any state
    /// was left different (§27's zero-residue rule is the reason the forced second-leg revert is
    /// evidence at all rather than a curiosity). Each of those is re-derived here from the published
    /// rows, not read back out of the row's `verdict` column.
    fn d4_rejudge(&mut self) {
        let mut table = Vec::new();
        let mut classification_problems = 0usize;
        let mut judgement_problems = 0usize;
        for row in &self.rows.clone() {
            let observed = row.observed();
            let expected = row.expected();
            let mut classification = Vec::new();
            let mut judgement = Vec::new();

            let status = status_word(observed.get("status").unwrap_or(&Value::Null));
            let raw = published_revert_bytes(observed);
            let payload = raw.as_ref().map(|bytes| decode_revert(bytes));
            let mut typed_args = Value::Null;

            match expected.get("outcome").and_then(Value::as_str) {
                Some("success") => {
                    if status != "Success" {
                        judgement.push(format!("the row expects success, the run says {status}"));
                    }
                    if raw.is_some() {
                        judgement
                            .push("a row that expects success carries revert data".to_string());
                    }
                    for field in ["contract_error", "revert_kind"] {
                        if !observed.get(field).is_none_or(Value::is_null) {
                            judgement.push(format!("{field} is set on a run that did not revert"));
                        }
                    }
                }
                Some("reverted") => {
                    if status != "Reverted" {
                        judgement.push(format!("the row expects a revert, the run says {status}"));
                    }
                    match &payload {
                        None => classification.push(
                            "the row's revert data is not published as 0x hex bytes".to_string(),
                        ),
                        Some(payload) => {
                            let kind = payload.kind();
                            let contract_error = payload.executor().map(|e| e.name().to_string());
                            let published_kind = observed
                                .get("revert_kind")
                                .and_then(Value::as_str)
                                .map(str::to_string);
                            if published_kind.as_deref() != Some(kind) {
                                classification.push(format!(
                                    "revert_kind: this gate reads {kind:?} from the bytes, the row \
                                     publishes {published_kind:?}"
                                ));
                            }
                            let published_error = observed
                                .get("contract_error")
                                .and_then(Value::as_str)
                                .map(str::to_string);
                            if published_error != contract_error {
                                classification.push(format!(
                                    "contract_error: this gate reads {contract_error:?} from the \
                                     bytes, the row publishes {published_error:?}"
                                ));
                            }
                            // The row's own `expected` block names the rejection it planted. A null
                            // there is the publisher declining to claim a contract-level reason,
                            // which is exactly what a pair's `Error(string)` looks like.
                            let claims = [
                                (
                                    "contract_error",
                                    expected.get("contract_error"),
                                    contract_error.clone(),
                                ),
                                (
                                    "revert_kind",
                                    expected.get("revert_kind"),
                                    Some(kind.to_string()),
                                ),
                            ];
                            for (field, claimed, found) in claims {
                                if let Some(claim) = claimed.and_then(Value::as_str) {
                                    if found.as_deref() != Some(claim) {
                                        classification.push(format!(
                                            "{field}: the row plants {claim:?}, the bytes decode \
                                             as {found:?}"
                                        ));
                                    }
                                }
                            }
                            // For the profit guard, the revert payload carries the plan's own floor.
                            // If the decoded floor is not the amount the row says it demanded, the
                            // transaction that reverted is not the transaction the row sent.
                            if let Some(ExecutorRevert::FinalShortfall { floor, delivered }) =
                                payload.executor()
                            {
                                let demanded = word_of(&observed["min_final_amount"]);
                                if Some(floor) != demanded {
                                    classification.push(format!(
                                        "FinalShortfall.floor is {floor}, the call demanded {demanded:?}"
                                    ));
                                }
                                typed_args = json!({
                                    "FinalShortfall": {
                                        "delivered": delivered.to_string(),
                                        "floor": floor.to_string(),
                                    }
                                });
                            }
                        }
                    }
                }
                Some(other) => judgement.push(format!("unknown expected outcome {other:?}")),
                None => judgement.push("the row declares no expected outcome".to_string()),
            }

            // --- what came back, against what the row claims came back
            let delivered_claim = expected.get("delivered").and_then(Value::as_str);
            let delivered_published = word_of(&observed["delivered"]);
            let delivered = match delivered_claim {
                None | Some("none") => {
                    if delivered_published.is_some() {
                        judgement.push(format!(
                            "the row claims nothing was returned and the run returned {delivered_published:?}"
                        ));
                    }
                    json!({ "claim": delivered_claim.unwrap_or("none"), "found": null })
                }
                Some("route_weth_out_leg2") => {
                    // The fixture's own quote prices the second leg; the success claim is that the
                    // run delivered exactly that. It is the one number in this directory that ties
                    // a simulated output back to the plan's arithmetic.
                    let ask = row
                        .value
                        .get("route")
                        .and_then(|r| r.get("leg_1"))
                        .and_then(|l| l.get("ask"));
                    let want = ask.and_then(word_of);
                    if want.is_none() || want != delivered_published {
                        judgement.push(format!(
                            "delivered: {delivered_published:?} against the second leg's published \
                             ask {want:?}"
                        ));
                    }
                    json!({ "claim": "route_weth_out_leg2", "leg_1_ask": want.map(|v| v.to_string()), "found": delivered_published.map(|v| v.to_string()) })
                }
                Some("any") => json!({
                    "claim": "any",
                    "note": "the publisher declines to judge the number, so this gate does not either",
                }),
                Some(other) => {
                    judgement.push(format!("unknown delivered claim {other:?}"));
                    json!(null)
                }
            };
            let logs_published = observed
                .get("logs")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(usize::MAX);
            let logs_claimed = expected.get("logs").filter(|v| !v.is_null()).cloned();
            if logs_claimed.is_none() {
                if observed.get("logs").is_none() {
                    judgement.push("no logs array is published".to_string());
                }
            } else if logs_claimed
                .as_ref()
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                != Some(logs_published)
            {
                judgement.push(format!(
                    "logs: the row claims {logs_claimed:?}, the run has {logs_published}"
                ));
            }

            // --- §27's residue. The publisher's own `residue_keys` names three things that count:
            // a reserve, a token balance, a storage word. The sender's native account is not one of
            // them — it pays for gas and its nonce moves on a reverted run too — so it is excluded
            // by name and published, not silently dropped.
            let differing = |rows: Option<&Vec<Value>>, before: &str, after: &str| -> Vec<String> {
                let rows = rows.cloned().unwrap_or_default();
                let mut out: Vec<String> = rows
                    .iter()
                    .filter(|entry| entry.get(before) != entry.get(after))
                    .map(|entry| entry.to_string())
                    .collect();
                out.sort();
                out
            };
            let reserve_rows = observed.get("reserves").and_then(Value::as_array).cloned();
            let balance_rows = observed.get("balances").and_then(Value::as_array).cloned();
            let state_changes = observed
                .get("state_changes")
                .cloned()
                .unwrap_or(Value::Null);
            let slot_rows = state_changes
                .get("slots")
                .and_then(Value::as_array)
                .cloned();
            let account_rows = state_changes
                .get("accounts")
                .and_then(Value::as_array)
                .cloned();
            let reserves_changed = differing(reserve_rows.as_ref(), "before", "after");
            let balances_changed = differing(balance_rows.as_ref(), "before", "after");
            let slots_changed = differing(slot_rows.as_ref(), "before", "after");
            let sender_rows: Vec<String> = {
                let mut rows: Vec<String> = account_rows
                    .unwrap_or_default()
                    .iter()
                    .map(|entry| {
                        format!(
                            "{} native {} → {} and nonce {} → {}",
                            entry["address"],
                            entry["balance_before"],
                            entry["balance_after"],
                            entry["nonce_before"],
                            entry["nonce_after"]
                        )
                    })
                    .collect();
                rows.sort();
                rows
            };
            let residue_found =
                reserves_changed.len() + balances_changed.len() + slots_changed.len();
            let residue_claim = expected.get("residue").and_then(Value::as_str);
            let residue_agrees = matches!(residue_claim, Some("none") if residue_found == 0)
                || matches!(residue_claim, Some("any") if residue_found > 0);
            if !residue_agrees {
                judgement.push(format!(
                    "residue: the row claims {residue_claim:?} and the published rows differ in \
                     {residue_found} places"
                ));
            }
            let market_moved_found = !reserves_changed.is_empty();
            if expected.get("market_moved").and_then(Value::as_bool) != Some(market_moved_found) {
                judgement.push(format!(
                    "market_moved: the published reserve rows say {market_moved_found}, the row \
                     claims {}",
                    expected["market_moved"]
                ));
            }

            if !classification.is_empty() {
                classification_problems += 1;
                self.note("d4", format!("{}: {}", row.key, classification.join("; ")));
            }
            if !judgement.is_empty() {
                judgement_problems += 1;
                self.note("d4", format!("{}: {}", row.key, judgement.join("; ")));
            }
            table.push(json!({
                "row": row.key,
                "scenario": row.scenario,
                "status": status,
                "revert_bytes": raw.map(|bytes| format!("{bytes:#x}")),
                "classified_by_this_gate": payload.as_ref().map(|payload| json!({
                    "kind": payload.kind(),
                    "contract_error": payload.executor().map(|e| e.name().to_string()),
                    "solidity_signature": payload.executor().map(|e| e.signature()),
                    "unrecognized_bytes": matches!(payload, RevertPayload::Unrecognized(_)),
                })),
                "typed_args_from_this_gate": typed_args,
                "as_published": {
                    "revert_kind": observed.get("revert_kind"),
                    "contract_error": observed.get("contract_error"),
                },
                "classification_problems": classification,
                "judgement_problems": judgement,
                "residue": {
                    "rule": "a reserve row, a token balance row or a storage word left different; \
                             the sender's native account is excluded because it pays gas and its \
                             nonce moves even on a reverted run",
                    "reserves_changed": reserves_changed.len(),
                    "balances_changed": balances_changed.len(),
                    "storage_words_changed": slots_changed.len(),
                    "total_judged": residue_found,
                    "claim": residue_claim,
                    "agrees": residue_agrees,
                },
                "storage_words_changed_rows": slots_changed,
                "reserves_changed_rows": reserves_changed,
                "balances_changed_rows": balances_changed,
                "sender_accounts_outside_the_residue_claim": sender_rows,
                "delivered": delivered,
                "logs": { "published": logs_published, "claimed": logs_claimed },
                "market_moved": {
                    "found_from_the_published_reserve_rows": market_moved_found,
                    "claimed": expected.get("market_moved"),
                },
            }));
        }
        let real = self.rejudge_the_real_failure();
        self.check(
            "d4_rejudge",
            json!({
                "rows_rejudged": table.len(),
                "rows_with_classification_problems": classification_problems,
                "rows_with_judgement_problems": judgement_problems,
                "rows": table,
                "real_chain_failure": real,
                "note": "the fixture rows are judged from `observed.status.Reverted.raw`; the real \
                         row publishes its revert data inside a Debug string, and its floor argument \
                         is compared against the plan's published `min_final_amount_wei`.",
            }),
        );
    }

    /// §49's D4 on the one failure that happened on a real chain. Its revert data is published as a
    /// Debug string rather than a byte field, so the hex word inside it is read out and decoded.
    /// §58 forbids turning an unread result into a plausible one, so a row that does not carry the
    /// pattern is reported as drift rather than classified anyway.
    fn rejudge_the_real_failure(&mut self) -> Value {
        let mut typed_args = Value::Null;
        let text = self.failure["simulation_attribution"]["status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let Some(after) = text.split("raw: ").nth(1) else {
            self.note(
                "d4",
                "real/giwa_failure.json: no `raw:` field to read revert bytes from",
            );
            return json!({ "readable": false, "why": "the published status string carries no raw field" });
        };
        let hex_word = after.split(',').next().unwrap_or_default().trim();
        let Some(bytes) = hex::decode(hex_word.strip_prefix("0x").unwrap_or(hex_word))
            .ok()
            .map(Bytes::from)
        else {
            self.note(
                "d4",
                format!(
                "real/giwa_failure.json: {hex_word:?} is not the hex revert data the row claims"
            ),
            );
            return json!({ "readable": false, "why": "the raw field is not decodable hex" });
        };
        let payload = decode_revert(&bytes);
        let mut problems = Vec::new();
        let published_kind = self.failure["simulation_attribution"]["revert_kind"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if payload.kind() != published_kind {
            problems.push(format!(
                "the bytes decode as {:?}, the row publishes {published_kind:?}",
                payload.kind()
            ));
        }
        let published_error = self.failure["simulation_attribution"]["contract_error"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let decoded_error = payload.executor().map(|e| e.name().to_string());
        if decoded_error.as_deref() != Some(published_error.as_str()) {
            problems.push(format!(
                "contract_error: this gate reads {decoded_error:?}, the row publishes \
                 {published_error:?}"
            ));
        }
        // The floor argument is the plan's own demand, published as `min_final_amount_wei`. Reading
        // it back out of the real chain's revert data is the strongest §47 check in this directory:
        // the deployed contract answered the transaction the execution crate encoded, argument for
        // argument.
        let floor = match payload.executor() {
            Some(ExecutorRevert::FinalShortfall { delivered, floor }) => {
                typed_args = json!({
                    "FinalShortfall": {
                        "delivered": delivered.to_string(),
                        "floor": floor.to_string(),
                    }
                });
                Some(floor)
            }
            _ => {
                problems.push(format!(
                    "the row's label is a profit-guard failure but the payload is {:?}",
                    payload.kind()
                ));
                None
            }
        };
        let demanded = field_u256(&self.failure["simulation_attribution"], "min_final_amount");
        if let (Some(floor), Ok(demanded)) = (floor, demanded) {
            if floor != demanded {
                problems.push(format!(
                    "FinalShortfall.floor is {floor}, the published plan demanded {demanded}"
                ));
            }
        }
        let receipt = self.failure["step"].clone();
        if receipt["receipt_status"].as_str() != Some("reverted")
            || receipt["receipt_log_count"].as_u64() != Some(0)
        {
            problems.push("the receipt row is not a zero-log revert".to_string());
        }
        if self.failure["no_partial_state"].as_bool() != Some(true) {
            problems.push("the row claims no partial state without saying so".to_string());
        }
        for problem in &problems {
            self.note("d4", format!("real/giwa_failure.json: {problem}"));
        }
        json!({
            "readable": true,
            "raw_bytes": format!("{bytes:#x}"),
            "raw_bytes_len": bytes.len(),
            "selector": hex_word.get(0..10),
            "classified_by_this_gate": {
                "kind": payload.kind(),
                "contract_error": decoded_error,
                "solidity_signature": payload.executor().map(|e| e.signature()),
            },
            "as_published": {
                "revert_kind": published_kind,
                "contract_error": published_error,
                "min_final_amount_wei": self.failure.get("min_final_amount_wei"),
                "simulation_attribution.min_final_amount": self.failure["simulation_attribution"].get("min_final_amount"),
            },
            "decoded_args": typed_args,
            "receipt": {
                "status": receipt.get("receipt_status"),
                "logs": receipt.get("receipt_log_count"),
                "transaction_hash": receipt.get("transaction_hash"),
            },
            "no_partial_state": self.failure.get("no_partial_state"),
            "problems": problems,
        })
    }
}

// ---------------------------------------------------------------------------
// Phase 5 — §49's D1/D2/D5: the plan layer, rebuilt from published facts
// ---------------------------------------------------------------------------

/// The row this gate plants its two plan-layer controls on: the fixture's own successful round trip,
/// because a control is only a control if the unmutated case would have been accepted.
const PLAN_CONTROL_ROW: &str = "fixtures/success.json#success_round_trip";

/// The freshness bound this gate declares on a published row's behalf (§8).
///
/// No fixture row carries one — the publisher ran a simulation, not the ladder — and a
/// [`PlanValidity`] cannot be built without a number, so the number is declared here and announced
/// as declared wherever it reaches an artifact. It is the bound §57's real run published as
/// `freshness_window.declared_max_block_age`; this gate compares the two and publishes whether they
/// agree rather than trusting this line, so a drift between the declaration and the run shows up in
/// the evidence instead of in a comment.
const GATE_MAX_BLOCK_AGE: u64 = 20;

/// A published address with its last hex digit moved. The sensitivity table and the two planted
/// controls need an address that is *not* the published one; deriving it from the published value
/// keeps a fixture address from accidentally becoming the mutant, and the parse cannot fail because
/// the string is built from the 40 hex digits of an address that already parsed.
fn shifted(address: Address) -> Address {
    let mut digits: Vec<char> = format!("{address:x}").chars().collect();
    let last = digits.len() - 1;
    let value = u8::from_str_radix(&digits[last].to_string(), 16).unwrap_or(0);
    digits[last] = char::from_digit(u32::from((value + 1) % 16), 16).unwrap_or('1');
    format!("0x{}", digits.iter().collect::<String>())
        .parse::<Address>()
        .expect("40 hex digits taken from a published address are still an address")
}

/// One reading of the route: the fields the calldata carries.
#[derive(Clone, Debug)]
struct RoutePack {
    legs: Vec<PlanLeg>,
    input_token: Address,
    input_amount: U256,
    min_final_output: U256,
    recipient: Address,
}

impl RoutePack {
    /// The two spellings a route has in this directory: a plan's derivation is positional (§5 — the
    /// first leg spends the plan's input, every later leg carries the leg above it whole), and the
    /// calldata has no field for it, so neither construction can read it off its own source.
    fn leg(
        index: usize,
        pool: Address,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
        amount_out: U256,
        min_amount_out: U256,
    ) -> PlanLeg {
        PlanLeg {
            pool,
            token_in,
            token_out,
            amount_in,
            amount_out,
            min_amount_out,
            derivation: if index == 0 {
                AmountDerivation::PlanInput
            } else {
                AmountDerivation::PreviousLegOutput
            },
        }
    }

    /// Construction A: the published `run_spec.call` object, field by field.
    fn from_published_fields(row: &Row) -> Result<Self, String> {
        let call = row
            .run_spec()
            .get("call")
            .ok_or_else(|| "run_spec.call is absent".to_string())?;
        let legs_json = call
            .get("legs")
            .and_then(Value::as_array)
            .ok_or_else(|| "run_spec.call.legs is not an array".to_string())?;
        let mut legs = Vec::with_capacity(legs_json.len());
        for (index, leg) in legs_json.iter().enumerate() {
            legs.push(Self::leg(
                index,
                field_addr(leg, "pool")?,
                field_addr(leg, "token_in")?,
                field_addr(leg, "token_out")?,
                field_u256(leg, "amount_in")?,
                field_u256(leg, "amount_out")?,
                field_u256(leg, "min_amount_out")?,
            ));
        }
        Ok(Self {
            legs,
            input_token: field_addr(call, "input_token")?,
            input_amount: field_u256(call, "amount_in")?,
            min_final_output: field_u256(call, "min_final_amount")?,
            recipient: field_addr(call, "recipient")?,
        })
    }

    /// Construction B: the same route read back out of the published calldata **bytes**. This is the
    /// half that makes D1 mean something: built from the fields alone, both constructions would
    /// share their author, and a field the encoder drops would be dropped by the witness too.
    fn from_published_calldata(row: &Row) -> Result<Self, String> {
        let calldata = field_bytes(row.run_spec(), "calldata")?;
        let call = decode_calldata(&calldata)
            .map_err(|e| format!("decoding the published calldata: {e}"))?;
        let (legs_json, input_token, amount_in, min_final_output, recipient) = match &call {
            ExecutorCall::Execute {
                legs,
                input_token,
                amount_in,
                min_final_amount,
                recipient,
            } => (
                legs.clone(),
                *input_token,
                *amount_in,
                *min_final_amount,
                *recipient,
            ),
            other => {
                return Err(format!(
                    "the published calldata decodes as {}, which is not the execute call this \
                     row's plan describes",
                    other.signature()
                ))
            }
        };
        let legs = legs_json
            .into_iter()
            .enumerate()
            .map(|(index, leg)| {
                Self::leg(
                    index,
                    leg.pool,
                    leg.token_in,
                    leg.token_out,
                    leg.amount_in,
                    leg.amount_out,
                    leg.min_amount_out,
                )
            })
            .collect();
        Ok(Self {
            legs,
            input_token,
            input_amount: amount_in,
            min_final_output,
            recipient,
        })
    }

    /// Where the two spellings disagree, named field by field rather than only as a hash difference.
    fn diff(&self, other: &Self) -> Vec<String> {
        let mut out = Vec::new();
        for (name, a, b) in [
            ("input_token", self.input_token, other.input_token),
            ("recipient", self.recipient, other.recipient),
        ] {
            if a != b {
                out.push(format!(
                    "{name}: {a:#x} from the fields, {b:#x} from the bytes"
                ));
            }
        }
        for (name, a, b) in [
            ("input_amount", self.input_amount, other.input_amount),
            (
                "min_final_output",
                self.min_final_output,
                other.min_final_output,
            ),
        ] {
            if a != b {
                out.push(format!("{name}: {a} from the fields, {b} from the bytes"));
            }
        }
        if self.legs.len() != other.legs.len() {
            out.push(format!(
                "leg count: {} from the fields, {} from the bytes",
                self.legs.len(),
                other.legs.len()
            ));
        }
        for index in 0..self.legs.len().min(other.legs.len()) {
            let a = &self.legs[index];
            let b = &other.legs[index];
            for (name, x, y) in [
                ("pool", a.pool, b.pool),
                ("token_in", a.token_in, b.token_in),
                ("token_out", a.token_out, b.token_out),
            ] {
                if x != y {
                    out.push(format!(
                        "legs[{index}].{name}: {x:#x} from the fields, {y:#x} from the bytes"
                    ));
                }
            }
            for (name, x, y) in [
                ("amount_in", a.amount_in, b.amount_in),
                ("amount_out", a.amount_out, b.amount_out),
                ("min_amount_out", a.min_amount_out, b.min_amount_out),
            ] {
                if x != y {
                    out.push(format!(
                        "legs[{index}].{name}: {x} from the fields, {y} from the bytes"
                    ));
                }
            }
        }
        out
    }

    /// §38's identity, read apart. The pools and the token transitions come out of the string rather
    /// than being compared to strings this gate prints, so a disagreement with the row's own route
    /// facts is this gate's finding and not this gate's format.
    fn parts(&self) -> (Vec<String>, Vec<String>) {
        (
            self.legs
                .iter()
                .map(|leg| format!("{:#x}", leg.pool))
                .collect(),
            {
                let mut tokens = self
                    .legs
                    .iter()
                    .map(|leg| format!("{:#x}", leg.token_in))
                    .collect::<Vec<_>>();
                if let Some(last) = self.legs.last() {
                    tokens.push(format!("{:#x}", last.token_out));
                }
                tokens
            },
        )
    }
}

/// §38's route identity read out of a published string: `m10-<chain>-<pool>><pool>-<token>><…>`.
fn route_id_parts(route_id: &str) -> Result<(u64, Vec<&str>, Vec<&str>), String> {
    let rest = route_id
        .strip_prefix("m10-")
        .ok_or_else(|| format!("{route_id:?} does not carry the `m10-` route prefix"))?;
    let mut pieces = rest.splitn(3, '-');
    let chain = pieces.next().unwrap_or_default();
    let pools = pieces
        .next()
        .ok_or_else(|| format!("{route_id:?} has no pool piece"))?;
    let tokens = pieces
        .next()
        .ok_or_else(|| format!("{route_id:?} has no token piece"))?;
    Ok((
        chain
            .parse::<u64>()
            .map_err(|e| format!("{chain:?} is not a chain id: {e}"))?,
        pools.split('>').collect(),
        tokens.split('>').collect(),
    ))
}

/// Everything a plan carries besides the route: the two published deployment facts, the published
/// block and the published run answer, and the five §5 fields no published row spells.
///
/// A fixture row describes a REVM run — the state, the call, the answer. It does not describe a
/// *plan*: no correlation id, no state fingerprint, no simulation id, no age bound, no funding
/// statement, because the publisher never built one (§5's types are the execution crate's and the
/// simulation harness had no use for them). Rebuilding a plan to exercise D1/D2/D5 needs those from
/// somewhere, so each is derived from a published field where one exists — the row's own semantic
/// key, its recipe digest, its §51 label — and is published under `declared_by_this_gate` where none
/// does. Nothing below quotes them as the producer's numbers.
#[derive(Clone)]
struct Shell {
    chain_id: u64,
    executor: Address,
    sender: Address,
    block_number: u64,
    block_hash: B256,
    correlation_id: String,
    state_fingerprint: String,
    simulation_id: B256,
    validity_provenance: String,
    funding: SenderFunding,
    market: MarketKind,
    outcome: SimulationOutcome,
    denomination_reason: String,
    profit_provenance: String,
}

impl Shell {
    fn from_row(row: &Row) -> Result<Self, String> {
        let spec = row.run_spec();
        let observed = row.observed();
        let block = observed.get("block").ok_or_else(|| {
            "observed.block is absent, so the row names no canonical block to price against"
                .to_string()
        })?;
        let block_number = field_u64(block, "number")?;
        let status = status_word(observed.get("status").unwrap_or(&Value::Null));
        let outcome = match status.as_str() {
            "Success" => SimulationOutcome::Succeeded {
                gas_used: field_u64(observed, "gas_used")?,
                proved_gas_limit: field_u64(spec, "gas_limit")?,
                final_amount: field_u256(observed, "delivered")?,
            },
            "Reverted" => SimulationOutcome::Reverted {
                revert: field_str(observed, "revert_kind")?,
            },
            other => {
                return Err(format!(
                    "observed.status is {other:?}, which is neither of the answers a plan can \
                     carry (§25: a run either completed, reverted, or was never run)"
                ))
            }
        };
        let recipe = field_str(row.state(), "recipe_keccak256")?;
        let recipe_file = field_str(row.state(), "recipe_file")?;
        let state_source = field_str(spec, "state_source")?;
        let market = MarketKind::parse(
            &field_str(&row.value, "market")?,
            &format!("{}: {state_source}", row.key),
        )
        .map_err(|e| format!("reading the row's §51 label: {e}"))?;
        let min_final_output = row
            .run_spec()
            .get("call")
            .and_then(|call| call.get("min_final_amount"))
            .cloned()
            .unwrap_or(Value::Null);
        // The decimal word, not the JSON value: a published `Value` prints with its quotes, and
        // this string is quoted verbatim in the plan hash.
        let min_final_spelling = word_of(&min_final_output)
            .map(|word| word.to_string())
            .unwrap_or_else(|| "absent".to_string());
        Ok(Self {
            chain_id: field_u64(spec, "chain_id")?,
            executor: field_addr(spec, "executor")?,
            sender: field_addr(spec, "operator")?,
            block_number,
            block_hash: field_hash(block, "hash")?,
            correlation_id: format!("evidence-gate:{}", row.key),
            state_fingerprint: format!("declared-state-recipe-{recipe}"),
            simulation_id: alloy_primitives::keccak256(
                format!("{}#{}", row.key, field_str(spec, "calldata_keccak256")?).as_bytes(),
            ),
            validity_provenance: format!(
                "declared by this gate for {}: the row prices its route at block {block_number}, \
                 published as observed.block.number, and publishes no age bound of its own",
                row.key
            ),
            funding: SenderFunding::Overridden {
                detail: format!(
                    "{recipe_file} ({recipe}) declares the accounts, allowances and executor code \
                     this run reads; no node was asked whether this sender could pay"
                ),
            },
            market,
            outcome,
            denomination_reason: row
                .value
                .get("route")
                .and_then(|route| route.get("profit_denomination"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    "the round trip settles in run_spec.call.input_token, the token the route \
                     starts and ends in"
                        .to_string()
                }),
            profit_provenance: format!(
                "min_final_amount as published in {}: {min_final_spelling}",
                row.key
            ),
        })
    }

    /// The gate's own side of the plan, published beside the rebuilt identity so a reader can see
    /// which numbers came off the disk and which came from this file.
    fn declared(&self) -> Value {
        json!({
            "status": "declared by this gate, not published by the row's producer",
            "correlation_id": self.correlation_id,
            "state_fingerprint": self.state_fingerprint,
            "simulation_id": format!("{:#x}", self.simulation_id),
            "max_block_age": GATE_MAX_BLOCK_AGE,
            "validity_provenance": self.validity_provenance,
            "funding": self.funding.describe(),
            "market": self.market.to_json(),
            "profit_denomination_reason": self.denomination_reason,
            "profit_provenance": self.profit_provenance,
        })
    }

    fn assemble(&self, route: &RoutePack) -> ArbitrageExecutionPlan {
        ArbitrageExecutionPlan::new(
            self.chain_id,
            self.executor,
            self.sender,
            route.recipient,
            route.input_token,
            route.input_amount,
            route.legs.clone(),
            route.min_final_output,
            PlanValidity {
                simulated_at_block: BlockNumber(self.block_number),
                max_block_age: GATE_MAX_BLOCK_AGE,
                provenance: self.validity_provenance.clone(),
            },
            SimulationContext {
                correlation_id: self.correlation_id.clone(),
                block_number: BlockNumber(self.block_number),
                block_hash: self.block_hash,
                state_fingerprint: self.state_fingerprint.clone(),
                simulation_id: self.simulation_id,
                outcome: self.outcome.clone(),
                funding: self.funding.clone(),
                market: self.market.clone(),
            },
            ProfitPolicy {
                denomination: ProfitDenomination::TokenSettled {
                    token: route.input_token,
                    reason: self.denomination_reason.clone(),
                },
                required_final_balance: route.min_final_output,
                provenance: self.profit_provenance.clone(),
            },
        )
    }
}

/// Both constructions of the plan a published row describes, with the two routes they were built
/// from: one read out of the row's field spellings, one decoded out of the row's calldata bytes,
/// sharing the deployment and run facts the row publishes.
#[derive(Clone)]
struct Rebuilt {
    shell: Shell,
    from_fields: RoutePack,
    from_bytes: RoutePack,
    plan_fields: ArbitrageExecutionPlan,
    plan_bytes: ArbitrageExecutionPlan,
}

fn rebuild(row: &Row) -> Result<Rebuilt, String> {
    let shell = Shell::from_row(row)?;
    let from_fields = RoutePack::from_published_fields(row)?;
    let from_bytes = RoutePack::from_published_calldata(row)?;
    Ok(Rebuilt {
        plan_fields: shell.assemble(&from_fields),
        plan_bytes: shell.assemble(&from_bytes),
        shell,
        from_fields,
        from_bytes,
    })
}

/// §49's D1 from the other side. `d1_plan_hash_stable` asks whether the same facts hash the same;
/// this asks whether *different* facts hash differently, which is §6's rule that a changed plan is a
/// different execution rather than a re-roll of the same one. Every mutant moves one field by one
/// unit or one digit; none re-encodes the same fact in a different spelling.
fn sensitivity(base: &ArbitrageExecutionPlan) -> Vec<Value> {
    let base_hash = base.plan_hash();
    let one = U256::from(1u64);
    let mutations: Vec<PlanMutation> = vec![
        ("chain_id".to_string(), Box::new(|p| p.chain_id += 1)),
        (
            "executor".to_string(),
            Box::new(|p| p.executor = shifted(p.executor)),
        ),
        (
            "sender".to_string(),
            Box::new(|p| p.sender = shifted(p.sender)),
        ),
        (
            "recipient".to_string(),
            Box::new(|p| p.recipient = shifted(p.recipient)),
        ),
        (
            "input_token".to_string(),
            Box::new(|p| p.input_token = shifted(p.input_token)),
        ),
        (
            "input_amount".to_string(),
            Box::new(move |p| p.input_amount += one),
        ),
        (
            "min_final_output".to_string(),
            Box::new(move |p| p.min_final_output += one),
        ),
        (
            "legs[0].pool".to_string(),
            Box::new(|p| p.legs[0].pool = shifted(p.legs[0].pool)),
        ),
        (
            "legs[0].token_out".to_string(),
            Box::new(|p| p.legs[0].token_out = shifted(p.legs[0].token_out)),
        ),
        (
            "legs[0].amount_in".to_string(),
            Box::new(move |p| p.legs[0].amount_in += one),
        ),
        (
            "legs[0].amount_out".to_string(),
            Box::new(move |p| p.legs[0].amount_out += one),
        ),
        (
            "legs[0].min_amount_out".to_string(),
            Box::new(move |p| p.legs[0].min_amount_out += one),
        ),
        (
            "legs[1].derivation".to_string(),
            Box::new(|p| {
                if let Some(leg) = p.legs.get_mut(1) {
                    leg.derivation = AmountDerivation::PlanInput;
                }
            }),
        ),
        ("legs.len()".to_string(), Box::new(|p| p.legs.truncate(1))),
        (
            "validity.simulated_at_block".to_string(),
            Box::new(|p| {
                p.validity.simulated_at_block = BlockNumber(p.validity.simulated_at_block.0 + 1)
            }),
        ),
        (
            "validity.max_block_age".to_string(),
            Box::new(|p| p.validity.max_block_age += 1),
        ),
        (
            "validity.provenance".to_string(),
            Box::new(|p| p.validity.provenance.push_str(" mutated")),
        ),
        (
            "simulation.correlation_id".to_string(),
            Box::new(|p| p.simulation.correlation_id.push_str(" mutated")),
        ),
        (
            "simulation.block_number".to_string(),
            Box::new(|p| p.simulation.block_number = BlockNumber(p.simulation.block_number.0 + 1)),
        ),
        (
            "simulation.block_hash".to_string(),
            Box::new(|p| {
                p.simulation.block_hash = alloy_primitives::keccak256(b"evidence-gate sensitivity")
            }),
        ),
        (
            "simulation.state_fingerprint".to_string(),
            Box::new(|p| p.simulation.state_fingerprint.push_str(" mutated")),
        ),
        (
            "simulation.simulation_id".to_string(),
            Box::new(|p| {
                p.simulation.simulation_id =
                    alloy_primitives::keccak256(b"evidence-gate sensitivity")
            }),
        ),
        (
            "simulation.outcome".to_string(),
            Box::new(|p| p.simulation.outcome = SimulationOutcome::NotRun),
        ),
        (
            "simulation.funding".to_string(),
            Box::new(|p| {
                p.simulation.funding = SenderFunding::RealState {
                    source: "declared by the sensitivity table".to_string(),
                }
            }),
        ),
        (
            "simulation.market".to_string(),
            Box::new(|p| {
                if let MarketKind::ControlledFixture { proves } = &mut p.simulation.market {
                    proves.push_str(" mutated");
                }
            }),
        ),
        (
            "profit.denomination".to_string(),
            Box::new(|p| {
                if let ProfitDenomination::TokenSettled { token, .. } = &mut p.profit.denomination {
                    *token = shifted(*token);
                }
            }),
        ),
        (
            "profit.required_final_balance".to_string(),
            Box::new(move |p| p.profit.required_final_balance += one),
        ),
        (
            "profit.provenance".to_string(),
            Box::new(|p| p.profit.provenance.push_str(" mutated")),
        ),
    ];
    mutations
        .into_iter()
        .map(|(field, mutate)| {
            let mut plan = base.clone();
            mutate(&mut plan);
            let hash = plan.plan_hash();
            json!({
                "field": field,
                "base_plan_hash": format!("{base_hash:#x}"),
                "mutant_plan_hash": format!("{hash:#x}"),
                "hash_changed": hash != base_hash,
            })
        })
        .collect()
}

/// How many rows of a table answer `true` to one column — counted here so no artifact text has to
/// quote the number itself.
fn count_true(table: &[Value], column: &str) -> usize {
    table
        .iter()
        .filter(|entry| entry.get(column) == Some(&Value::Bool(true)))
        .count()
}

/// A published string several levels down, or `null` if the path does not exist. Absent is the
/// honest answer here; a default would be a number this gate invented.
fn path_str<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    let mut cursor = value;
    for key in keys {
        cursor = cursor.get(*key)?;
    }
    cursor.as_str()
}

impl Gate {
    /// §49's D1, D2 and D5 over every published scenario row, §50's two plan-layer controls, the
    /// §50 census of the eight contract-layer ones, and the route identity the real run published.
    fn plan_rebuild(&mut self) {
        let mut table: Vec<Value> = Vec::new();
        let mut anchor: Option<Rebuilt> = None;
        let rows = self.rows.clone();
        for row in &rows {
            let rebuilt = match rebuild(row) {
                Ok(rebuilt) => rebuilt,
                Err(e) => {
                    self.note("plan", format!("{}: {e}", row.key));
                    continue;
                }
            };
            let Rebuilt {
                shell,
                from_fields,
                from_bytes,
                plan_fields,
                plan_bytes,
            } = &rebuilt;
            if row.key == PLAN_CONTROL_ROW {
                anchor = Some(rebuilt.clone());
            }

            // D1 — two constructions of the same published facts, one identity.
            let hash_fields = plan_fields.plan_hash();
            let hash_bytes = plan_bytes.plan_hash();
            if hash_fields != hash_bytes {
                self.note(
                    "plan",
                    format!(
                        "{}: the route spelled as fields hashes to {hash_fields:#x} and as bytes to \
                         {hash_bytes:#x}; the two spellings this directory publishes are not one \
                         plan",
                        row.key
                    ),
                );
            }
            let route_spelling_diff = from_fields.diff(from_bytes);
            if !route_spelling_diff.is_empty() {
                self.note(
                    "plan",
                    format!("{}: {}", row.key, route_spelling_diff.join("; ")),
                );
            }

            // D2 — the execution crate's encoder against the bytes the simulation crate ran.
            let encoded = plan_fields.calldata();
            let encoded_from_bytes = plan_bytes.calldata();
            let published = field_bytes(row.run_spec(), "calldata");
            let bytes_agree = published.as_ref().map(|p| *p == encoded).unwrap_or(false);
            if !bytes_agree {
                self.note(
                    "plan",
                    format!(
                        "{}: the execution crate encodes {} bytes ({:#x}), the row publishes {} \
                         ({}) — §47's ban on Rust calldata that is not the deployed ABI is exactly \
                         this difference",
                        row.key,
                        encoded.len(),
                        alloy_primitives::keccak256(encoded.as_ref()),
                        published
                            .as_ref()
                            .map(|p| format!("{} bytes", p.len()))
                            .unwrap_or_else(|e| format!("nothing ({e})")),
                        field_str(row.run_spec(), "calldata_keccak256").unwrap_or_default(),
                    ),
                );
            }
            let digest_recomputed = format!("{:#x}", alloy_primitives::keccak256(encoded.as_ref()));
            let digest_published =
                field_str(row.run_spec(), "calldata_keccak256").unwrap_or_default();
            if digest_recomputed != digest_published {
                self.note(
                    "plan",
                    format!(
                        "{}: keccak of the bytes this gate encodes is {digest_recomputed}, the row \
                         publishes {digest_published}",
                        row.key
                    ),
                );
            }
            if encoded != encoded_from_bytes {
                self.note(
                    "plan",
                    format!(
                        "{}: the plan built from the published fields and the plan built from the \
                         published bytes encode to different calldata",
                        row.key
                    ),
                );
            }
            let len_published = row
                .run_spec()
                .get("calldata_len")
                .cloned()
                .unwrap_or(Value::Null);
            let len_agrees =
                len_published.as_u64() == Some(u64::try_from(encoded.len()).unwrap_or(u64::MAX));
            if !len_agrees {
                self.note(
                    "plan",
                    format!(
                        "{}: this gate encodes {} bytes, the row publishes calldata_len {len_published}",
                        row.key,
                        encoded.len()
                    ),
                );
            }

            // D5 — the route identity, and whether the identity names the route the row published.
            let route_fields = plan_fields.route_id();
            let route_bytes = plan_bytes.route_id();
            if route_fields != route_bytes {
                self.note(
                    "plan",
                    format!(
                        "{}: the same route ids as {route_fields} and {route_bytes} from its two \
                         spellings",
                        row.key
                    ),
                );
            }
            let (expect_pools, expect_tokens) = from_fields.parts();
            let mut identity_names_the_route = json!({
                "readable": false,
                "why": "not attempted",
            });
            let route_parts = match route_id_parts(&route_fields) {
                Ok((chain, pools, tokens)) => {
                    let pools = pools
                        .iter()
                        .copied()
                        .map(|pool| pool.to_string())
                        .collect::<Vec<String>>();
                    let tokens = tokens
                        .iter()
                        .copied()
                        .map(|token| token.to_string())
                        .collect::<Vec<String>>();
                    identity_names_the_route = json!({
                        "readable": true,
                        "chain_id_in_the_id": chain,
                        "chain_id_published": shell.chain_id,
                        "pools_in_the_id": pools,
                        "pools_in_the_published_route": expect_pools,
                        "tokens_in_the_id": tokens,
                        "tokens_in_the_published_route": expect_tokens,
                    });
                    chain == shell.chain_id && pools == expect_pools && tokens == expect_tokens
                }
                Err(e) => {
                    self.note("plan", format!("{}: {}", row.key, e));
                    identity_names_the_route = json!({ "readable": false, "why": e });
                    false
                }
            };
            if !route_parts {
                self.note(
                    "plan",
                    format!(
                        "{}: §38's route identity does not name the pools and tokens the row \
                         publishes",
                        row.key
                    ),
                );
            }

            // What the plan layer itself makes of the row, over the binding the row names.
            let binding = ExecutionBinding {
                chain_id: shell.chain_id,
                executor: shell.executor,
            };
            let rejections = plan_fields.validate(&binding);
            let codes = rejections
                .iter()
                .map(evm_execution::PlanRejection::code)
                .collect::<Vec<_>>();
            table.push(json!({
                "row": row.key,
                "d1_plan_hash_from_published_fields": format!("{hash_fields:#x}"),
                "d1_plan_hash_from_published_calldata": format!("{hash_bytes:#x}"),
                "d1_plan_hash_stable": hash_fields == hash_bytes,
                "published_plan_hash": Value::Null,
                "d2_calldata_equal_to_published_bytes": bytes_agree,
                "d2_calldata_hash_equal_to_published_digest": digest_recomputed == digest_published,
                "d2_calldata_len_agrees": len_agrees,
                "recomputed_calldata_len": encoded.len(),
                "published_calldata_len": len_published,
                "published_calldata_keccak256": digest_published,
                "d5_route_id_stable": route_fields == route_bytes,
                "d5_route_id": route_fields,
                "published_route_id": Value::Null,
                "d5_identity_names_the_published_route": route_parts,
                "d5_route_id_parts": identity_names_the_route,
                "route_spelling_differences": route_spelling_diff,
                "plan_layer_refusals_over_the_rows_own_binding": codes,
                "plan_layer_refusal_reasons": rejections.iter().map(|r| r.reason()).collect::<Vec<_>>(),
                "contract_layer_refusal_published_in_the_row": {
                    "contract_error": row.observed().get("contract_error").cloned().unwrap_or(Value::Null),
                    "revert_kind": row.observed().get("revert_kind").cloned().unwrap_or(Value::Null),
                },
                "declared_by_this_gate": shell.declared(),
            }));
        }

        let sensitivity = match &anchor {
            Some(rebuilt) => sensitivity(&rebuilt.plan_fields),
            None => {
                self.note(
                    "plan",
                    format!(
                        "{PLAN_CONTROL_ROW} is not in the directory, so the sensitivity table and \
                         both planted plan-layer controls have nothing to stand on"
                    ),
                );
                Vec::new()
            }
        };
        for entry in &sensitivity {
            if entry["hash_changed"] != Value::Bool(true) {
                self.note(
                    "plan",
                    format!(
                        "the sensitivity mutation of {} leaves the plan hash unchanged, so §49's D1 \
                         does not cover that field",
                        entry["field"]
                    ),
                );
            }
        }

        let controls = self.plant_plan_controls(anchor.as_ref());
        let contract_layer = self.contract_layer_census();
        let real = self.real_run_route();
        let published_age = self
            .real
            .get("freshness_window")
            .and_then(|window| window.get("declared_max_block_age"))
            .and_then(Value::as_u64);

        self.check(
            "plan_layer",
            json!({
                "rows_in_the_directory": self.rows.len(),
                "rows_rebuilt": table.len(),
                "d1_plan_hash_stable": count_true(&table, "d1_plan_hash_stable"),
                "d2_calldata_equal_to_published_bytes": count_true(&table, "d2_calldata_equal_to_published_bytes"),
                "d2_calldata_hash_equal_to_published_digest": count_true(&table, "d2_calldata_hash_equal_to_published_digest"),
                "d2_calldata_len_agrees": count_true(&table, "d2_calldata_len_agrees"),
                "d5_route_id_stable": count_true(&table, "d5_route_id_stable"),
                "d5_identity_names_the_published_route": count_true(&table, "d5_identity_names_the_published_route"),
                "plan_layer_facts_published_by_the_rows": Value::Null,
                "why_null": "every scenario row in this directory describes a REVM run — a state \
                            recipe, a call, an answer — and none carries §5's plan-side fields, \
                            because the harness that produced them never built a plan. The gate \
                            declares the five it needs and labels them as its own; a row's plan \
                            hash is therefore not a published fact and is published as null rather \
                            than as this gate's number.",
                "declared_max_block_age": GATE_MAX_BLOCK_AGE,
                "the_real_runs_published_declared_max_block_age": published_age,
                "declared_bound_matches_the_real_run": published_age == Some(GATE_MAX_BLOCK_AGE),
                "sensitivity": {
                    "fields_mutated": sensitivity.len(),
                    "hashes_changed": count_true(&sensitivity, "hash_changed"),
                    "table": sensitivity,
                },
                "plan_layer_controls": controls,
                "contract_layer_controls": contract_layer,
                "real_run": real,
                "per_row": table,
            }),
        );
    }

    /// §50's two plan-layer controls, planted here because the refusal they demonstrate happens in
    /// the execution crate: there is no REVM run to observe and no bytecode to point at.
    ///
    /// Each artifact carries the unmutated case beside it — the same plan over the binding the row
    /// itself names, which is accepted — so the file shows the refusal is about the binding and not
    /// about a route that was already broken. That twin is the difference between a control and a
    /// complaint.
    fn plant_plan_controls(&mut self, anchor: Option<&Rebuilt>) -> Value {
        let Some(rebuilt) = anchor else {
            return json!({ "planted": [], "why": "the anchor row could not be rebuilt" });
        };
        let shell = &rebuilt.shell;
        let plan = &rebuilt.plan_fields;
        let matching = ExecutionBinding {
            chain_id: shell.chain_id,
            executor: shell.executor,
        };
        let own_rejections = plan.validate(&matching);
        if !own_rejections.is_empty() {
            self.note(
                "plan",
                format!(
                    "{PLAN_CONTROL_ROW}: the anchor plan is refused by its own row's binding ({}), \
                     so a control planted on it proves nothing about the binding",
                    own_rejections
                        .iter()
                        .map(evm_execution::PlanRejection::code)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
        let twin = ExecutablePlan::new(plan.clone(), &matching);
        if twin.is_err() {
            self.note(
                "plan",
                format!(
                    "{PLAN_CONTROL_ROW}: ExecutablePlan::new refused the plan its own row's \
                     binding validates: {twin:?}",
                ),
            );
        }

        let mut planted = Vec::new();
        for (name, rel, _layer, expected_code) in
            CONTROLS.iter().filter(|control| control.2 == "plan")
        {
            let binding = match *expected_code {
                "wrong_chain" => ExecutionBinding {
                    chain_id: shell.chain_id + 1,
                    executor: shell.executor,
                },
                "wrong_executor" => ExecutionBinding {
                    chain_id: shell.chain_id,
                    executor: shifted(shell.executor),
                },
                other => {
                    self.note(
                        "plan",
                        format!("{name}: {other:?} is not a refusal this gate knows how to plant"),
                    );
                    continue;
                }
            };
            let rejections = plan.validate(&binding);
            let codes = rejections
                .iter()
                .map(evm_execution::PlanRejection::code)
                .collect::<Vec<_>>();
            let exactly_one = codes.len() == 1 && codes[0] == *expected_code;
            if !exactly_one {
                self.note(
                    "plan",
                    format!(
                        "{name}: planting {expected_code} produced {} instead",
                        if codes.is_empty() {
                            "no refusal at all".to_string()
                        } else {
                            codes.join(", ")
                        }
                    ),
                );
            }
            let boundary = ExecutablePlan::new(plan.clone(), &binding);
            let boundary_refused = matches!(&boundary, Err(ExecutionError::PlanRejected(_)));
            if !boundary_refused {
                self.note(
                    "plan",
                    format!(
                        "{name}: ExecutablePlan::new froze a plan the binding forbids, so §6's \
                         boundary is not where the task book says it is"
                    ),
                );
            }
            let plan_hash = plan.plan_hash();
            let body = json!({
                "control": name,
                "layer": "plan",
                "section": "§50's planted controls, refused under §7 and §36",
                "planted_on_row": PLAN_CONTROL_ROW,
                "what_was_mutated": "the execution binding this plan is handed, not the route it \
                                    carries: the chain id for this control, the executor address \
                                    for the other",
                "what_was_not_mutated": "the legs, the amounts, the floors, the pinned block, the \
                                        simulation answer",
                "plan": {
                    "plan_hash": format!("{plan_hash:#x}"),
                    "calldata_hash": format!("{:#x}", plan.calldata_hash()),
                    "calldata_len": plan.calldata().len(),
                    "route_id": plan.route_id(),
                    "chain_id": shell.chain_id,
                    "executor_as_the_plan_names": format!("{:#x}", shell.executor),
                    "sender": format!("{:#x}", shell.sender),
                    "input_amount": plan.input_amount.to_string(),
                    "min_final_output": plan.min_final_output.to_string(),
                    "published_by": "rebuilt by this gate from the row's own published fields and \
                                     bytes (§49's D1/D2), not copied from a producer",
                },
                "configured_binding": {
                    "chain_id": binding.chain_id,
                    "executor": format!("{:#x}", binding.executor),
                    "why_this_is_the_mutant": if *expected_code == "wrong_chain" {
                        "one above the chain id the row publishes; the route itself is untouched"
                    } else {
                        "the published executor address with its last hex digit moved, so it names \
                         no contract in this directory"
                    },
                },
                "refusal": {
                    "codes": codes,
                    "count": rejections.len(),
                    "reasons": rejections.iter().map(|r| r.reason()).collect::<Vec<_>>(),
                    "expected_code": expected_code,
                    "exactly_the_one_named_refusal": exactly_one,
                },
                "boundary": {
                    "call": "ExecutablePlan::new(plan, binding)",
                    "constructed": boundary.is_ok(),
                    "error_class": if boundary_refused { "ExecutionError::PlanRejected" } else { "none" },
                    "message": boundary.as_ref().err().map(|e| e.to_string()),
                    "note": "§40: the refusal reaches the caller as a named class, not as a string \
                             invented at the call site",
                },
                "unmutated_twin": {
                    "binding": { "chain_id": matching.chain_id, "executor": format!("{:#x}", matching.executor) },
                    "refusal_codes": own_rejections.iter().map(|r| r.code()).collect::<Vec<_>>(),
                    "boundary_constructed": twin.is_ok(),
                    "frozen_identity": twin.as_ref().ok().map(|frozen| json!({
                        "plan_hash": format!("{:#x}", frozen.plan_hash()),
                        "calldata_hash": format!("{:#x}", frozen.calldata_hash()),
                        "route_id": frozen.route_id(),
                    })),
                    "why_it_is_here": "without it this file would only show that some plan is \
                                       refused, which a broken route would show too",
                },
                "declared_by_this_gate": shell.declared(),
                "what_this_proves": "the execution crate refuses a route whose deployment does not \
                                     match the one it was configured for, and it refuses it by name \
                                     — §7's chain and §36's target drift are the two ways a correct \
                                     route becomes somebody else's transaction",
                "what_this_does_not_prove": "nothing about the contract: the refusal happens before \
                                             a call is made, so REVM never ran, no bytecode was \
                                             touched, and no bytes were signed or sent",
            });
            self.write_json(rel, body);
            planted.push(json!({
                "control": name,
                "file": rel,
                "expected_refusal": expected_code,
                "refusal_codes": codes,
                "exactly_the_one_named_refusal": exactly_one,
                "boundary_refused": boundary_refused,
                "digest": self.digest_of(rel),
            }));
        }
        json!({
            "controls": planted,
            "planted_count": planted.len(),
            "expected_count": CONTROLS.iter().filter(|c| c.2 == "plan").count(),
            "anchor_row": PLAN_CONTROL_ROW,
            "anchor_plan_hash": format!("{:#x}", plan.plan_hash()),
        })
    }

    /// §50's other eight: the published refusal has to be the one the task book names. Read out of
    /// the row, not re-judged here — D4 already re-judges the classification, and this is the census
    /// that says all ten controls exist and are labelled with the layer that refused them.
    fn contract_layer_census(&mut self) -> Value {
        let mut rows_without_a_control = Vec::new();
        let mut controls = Vec::new();
        for (name, rel, _layer, expected) in CONTROLS.iter().filter(|c| c.2 == "contract") {
            let found: Vec<&Row> = self.rows.iter().filter(|row| row.file == *rel).collect();
            let row_count = found.len();
            let published: Vec<Value> = found
                .iter()
                .map(|row| {
                    let contract_error = row
                        .observed()
                        .get("contract_error")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let revert_kind = row
                        .observed()
                        .get("revert_kind")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let label = if contract_error == Value::Null {
                        revert_kind.as_str().map(str::to_string)
                    } else {
                        contract_error.as_str().map(str::to_string)
                    };
                    json!({
                        "row": row.key,
                        "contract_error": contract_error,
                        "revert_kind": revert_kind,
                        "label_as_published": label,
                        "names_the_expected_refusal": label.as_deref() == Some(*expected),
                    })
                })
                .collect();
            if row_count == 0 {
                rows_without_a_control.push(rel.to_string());
            }
            for entry in &published {
                if entry["names_the_expected_refusal"] != Value::Bool(true) {
                    self.note(
                        "plan",
                        format!(
                            "{}: §50 names this control {expected:?}, the row publishes {}",
                            entry["row"], entry["label_as_published"]
                        ),
                    );
                }
            }
            controls.push(json!({
                "control": name,
                "file": rel,
                "layer": "contract",
                "expected_refusal": expected,
                "rows": published,
                "every_row_names_the_expected_refusal": !published.is_empty()
                    && published
                        .iter()
                        .all(|entry| entry["names_the_expected_refusal"] == Value::Bool(true)),
            }));
        }
        for rel in &rows_without_a_control {
            self.note(
                "plan",
                format!("{rel} is listed as a §50 control and no scenario row in it was loaded"),
            );
        }
        json!({
            "controls": controls,
            "expected_count": CONTROLS.iter().filter(|c| c.2 == "contract").count(),
            "files_with_no_loaded_row": rows_without_a_control,
            "note": "the refusal of a contract-layer control is the contract's own answer, published \
                     as observed.contract_error, or — for a revert the executor did not raise — as \
                     the classification D4 re-derives from the published revert bytes.",
        })
    }

    /// §49's D5 on the one route in this directory a node confirmed, and the identity table for the
    /// real transaction: which published simulation is the call that was sent, answered by hash
    /// equality rather than by reading the harness that made it.
    fn real_run_route(&mut self) -> Value {
        // Both live files are cloned once: this phase reads strings out of them and also records
        // drift notes, and a `&str` borrowed from `self.real` would hold `self` borrowed across
        // every `self.note` call below.
        let real = self.real.clone();
        let failure = self.failure.clone();
        let mut problems = Vec::new();
        let Some(route_id) = path_str(&real, &["execution", "route_id"]) else {
            self.note(
                "plan",
                "real/giwa_execution.json publishes no execution.route_id",
            );
            return json!({ "readable": false });
        };
        let (chain, pools, tokens) = match route_id_parts(route_id) {
            Ok(parts) => parts,
            Err(e) => {
                self.note("plan", format!("reading the published route id: {e}"));
                return json!({ "readable": false, "why": e });
            }
        };
        // Owned, so they can be compared against the strings this phase builds and still outlive
        // the drift notes.
        let pools: Vec<String> = pools.iter().map(|pool| (*pool).to_string()).collect();
        let tokens: Vec<String> = tokens.iter().map(|token| (*token).to_string()).collect();
        let published_chain = field_u64(&real, "chain_id");
        if Ok(chain) != published_chain {
            problems.push(format!(
                "the route id says chain {chain}, the file publishes {}",
                published_chain.unwrap_or_default()
            ));
        }
        let buy = field_addr(&real["route"], "buy_pool");
        let sell = field_addr(&real["route"], "sell_pool");
        let input = field_addr(&real["evidence_row_31"], "input_asset");
        let published_pools: Vec<String> = [buy.as_ref(), sell.as_ref()]
            .into_iter()
            .filter_map(|pool| pool.map(|pool| format!("{pool:#x}")).ok())
            .collect();
        if pools != published_pools {
            problems.push(format!(
                "the route id names pools {pools:?} and the file's route facts name {published_pools:?}"
            ));
        }
        // The middle token is the other asset of the pool the route buys through; the file names it
        // per pool, so the gate looks it up rather than reading one string twice.
        fn mid_token(real: &Value, buy: Address, input: Address) -> Result<String, String> {
            let key = format!("{buy:#x}");
            let row = real["reserves_at_pinned_block"]
                .as_array()
                .and_then(|rows| {
                    rows.iter()
                        .find(|r| r.get("pool").and_then(Value::as_str) == Some(key.as_str()))
                })
                .ok_or_else(|| format!("no reserve row for pool {key}"))?;
            let token0 = field_addr(row, "token0")?;
            let token1 = field_addr(row, "token1")?;
            if token0 == input {
                Ok(format!("{token1:#x}"))
            } else if token1 == input {
                Ok(format!("{token0:#x}"))
            } else {
                Err(format!(
                    "pool {key} holds {token0:#x} and {token1:#x}, neither of which is the \
                     route's input asset"
                ))
            }
        }
        let mid_result: Result<String, String> = match (buy, input.as_ref()) {
            (Ok(buy_addr), Ok(input_addr)) => mid_token(&real, buy_addr, *input_addr),
            _ => Err(
                "the real run publishes no route.buy_pool or evidence_row_31.input_asset, so \
                     the middle token of the route cannot be derived from the file"
                    .to_string(),
            ),
        };
        if let Err(e) = &mid_result {
            problems.push(e.clone());
        }
        let expected_tokens: Vec<String> = match (input.as_ref().ok(), mid_result.as_ref().ok()) {
            (Some(input_addr), Some(mid)) => vec![
                format!("{input_addr:#x}"),
                mid.clone(),
                format!("{input_addr:#x}"),
            ],
            _ => Vec::new(),
        };
        if tokens != expected_tokens {
            problems.push(format!(
                "the route id names token transitions {tokens:?} and the file's route facts name \
                 {expected_tokens:?}"
            ));
        }

        const SPELLINGS: [(&str, &[&str]); 7] = [
            ("execution.calldata_hash", &["execution", "calldata_hash"]),
            (
                "simulation.open_floor.calldata_hash",
                &["simulation", "open_floor", "calldata_hash"],
            ),
            (
                "simulation.profit_floor.calldata_hash",
                &["simulation", "profit_floor", "calldata_hash"],
            ),
            (
                "simulation.floor_too_high.calldata_hash",
                &["simulation", "floor_too_high", "calldata_hash"],
            ),
            (
                "reconciliation_32.route.executed_calldata_hash",
                &["reconciliation_32", "route", "executed_calldata_hash"],
            ),
            (
                "reconciliation_32.route.simulation_calldata_hash",
                &["reconciliation_32", "route", "simulation_calldata_hash"],
            ),
            (
                "giwa_failure.simulation_attribution.calldata_hash",
                &["simulation_attribution", "calldata_hash"],
            ),
        ];
        let mut spellings: Vec<(&str, Option<String>)> = Vec::new();
        for (label, keys) in SPELLINGS.iter() {
            let source = if label.starts_with("giwa_failure") {
                &failure
            } else {
                &real
            };
            spellings.push((*label, path_str(source, keys).map(str::to_string)));
        }
        let sent = path_str(&real, &["execution", "calldata_hash"]).map(str::to_string);
        let spelled: Vec<Value> = spellings
            .iter()
            .map(|(label, value)| {
                json!({
                    "spelling": label,
                    "calldata_hash": value,
                    "is_the_transaction_that_was_sent": value
                        .as_ref()
                        .map(|hash| Some(hash.clone()) == sent)
                        .unwrap_or(false),
                })
            })
            .collect();
        let distinct: BTreeSet<String> = spellings
            .iter()
            .filter_map(|(_, value)| value.clone())
            .collect();
        let failure_floor = path_str(&failure, &["min_final_amount_wei"]).map(str::to_string);
        let attribution_floor =
            path_str(&failure, &["simulation_attribution", "min_final_amount"]).map(str::to_string);
        let variant_floor = path_str(&real, &["simulation", "floor_too_high", "min_final_amount"])
            .map(str::to_string);
        if !(failure_floor.is_some()
            && failure_floor == attribution_floor
            && failure_floor == variant_floor)
        {
            problems.push(format!(
                "the reverted transaction's floor is not the same number in all three published \
                 spellings: {:?}, {:?}, {:?}",
                failure_floor, attribution_floor, variant_floor
            ));
        }
        for problem in &problems {
            self.note("plan", format!("real/giwa_execution.json: {problem}"));
        }
        let simulated_variants: Vec<Value> = ["open_floor", "profit_floor", "floor_too_high"]
            .into_iter()
            .map(|label| json!({
                "label": label,
                "calldata_hash": real["simulation"][label]["calldata_hash"].clone(),
                "min_final_amount": real["simulation"][label]["min_final_amount"].clone(),
                "is_the_transaction_that_was_sent": path_str(&real, &["simulation", label, "calldata_hash"]).map(str::to_string) == sent,
            }))
            .collect();
        json!({
            "published_route_id": route_id,
            "d5_route_id_parts_match_the_published_route_facts": problems.is_empty(),
            "chain_id_in_the_id": chain,
            "pools_in_the_id": pools,
            "tokens_in_the_id": tokens,
            "the_mid_token_from_the_pinned_reserves": mid_result.ok(),
            "calldata_hash_spellings": spelled,
            "distinct_calldata_hash_count": distinct.len(),
            "the_sent_transaction_is": sent,
            "the_three_simulated_variants": simulated_variants,
            "floor_spellings": {
                "giwa_failure.min_final_amount_wei": failure_floor,
                "giwa_failure.simulation_attribution.min_final_amount": attribution_floor,
                "giwa_execution.simulation.floor_too_high.min_final_amount": variant_floor,
                "all_three_agree": failure_floor.is_some() && failure_floor == attribution_floor && failure_floor == variant_floor,
            },
            "plan_identity": {
                "published_plan_hash": self.real["execution"]["plan_hash"].clone(),
                "recomputed_by_this_gate": Value::Null,
                "why_not_recomputed": "the sent plan's per-leg floors are not published in this \
                                       directory — the route table gives the two asks and the call \
                                       gives the final floor, and a plan built with floors this gate \
                                       guessed would hash to a value that is not the run's. §61's \
                                       rule for a fact that does not exist is null, and §49's D1 is \
                                       about the same plan rather than a plausible one.",
                "published_calldata_hash": self.real["execution"]["calldata_hash"].clone(),
                "recomputed_calldata_hash": Value::Null,
                "why_not_recomputed_for_the_same_reason": "the real run published no calldata bytes, \
                                                           only their hash and length, so there is \
                                                           nothing here for the execution crate's \
                                                           encoder to be checked against.",
                "published_calldata_len": self.real["execution"]["calldata_len"].clone(),
            },
            "problems": problems,
        })
    }
}

// ---------------------------------------------------------------------------
// Phase 6 — the live run, read once, so every phase below quotes the same bytes
// ---------------------------------------------------------------------------

/// The commit this run judges, read out of `.git` instead of by running `git`.
///
/// A gate that shells out to a binary has taken on that binary's version, its configuration, and
/// whatever a hook decides to print; §60's tree is supposed to be rebuildable by a reader who has
/// only the files. So: `HEAD`, then the loose ref it names, then `packed-refs`, and if none of
/// those answer, `manifest.json`'s `git_commit` is `null` with the reason beside it (§61).
fn dot_git(root: &Path) -> Option<PathBuf> {
    let direct = root.join(".git");
    if direct.is_dir() {
        return Some(direct);
    }
    let text = std::fs::read_to_string(&direct).ok()?;
    let gitdir = text.trim().strip_prefix("gitdir:")?.trim();
    if gitdir.is_empty() {
        return None;
    }
    let path = Path::new(gitdir);
    Some(if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    })
}

fn read_git_commit(root: &Path) -> Value {
    let mut how: Vec<String> = Vec::new();
    let Some(gitdir) = dot_git(root) else {
        how.push("no .git entry at the workspace root".to_string());
        return json!({ "git_commit": Value::Null, "how_it_was_read": how });
    };
    let Ok(head) = std::fs::read(gitdir.join("HEAD")) else {
        how.push(".git/HEAD could not be read".to_string());
        return json!({ "git_commit": Value::Null, "how_it_was_read": how });
    };
    let head = String::from_utf8_lossy(&head).trim().to_string();
    let candidate = if let Some(rest) = head.strip_prefix("ref: ") {
        let name = rest.trim().to_string();
        how.push(format!(".git/HEAD is a symbolic ref to {name}"));
        let loose = std::fs::read_to_string(gitdir.join(&name))
            .ok()
            .map(|text| text.trim().to_string());
        match loose {
            Some(sha) => {
                how.push(format!("read from the loose ref {name}"));
                Some(sha)
            }
            None => {
                how.push(format!("no loose ref {name}; searching packed-refs"));
                let packed = std::fs::read_to_string(gitdir.join("packed-refs")).ok();
                let found = packed.and_then(|text| {
                    text.lines().map(str::trim).find_map(|line| {
                        let mut parts = line.split_whitespace();
                        let sha = parts.next()?;
                        let name_of_line = parts.next()?;
                        (name_of_line == name && sha.len() == 40).then(|| sha.to_string())
                    })
                });
                how.push(match &found {
                    Some(_) => "found in packed-refs".to_string(),
                    None => "absent from both the loose refs and packed-refs".to_string(),
                });
                found
            }
        }
    } else if head.len() == 40 {
        how.push(".git/HEAD is detached at a commit".to_string());
        Some(head.clone())
    } else {
        how.push(format!(
            ".git/HEAD is neither a symref nor a 40-character object id: {head:?}"
        ));
        None
    };
    let sha = candidate.filter(|text| {
        text.len() == 40
            && text
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    });
    json!({
        "git_commit": sha,
        "how_it_was_read": how,
        "note": "the commit the evidence directory was last judged against, not the commit the \
                 real transactions were sent in — §61 asks for one git_commit field and this is \
                 the honest reading of it. It is read from files rather than by running `git`, so \
                 a reader with only this directory and this file can reach the same value.",
    })
}

/// `keccak256(rlp([sender, nonce]))` — the address a `CREATE` lands on, derived here from the two
/// published facts that name it and compared against the address the live run says it deployed to.
fn create_address(sender: Address, nonce: u64) -> String {
    let mut payload: Vec<u8> = Vec::with_capacity(2 + 20 + 9);
    payload.push(0x94);
    payload.extend_from_slice(sender.as_slice());
    if nonce == 0 {
        payload.push(0x80);
    } else if nonce < 128 {
        payload.push(nonce as u8);
    } else {
        let bytes = nonce.to_be_bytes();
        let first = bytes
            .iter()
            .position(|b| *b != 0)
            .unwrap_or(bytes.len() - 1);
        let minimal = &bytes[first..];
        payload.push(0x80 + minimal.len() as u8);
        payload.extend_from_slice(minimal);
    }
    let mut encoded = Vec::with_capacity(payload.len() + 1);
    encoded.push(0xc0 + payload.len() as u8);
    encoded.extend_from_slice(&payload);
    let hash = alloy_primitives::keccak256(&encoded);
    format!("{:#x}", Address::from_slice(&hash.as_slice()[12..]))
}

impl Gate {
    /// Read the four files the live run published and cross-check the two that describe the same
    /// deployment. Nothing is recomputed here that a later phase recomputes better; this is the
    /// reading, plus the agreement checks that decide whether the two files may be quoted at all.
    fn read_run(&mut self) -> Value {
        for (slot, rel) in [
            ("real", "real/giwa_execution.json"),
            ("failure", "real/giwa_failure.json"),
            ("deployment", "contract/deployment.json"),
            ("bytecode", "contract/bytecode_hash.json"),
            ("ladder_steps", "real/giwa_ladder_steps.json"),
            ("preconditions", "real/preconditions.json"),
        ] {
            let reading = self.read_evidence(rel);
            match reading {
                Ok(doc) => self.set_doc(slot, &doc),
                Err(e) => self.note("live", e),
            }
        }
        let ladder = self.read_evidence("real/giwa_ladder_steps.json");
        let preconditions_ok = self.summary.contains_key("preconditions");

        // The two deployment files are the same fact spelled twice by the same harness. If they
        // disagree, `manifest.json` would have to pick one, and a manifest that picks is not a
        // manifest.
        let mut shared: Vec<Value> = Vec::new();
        for (field, dep_path, hash_path) in [
            ("chain_id", "chain_id", "chain_id"),
            (
                "deployment address",
                "contract_address",
                "deployment_address",
            ),
            ("deployment transaction", "deployment_tx", "deployment_tx"),
            ("deployment block", "deployment_block", "deployment_block"),
            (
                "creation code hash",
                "creation_code_hash",
                "creation_code_keccak256",
            ),
            (
                "creation code length",
                "creation_code_bytes",
                "creation_code_bytes",
            ),
            ("operator", "operator", "operator"),
            ("ABI version", "abi_version", "abi_version"),
        ] {
            let a = self
                .deployment
                .get(dep_path)
                .cloned()
                .unwrap_or(Value::Null);
            let b = self.bytecode.get(hash_path).cloned().unwrap_or(Value::Null);
            let agree = !a.is_null() && a == b;
            if !agree {
                self.note(
                    "live",
                    format!(
                        "contract/deployment.json and contract/bytecode_hash.json disagree \
                            about {field}: {a} vs {b}"
                    ),
                );
            }
            shared.push(json!({
                "field": field,
                "deployment_json": a,
                "bytecode_hash_json": b,
                "agree": agree,
            }));
        }
        // The deployment row is the ladder's first step, restated. Same transaction, so every
        // receipt field must read the same; a restatement that drifted would mean one of the two
        // files was assembled from a different answer than the other.
        let mut step_rows: Vec<Value> = Vec::new();
        let ladder_steps: Vec<Value> = ladder
            .as_ref()
            .ok()
            .and_then(|doc| doc.get("steps"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(first) = ladder_steps.first() {
            for (field, step_field) in [
                ("deployment_tx", "transaction_hash"),
                ("deployment_block", "receipt_block_number"),
                ("contract_address", "receipt_contract_address"),
                ("operator", "recovered_sender"),
            ] {
                let a = self.deployment.get(field).cloned().unwrap_or(Value::Null);
                let b = first.get(step_field).cloned().unwrap_or(Value::Null);
                if a != b {
                    self.note(
                        "live",
                        format!(
                            "contract/deployment.json's {field} is {a}, the ladder's first \
                                step's {step_field} is {b}"
                        ),
                    );
                }
                step_rows.push(json!({
                    "field": field,
                    "deployment_json": a,
                    "ladder_first_step": b,
                    "step_field": step_field,
                    "agree": a == b,
                }));
            }
            // The deployment row carries the ladder's step object verbatim under `step`, so the two
            // files must spell the transaction-input fields the same way too. `input_hash` and
            // `input_bytes` are the bytes the CREATE transaction sent — the creation code plus the
            // one 32-byte constructor argument — so they are compared with the step, not with the
            // artifact, and re-derived from the artifact where the bytecode phase reads it.
            for step_field in [
                "input_hash",
                "input_bytes",
                "nonce",
                "receipt_gas_used",
                "receipt_status",
                "submission",
            ] {
                let a = at(&self.deployment, &["step", step_field])
                    .cloned()
                    .unwrap_or(Value::Null);
                let b = first.get(step_field).cloned().unwrap_or(Value::Null);
                if a != b {
                    self.note(
                        "live",
                        format!(
                            "contract/deployment.json's step.{step_field} is {a}, the ladder's \
                                first step's {step_field} is {b}"
                        ),
                    );
                }
                step_rows.push(json!({
                    "field": format!("step.{step_field}"),
                    "deployment_json": a,
                    "ladder_first_step": b,
                    "step_field": step_field,
                    "agree": a == b,
                }));
            }
        } else {
            self.note(
                "live",
                "real/giwa_ladder_steps.json publishes no steps array",
            );
        }
        // Every file in the directory that claims to be about the deployed contract has to name the
        // same executor, or the directory is describing two deployments.
        let mut executor_spellings: Vec<Value> = Vec::new();
        for (label, value) in [
            (
                "contract/deployment.json",
                self.deployment
                    .get("contract_address")
                    .cloned()
                    .unwrap_or(Value::Null),
            ),
            (
                "real/giwa_execution.json",
                self.real.get("executor").cloned().unwrap_or(Value::Null),
            ),
            (
                "real/giwa_failure.json",
                self.failure.get("executor").cloned().unwrap_or(Value::Null),
            ),
            (
                "real/giwa_ladder_steps.json",
                ladder
                    .as_ref()
                    .ok()
                    .and_then(|d| d.get("executor"))
                    .cloned()
                    .unwrap_or(Value::Null),
            ),
        ] {
            let same = !value.is_null()
                && value
                    == self
                        .deployment
                        .get("contract_address")
                        .cloned()
                        .unwrap_or(Value::Null);
            if !same {
                self.note(
                    "live",
                    format!("{label} does not name the deployed executor"),
                );
            }
            executor_spellings.push(json!({ "file": label, "executor": value, "same": same }));
        }
        // The fixture rows run the executor at the address their state recipe deploys it to, which
        // is not the address the live run deployed it at. That is a fact about the directory, not a
        // defect in it — the controlled fixtures never touched the real chain — so it is published
        // rather than noted.
        let rows = self.rows.clone();
        let fixture_executor: Vec<Value> = rows
            .iter()
            .filter_map(|row| field_str(row.run_spec(), "executor").ok())
            .collect::<BTreeSet<String>>()
            .into_iter()
            .map(|value| {
                json!({
                    "executor": value,
                    "is_the_deployed_executor": value == self.deployment
                        .get("contract_address")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    "reading": "the address a state recipe deploys the fixture bytecode to; \
                                real/giwa_ladder_steps.json names the address the live deployment \
                                transaction created",
                })
            })
            .collect();

        // §44's address prediction, recomputed from the two facts that determine it.
        let sender = field_addr(&self.deployment, "operator");
        let nonce = ladder_steps
            .first()
            .and_then(|first| first.get("nonce"))
            .and_then(Value::as_u64);
        let predicted = match (sender, nonce) {
            (Ok(sender), Some(nonce)) => {
                let mine = create_address(sender, nonce);
                let published =
                    path_str(&self.deployment, &["predicted_address"]).map(str::to_string);
                if published.as_deref() != Some(mine.as_str()) {
                    self.note(
                        "live",
                        format!(
                            "keccak(rlp([{sender:#x}, {nonce}])) is {mine}, the file predicts {}",
                            published.as_deref().unwrap_or("nothing")
                        ),
                    );
                }
                json!({
                    "recomputed": mine,
                    "published_predicted_address": path_str(&self.deployment, &["predicted_address"]),
                    "published_receipt_contract_address": path_str(&self.deployment, &["contract_address"]),
                    "sender": path_str(&self.deployment, &["operator"]),
                    "nonce": nonce,
                    "agree": published.as_deref() == Some(mine.as_str()),
                    "formula": "keccak256(rlp([sender, nonce])) — the CREATE address, derived here \
                                from the operator and the creation transaction's nonce, both of \
                                which the ladder publishes",
                })
            }
            _ => {
                self.note(
                    "live",
                    "the deployment row or the ladder's first step does not publish \
                                   both the sender and the nonce the CREATE address needs",
                );
                json!({ "recomputed": Value::Null })
            }
        };

        self.check(
            "live_run",
            json!({
                "files_read": [
                    "real/giwa_execution.json",
                    "real/giwa_failure.json",
                    "contract/deployment.json",
                    "contract/bytecode_hash.json",
                    "real/giwa_ladder_steps.json",
                    "real/preconditions.json",
                ],
                "readable": {
                    "giwa_execution": self.real.is_object(),
                    "giwa_failure": self.failure.is_object(),
                    "deployment": self.deployment.is_object(),
                    "bytecode_hash": self.bytecode.is_object(),
                    "ladder_steps": ladder.is_ok(),
                    "preconditions": preconditions_ok,
                },
                "ladder_step_count": ladder_steps.len(),
                "deployment_and_bytecode_hash_agree": shared.iter().all(|e| e["agree"] == Value::Bool(true)),
                "deployment_fields": shared,
                "deployment_row_is_the_ladders_first_step": step_rows,
                "the_deployed_executor_is_named_by": executor_spellings,
                "fixture_executor_address_is_not_the_deployed_one": fixture_executor,
                "create_address_recomputation": predicted,
                "chain_id": self.real.get("chain_id").cloned().unwrap_or(Value::Null),
                "market": self.real.get("market").cloned().unwrap_or(Value::Null),
            }),
        )
    }
}

// ---------------------------------------------------------------------------
// Phase 7 — §47's ABI and bytecode evidence
// ---------------------------------------------------------------------------

/// The compiler's own selector table, section by section.
///
/// `contracts/artifacts/ArbitrageExecutor.signatures` is what `solc` printed for the bytecode M10
/// deployed — a listing produced by neither this workspace's Rust nor this gate. It is the third
/// witness in the selector comparison, and the only one that can catch a Rust module that invented
/// a signature nobody compiled.
fn parse_selector_table(text: &str) -> BTreeMap<String, Vec<(String, String)>> {
    let mut sections: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut current = "preamble".to_string();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.ends_with(':') {
            current = trimmed.trim_end_matches(':').trim().to_string();
            sections.entry(current.clone()).or_default();
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        sections
            .entry(current.clone())
            .or_default()
            .push((key.trim().to_string(), value.trim().to_string()));
    }
    sections
}

/// A parameter's canonical Solidity type, derived mechanically from the compiler's ABI JSON:
/// `tuple` becomes its components in parentheses, and every array suffix is kept.
fn canonical_type(input: &Value) -> String {
    let declared = input
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Some(open) = declared.find('[') {
        let base = &declared[..open];
        let suffix = &declared[open..];
        let element = json!({
            "type": base,
            "components": input.get("components").cloned().unwrap_or(Value::Null),
        });
        return format!("{}{}", canonical_type(&element), suffix);
    }
    if declared == "tuple" {
        let components = input
            .get("components")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        return format!(
            "({})",
            components
                .iter()
                .map(canonical_type)
                .collect::<Vec<String>>()
                .join(",")
        );
    }
    declared.to_string()
}

/// `name(typeA,typeB)` from an ABI entry's `inputs` — the string whose keccak the chain uses.
fn canonical_signature(entry: &Value) -> String {
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let inputs = entry
        .get("inputs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    format!(
        "{name}({})",
        inputs
            .iter()
            .map(canonical_type)
            .collect::<Vec<String>>()
            .join(",")
    )
}

/// Every `keyword Name(...)` declaration line in the contract source, by identifier.
fn declared_in_source(source: &str, keyword: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix(keyword) else {
            continue;
        };
        if rest.chars().next().is_some_and(|c| !c.is_whitespace()) {
            continue;
        }
        let rest = rest.trim_start();
        let name = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect::<String>();
        if !name.is_empty() {
            out.push((name, trimmed.to_string()));
        }
    }
    out
}

/// A selector or topic in whichever width the compiler printed it.
fn selector_text(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

/// `contracts/artifacts/*.bin*` are bare lowercase hex on one line, which is not JSON, so they get
/// their own reader. Anything that is not hex is reported as such rather than decoded optimistically.
fn read_artifact_hex(root: &Path, rel: &str) -> Result<Vec<u8>, String> {
    let text = read_text(root, rel)?;
    let trimmed = text.trim();
    let body = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    if body.is_empty() {
        return Err(format!("{rel} is empty"));
    }
    if !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "{rel} is not bare hex: its first non-hex character is {:?}",
            body.chars().find(|c| !c.is_ascii_hexdigit())
        ));
    }
    hex::decode(body).map_err(|e| format!("decoding {rel}: {e}"))
}

impl Gate {
    /// §47's first artifact: the ABI, pinned four ways — the compiler's structured ABI, the
    /// compiler's selector listing, this gate's own keccak of the signature strings both produced,
    /// and what the evidence rows actually put on the wire.
    ///
    /// The failure §47 names is `Rust calldata ≠ 实际部署 contract ABI`, and it is only detectable
    /// if the comparison runs against something the Rust side does not own. So the two artifact
    /// files (produced by `solc`) are the authority here; the crate's derived constants are the
    /// defendant; and the rows' published `signature`, `selector`, revert bytes, and log topics are
    /// what a real EVM actually saw.
    fn contract_abi(&mut self) -> Value {
        let abi_rel = "contracts/artifacts/ArbitrageExecutor.abi";
        let sig_rel = "contracts/artifacts/ArbitrageExecutor.signatures";
        let sol_rel = "contracts/ArbitrageExecutor.sol";

        let abi_doc = match read_json(&self.root, abi_rel) {
            Ok(doc) => doc,
            Err(e) => {
                self.note("abi", e);
                return json!({ "readable": false });
            }
        };
        let entries = abi_doc.as_array().cloned().unwrap_or_default();
        let table_text = match read_text(&self.root, sig_rel) {
            Ok(text) => text,
            Err(e) => {
                self.note("abi", e);
                String::new()
            }
        };
        let source = match read_text(&self.root, sol_rel) {
            Ok(text) => text,
            Err(e) => {
                self.note("abi", e);
                String::new()
            }
        };
        let table = parse_selector_table(&table_text);
        let mut by_selector: BTreeMap<String, String> = BTreeMap::new();
        let mut by_signature: BTreeMap<String, String> = BTreeMap::new();
        let table_entries: usize = table.values().map(Vec::len).sum();
        for (section, rows) in &table {
            for (key, signature) in rows {
                by_selector.insert(key.clone(), format!("{section} | {signature}"));
                by_signature.insert(signature.clone(), key.clone());
            }
        }

        // 1. ABI JSON → canonical signature → keccak → the compiler's own table.
        let mut surface: Vec<Value> = Vec::new();
        for entry in &entries {
            let kind = entry
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            if kind == "constructor" {
                continue;
            }
            let signature = canonical_signature(entry);
            let digest = alloy_primitives::keccak256(signature.as_bytes());
            let four = selector_text(&digest.as_slice()[..4]);
            let topic = selector_text(digest.as_slice());
            let width = if kind == "event" { 32 } else { 4 };
            let derived = if width == 4 { &four } else { &topic };
            let in_the_table = by_selector.get(derived.as_str()).cloned();
            let section = match kind {
                "function" => "Function signatures",
                "error" => "Error signatures",
                "event" => "Event signatures",
                other => other,
            };
            let exact = in_the_table
                .as_deref()
                .map(|v| v == format!("{section} | {signature}"))
                .unwrap_or(false);
            if !exact {
                self.note(
                    "abi",
                    format!(
                        "the compiler's ABI derives {kind} {signature} (keccak {derived}), and its \
                         listing says {in_the_table:?} — the two files solc wrote do not describe \
                         one ABI"
                    ),
                );
            }
            surface.push(json!({
                "kind": kind,
                "name": entry.get("name").cloned().unwrap_or(Value::Null),
                "state_mutability": entry.get("stateMutability").cloned().unwrap_or(Value::Null),
                "inputs": entry.get("inputs").cloned().unwrap_or(Value::Null),
                "outputs": entry.get("outputs").cloned().unwrap_or(Value::Null),
                "anonymous": entry.get("anonymous").cloned().unwrap_or(Value::Null),
                "signature_derived_from_the_abi_json": signature,
                "keccak_of_that_signature": format!("0x{topic}"),
                "selector_or_topic0_width_bytes": width,
                "selector_or_topic0": format!("0x{derived}"),
                "listed_by_the_compiler_with_the_same_spelling": exact,
                "compiler_listing_line": in_the_table,
            }));
        }
        // …and the reverse: a line in the compiler's listing that its own ABI does not produce.
        let derived_set: BTreeSet<String> = surface
            .iter()
            .filter_map(|e| {
                e["signature_derived_from_the_abi_json"]
                    .as_str()
                    .map(str::to_string)
            })
            .collect();
        let table_only: Vec<Value> = by_signature
            .iter()
            .filter(|(signature, _)| !derived_set.contains(*signature))
            .map(|(signature, selector)| json!({ "listing": selector, "signature": signature }))
            .collect();
        for entry in &table_only {
            self.note(
                "abi",
                format!(
                    "the compiler lists {} as {}, which its ABI JSON does not describe",
                    entry["signature"], entry["listing"]
                ),
            );
        }

        // 2. The Rust crate's own answer for the one selector every row uses.
        let execute_signature = canonical_signature(
            &entries
                .iter()
                .find(|e| e.get("name").and_then(Value::as_str) == Some("execute"))
                .cloned()
                .unwrap_or(Value::Null),
        );
        let hand_from_rust = selector_text(&selector_from_signature(&execute_signature));
        let crate_hex = execute_selector_hex();
        let rows = self.rows.clone();
        let mut row_signatures: Vec<Value> = Vec::new();
        for (signature, selector) in rows
            .iter()
            .filter_map(|row| {
                let observed = row.observed();
                let signature = field_str(observed, "signature").ok()?;
                let selector = field_str(observed, "selector").ok()?;
                Some((signature, selector))
            })
            .collect::<BTreeSet<(String, String)>>()
        {
            let digest = alloy_primitives::keccak256(signature.as_bytes());
            let mine = selector_text(&digest.as_slice()[..4]);
            let agrees = mine == *selector;
            if !agrees {
                self.note(
                    "abi",
                    format!(
                        "a row publishes {signature} with selector {selector}, and keccak of that \
                         string is {mine}"
                    ),
                );
            }
            row_signatures.push(json!({
                "signature": signature,
                "published_selector": selector,
                "keccak_of_the_published_signature": format!("0x{mine}"),
                "keccak_via_the_protocol_crate": format!("0x{}", selector_text(
                    selector_from_signature(&signature).as_slice()
                )),
                "agrees_with_the_compilers_listing": by_signature.get(&signature).cloned(),
                "is_the_execute_selector_of_the_crate": if signature == execute_signature {
                    Some(crate_hex.clone() == format!("0x{selector}"))
                } else {
                    None
                },
                "agree": agrees,
            }));
        }
        if hand_from_rust != crate_hex.trim_start_matches("0x") {
            self.note(
                "abi",
                format!(
                    "the protocol crate's own keccak path says 0x{hand_from_rust} for \
                     {execute_signature} and its generated constant says {crate_hex}"
                ),
            );
        }

        // 3. The revert bytes the rows published, selector by selector.
        let errors_by_selector: BTreeMap<String, String> = surface
            .iter()
            .filter(|e| e["kind"] == Value::String("error".to_string()))
            .filter_map(|e| {
                let selector = e["selector_or_topic0"]
                    .as_str()?
                    .trim_start_matches("0x")
                    .to_string();
                let name = e["name"].as_str()?.to_string();
                Some((selector, name))
            })
            .collect();
        let standard = [
            ("Error(string)", "08c379a0"),
            ("Panic(uint256)", "4e487b71"),
        ];
        let mut reverts: Vec<Value> = Vec::new();
        for row in &rows {
            let Some(raw) = published_revert_bytes(row.observed()) else {
                continue;
            };
            if raw.len() < 4 {
                reverts.push(json!({
                    "row": row.key,
                    "bytes": raw.len(),
                    "note": "shorter than a selector — an empty or bare revert, which §40's \
                             classification names as such",
                }));
                continue;
            }
            let selector = selector_text(&raw[..4]);
            let named = errors_by_selector.get(&selector).cloned();
            let standard_named = standard
                .iter()
                .find(|(_, sel)| *sel == selector.as_str())
                .map(|(sig, _)| (*sig).to_string());
            let published = row
                .observed()
                .get("contract_error")
                .cloned()
                .unwrap_or(Value::Null);
            let published_kind = row
                .observed()
                .get("revert_kind")
                .cloned()
                .unwrap_or(Value::Null);
            let label = published
                .as_str()
                .or_else(|| published_kind.as_str())
                .map(str::to_string);
            let explained = named.is_some() || standard_named.is_some();
            if !explained {
                self.note(
                    "abi",
                    format!(
                        "{}: the revert starts with 0x{selector}, which is none of the {} errors \
                         this ABI declares nor the two standard reverts — §47's ban is exactly a \
                         payload nobody's ABI can name",
                        row.key,
                        errors_by_selector.len()
                    ),
                );
            }
            if let (Some(from_bytes), Some(text)) = (&named, label.as_ref()) {
                if from_bytes != text {
                    self.note(
                        "abi",
                        format!(
                            "{}: the bytes say {from_bytes} and the row labels it {text}",
                            row.key
                        ),
                    );
                }
            }
            // An executor error is agreed when the bytes name it; a standard revert is agreed when
            // the row's own label is the standard signature this gate hashed to that selector.
            let standard_label = standard
                .iter()
                .find(|(_, sel)| *sel == selector.as_str())
                .map(|(sig, _)| (*sig).to_string());
            let agreed = match (&named, &standard_label, label.as_ref()) {
                (Some(from_bytes), _, Some(text)) => from_bytes == text,
                (None, Some(sig), Some(text)) => sig == text,
                _ => false,
            };
            reverts.push(json!({
                "row": row.key,
                "bytes": raw.len(),
                "first_four_bytes": format!("0x{selector}"),
                "named_by_this_abi": named,
                "named_by_a_standard_signature": standard_label,
                "label_published_by_the_row": label,
                "bytes_and_label_agree": agreed,
            }));
        }

        // 4. The topics the successful runs actually emitted.
        let mut emitted: BTreeMap<String, usize> = BTreeMap::new();
        let mut events_at_executor: usize = 0;
        for row in &rows {
            let Ok(executor) = field_str(row.observed(), "executor") else {
                continue;
            };
            let Some(logs) = row.observed().get("logs").and_then(Value::as_array) else {
                continue;
            };
            for log in logs {
                let Some(topic) = log
                    .get("topics")
                    .and_then(Value::as_array)
                    .and_then(|t| t.first())
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                let address = log
                    .get("address")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if address != executor {
                    continue;
                }
                events_at_executor += 1;
                *emitted.entry(topic.to_string()).or_default() += 1;
            }
        }
        let events_by_topic: BTreeMap<String, String> = surface
            .iter()
            .filter(|e| e["kind"] == Value::String("event".to_string()))
            .filter_map(|e| {
                let topic = e["selector_or_topic0"].as_str()?.to_string();
                let name = e["name"].as_str()?.to_string();
                Some((topic, name))
            })
            .collect();
        let mut named_events: BTreeMap<String, usize> = BTreeMap::new();
        let mut unmatched_topics: Vec<String> = Vec::new();
        for (topic, count) in &emitted {
            match events_by_topic.get(topic) {
                Some(name) => {
                    *named_events.entry(name.clone()).or_default() += *count;
                }
                None => unmatched_topics.push(topic.clone()),
            }
        }
        // The ERC-20 transfers in the same logs are the pair and token contracts talking, not the
        // executor, and are excluded by the address filter above; so any topic emitted *at* the
        // executor address that this ABI cannot name is a §47 finding.
        for topic in &unmatched_topics {
            self.note(
                "abi",
                format!(
                    "a published run emitted topic0 {topic} at the executor address, which \
                         this ABI does not declare"
                ),
            );
        }

        // 5. The source's own declarations, counted rather than quoted.
        let source_errors = declared_in_source(&source, "error");
        let source_events = declared_in_source(&source, "event");
        let source_functions = declared_in_source(&source, "function");
        let abi_count = |kind: &str| {
            surface
                .iter()
                .filter(|e| e["kind"] == Value::String(kind.to_string()))
                .count()
        };
        for (kind, declared, names) in [
            ("error", source_errors.len(), &source_errors),
            ("event", source_events.len(), &source_events),
        ] {
            let in_abi = abi_count(kind);
            if in_abi != declared {
                self.note(
                    "abi",
                    format!(
                        "the compiler's ABI declares {in_abi} {kind}s and the source declares \
                         {declared} — one of them is describing a different contract"
                    ),
                );
            }
            let set: BTreeSet<String> = names.iter().map(|(n, _)| n.clone()).collect();
            for entry in &surface {
                if entry["kind"] != Value::String(kind.to_string()) {
                    continue;
                }
                if let Some(name) = entry["name"].as_str() {
                    if !set.contains(name) {
                        self.note(
                            "abi",
                            format!(
                                "the ABI declares {kind} {name}, which no declaration line in \
                                     {sol_rel} names"
                            ),
                        );
                    }
                }
            }
        }
        let function_lines: BTreeSet<String> = source_functions
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        let public_getters: BTreeSet<String> = source
            .lines()
            .filter_map(|line| {
                let mut rest = line.split_once(" public ")?.1.trim_start();
                // `uint256 public constant MAX_LEGS = 4;` puts a modifier between `public` and the
                // variable name, and the getter solc derives is named after the variable, not after
                // the modifier.
                for modifier in ["constant", "immutable"] {
                    if let Some(after) = rest.strip_prefix(modifier) {
                        rest = after.trim_start();
                        break;
                    }
                }
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                (!name.is_empty()).then_some(name)
            })
            .collect();
        let mut function_surface: Vec<Value> = Vec::new();
        for entry in &surface {
            if entry["kind"] != Value::String("function".to_string()) {
                continue;
            }
            let name = entry["name"].as_str().unwrap_or_default().to_string();
            let declared_as = if function_lines.contains(&name) {
                "a function declaration in the source"
            } else if public_getters.contains(&name) {
                "a public state variable in the source, which solc exposes as a getter"
            } else {
                "nothing — no line in the source names it"
            };
            if declared_as.starts_with("nothing") {
                self.note("abi", format!("{name} is in the ABI and not in {sol_rel}"));
            }
            function_surface.push(json!({
                "name": name,
                "signature": entry["signature_derived_from_the_abi_json"],
                "selector": entry["selector_or_topic0"],
                "declared_in_the_source_as": declared_as,
            }));
        }

        let body = json!({
            "what_this_is": "§47's ABI evidence, and §60's contract/abi.json. The ABI below is not \
                             transcribed from the Rust module that encodes it: it is read out of \
                             solc's own artifact, re-derived as signature strings by this gate, \
                             hashed here, and then compared with the compiler's printed selector \
                             table, with the protocol crate's derived constants, and with what the \
                             published runs actually put on the wire.",
            "sources": {
                "solc_abi_json": { "file": abi_rel, "digest": file_digest(&self.root, abi_rel).ok(), "entries": entries.len() },
                "solc_selector_listing": { "file": sig_rel, "digest": file_digest(&self.root, sig_rel).ok(), "lines": table_entries, "sections": table.keys().cloned().collect::<Vec<_>>() },
                "contract_source": { "file": sol_rel, "digest": file_digest(&self.root, sol_rel).ok(), "error_declarations": source_errors.len(), "event_declarations": source_events.len(), "function_declarations": source_functions.len(), "public_getter_declarations": public_getters.len() },
                "abi_version_as_published": self.deployment.get("abi_version").cloned().unwrap_or(Value::Null),
            },
            "surface": surface,
            "functions": function_surface,
            "listing_lines_with_no_abi_entry": table_only,
            "the_execute_selector_four_ways": {
                "signature_derived_from_the_abi_json": execute_signature,
                "keccak_computed_by_this_gate": format!("0x{hand_from_rust}"),
                "protocol_crates_generated_constant": crate_hex,
                "compilers_own_listing": by_signature.get(&execute_signature).cloned(),
                "published_by_every_scenario_row_as_observed_selector": row_signatures,
            },
            "revert_payload_selectors": {
                "rows_with_published_revert_bytes": reverts.len(),
                "table": reverts,
                "standard_signatures_handed_here": standard.iter().map(|(s, sel)| json!({
                    "signature": s,
                    "keccak_by_this_gate": format!("0x{}", selector_text(&alloy_primitives::keccak256(s.as_bytes()).as_slice()[..4])),
                    "expected": sel,
                })).collect::<Vec<_>>(),
            },
            "event_emissions": {
                "logs_emitted_at_the_executor_address": events_at_executor,
                "distinct_topic0_values": emitted.len(),
                "named_by_this_abi": named_events,
                "unnamed_by_this_abi": unmatched_topics,
            },
            "counts": {
                "abi_entries_including_constructor": entries.len(),
                "functions": abi_count("function"),
                "errors": abi_count("error"),
                "events": abi_count("event"),
                "selectors_and_topics_derived_by_this_gate": surface.len(),
            },
            "what_this_proves": "one ABI, four independent spellings of it: the bytes REVM ran \
                                 decode under these selectors, every revert payload in this \
                                 directory names an error this ABI declares, every log the \
                                 executor emitted matches an event topic this ABI derives, and the \
                                 compiler's own listing agrees with its own JSON. §47's ban holds \
                                 for the calldata this workspace produces.",
            "what_this_does_not_prove": "that the code at the deployed address is this ABI's code. \
                                         Nothing here asks a node for runtime bytecode — §51 \
                                         forbids a gate from adding a read — so the link from this \
                                         ABI to chain state is the same single compilation §42/§43 \
                                         pin, restated in contract/runtime_bytecode.json.",
        });
        self.write_json("contract/abi.json", json!({ "body": body.clone() }));
        let digests: Vec<Value> = WRITTEN
            .iter()
            .filter_map(|rel| {
                self.digest_of(rel)
                    .map(|d| json!({ "file": rel, "digest": d }))
            })
            .collect();
        self.check(
            "contract_abi",
            json!({
                "file": "contract/abi.json",
                "digest": self.digest_of("contract/abi.json"),
                "written_files_so_far": digests,
                "surface_count": surface.len(),
                "counts": body["counts"],
                "rows_publishing_the_execute_selector": row_signatures.len(),
                "revert_bytes_rows_matched": reverts.iter().filter(|e| e["bytes_and_label_agree"] == Value::Bool(true)).count(),
                "revert_bytes_rows_total": reverts.len(),
                "events_at_the_executor_address": events_at_executor,
                "unnamed_event_topics": unmatched_topics.len(),
                "listing_lines_with_no_abi_entry": table_only.len(),
                "abi_version": self.deployment.get("abi_version").cloned().unwrap_or(Value::Null),
            }),
        );
        body
    }

    /// §47's second artifact: the runtime code, and the only bridge in this directory between the
    /// bytecode REVM simulated and the bytecode the deployment transaction carried.
    fn runtime_bytecode(&mut self) -> Value {
        let creation_rel = "contracts/artifacts/ArbitrageExecutor.bin";
        let runtime_rel = "contracts/artifacts/ArbitrageExecutor.bin-runtime";
        let creation = read_artifact_hex(&self.root, creation_rel);
        let runtime = read_artifact_hex(&self.root, runtime_rel);
        let (creation, runtime) = match (creation, runtime) {
            (Ok(c), Ok(r)) => (c, r),
            (Err(e), _) | (_, Err(e)) => {
                self.note("bytecode", e);
                return json!({ "readable": false });
            }
        };
        let creation_hash = format!("{:#x}", alloy_primitives::keccak256(&creation));
        let runtime_hash = format!("{:#x}", alloy_primitives::keccak256(&runtime));
        let published_creation =
            path_str(&self.deployment, &["creation_code_hash"]).map(str::to_string);
        let published_creation_in_hash_file =
            path_str(&self.bytecode, &["creation_code_keccak256"]).map(str::to_string);
        for (label, published) in [
            ("contract/deployment.json", &published_creation),
            (
                "contract/bytecode_hash.json",
                &published_creation_in_hash_file,
            ),
        ] {
            if published.as_deref() != Some(creation_hash.as_str()) {
                self.note(
                    "bytecode",
                    format!(
                        "{label} publishes {published:?} as the creation-code hash; keccak of \
                         {creation_rel} is {creation_hash}"
                    ),
                );
            }
        }
        let counted = creation.len() as u64;
        for (source, published) in [
            (
                "contract/deployment.json",
                self.deployment.get("creation_code_bytes").cloned(),
            ),
            (
                "contract/bytecode_hash.json",
                self.bytecode.get("creation_code_bytes").cloned(),
            ),
        ] {
            match published.as_ref().and_then(Value::as_u64) {
                Some(claimed) if claimed == counted => {}
                Some(claimed) => self.note(
                    "bytecode",
                    format!("{source} claims creation_code_bytes {claimed}, this gate counts                             {counted} bytes"),
                ),
                None => self.note(
                    "bytecode",
                    format!("{source} does not publish creation_code_bytes as a number"),
                ),
            }
        }
        // The creation transaction sent the compiler's creation code with one 32-byte constructor
        // argument appended — `constructor(address operator_)` — so its input hash is not the
        // artifact's hash and the two must not be compared with each other. What can be re-derived
        // is the argument: the artifact plus the operator this directory names has to reproduce the
        // input hash the ladder published.
        let ladder_first_step = self
            .summary
            .get("ladder_steps")
            .and_then(|doc| doc.get("steps"))
            .and_then(Value::as_array)
            .and_then(|steps| steps.first())
            .cloned()
            .unwrap_or(Value::Null);
        let ladder_input_hash = ladder_first_step
            .get("input_hash")
            .and_then(Value::as_str)
            .map(str::to_string);
        let ladder_input_bytes = ladder_first_step.get("input_bytes").and_then(Value::as_u64);
        let creation_transaction = match field_addr(&self.deployment, "operator") {
            Ok(operator) => {
                let mut argument = [0u8; 32];
                argument[12..].copy_from_slice(operator.as_slice());
                let mut input = creation.clone();
                input.extend_from_slice(&argument);
                let hash = keccak_bytes(&input);
                let hash_agrees = ladder_input_hash.as_deref() == Some(hash.as_str());
                let bytes_agree = ladder_input_bytes == Some(input.len() as u64);
                if !hash_agrees {
                    self.note(
                        "bytecode",
                        format!(
                            "keccak of the compiler's creation code with the published operator's \
                             32-byte argument appended is {hash}; the ladder's first step publishes \
                             {ladder_input_hash:?} — either the bytes that were sent are not this \
                             artifact, or the constructor argument is not the operator this \
                             directory names"
                        ),
                    );
                }
                if !bytes_agree {
                    self.note(
                        "bytecode",
                        format!(
                            "the creation code is {} bytes, the constructor takes one 32-byte word, \
                             and the ladder's first step published {ladder_input_bytes:?} bytes of \
                             input",
                            creation.len()
                        ),
                    );
                }
                json!({
                    "constructor": "constructor(address operator_), read out of the contract source",
                    "operator_argument": format!("{operator:#x}"),
                    "input_is": "the creation code with the one argument word appended",
                    "creation_code_bytes": creation.len(),
                    "recomputed_input_bytes": input.len(),
                    "published_input_bytes": ladder_input_bytes,
                    "recomputed_input_keccak256": hash,
                    "published_input_hash": ladder_input_hash,
                    "creation_code_keccak256": creation_hash,
                    "agree": hash_agrees && bytes_agree,
                    "why_this_is_here": "§48 asks what a reader can re-verify from real-chain \
                                         evidence without a node; this is the one hash in the \
                                         directory that is derived from the compiler's bytes and \
                                         the deployment row alone, and it is what ties the \
                                         deployed contract's operator to this artifact",
                })
            }
            Err(e) => {
                self.note(
                    "bytecode",
                    format!(
                        "{e}, so the constructor argument cannot be re-derived from the \
                            published deployment row"
                    ),
                );
                json!({ "agree": false, "why_not": e })
            }
        };
        // The runtime code is what the fixture deploys, what every recipe that runs the executor
        // declares, and — for a constructor that copies its body — the tail of the creation code.
        let suffix = creation.len() >= runtime.len()
            && creation[creation.len() - runtime.len()..] == runtime[..];
        let rows = self.rows.clone();
        let mut recipes: BTreeMap<String, Value> = BTreeMap::new();
        for row in &rows {
            let rel = field_str(row.state(), "recipe_file").unwrap_or_default();
            if rel.is_empty() || recipes.contains_key(&rel) {
                continue;
            }
            let executor = match field_str(row.run_spec(), "executor") {
                Ok(text) => text,
                Err(e) => {
                    self.note("bytecode", format!("{}: {e}", row.key));
                    continue;
                }
            };
            let entry = match read_json(&self.root, &rel) {
                Ok(doc) => {
                    let rows = doc
                        .get("rows")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let declared: Vec<Value> = rows
                        .iter()
                        .filter(|r| r.get("kind").and_then(Value::as_str) == Some("account"))
                        .filter(|r| r.get("address").and_then(Value::as_str) == Some(executor.as_str()))
                        .map(|r| {
                            let code = field_bytes(r, "code").ok();
                            let count = r.get("code_bytes").cloned().unwrap_or(Value::Null);
                            let (mine, matches_bytes) = match &code {
                                Some(bytes) => (
                                    Some(format!("{:#x}", alloy_primitives::keccak256(bytes.as_ref()))),
                                    count.as_u64() == Some(bytes.len() as u64),
                                ),
                                None => (None, false),
                            };
                            json!({
                                "address": r.get("address").cloned().unwrap_or(Value::Null),
                                "code_bytes_declared": count,
                                "code_bytes_measured": code.as_ref().map(|b| b.len()),
                                "length_declared_matches_the_bytes": matches_bytes,
                                "keccak_of_the_declared_code": mine.clone(),
                                "is_the_compilers_runtime_artifact": mine.as_deref() == Some(runtime_hash.as_str()),
                            })
                        })
                        .collect();
                    if declared.is_empty() {
                        self.note(
                            "bytecode",
                            format!("{rel} declares no account row for the executor {executor}"),
                        );
                    }
                    for row in &declared {
                        if row["is_the_compilers_runtime_artifact"] != Value::Bool(true) {
                            self.note(
                                "bytecode",
                                format!(
                                    "{rel}: the code it deploys at {executor} is not the compiler's \
                                     runtime artifact"
                                ),
                            );
                        }
                    }
                    json!({
                        "executor": executor,
                        "declared_rows": declared,
                        "recipe_keccak256": field_str(row.state(), "recipe_keccak256").ok(),
                    })
                }
                Err(e) => {
                    self.note("bytecode", e);
                    json!({ "executor": executor, "error": true })
                }
            };
            recipes.insert(rel, entry);
        }

        let body = json!({
            "what_this_is": "§47's bytecode evidence, and §60's contract/bytecode_hash.json sibling. \
                             The published deployment files hash the *creation* code; this file \
                             supplies the missing half — the runtime code hash — and shows that the \
                             same runtime code is the one every state recipe in this directory \
                             deploys for REVM.",
            "artifacts": {
                "creation": {
                    "file": creation_rel,
                    "bytes": creation.len(),
                    "keccak256": creation_hash.clone(),
                    "digest_of_the_artifact_file": file_digest(&self.root, creation_rel).ok(),
                },
                "runtime": {
                    "file": runtime_rel,
                    "bytes": runtime.len(),
                    "keccak256": runtime_hash.clone(),
                    "digest_of_the_artifact_file": file_digest(&self.root, runtime_rel).ok(),
                },
            },
            "published_by_the_live_run": {
                "contract/deployment.json": {
                    "creation_code_hash": published_creation,
                    "creation_code_bytes": self.deployment.get("creation_code_bytes").cloned().unwrap_or(Value::Null),
                    "deployment_address": self.deployment.get("contract_address").cloned().unwrap_or(Value::Null),
                    "deployment_tx": self.deployment.get("deployment_tx").cloned().unwrap_or(Value::Null),
                    "deployment_block": self.deployment.get("deployment_block").cloned().unwrap_or(Value::Null),
                    "abi_version": self.deployment.get("abi_version").cloned().unwrap_or(Value::Null),
                },
                "contract/bytecode_hash.json": {
                    "creation_code_keccak256": published_creation_in_hash_file,
                    "creation_code_bytes": self.bytecode.get("creation_code_bytes").cloned().unwrap_or(Value::Null),
                },
                "the_ladders_creation_step": creation_transaction,
            },
            "creation_contains_runtime_as_its_tail": suffix,
            "runtime_is_not_hashed_by_the_live_files": {
                "value": true,
                "why": "deployment.json and bytecode_hash.json were written by a run that could ask \
                        a node for the code it deployed and did not; neither publishes a runtime \
                        hash. §47 asks for one, so this file derives it from the compiler's runtime \
                        artifact instead of inventing a chain read for it.",
            },
            "recipes_that_deploy_the_runtime_code": recipes,
            "recipe_count": recipes.len(),
            "what_this_proves": "the code REVM simulated is byte-identical to the code the compiler \
                                 emitted for the contract that was deployed: one artifact, one hash, \
                                 and every fixture in this directory runs that hash. §47's ban on \
                                 `Rust calldata ≠ 实际部署 contract ABI` has no room to open when \
                                 the simulated code and the deployed code are the same bytes.",
            "what_this_does_not_prove": "that the deployed address currently holds these bytes. \
                                         Confirming that is an `eth_getCode` call, which §51 forbids \
                                         this gate from adding, and which the live harness did not \
                                         record. The claim here is artifact-to-artifact, and the \
                                         artifact-to-chain link is the deployment transaction's own \
                                         input hash, quoted above.",
        });
        self.write_json(
            "contract/runtime_bytecode.json",
            json!({ "body": body.clone() }),
        );
        self.check(
            "runtime_bytecode",
            json!({
                "file": "contract/runtime_bytecode.json",
                "digest": self.digest_of("contract/runtime_bytecode.json"),
                "runtime_bytes": runtime.len(),
                "creation_bytes": creation.len(),
                "runtime_keccak256": runtime_hash,
                "creation_keccak256": creation_hash,
                "creation_tail_holds_runtime": suffix,
                "recipes_checked": recipes.len(),
                "recipes_running_the_compilers_runtime": recipes
                    .iter()
                    .filter(|(_, e)| {
                        e["declared_rows"]
                            .as_array()
                            .map(|rows| rows.iter().any(|r| r["is_the_compilers_runtime_artifact"] == Value::Bool(true)))
                            .unwrap_or(false)
                    })
                    .count(),
            }),
        );
        body
    }
}

// ---------------------------------------------------------------------------
// Phase 8 — §56/§31/§32/§8/§39: the lifecycle, recomputed from the rows the live run published
// ---------------------------------------------------------------------------

fn keccak_bytes(bytes: &[u8]) -> String {
    format!("{:#x}", alloy_primitives::keccak256(bytes))
}

/// A published value beside the gate's own answer. Every row of `execution/lifecycle.json` is built
/// with this, so a reader can check the arithmetic without re-implementing it, and a disagreement
/// names both sides instead of just failing.
fn equals(check: &str, published: Value, recomputed: Value, agree: bool) -> Value {
    json!({
        "check": check,
        "published": published,
        "recomputed_by_this_gate": recomputed,
        "agree": agree,
    })
}

fn wei_check(check: &str, published: Option<U256>, recomputed: Option<U256>) -> Value {
    let agree = published.is_some() && published == recomputed;
    equals(
        check,
        json!(published.map(|word| word.to_string())),
        json!(recomputed.map(|word| word.to_string())),
        agree,
    )
}

fn text_check(check: &str, published: Option<&str>, recomputed: Option<String>) -> Value {
    let agree = published.is_some() && published == recomputed.as_deref();
    equals(check, json!(published), json!(recomputed), agree)
}

fn bool_check(check: &str, published: Option<bool>, recomputed: bool) -> Value {
    equals(
        check,
        json!(published),
        json!(recomputed),
        published == Some(recomputed),
    )
}

/// A published field rendered as text whatever scalar spelling it uses, for the tables that show a
/// field's value rather than judge it.
fn field_text(v: &Value, key: &str) -> Option<String> {
    match v.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(other) => Some(other.to_string()),
    }
}

/// `Freshness` in the one-word spelling the record uses, so a recomputed verdict and a published
/// verdict can be compared as text.
fn freshness_word(freshness: &Freshness) -> String {
    match freshness {
        Freshness::Active => "Active".to_string(),
        Freshness::Stale { .. } => "Stale".to_string(),
        Freshness::Unknown => "Unknown".to_string(),
    }
}

impl Gate {
    /// §56's ladder, §31's balance row, §32's simulation-to-reality diff, §8's freshness bound and
    /// §39's joinable ids — all of it recomputed out of the six files the live run published, with
    /// no node behind it. The gate does not re-run the lifecycle: `executor_lifecycle.rs` walks the
    /// real types through it. What lives here is the arithmetic a reader would otherwise have to do
    /// by hand, and the equalities that only hold if the row was written from the same receipt the
    /// node returned.
    fn lifecycle(&mut self) -> Value {
        // Owned copies: this phase both reads strings out of the live documents and records drift
        // about them, and a borrow from `self.real` would hold `self` across every `note` call.
        let real = self.real.clone();
        let failure = self.failure.clone();
        let ladder = self
            .summary
            .get("ladder_steps")
            .cloned()
            .unwrap_or(Value::Null);
        let preconditions = self
            .summary
            .get("preconditions")
            .cloned()
            .unwrap_or(Value::Null);
        let deployment = self.deployment.clone();
        let execution = real.get("execution").cloned().unwrap_or(Value::Null);
        let report = execution.get("report").cloned().unwrap_or(Value::Null);
        let record = report.get("lifecycle").cloned().unwrap_or(Value::Null);
        let row31 = real.get("evidence_row_31").cloned().unwrap_or(Value::Null);
        let recon = real
            .get("reconciliation_32")
            .cloned()
            .unwrap_or(Value::Null);
        let window = real.get("freshness_window").cloned().unwrap_or(Value::Null);
        let step = failure.get("step").cloned().unwrap_or(Value::Null);
        let attribution = failure
            .get("simulation_attribution")
            .cloned()
            .unwrap_or(Value::Null);

        let mut audit: Vec<Value> = Vec::new();

        // ---- §31's native-asset ledger, end to end -------------------------
        // The identity of a row is its label (or, for the two calls the lifecycle made, its
        // transaction hash), never its position in a file: a ledger that reorders when the files
        // reorder is a ledger that silently re-derives a different chain.
        let mut steps: Vec<Value> = ladder
            .get("steps")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        steps.sort_by_key(|step| field_u64(step, "nonce").unwrap_or(u64::MAX));

        let ladder_operator = path_str(&ladder, &["operator"]).map(str::to_string);
        let mut ledger: Vec<Value> = Vec::new();
        let mut previous: Option<(&Value, U256, U256, U256)> = None;
        for step_row in &steps {
            let label = path_str(step_row, &["label"])
                .unwrap_or("an unlabeled step")
                .to_string();
            let spent = wei_check(
                &format!("{label}: maximum_spend_wei"),
                field_u256(step_row, "maximum_spend_wei").ok(),
                Some(
                    field_u256(step_row, "gas_limit").unwrap_or(U256::ZERO)
                        * field_u256(step_row, "max_fee_per_gas_wei").unwrap_or(U256::ZERO)
                        + field_u256(step_row, "value_wei").unwrap_or(U256::ZERO),
                ),
            );
            let l2 = wei_check(
                &format!("{label}: receipt_l2_cost_wei"),
                field_u256(step_row, "receipt_l2_cost_wei").ok(),
                Some(
                    field_u256(step_row, "receipt_gas_used").unwrap_or(U256::ZERO)
                        * field_u256(step_row, "receipt_effective_gas_price_wei")
                            .unwrap_or(U256::ZERO),
                ),
            );
            let mut carried = Vec::new();
            if let Some((before_step, before_l2, before_l1, before_value)) = previous {
                let before_label = path_str(before_step, &["label"])
                    .unwrap_or("an unlabeled step")
                    .to_string();
                let expected = field_u256(before_step, "balance_wei").unwrap_or(U256::ZERO)
                    - (before_l2 + before_l1 + before_value);
                carried.push(wei_check(
                    &format!(
                        "{before_label} → {label}: the operator's native balance read at each step's \
                         head differs by exactly the earlier step's gas bill plus the value it sent"
                    ),
                    field_u256(step_row, "balance_wei").ok(),
                    Some(expected),
                ));
            }
            carried.push(text_check(
                &format!("{label}: recovered_sender"),
                path_str(step_row, &["recovered_sender"]),
                ladder_operator.clone(),
            ));
            carried.push(equals(
                &format!("{label}: chain id"),
                step_row.get("chain_id").cloned().unwrap_or(Value::Null),
                json!(ladder.get("chain_id").cloned().unwrap_or(Value::Null)),
                step_row.get("chain_id") == ladder.get("chain_id"),
            ));
            ledger.push(json!({
                "row": label,
                "identity": "the label the harness gave this step, which the operator wrote, plus \
                            the nonce the chain assigned it — neither is a file position",
                "nonce": step_row.get("nonce").cloned().unwrap_or(Value::Null),
                "transaction_hash": step_row.get("transaction_hash").cloned().unwrap_or(Value::Null),
                "submitted_to": step_row.get("to").cloned().unwrap_or(Value::Null),
                "head_number": step_row.get("head_number").cloned().unwrap_or(Value::Null),
                "receipt_block_number": step_row.get("receipt_block_number").cloned().unwrap_or(Value::Null),
                "gas_limit": step_row.get("gas_limit").cloned().unwrap_or(Value::Null),
                "max_fee_per_gas_wei": step_row.get("max_fee_per_gas_wei").cloned().unwrap_or(Value::Null),
                "value_wei": step_row.get("value_wei").cloned().unwrap_or(Value::Null),
                "maximum_spend_wei": step_row.get("maximum_spend_wei").cloned().unwrap_or(Value::Null),
                "receipt_gas_used": step_row.get("receipt_gas_used").cloned().unwrap_or(Value::Null),
                "receipt_effective_gas_price_wei": step_row.get("receipt_effective_gas_price_wei").cloned().unwrap_or(Value::Null),
                "receipt_l2_cost_wei": step_row.get("receipt_l2_cost_wei").cloned().unwrap_or(Value::Null),
                "receipt_l1_fee_wei": step_row.get("receipt_l1_fee_wei").cloned().unwrap_or(Value::Null),
                "receipt_status": step_row.get("receipt_status").cloned().unwrap_or(Value::Null),
                "receipt_log_count": step_row.get("receipt_log_count").cloned().unwrap_or(Value::Null),
                "balance_read_wei": step_row.get("balance_wei").cloned().unwrap_or(Value::Null),
                "submission_detail": step_row.get("detail").cloned().unwrap_or(Value::Null),
                "checks": carried.iter().cloned().chain([spent.clone(), l2.clone()]).collect::<Vec<_>>(),
            }));
            audit.extend(carried);
            audit.push(spent);
            audit.push(l2);
            previous = Some((
                step_row,
                field_u256(step_row, "receipt_l2_cost_wei").unwrap_or(U256::ZERO),
                field_u256(step_row, "receipt_l1_fee_wei").unwrap_or(U256::ZERO),
                field_u256(step_row, "value_wei").unwrap_or(U256::ZERO),
            ));
        }

        // The two transactions the execution lifecycle sent, read out of the same three wallet rows
        // the §31/§58 files publish. Their bill is what the wallet moved by; the wrap step is the
        // only place the ledger carries value rather than paying for gas.
        let wallet_before = real
            .get("wallet_at_block_before_execute")
            .cloned()
            .unwrap_or(Value::Null);
        let wallet_after = real.get("wallet_after").cloned().unwrap_or(Value::Null);
        let native_drop = field_u256(&wallet_before, "native_wei")
            .ok()
            .zip(field_u256(&wallet_after, "native_wei").ok())
            .map(|(before, after)| before - after);
        let execute_bill = field_u256(&row31, "l2_fee")
            .ok()
            .zip(field_u256(&row31, "l1_fee").ok())
            .map(|(l2, l1)| l2 + l1);
        audit.push(wei_check(
            "execute: the operator's native balance fell by exactly the receipt's L2 cost plus the \
             L1 fee, and by nothing else",
            native_drop,
            execute_bill,
        ));
        audit.push(wei_check(
            "execute: total_fee equals l2_fee + l1_fee",
            field_u256(&row31, "total_fee").ok(),
            execute_bill,
        ));
        audit.push(wei_check(
            "execute: l2_fee equals gas_used × effective_gas_price",
            field_u256(&row31, "l2_fee").ok(),
            Some(
                field_u256(&row31, "gas_used").unwrap_or(U256::ZERO)
                    * field_u256(&row31, "effective_gas_price").unwrap_or(U256::ZERO),
            ),
        ));
        audit.push(wei_check(
            "execute: the record's hex fees are the §31 row's decimal fees",
            field_u256(&row31, "total_fee").ok(),
            field_u256(&record, "total_fee").ok(),
        ));
        audit.push(wei_check(
            "execute: the record's hex L2 fee is the §31 row's",
            field_u256(&row31, "l2_fee").ok(),
            field_u256(&record, "l2_fee").ok(),
        ));
        audit.push(wei_check(
            "execute: the record's hex L1 fee is the §31 row's",
            field_u256(&row31, "l1_fee").ok(),
            field_u256(&record, "l1_fee").ok(),
        ));

        let failure_before = failure
            .get("wallet_before_failure")
            .cloned()
            .unwrap_or(Value::Null);
        let failure_after = failure
            .get("wallet_after_failure")
            .cloned()
            .unwrap_or(Value::Null);
        let failure_drop = field_u256(&failure_before, "native_wei")
            .ok()
            .zip(field_u256(&failure_after, "native_wei").ok())
            .map(|(before, after)| before - after);
        let failure_bill = field_u256(&step, "receipt_l2_cost_wei")
            .ok()
            .zip(field_u256(&step, "receipt_l1_fee_wei").ok())
            .map(|(l2, l1)| l2 + l1);
        audit.push(wei_check(
            "failure: the reverted transaction's bill is the whole native balance movement, so the \
             operator paid for the failed call and received nothing back",
            failure_drop,
            failure_bill,
        ));

        // ---- §31's asset row -----------------------------------------------
        let principal = field_u256(&real, "principal_wei").ok();
        let gross = field_u256(
            recon.get("output").unwrap_or(&Value::Null),
            "actual_gross_out_in_the_execute_block",
        )
        .ok();
        let net_from_wallet = field_u256(&wallet_after, "weth_operator_wei")
            .ok()
            .zip(field_u256(&wallet_before, "weth_operator_wei").ok())
            .map(|(after, before)| after - before);
        audit.push(wei_check(
            "§31: the WETH the operator actually gained, read off the two wallet rows",
            field_u256(&row31, "balance_delta").ok(),
            net_from_wallet,
        ));
        audit.push(wei_check(
            "§31: output_amount is the same delta the balance columns print",
            field_u256(&row31, "output_amount").ok(),
            field_u256(&row31, "balance_delta").ok(),
        ));
        audit.push(wei_check(
            "§31: the gross the route paid out is the net delta plus this plan's principal",
            gross,
            net_from_wallet
                .zip(principal)
                .map(|(net, principal)| net + principal),
        ));
        audit.push(wei_check(
            "§31: the route's published second-leg ask is that gross",
            field_u256(real.get("route").unwrap_or(&Value::Null), "ask_leg2").ok(),
            gross,
        ));
        audit.push(wei_check(
            "§31: token0 (the middle asset) left the run where it started",
            field_u256(&row31, "token0_balance_delta").ok(),
            Some(U256::ZERO),
        ));
        audit.push(equals(
            "§31: realized_profit is null, not zero — §34's two denominations do not add",
            row31.get("realized_profit").cloned().unwrap_or(Value::Null),
            json!(
                "null because the route settles in WETH and the bill is paid in the native asset"
            ),
            row31.get("realized_profit").map(Value::is_null) == Some(true)
                && row31.get("realized_profit_status").map(Value::is_null) == Some(true),
        ));

        // ---- §32: what the simulation said against what the chain did ------
        let gas_recon = recon.get("gas").cloned().unwrap_or(Value::Null);
        let input_recon = recon.get("input").cloned().unwrap_or(Value::Null);
        let output_recon = recon.get("output").cloned().unwrap_or(Value::Null);
        audit.push(wei_check(
            "§32: the gas limit that was signed is the limit the simulation completed at plus the \
             declared margin, and nothing else",
            field_u256(&gas_recon, "signed_transaction_gas_limit").ok(),
            field_u256(&gas_recon, "simulation_proved_gas_limit")
                .ok()
                .zip(field_u256(&gas_recon, "declared_margin").ok())
                .map(|(proved, margin)| proved + margin),
        ));
        audit.push(wei_check(
            "§32: the record carries the proved limit, not the signed one",
            field_u256(&gas_recon, "simulation_proved_gas_limit").ok(),
            field_u256(&gas_recon, "record_gas_limit").ok(),
        ));
        audit.push(wei_check(
            "§32: the record's own gas_limit field is that proved limit",
            field_u256(&record, "gas_limit").ok(),
            field_u256(&gas_recon, "record_gas_limit").ok(),
        ));
        audit.push(wei_check(
            "§32: the gas the run used is the gas the receipt reports, in simulation and on chain \
             alike — the §13 bound is satisfied twice over",
            field_u256(&gas_recon, "simulation_gas_used").ok(),
            field_u256(&gas_recon, "receipt_gas_used").ok(),
        ));
        audit.push(wei_check(
            "§32: the receipt's gas used is the §31 row's",
            field_u256(&gas_recon, "receipt_gas_used").ok(),
            field_u256(&row31, "gas_used").ok(),
        ));
        audit.push(wei_check(
            "§32: the principal the simulation was given is the principal the chain moved",
            field_u256(&input_recon, "simulation_amount_in").ok(),
            field_u256(&input_recon, "chain_balance_input").ok(),
        ));
        audit.push(wei_check(
            "§32: and it is the principal the plan declares",
            field_u256(&input_recon, "chain_balance_input").ok(),
            principal,
        ));
        audit.push(wei_check(
            "§32: the delivered-versus-actual difference is zero",
            field_u256(&output_recon, "difference").ok(),
            Some(U256::ZERO),
        ));
        audit.push(wei_check(
            "§32: the difference the §31 row would print is gross minus principal minus the net delta",
            gross
                .zip(net_from_wallet)
                .zip(principal)
                .map(|((gross, net), principal)| gross - net - principal),
            Some(U256::ZERO),
        ));
        audit.push(wei_check(
            "§13: the bill the record estimated is the proved limit at the declared cap",
            field_u256(&record, "estimated_execution_cost_wei").ok(),
            field_u256(&record, "gas_limit")
                .ok()
                .zip(field_u256(&record, "max_fee_per_gas").ok())
                .map(|(limit, cap)| limit * cap),
        ));

        // ---- §39: the ids that join a record back to a plan ----------------
        let simulation_id = field_hash(&record, "simulation_id").ok();
        let plan_hash = field_hash(&execution, "plan_hash").ok();
        let recomputed_risk_decision = match (simulation_id, plan_hash) {
            (Some(simulation_id), Some(plan_hash)) => {
                let mut bytes = Vec::with_capacity(64);
                bytes.extend_from_slice(simulation_id.as_slice());
                bytes.extend_from_slice(plan_hash.as_slice());
                Some(keccak_bytes(&bytes))
            }
            _ => None,
        };
        audit.push(text_check(
            "§39: risk_decision_id is keccak(simulation_id ‖ plan_hash)",
            path_str(&record, &["risk_decision_id"]),
            recomputed_risk_decision,
        ));
        audit.push(text_check(
            "§30: the idempotency key's middle part is the simulation id",
            path_str(&record, &["idempotency_key"])
                .and_then(|key| key.split('|').nth(1))
                .map(str::to_string)
                .as_deref(),
            simulation_id.map(|id| format!("{id:#x}")),
        ));
        audit.push(text_check(
            "§30: and its first part is the opportunity id",
            path_str(&record, &["idempotency_key"])
                .and_then(|key| key.split('|').next())
                .map(str::to_string)
                .as_deref(),
            path_str(&record, &["opportunity_id"]).map(str::to_string),
        ));
        audit.push(text_check(
            "the record's route id is the one the execution row published",
            path_str(&record, &["route_id"]),
            path_str(&execution, &["route_id"]).map(str::to_string),
        ));
        audit.push(text_check(
            "the record's transaction hash is the §31 row's",
            path_str(&record, &["transaction_hash"]),
            path_str(&row31, &["tx_hash"]).map(str::to_string),
        ));
        audit.push(text_check(
            "the record's target is the deployed executor",
            path_str(&record, &["target"]),
            path_str(&deployment, &["contract_address"]).map(str::to_string),
        ));
        audit.push(text_check(
            "and the deployed executor is the one the run's own row names",
            path_str(&row31, &["executor_address"]),
            path_str(&deployment, &["contract_address"]).map(str::to_string),
        ));
        audit.push(text_check(
            "§34: an ERC-20 route is sent with zero value on the transaction",
            path_str(&record, &["value_wei"]),
            Some("0x0".to_string()),
        ));

        // ---- the nonce ladder, across all three files ---------------------
        let mut nonces: Vec<(String, Option<u64>)> = steps
            .iter()
            .map(|step_row| {
                (
                    path_str(step_row, &["label"])
                        .unwrap_or("an unlabeled step")
                        .to_string(),
                    field_u64(step_row, "nonce").ok(),
                )
            })
            .collect();
        nonces.push(("execute".to_string(), field_u64(&record, "nonce").ok()));
        nonces.push(("failure".to_string(), field_u64(&step, "nonce").ok()));
        let mut nonce_steps: Vec<Value> = Vec::new();
        for window in nonces.windows(2) {
            let (previous, next) = (&window[0], &window[1]);
            let step_size = match (previous.1, next.1) {
                (Some(from), Some(to)) => to.checked_sub(from),
                _ => None,
            };
            nonce_steps.push(json!({
                "from": previous.0,
                "to": next.0,
                "from_nonce": previous.1,
                "to_nonce": next.1,
                "exactly_one_later": step_size.map(|delta| delta == 1),
            }));
            match step_size {
                Some(1) => {}
                Some(delta) => self.note(
                    "lifecycle",
                    format!(
                        "the nonce goes {} → {} with a gap of {delta}: this run's transactions are \
                         one EOA's serial sequence, and a gap means a transaction this directory \
                         does not describe was sent in between",
                        previous.0, next.0
                    ),
                ),
                None => self.note(
                    "lifecycle",
                    format!(
                        "{} and {} do not both carry a readable nonce, so this rung of the ladder \
                         cannot be judged",
                        previous.0, next.0
                    ),
                ),
            }
        }

        // ---- §8: the freshness bound, judged with the plan's own type ------
        let priced_at_block = field_u64(&window, "priced_at_block").ok();
        let send_head_block = field_u64(&window, "send_head_block").ok();
        let declared_max_block_age = field_u64(&window, "declared_max_block_age").ok();
        let mut freshness_table: Vec<Value> = Vec::new();
        if let (Some(priced), Some(head), Some(bound)) =
            (priced_at_block, send_head_block, declared_max_block_age)
        {
            let validity = PlanValidity {
                simulated_at_block: BlockNumber(priced),
                max_block_age: bound,
                provenance: "the plan's own declared bound, read from \
                             real/giwa_execution.json's freshness_window"
                    .to_string(),
            };
            let at_send = validity.freshness_at(BlockNumber(head));
            let at_seal = validity.freshness_at(BlockNumber(
                record
                    .get("execution_block")
                    .and_then(Value::as_u64)
                    .unwrap_or(head),
            ));
            let one_past = validity.freshness_at(BlockNumber(head + bound + 1));
            let behind = validity.freshness_at(BlockNumber(priced - 1));
            audit.push(text_check(
                "§8: this run was Active at the head it was sent into, judged by the plan's own \
                 freshness function over the plan's own bound",
                path_str(&execution, &["freshness"]),
                Some(freshness_word(&at_send)),
            ));
            // The two synthetic heads are a demonstration, not a comparison: nothing in the
            // directory publishes a Stale verdict, so it is `freshness_8.cases` that carries them
            // and this line that refuses to let the demonstration pass quietly if the bound stops
            // biting.
            if freshness_word(&one_past) != "Stale" || freshness_word(&behind) != "Stale" {
                self.note(
                    "lifecycle",
                    format!(
                        "§8: the plan's freshness function is supposed to say Stale one block past \
                         the declared bound and at a head behind the priced block; it said {} and \
                         {}, so the bound is a caption and not a check",
                        freshness_word(&one_past),
                        freshness_word(&behind)
                    ),
                );
            }
            freshness_table.push(json!({
                "head": head,
                "verdict": freshness_word(&at_send),
                "why": "the head the transaction was sent into",
            }));
            freshness_table.push(json!({
                "head": record.get("execution_block").cloned().unwrap_or(Value::Null),
                "verdict": freshness_word(&at_seal),
                "why": "the block that actually sealed the transaction — the bound is measured to \
                        the head at send time, and this row shows what the same function says at \
                        the receipt block",
            }));
            freshness_table.push(json!({
                "head": head + bound + 1,
                "verdict": freshness_word(&one_past),
                "why": "one block past the declared bound",
            }));
            freshness_table.push(json!({
                "head": priced - 1,
                "verdict": freshness_word(&behind),
                "why": "a head behind the block the route was priced at",
            }));
            audit.push(equals(
                "§8: the gap the file publishes is the two block numbers it publishes, subtracted",
                window
                    .get("blocks_between_priced_and_send")
                    .cloned()
                    .unwrap_or(Value::Null),
                json!(head - priced),
                field_u64(&window, "blocks_between_priced_and_send").ok() == Some(head - priced),
            ));
            audit.push(text_check(
                "§8: the block the route was priced at is the record's opportunity block",
                path_str(&window, &["priced_at_hash"]),
                path_str(&record, &["opportunity_block_hash"]).map(str::to_string),
            ));
            audit.push(equals(
                "§8: and the plan's declared bound is the one the gate runs under",
                json!(bound),
                json!(GATE_MAX_BLOCK_AGE),
                bound == GATE_MAX_BLOCK_AGE,
            ));
        } else {
            self.note(
                "lifecycle",
                "real/giwa_execution.json's freshness_window does not publish priced_at_block, \
                 send_head_block and declared_max_block_age, so §8 cannot be judged here",
            );
        }

        // ---- §27/§58: the real failure, judged out of its own two wallet rows
        let mut residue_rows: Vec<Value> = Vec::new();
        let mut residue_clean = true;
        if let (Some(before), Some(after)) = (
            failure
                .get("wallet_before_failure")
                .and_then(Value::as_object),
            failure
                .get("wallet_after_failure")
                .and_then(Value::as_object),
        ) {
            for (key, before_value) in before {
                if key == "block_number" || key == "native_wei" {
                    continue;
                }
                let after_value = after.get(key);
                let same = after_value == Some(before_value);
                if !same {
                    residue_clean = false;
                }
                residue_rows.push(json!({
                    "field": key,
                    "before": before_value,
                    "after": after_value.cloned().unwrap_or(Value::Null),
                    "unchanged": same,
                }));
            }
        } else {
            self.note(
                "lifecycle",
                "real/giwa_failure.json does not publish both wallet rows, so §27's residue claim \
                 cannot be recomputed here",
            );
        }
        audit.push(bool_check(
            "§27: with the gas bill set aside, nothing but the operator's native balance moved \
             across the reverted call — the gate's own reading of no_partial_state",
            failure.get("no_partial_state").and_then(Value::as_bool),
            residue_clean,
        ));
        audit.push(equals(
            "§27: and the reverted receipt emitted no log",
            step.get("receipt_log_count")
                .cloned()
                .unwrap_or(Value::Null),
            json!(0u64),
            field_u64(&step, "receipt_log_count").ok() == Some(0),
        ));
        audit.push(equals(
            "§58: the receipt status is the failure the file says it is",
            step.get("receipt_status").cloned().unwrap_or(Value::Null),
            json!("reverted"),
            path_str(&step, &["receipt_status"]) == Some("reverted")
                && failure.get("reverted").and_then(Value::as_bool) == Some(true),
        ));
        audit.push(equals(
            "§27: the residue was read at the block the after-row names",
            failure
                .get("residue_read_at_block")
                .cloned()
                .unwrap_or(Value::Null),
            failure_after
                .get("block_number")
                .cloned()
                .unwrap_or(Value::Null),
            failure.get("residue_read_at_block") == failure_after.get("block_number"),
        ));
        audit.push(text_check(
            "the calldata the reverted call sent is the calldata the attribution simulated",
            path_str(&step, &["input_hash"]),
            path_str(&attribution, &["calldata_hash"]).map(str::to_string),
        ));
        audit.push(equals(
            "and it is the same length the attribution published",
            step.get("input_bytes").cloned().unwrap_or(Value::Null),
            attribution
                .get("calldata_len")
                .cloned()
                .unwrap_or(Value::Null),
            step.get("input_bytes") == attribution.get("calldata_len"),
        ));
        audit.push(wei_check(
            "§58: the floor that failed is the floor the plan published",
            field_u256(&attribution, "min_final_amount").ok(),
            field_u256(&failure, "min_final_amount_wei").ok(),
        ));
        audit.push(text_check(
            "the failure file's two spellings of the refusal agree",
            path_str(&attribution, &["contract_error"]),
            path_str(&attribution, &["revert_kind"]).map(str::to_string),
        ));
        let gate_rejudge = self
            .summary
            .get("d4_rejudge")
            .and_then(|d| d.get("real_chain_failure"))
            .and_then(|real| real.get("classified_by_this_gate"))
            .and_then(|decoded| decoded.get("contract_error"))
            .cloned()
            .unwrap_or(Value::Null);
        let published_error = attribution
            .get("contract_error")
            .cloned()
            .unwrap_or(Value::Null);
        audit.push(equals(
            "§49's D4: the classification the gate decoded from the revert bytes is the one the \
             failure file names",
            published_error.clone(),
            gate_rejudge.clone(),
            !gate_rejudge.is_null() && gate_rejudge == published_error,
        ));

        // ---- the run's own identity, once more, so nothing floats -----------
        audit.push(wei_check(
            "the principal the preconditions planned is the principal the execute row prices",
            principal,
            field_u256(&preconditions, "planned_principal_wei").ok(),
        ));
        audit.push(wei_check(
            "and it is the principal the ladder published",
            principal,
            field_u256(&ladder, "principal_wei").ok(),
        ));
        audit.push(wei_check(
            "and the wrap step sent exactly that much native asset",
            principal,
            steps
                .iter()
                .find(|row| path_str(row, &["label"]) == Some("wrap-principal"))
                .and_then(|row| field_u256(row, "value_wei").ok()),
        ));
        audit.push(text_check(
            "the operator the ladder sent from is the operator the deployment recorded",
            path_str(&ladder, &["operator"]),
            path_str(&deployment, &["operator"]).map(str::to_string),
        ));
        audit.push(text_check(
            "and is the sender the lifecycle record carries",
            path_str(&record, &["sender"]),
            path_str(&ladder, &["operator"]).map(str::to_string),
        ));

        let unique_hashes: BTreeSet<String> = steps
            .iter()
            .filter_map(|step_row| path_str(step_row, &["transaction_hash"]).map(str::to_string))
            .chain(
                [
                    path_str(&record, &["transaction_hash"]).map(str::to_string),
                    path_str(&step, &["transaction_hash"]).map(str::to_string),
                ]
                .into_iter()
                .flatten(),
            )
            .collect();
        let sent_count = steps.len() + 2;
        audit.push(equals(
            "every transaction in this directory is a distinct transaction",
            json!(sent_count),
            json!(unique_hashes.len()),
            unique_hashes.len() == sent_count,
        ));

        // ---- §56's rungs, and the published field that stands for each -----
        // `execution` is the plan's rung, `record` is the lifecycle's, `report` is the stage's. Each
        // rung is named with the file that publishes it, so a reader can go and look.
        let mut rungs: Vec<Value> = Vec::new();
        for (rung, where_it_ran, doc, fields) in [
            (
                "ArbitrageExecutionPlan",
                "real/giwa_execution.json's execution block",
                &execution,
                ["plan_hash", "calldata_hash", "route_id", "gas_policy"].to_vec(),
            ),
            (
                "TransactionIntent",
                "crates/execution/src/arbitrage.rs, walked in code by \
                 crates/execution/tests/executor_lifecycle.rs",
                &record,
                [
                    "sender",
                    "target",
                    "chain_id",
                    "nonce",
                    "gas_limit",
                    "value_wei",
                    "simulation_id",
                    "risk_decision_id",
                    "opportunity_id",
                ]
                .to_vec(),
            ),
            (
                "Build",
                "real/giwa_execution.json's lifecycle record",
                &record,
                [
                    "transaction_type",
                    "max_fee_per_gas",
                    "max_priority_fee_per_gas",
                    "estimated_execution_cost_wei",
                ]
                .to_vec(),
            ),
            (
                "Sign",
                "the live harness signed once; its only trace here is the hash the node accepted",
                &record,
                ["transaction_hash"].to_vec(),
            ),
            (
                "Submit",
                "real/giwa_execution.json's execution.report",
                &report,
                ["lane", "execution_id", "detail"].to_vec(),
            ),
            (
                "Receipt",
                "real/giwa_execution.json's lifecycle record",
                &record,
                [
                    "status",
                    "gas_used",
                    "effective_gas_price",
                    "execution_block",
                    "execution_block_hash",
                    "l1_fee",
                    "l2_fee",
                    "total_fee",
                ]
                .to_vec(),
            ),
            (
                "ExecutionRecord",
                "real/giwa_execution.json's lifecycle record",
                &record,
                [
                    "route_transactions",
                    "state_fingerprint",
                    "idempotency_key",
                    "blocked_reason",
                    "failure",
                ]
                .to_vec(),
            ),
        ] {
            rungs.push(json!({
                "rung": rung,
                "where_it_ran": where_it_ran,
                "published_fields": fields
                    .into_iter()
                    .map(|field| json!({
                        "field": field,
                        "value": field_text(doc, field),
                    }))
                    .collect::<Vec<_>>(),
            }));
        }

        // §24's reuse claim, made checkable: the record is the existing type's field list, and a
        // reader can see which of those fields this run filled and which it left null.
        let mut filled: Vec<String> = Vec::new();
        let mut unfilled: Vec<String> = Vec::new();
        if let Some(fields) = record.as_object() {
            for (key, value) in fields {
                if value.is_null() {
                    unfilled.push(key.clone());
                } else {
                    filled.push(key.clone());
                }
            }
        }
        let honest_nulls: Vec<String> = unfilled
            .iter()
            .filter(|key| {
                [
                    "realized_profit",
                    "gross_output",
                    "gross_profit",
                    "input_amount",
                    "input_asset",
                    "profit_status",
                    "simulation_profit_wei",
                ]
                .contains(&key.as_str())
            })
            .cloned()
            .collect();

        for entry in &audit {
            if entry["agree"] != Value::Bool(true) {
                self.note(
                    "lifecycle",
                    format!(
                        "{}: published {}, this gate recomputed {}",
                        entry["check"], entry["published"], entry["recomputed_by_this_gate"]
                    ),
                );
            }
        }
        let check_count = audit.len();
        let checks = audit_rows(&audit);

        let body = json!({
            "files_recomputed_from": [
                "real/giwa_execution.json",
                "real/giwa_failure.json",
                "real/giwa_ladder_steps.json",
                "real/preconditions.json",
                "contract/deployment.json",
            ],
            "how_to_read_this": "every check row carries the published value and the value this gate \
                                recomputed from other published fields, and `agree` is the reader's \
                                shorthand for the two being equal. Nothing here re-runs REVM or a \
                                node: §56's ladder ran on chain, and this file checks that the rows \
                                describing it add up.",
            "s56_ladder": rungs,
            "s56_note": "the chain ExecutablePlan → TransactionIntent → Build → Sign → Submit → \
                         Receipt → ExecutionRecord is walked with the real execution types by \
                         crates/execution/tests/executor_lifecycle.rs; the table above is the same \
                         seven rungs spelled out of the live run's own record, so the ladder is \
                         evidenced both in code and on chain.",
            "native_asset_ledger": {
                "identity_of_a_row": "the label the harness gave the step plus the nonce the chain \
                                      assigned it; rows are ordered by nonce, never by file order",
                "rows": ledger,
                "step_count": steps.len(),
                "begins_at_the_preconditions_read": wei_check(
                    "§31: the ladder's first balance read is the preconditions' balance read",
                    field_u256(&preconditions, "native_balance_wei").ok(),
                    steps.first().and_then(|row| field_u256(row, "balance_wei").ok()),
                ),
                "hands_off_to_the_execute_row": wei_check(
                    "§31: the last ladder step's balance, minus that step's own bill and value, is \
                     the balance the execute transaction was sent from",
                    field_u256(&wallet_before, "native_wei").ok(),
                    steps.last().and_then(|row| {
                        field_u256(row, "balance_wei")
                            .unwrap_or(U256::ZERO)
                            .checked_sub(
                                field_u256(row, "receipt_l2_cost_wei").unwrap_or(U256::ZERO)
                                    + field_u256(row, "receipt_l1_fee_wei").unwrap_or(U256::ZERO)
                                    + field_u256(row, "value_wei").unwrap_or(U256::ZERO),
                            )
                    }),
                ),
                "the_execute_row_and_the_failure_row_are_one_sequence": bool_check(
                    "§57/§58: the failed call was sent from the balance the successful one left \
                     behind",
                    None,
                    field_u256(&wallet_after, "native_wei").ok()
                        == field_u256(&failure_before, "native_wei").ok(),
                ),
            },
            "nonce_ladder": nonce_steps,
            "asset_movement_31": {
                "wallet_at_block_before_execute": wallet_before,
                "wallet_after": wallet_after,
                "principal_wei": real.get("principal_wei").cloned().unwrap_or(Value::Null),
                "gross_out": recon.get("output").and_then(|o| o.get("actual_gross_out_in_the_execute_block")).cloned().unwrap_or(Value::Null),
                "net_weth_delta": row31.get("balance_delta").cloned().unwrap_or(Value::Null),
                "note": "§31's row is the receipt's own numbers; the two wallet rows are reads at \
                        blocks, and §12/§14 is why the profit cell is null rather than a \
                        subtraction across denominations. The surplus that was already in the \
                        operator's WETH balance before this run is quoted by wallet_before_note.",
            },
            "reconciliation_32": gas_recon,
            "reconciliation_32_input": input_recon,
            "reconciliation_32_output": output_recon,
            "freshness_8": {
                "priced_at_block": window.get("priced_at_block").cloned().unwrap_or(Value::Null),
                "send_head_block": window.get("send_head_block").cloned().unwrap_or(Value::Null),
                "declared_max_block_age": window.get("declared_max_block_age").cloned().unwrap_or(Value::Null),
                "judged_by": "evm_execution::PlanValidity::freshness_at, the plan's own §8 function, \
                              given the block numbers the file publishes",
                "cases": freshness_table,
                "published_freshness": execution.get("freshness").cloned().unwrap_or(Value::Null),
            },
            "real_failure_58": {
                "step": step,
                "wallet_rows": [failure_before, failure_after],
                "residue_fields_recomputed": residue_rows,
                "no_partial_state_as_published": failure.get("no_partial_state").cloned().unwrap_or(Value::Null),
                "expected_as_published": failure.get("expected").cloned().unwrap_or(Value::Null),
                "what_this_is": failure.get("what_this_is").cloned().unwrap_or(Value::Null),
                "note": "the §27 claim is recomputed as a per-field comparison of the two wallet \
                        rows, with the native balance excluded because that is the one thing a \
                        reverted transaction legitimately spends.",
            },
            "record_field_inventory": {
                "fields_in_the_published_record": filled.len() + unfilled.len(),
                "filled": filled,
                "null": unfilled,
                "null_because_the_denominations_do_not_add": honest_nulls.len(),
                "note": "§24's reuse is this list: the live run wrote an ExecutionRecord with the \
                         fields the existing lifecycle type declares, through the existing stage, \
                         and did not define a parallel record for M10. §58/§59 is the reason some \
                         of those fields are null rather than zero.",
            },
            "audit_checks": checks,
            "audit": audit,
            "what_this_proves": "the rows the live run published are internally consistent: the \
                                 gas bill, the balance movements, the ids, the freshness bound and \
                                 the residue claim all recompute out of each other, and the two \
                                 real transactions sit in one nonce sequence with no gap.",
            "what_this_does_not_prove": "that the run was profitable. §59's verdict is NOT_PROVEN, \
                                         the profit cells above are null because the route settles \
                                         in WETH and the bill is paid in the native asset, and an \
                                         included receipt is not a realised gain (§52).",
        });

        self.write_json("execution/lifecycle.json", json!({ "body": body.clone() }));
        self.check(
            "lifecycle",
            json!({
                "file": "execution/lifecycle.json",
                "digest": self.digest_of("execution/lifecycle.json"),
                "checks": check_count,
                "checks_agreeing": checks["agreeing"],
                "ledger_rows": steps.len(),
                "nonce_ladder_rows": nonce_steps.len(),
                "record_fields": filled.len() + unfilled.len(),
            }),
        );
        body
    }
}

/// The `agree` column of the audit table, counted by the phase that reads it.
fn audit_rows(audit: &[Value]) -> Value {
    json!({
        "checks": audit.len(),
        "agreeing": audit.iter().filter(|e| e["agree"] == Value::Bool(true)).count(),
        "disagreeing": audit.iter().filter(|e| e["agree"] != Value::Bool(true)).map(|e| e["check"].clone()).collect::<Vec<_>>(),
    })
}

// @@CHUNK6C_END@@

// ---------------------------------------------------------------------------
// Reading a number another phase already published
// ---------------------------------------------------------------------------

/// Walk a JSON document by key path. The phases below quote counts that an earlier phase computed
/// and wrote into `Gate::summary`; re-deriving them here would mean two answers to one question,
/// and a gate that disagrees with itself is not a gate.
fn at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

/// A count read out of a published value, zero when the path is absent — so a phase that did not
/// run reads as a failed gate rather than as a `null` a reader could mistake for a pass.
fn count_at(value: &Value, path: &[&str]) -> u64 {
    at(value, path).and_then(Value::as_u64).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Phase 11 — §49's five gates in one artifact, plus this directory's byte identity
// ---------------------------------------------------------------------------

impl Gate {
    /// §49's D1–D5, answered in the one place a reader looks for them, each with the counts that
    /// decide it and the phase that produced them.
    ///
    /// This artifact deliberately does **not** carry the `unchanged_from_previous_run` column that
    /// `write_bytes` records. That value depends on what an *earlier* run left on disk, so a copy
    /// here would make two identical runs of this gate produce different bytes in the file whose
    /// subject is byte identity. It lives in `manifest.json`, which is written last and which
    /// nothing else digests.
    fn determinism(&mut self) -> Value {
        let plan = self
            .summary
            .get("plan_layer")
            .cloned()
            .unwrap_or(Value::Null);
        let d3 = self
            .summary
            .get("d3_replay")
            .cloned()
            .unwrap_or(Value::Null);
        let d4 = self
            .summary
            .get("d4_rejudge")
            .cloned()
            .unwrap_or(Value::Null);
        let row_inventory = self
            .summary
            .get("row_inventory")
            .cloned()
            .unwrap_or(Value::Null);

        let published_rows = count_at(&row_inventory, &["published_rows"]);
        let distinct_runs = count_at(&row_inventory, &["distinct_revm_runs"]);
        let in_directory = count_at(&plan, &["rows_in_the_directory"]);
        let rebuilt = count_at(&plan, &["rows_rebuilt"]);
        let d1_stable = count_at(&plan, &["d1_plan_hash_stable"]);
        let sens_fields = count_at(&plan, &["sensitivity", "fields_mutated"]);
        let sens_changed = count_at(&plan, &["sensitivity", "hashes_changed"]);
        let d2_bytes = count_at(&plan, &["d2_calldata_equal_to_published_bytes"]);
        let d2_digest = count_at(&plan, &["d2_calldata_hash_equal_to_published_digest"]);
        let d2_len = count_at(&plan, &["d2_calldata_len_agrees"]);
        let d5_stable = count_at(&plan, &["d5_route_id_stable"]);
        let d5_names = count_at(&plan, &["d5_identity_names_the_published_route"]);
        let replayed = count_at(&d3, &["runs_replayed"]);
        let identical = count_at(&d3, &["runs_byte_identical"]);
        let rejudged = count_at(&d4, &["rows_rejudged"]);
        let class_problems = count_at(&d4, &["rows_with_classification_problems"]);
        let judge_problems = count_at(&d4, &["rows_with_judgement_problems"]);
        // `u64::MAX` for an unreadable row on purpose: a real-chain failure this gate could not
        // decode is the one case where "I don't know" must not be allowed to read as "no problems".
        let real_failure_problems = at(&d4, &["real_chain_failure", "problems"])
            .and_then(Value::as_array)
            .map(|entries| entries.len() as u64)
            .unwrap_or(u64::MAX);

        let all_rows_rebuilt =
            rebuilt > 0 && rebuilt == in_directory && in_directory == published_rows;
        let gates = vec![
            json!({
                "gate": "D1",
                "task_book_question": "same plan → same plan hash",
                "answered_by": "plan_rebuild, over every published scenario row",
                "measurements": {
                    "published_rows_in_the_directory": published_rows,
                    "rows_rebuilt": rebuilt,
                    "rows_whose_field_spelling_and_byte_spelling_hash_the_same": d1_stable,
                    "sensitivity_fields_mutated": sens_fields,
                    "sensitivity_mutants_that_changed_the_plan_hash": sens_changed,
                },
                "pass": all_rows_rebuilt
                    && d1_stable == rebuilt
                    && sens_fields > 0
                    && sens_changed == sens_fields,
                "why_the_sensitivity_table_is_part_of_D1": "a hash that repeats for the same plan is \
                                                            only half of §6's rule; a hash that also \
                                                            repeats for a *different* plan would let \
                                                            one execution be re-rolled into another",
            }),
            json!({
                "gate": "D2",
                "task_book_question": "same plan → same calldata",
                "answered_by": "plan_rebuild — the execution crate's encoder against the bytes the \
                                simulation crate ran",
                "measurements": {
                    "rows_rebuilt": rebuilt,
                    "rows_whose_encoded_bytes_equal_the_published_bytes": d2_bytes,
                    "rows_whose_keccak_equals_the_published_digest": d2_digest,
                    "rows_whose_length_equals_the_published_length": d2_len,
                },
                "pass": all_rows_rebuilt && d2_bytes == rebuilt && d2_digest == rebuilt && d2_len == rebuilt,
                "what_a_failure_would_mean": "§47's ban in its only testable form: a plan whose Rust \
                                              encoding is not the bytes REVM executed describes a \
                                              transaction nobody could send, and every fixture below \
                                              it would be describing somebody else's call",
            }),
            json!({
                "gate": "D3",
                "task_book_question": "same fixture → same simulation result",
                "answered_by": "d3_replay — a different crate, a different cargo test invocation, \
                                the published recipe and the published spec",
                "measurements": {
                    "published_rows_in_the_directory": published_rows,
                    "distinct_revm_runs": distinct_runs,
                    "runs_replayed": replayed,
                    "runs_whose_whole_observed_block_is_json_identical": identical,
                },
                "pass": replayed > 0 && identical == replayed && replayed == distinct_runs,
                "note": "one run per (recipe digest, calldata digest), not one per row: two rows \
                         that ran the same call against the same state are one REVM run, and \
                         counting them twice would inflate the census",
            }),
            json!({
                "gate": "D4",
                "task_book_question": "same failure fixture → same revert classification",
                "answered_by": "d4_rejudge — decode_revert over the published revert bytes, plus §27's \
                                residue judged out of the published state rows",
                "measurements": {
                    "rows_rejudged": rejudged,
                    "rows_with_classification_problems": class_problems,
                    "rows_with_judgement_problems": judge_problems,
                    "real_chain_failure_problems": real_failure_problems,
                },
                "pass": rejudged > 0 && rejudged == published_rows && class_problems == 0
                    && judge_problems == 0 && real_failure_problems == 0,
            }),
            json!({
                "gate": "D5",
                "task_book_question": "same route → same route id",
                "answered_by": "plan_rebuild, and the identity table the real run published",
                "measurements": {
                    "rows_rebuilt": rebuilt,
                    "rows_whose_two_spellings_id_the_route_the_same": d5_stable,
                    "rows_where_the_id_names_the_published_pools_and_tokens": d5_names,
                    "the_real_runs_route_id_parts_match": at(&plan, &[
                        "real_run",
                        "d5_route_id_parts_match_the_published_route_facts",
                    ]) == Some(&Value::Bool(true)),
                },
                "pass": all_rows_rebuilt && d5_stable == rebuilt && d5_names == rebuilt
                    && at(&plan, &["real_run", "d5_route_id_parts_match_the_published_route_facts"])
                        == Some(&Value::Bool(true)),
                "why_the_second_column_is_the_load_bearing_one": "a stable id that names the wrong \
                                                                  pools is a stable id for nothing; §38 \
                                                                  asks the identity to *name the route*",
            }),
        ];
        let gates_passing = gates
            .iter()
            .filter(|g| g["pass"] == Value::Bool(true))
            .count();

        // §50's ten controls, counted from the table rather than from a number typed into prose.
        let expected_plan = CONTROLS
            .iter()
            .filter(|control| control.2 == "plan")
            .count();
        let expected_contract = CONTROLS
            .iter()
            .filter(|control| control.2 == "contract")
            .count();
        let planted_plan = count_at(&plan, &["plan_layer_controls", "planted_count"]);
        let contract_rows: Vec<Value> = at(&plan, &["contract_layer_controls", "controls"])
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let contract_named = contract_rows
            .iter()
            .filter(|entry| entry["every_row_names_the_expected_refusal"] == Value::Bool(true))
            .count();
        let controls: Vec<Value> = CONTROLS
            .iter()
            .map(|(name, rel, layer, expected)| {
                json!({
                    "control": name,
                    "file": rel,
                    "refused_at": layer,
                    "expected_refusal": expected,
                    "file_exists": self.evidence.join(rel).is_file(),
                    "answered_by": if *layer == "plan" {
                        "plan_layer.plan_layer_controls: this gate plants the mutation on the row \
                         named there and quotes the execution crate's refusal, because the refusal \
                         happens before any byte exists"
                    } else {
                        "plan_layer.contract_layer_controls for the label, and d4_rejudge for the \
                         classification: REVM ran the deployed bytecode and the contract answered"
                    },
                })
            })
            .collect();

        // Byte identity. The inputs are digested from disk; the artifacts are digested from the
        // bytes this run wrote, so a failed write cannot be reported as a digest of stale content.
        let mut inputs: Vec<Value> = Vec::new();
        for rel in PUBLISHED.iter().chain(PUBLISHED_RECIPES.iter()) {
            let read = file_digest(&self.root, &format!("{EVIDENCE_REL}/{rel}"));
            match read {
                Ok(digest) => inputs.push(json!({ "file": rel, "digest": digest })),
                Err(e) => self.note("determinism", e),
            }
        }
        let artifacts: Vec<Value> = WRITTEN
            .iter()
            .filter_map(|rel| {
                self.digest_of(rel)
                    .map(|digest| json!({ "file": rel, "digest": digest }))
            })
            .collect();

        let phases = [
            "inventory",
            "row_inventory",
            "live_run",
            "state_integrity",
            "d3_replay",
            "d4_rejudge",
            "plan_layer",
            "contract_abi",
            "runtime_bytecode",
            "lifecycle",
        ];
        let ledger: Vec<Value> = phases
            .iter()
            .map(|phase| {
                json!({
                    "phase": phase,
                    "ran": self.summary.contains_key(*phase),
                    "published_keys": self
                        .summary
                        .get(*phase)
                        .and_then(Value::as_object)
                        .map(|map| map.keys().cloned().collect::<Vec<_>>())
                        .unwrap_or_default(),
                })
            })
            .collect();

        let drift: Vec<String> = self.drift.clone();
        let body = json!({
            "gates": gates,
            "gates_answered": gates.len(),
            "gates_passing": gates_passing,
            "negative_controls_50": {
                "controls": controls,
                "expected_plan_layer": expected_plan,
                "planned_by_this_gate": planted_plan,
                "expected_contract_layer": expected_contract,
                "contract_layer_rows_naming_their_refusal": contract_named,
                "all_ten_answered": planted_plan as usize == expected_plan
                    && contract_named == expected_contract
                    && controls.iter().all(|entry| entry["file_exists"] == Value::Bool(true)),
            },
            "byte_identity": {
                "inputs": inputs,
                "artifacts_written_before_this_file": artifacts,
                "not_digestable_here": [
                    "recompute/deterministic.json",
                    "manifest.json",
                    "README.md",
                ],
                "why": "this file cannot digest itself, and the README and the manifest are written \
                        after it; both are digested by manifest.json, which is the last file this \
                        run writes and which nothing else digests. The run-dependent column \
                        unchanged_from_previous_run lives there for the same reason.",
            },
            "phase_ledger": ledger,
            "rpc_reads": "none. This file builds no adapter and names no endpoint; every value in \
                          the five tables above was read out of this directory, which is §51's \
                          answer given by construction rather than by measurement.",
            "drift": drift.clone(),
            "drift_count": drift.len(),
            "verdict": {
                "directory_recomputes": gates_passing == gates.len() && drift.is_empty(),
                "reading": "true only when all five §49 gates pass their own counts and no published \
                            claim disagreed with this run's answer anywhere in any phase",
            },
        });

        self.write_json(
            "recompute/deterministic.json",
            json!({ "body": body.clone() }),
        );
        self.check(
            "determinism",
            json!({
                "file": "recompute/deterministic.json",
                "digest": self.digest_of("recompute/deterministic.json"),
                "gates_answered": gates.len(),
                "gates_passing": gates_passing,
                "drift_count": drift.len(),
            }),
        );
        body
    }
}

// ---------------------------------------------------------------------------
// Phase 12 — README.md: how to re-run the directory, and what may not be inferred from it
// ---------------------------------------------------------------------------

impl Gate {
    /// §60's `README.md`. Every number in it is interpolated from this run's own tables, because
    /// prose that quotes a count is prose that goes stale silently the moment a row is added.
    fn readme(&mut self) {
        let plan = self
            .summary
            .get("plan_layer")
            .cloned()
            .unwrap_or(Value::Null);
        let d3 = self
            .summary
            .get("d3_replay")
            .cloned()
            .unwrap_or(Value::Null);
        let d4 = self
            .summary
            .get("d4_rejudge")
            .cloned()
            .unwrap_or(Value::Null);
        let rows = self
            .summary
            .get("row_inventory")
            .cloned()
            .unwrap_or(Value::Null);
        let state = self
            .summary
            .get("state_integrity")
            .cloned()
            .unwrap_or(Value::Null);
        let abi = self
            .summary
            .get("contract_abi")
            .cloned()
            .unwrap_or(Value::Null);
        let runtime = self
            .summary
            .get("runtime_bytecode")
            .cloned()
            .unwrap_or(Value::Null);
        let lifecycle = self
            .summary
            .get("lifecycle")
            .cloned()
            .unwrap_or(Value::Null);

        // The tree, grouped by directory, from the two published lists and this gate's own.
        let mut tree: BTreeMap<String, Vec<&str>> = BTreeMap::new();
        for rel in PUBLISHED
            .iter()
            .chain(PUBLISHED_RECIPES.iter())
            .chain(WRITTEN.iter())
            .filter(|rel| !rel.ends_with(".md") && !rel.ends_with("manifest.json"))
        {
            let rel: &str = rel;
            let directory = match rel.rsplit_once('/') {
                Some((parent, _)) => parent.to_string(),
                None => ".".to_string(),
            };
            tree.entry(directory).or_default().push(rel);
        }
        let mut listing = String::new();
        for (directory, files) in &tree {
            listing.push_str(&format!("{directory}/\n"));
            for rel in files {
                let leaf = rel.rsplit_once('/').map(|(_, name)| name).unwrap_or(rel);
                let owner = if WRITTEN.contains(rel) {
                    "gate"
                } else {
                    "assembler"
                };
                listing.push_str(&format!("  {leaf}    ({owner})\n"));
            }
        }

        let published_rows = count_at(&rows, &["published_rows"]);
        let distinct_runs = count_at(&rows, &["distinct_revm_runs"]);
        // Three facts a README would normally type by hand. All three are read here: a caption that
        // goes stale while the gate stays green is worse than no caption.
        let mut fixture_blocks: BTreeSet<u64> = BTreeSet::new();
        for row in &self.rows {
            if let Some(block) = row
                .run_spec()
                .get("priced_at_block")
                .and_then(Value::as_u64)
            {
                fixture_blocks.insert(block);
            }
        }
        let fixture_block = if fixture_blocks.len() == 1 {
            fixture_blocks.iter().next().copied().unwrap_or(0)
        } else {
            0
        };
        let text = format!(
            "# M10 — the Arbitrage Executor Contract evidence directory\n\n\
             Chain {chain}, fixture block {block}, one deployed executor contract, and one nonce\n\
             ladder that carries the deployment, the approvals and allowances, the funded wrap, the\n\
             execute call ({execute}) and a deliberate failure ({failure}) that reverted as\n\
             `{revert_label}`. {ladder_rows} ladder rows are recomputed in\n\
             `execution/lifecycle.json`. {published_rows} scenario rows, {distinct_runs} distinct\n\
             REVM runs behind them, plus the live run's own rows.\n\n\
             Two crates wrote this directory.\n\n\
             * `crates/simulation/tests/executor_evidence.rs` is the assembler: it owns the\n\
               fixtures, runs them in REVM over the real deployed bytecode, and publishes what it\n\
               observed. It also ran the live deployment and the two real transactions.\n\
             * `{gate_path}` is the independent recompute: it reads **only the JSON files in this\n\
               directory**, rebuilds every plan, re-encodes every call, replays every run, and\n\
               writes the eight files marked `(gate)` below. It imports no fixture builder, no\n\
               recipe constant and no address literal from the assembler.\n\n\
             A gate that reused the assembler's harness would be a receipt: the same code, the same\n\
             state, the same answer quoted back. That is why this directory is judged from a second\n\
             crate, compiled and started by a different `cargo test` invocation.\n\n\
             ## Re-running it\n\n\
             ```text\n{assemble}\n```\n\n\
             ```text\n{check}\n```\n\n\
             Both must run with `--test-threads=1`: the assembly scratch under `target/` is shared\n\
             across processes and parallel runs corrupt each other's files. Those two command lines\n\
             are the only supported way to rebuild this directory.\n\n\
             ## What is here\n\n\
             `(assembler)` = written by the harness that ran the fixtures and the live chain;\n\
             `(gate)` = written by the recompute, and the only files it may write.\n\n\
             ```text\n{listing}```\n\n\
             ## The five determinism gates (§49)\n\n\
             | gate | question | this run's answer |\n\
             |---|---|---|\n\
             | D1 | same plan → same plan hash | {d1} of {rebuilt} rows hash the same from their two\n\
             | | | spellings; {sens_changed} of {sens_fields} mutants change the hash |\n\
             | D2 | same plan → same calldata | {d2_bytes} rows encode to the published bytes,\n\
             | | | {d2_digest} to the published digest |\n\
             | D3 | same fixture → same simulation result | {identical} of {replayed} REVM runs are\n\
             | | | JSON-identical to the published `observed` block |\n\
             | D4 | same failure → same classification | {rejudged} rows re-judged,\n\
             | | | {class_problems} classification and {judge_problems} judgement problems |\n\
             | D5 | same route → same route id | {d5_stable} rows stable, {d5_names} name the\n\
             | | | pools and tokens the row publishes |\n\n\
             The per-row tables are in `recompute/deterministic.json`, `execution/lifecycle.json`,\n\
             and in the phase file each row names.\n\n\
             ## What the gate re-derived about the contract\n\n\
             * `{abi_surface}` function and event entries in `contract/abi.json`, tied to the\n\
               `{abi_rows}` rows that publish the execute selector, and `{abi_reverts}` of\n\
               `{abi_revert_total}` published revert-byte rows decoded back to the label the row\n\
               carries. This is §47's ban made testable: the ABI is read out of the Solidity\n\
               source, and the selectors it implies are compared with the bytes the runs executed.\n\
             * `{runtime_bytes}` runtime bytes and `{creation_bytes}` creation bytes hashed in\n\
               `contract/runtime_bytecode.json`; `{runtime_recipes}` of `{runtime_checked}` state\n\
               recipes put the compiler's runtime artifact at the executor address.\n\
             * `{lifecycle_checks}` recomputed checks in `execution/lifecycle.json`, `{lifecycle_agreeing}`\n\
               of them agreeing with the row they quote — the gas bill, the balance chain, the nonce\n\
               ladder, the §31 asset rows, the §32 reconciliation, §8's freshness bound judged by the\n\
               plan's own function, and §27's zero-residue claim.\n\
             * `{recipe_files}` state recipes verified row by row against the committed fixture in\n               `state_integrity`, so a replay below cannot be run on a state nobody attested.\n\n\
             ## What may **not** be inferred from this directory\n\n\
             * **Not a realised profit.** `real/giwa_execution.json`'s `realized_profit`,\n\
               `gross_profit`, `input_amount` and `profit_status` cells are `null`, not zero,\n\
               because the route settles in WETH while the gas bill is paid in the native asset —\n\
               the two sides do not add (§34). An included receipt with `status = 1` is not a\n\
               profitable arbitrage (§52), and §30's real profitable round trip is NOT_PROVEN (§59).\n\
             * **There is no `real/giwa_success.json`, and none is missing.** §60's last line says\n\
               not to create a fabricated one; the successful real transaction that exists is the\n\
               round trip whose profit was never proved, and it is published as\n\
               `real/giwa_execution.json`.\n\
             * **The fixtures are not a market.** Every scenario row is a CONTROLLED_FIXTURE over a\n\
               recorded block plus declared words; `states/` names the recipe a row was built from.\n\
             * **A plan hash over a fixture row is this gate's declaration, not a published fact.**\n\
               No scenario row carries §5's plan-side fields, so `recompute/deterministic.json`\n\
               publishes them as null and labels the bound it declares.\n\
             * **Nothing here was broadcast by the gate.** No key is read, nothing is signed, no\n\
               node is contacted (§51).\n\n\
             ## Secrets, network, time\n\n\
             No private key and no endpoint address appears in any file in this directory. The live\n\
             run's `real/preconditions.json` names the environment variable it reads and nothing of\n\
             its value. No wall-clock value appears in the eight `(gate)` files, so rebuilding this\n\
             directory is byte-for-byte equal forever; the assembler's lifecycle record does carry\n\
             its own stage timestamps, which is why byte identity is stated per file rather than for\n\
             the directory as a whole.\n\n\
             ## §50's planted negative controls\n\n\
             {controls_each} controls, each bound to a file and to the refusal that file must show:\n\
             {controls_list}\n\n\
             {plan_expected} of them are planted at the plan layer and written by this gate; the\n\
             {contract_expected} contract-layer ones are refused by the deployed bytecode running in\n\
             REVM.\n\n\
             ## Judgment\n\n\
             This directory stands or falls on `recompute/deterministic.json`'s `verdict` and on the\n\
             single assertion at the end of the gate: a published claim that disagrees with this\n\
             run's recomputation is collected as drift, written into that file, and named there —\n\
             never panicked past, never averaged away.\n",
            execute = path_str(&self.real, &["execution", "report", "lifecycle", "transaction_hash"])
                .unwrap_or("no transaction hash was published"),
            failure = path_str(&self.failure, &["step", "transaction_hash"])
                .unwrap_or("no transaction hash was published"),
            chain = at(&self.deployment, &["chain_id"])
                .cloned()
                .unwrap_or(Value::Null),
            block = fixture_block,
            revert_label = path_str(&self.failure, &["simulation_attribution", "contract_error"])
                .unwrap_or("no contract error was published"),
            ladder_rows = count_at(&lifecycle, &["nonce_ladder_rows"]),
            gate_path = ASSEMBLED_BY,
            assemble = ASSEMBLE_COMMAND,
            check = CHECK_COMMAND,
            listing = listing,
            published_rows = published_rows,
            distinct_runs = distinct_runs,
            d1 = count_at(&plan, &["d1_plan_hash_stable"]),
            rebuilt = count_at(&plan, &["rows_rebuilt"]),
            sens_changed = count_at(&plan, &["sensitivity", "hashes_changed"]),
            sens_fields = count_at(&plan, &["sensitivity", "fields_mutated"]),
            d2_bytes = count_at(&plan, &["d2_calldata_equal_to_published_bytes"]),
            d2_digest = count_at(&plan, &["d2_calldata_hash_equal_to_published_digest"]),
            identical = count_at(&d3, &["runs_byte_identical"]),
            replayed = count_at(&d3, &["runs_replayed"]),
            rejudged = count_at(&d4, &["rows_rejudged"]),
            class_problems = count_at(&d4, &["rows_with_classification_problems"]),
            judge_problems = count_at(&d4, &["rows_with_judgement_problems"]),
            d5_stable = count_at(&plan, &["d5_route_id_stable"]),
            d5_names = count_at(&plan, &["d5_identity_names_the_published_route"]),
            abi_surface = count_at(&abi, &["surface_count"]),
            abi_rows = count_at(&abi, &["rows_publishing_the_execute_selector"]),
            abi_reverts = count_at(&abi, &["revert_bytes_rows_matched"]),
            abi_revert_total = count_at(&abi, &["revert_bytes_rows_total"]),
            runtime_bytes = count_at(&runtime, &["runtime_bytes"]),
            creation_bytes = count_at(&runtime, &["creation_bytes"]),
            runtime_recipes = count_at(&runtime, &["recipes_running_the_compilers_runtime"]),
            runtime_checked = count_at(&runtime, &["recipes_checked"]),
            lifecycle_checks = count_at(&lifecycle, &["checks"]),
            lifecycle_agreeing = count_at(&lifecycle, &["checks_agreeing"]),
            recipe_files = at(&state, &["recipes"])
                .and_then(Value::as_array)
                .map(|entries| entries.len())
                .unwrap_or(0),
            controls_each = CONTROLS.iter().count(),
            plan_expected = CONTROLS.iter().filter(|control| control.2 == "plan").count(),
            contract_expected = CONTROLS.iter().filter(|control| control.2 == "contract").count(),
            controls_list = CONTROLS
                .iter()
                .map(|(name, rel, layer, expected)| {
                    format!("  * `{name}` — `{rel}`, refused at the {layer} layer as `{expected}`")
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
        self.write_text("README.md", &text);
        self.check(
            "readme",
            json!({
                "file": "README.md",
                "digest": self.digest_of("README.md"),
                "lines": text.lines().count(),
                "numbers_are_interpolated": true,
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// Phase 13 — §61's manifest, written last so it can digest everything else
// ---------------------------------------------------------------------------

impl Gate {
    /// §61's eleven fields. Each one is quoted from a published file or recomputed from the file it
    /// names, and a field with no published source is `null` — never `0`, never a plausible value.
    fn manifest(&mut self) -> Value {
        let git = read_git_commit(&self.root);
        let record = at(&self.real, &["execution", "report", "lifecycle"])
            .cloned()
            .unwrap_or(Value::Null);
        let runtime = self
            .summary
            .get("runtime_bytecode")
            .cloned()
            .unwrap_or(Value::Null);
        let lifecycle = self
            .summary
            .get("lifecycle")
            .cloned()
            .unwrap_or(Value::Null);

        // §61's `test_fixture_hash`, recomputed rather than quoted: the rows all name one committed
        // fixture, and the file is on disk, so the digest is derived here and compared with the
        // claim. Two different files named by the rows would make the field ambiguous, and an
        // ambiguous field is null.
        let rows = self.rows.clone();
        let mut fixture_files: BTreeSet<String> = BTreeSet::new();
        let mut fixture_claims: BTreeSet<String> = BTreeSet::new();
        for row in &rows {
            if let Some(file) = path_str(row.state(), &["committed_fixture", "file"]) {
                fixture_files.insert(file.to_string());
            }
            if let Some(hash) = path_str(row.state(), &["committed_fixture", "keccak256"]) {
                fixture_claims.insert(hash.to_string());
            }
        }
        let fixture_file = if fixture_files.len() == 1 {
            fixture_files.iter().next().cloned()
        } else {
            None
        };
        let fixture_claim = if fixture_claims.len() == 1 {
            fixture_claims.iter().next().cloned()
        } else {
            None
        };
        let recomputed_fixture = fixture_file
            .as_deref()
            .and_then(|file| file_digest(&self.root, file).ok());
        let fixture_agrees = match (&recomputed_fixture, &fixture_claim) {
            (Some(digest), Some(claim)) => digest["keccak256"].as_str() == Some(claim.as_str()),
            _ => false,
        };
        if !fixture_agrees {
            self.note(
                "manifest",
                format!(
                    "the committed fixture's own digest does not match the claim the rows publish: \
                     recomputed {recomputed_fixture:?} against {fixture_claim:?} over \
                     {fixture_file:?}"
                ),
            );
        }

        // §61 asks for one `contract_hash` and there are two true hashes of one contract: the code
        // the deployment transaction carried, and the runtime code that code returned. The field
        // carries the former — it is the hash a reader can recompute from the transaction alone —
        // and both are published beside it so nothing is hidden by the choice.
        let creation_hash = at(&self.deployment, &["creation_code_hash"])
            .cloned()
            .unwrap_or(Value::Null);
        let fields: Vec<(&str, Value, String)> = vec![
            (
                "git_commit",
                git.get("git_commit").cloned().unwrap_or(Value::Null),
                "read from .git/HEAD, then the loose ref or packed-refs — never by running git"
                    .to_string(),
            ),
            (
                "chain_id",
                at(&self.deployment, &["chain_id"])
                    .cloned()
                    .unwrap_or(Value::Null),
                "contract/deployment.json, cross-read against contract/bytecode_hash.json in \
                 live_run.shared_fields and against the chain id in both real records"
                    .to_string(),
            ),
            (
                "executor_address",
                at(&self.deployment, &["contract_address"])
                    .cloned()
                    .unwrap_or(Value::Null),
                "contract/deployment.json; live_run re-derived it from keccak(rlp([sender, nonce])) and \
                 published whether the prediction matches the receipt".to_string(),
            ),
            (
                "deployment_tx",
                at(&self.deployment, &["deployment_tx"])
                    .cloned()
                    .unwrap_or(Value::Null),
                "contract/deployment.json, and it is the first row of real/giwa_ladder_steps.json"
                    .to_string(),
            ),
            (
                "deployment_block",
                at(&self.deployment, &["deployment_block"])
                    .cloned()
                    .unwrap_or(Value::Null),
                "contract/deployment.json".to_string(),
            ),
            (
                "contract_hash",
                creation_hash,
                "contract/deployment.json's creation_code_hash — the hash of the code the deployment \
                 transaction carried, recomputable from that transaction alone"
                    .to_string(),
            ),
            (
                "test_fixture_hash",
                fixture_claim
                    .clone()
                    .map(Value::from)
                    .unwrap_or(Value::Null),
                "keccak256 of the committed fixture file, derived by this gate over the bytes on disk \
                 and compared with the claim every scenario row publishes"
                    .to_string(),
            ),
            (
                "plan_hash",
                at(&self.real, &["execution", "plan_hash"])
                    .cloned()
                    .unwrap_or(Value::Null),
                "real/giwa_execution.json's own execution.plan_hash — quoted, not recomputed: the sent \
                 plan's per-leg floors are not published anywhere here, so a plan hash this gate built \
                 would be a guess about somebody else's route (see plan_layer.real_run.plan_identity)"
                    .to_string(),
            ),
            (
                "calldata_hash",
                at(&self.real, &["execution", "calldata_hash"])
                    .cloned()
                    .unwrap_or(Value::Null),
                "real/giwa_execution.json's execution.calldata_hash, whose length the ABI table in \
                 contract/abi.json independently explains"
                    .to_string(),
            ),
            (
                "simulation_hash",
                at(&record, &["simulation_id"])
                    .cloned()
                    .unwrap_or(Value::Null),
                "the simulation the sent transaction was bound to — the record's simulation_id, which is \
                 also the first half of its risk_decision_id".to_string(),
            ),
            (
                "real_tx_hash",
                at(&record, &["transaction_hash"])
                    .cloned()
                    .unwrap_or(Value::Null),
                "the execute transaction: included, with its receipt bound to a block. The deployment \
                 and the deliberate failure are in real_transaction_hashes below"
                    .to_string(),
            ),
        ];

        let mut manifest_body = Map::new();
        let mut provenance = Vec::new();
        let mut absent = Vec::new();
        for (name, value, source) in &fields {
            manifest_body.insert((*name).to_string(), value.clone());
            provenance
                .push(json!({ "field": name, "read_from": source, "is_null": value.is_null() }));
            if value.is_null() {
                absent.push(json!({
                    "field": name,
                    "value": "null",
                    "rule": "§61: a field that does not exist is null, never 0, never a fabricated value"
                }));
            }
        }

        let mut digests: Vec<Value> = Vec::new();
        for rel in PUBLISHED
            .iter()
            .chain(PUBLISHED_RECIPES.iter())
            .chain(WRITTEN.iter())
            .filter(|rel| !rel.ends_with("manifest.json"))
        {
            let read = file_digest(&self.root, &format!("{EVIDENCE_REL}/{rel}"));
            match read {
                Ok(digest) => digests.push(json!({ "file": rel, "digest": digest })),
                Err(e) => self.note("manifest", e),
            }
        }
        let digest_count = digests.len();

        manifest_body.insert(
            "field_provenance".to_string(),
            json!({
                "fields": provenance,
                "note": "the eleven fields above are the §61 list in the §61 order; this table says \
                         where each was read from and whether it is null",
            }),
        );
        manifest_body.insert("absent_fields".to_string(), json!(absent));
        manifest_body.insert(
            "contract_hash_readings".to_string(),
            json!({
                "creation_code_keccak256": at(&self.bytecode, &["creation_code_keccak256"]).cloned().unwrap_or(Value::Null),
                "runtime_code_keccak256": runtime.get("runtime_keccak256").cloned().unwrap_or(Value::Null),
                "runtime_bytes": runtime.get("runtime_bytes").cloned().unwrap_or(Value::Null),
                "creation_bytes": runtime.get("creation_bytes").cloned().unwrap_or(Value::Null),
                "creation_code_tail_holds_the_runtime_code": runtime.get("creation_tail_holds_runtime").cloned().unwrap_or(Value::Null),
                "this_manifest_carries": "creation_code_keccak256",
                "why": "a reader holding only the deployment transaction can recompute it; the \
                        runtime hash is what the deployed account returns today, and both are \
                        published so neither reading is hidden by the choice",
            }),
        );
        manifest_body.insert(
            "test_fixture_identity".to_string(),
            json!({
                "file": fixture_file,
                "recomputed_by_this_gate": recomputed_fixture,
                "claimed_by_every_scenario_row": fixture_claim,
                "rows_naming_it": rows.iter().filter(|row| {
                    row.state()
                        .get("committed_fixture")
                        .and_then(|fixture| fixture.get("file"))
                        .and_then(Value::as_str)
                        == fixture_file.as_deref()
                }).count(),
                "rows_in_the_directory": rows.len(),
                "recomputed_equals_claimed": fixture_agrees,
            }),
        );
        manifest_body.insert(
            "real_transaction_hashes".to_string(),
            json!({
                "deployment": at(&self.deployment, &["deployment_tx"]).cloned().unwrap_or(Value::Null),
                "execute_included": at(&record, &["transaction_hash"]).cloned().unwrap_or(Value::Null),
                "failure_reverted": at(&self.failure, &["step", "transaction_hash"]).cloned().unwrap_or(Value::Null),
                "nonce_ladder": at(&lifecycle, &["nonce_ladder_rows"]).cloned().unwrap_or(Value::Null),
                "note": "three transactions from one EOA in one nonce sequence: §57's controlled \
                         call, §58's controlled failure, and the deployment that made both possible. \
                         §61's real_tx_hash is the execute row because it is the only one that \
                         carried a route to completion.",
            }),
        );
        manifest_body.insert(
            "file_digests".to_string(),
            json!({
                "files": digests,
                "files_digest": digest_count,
                "note": "every file in this directory except this manifest, as a digest of the bytes \
                         this run found on disk or wrote. manifest.json is written last, so its own \
                         digest is nowhere — a file cannot stand in its own checksum table.",
            }),
        );
        manifest_body.insert(
            "byte_identity".to_string(),
            self.summary
                .get("byte_identity")
                .cloned()
                .unwrap_or(Value::Null),
        );
        let determinism = self
            .summary
            .get("determinism")
            .cloned()
            .unwrap_or(Value::Null);
        let gates_answered = count_at(&determinism, &["gates_answered"]);
        let gates_passing = count_at(&determinism, &["gates_passing"]);
        manifest_body.insert(
            "verdict".to_string(),
            json!({
                "directory_recomputes": self.drift.is_empty()
                    && gates_answered > 0
                    && gates_passing == gates_answered,
                "drift": self.drift.clone(),
                "drift_count": self.drift.len(),
                "determinism_digest": determinism.get("digest").cloned().unwrap_or(Value::Null),
                "reading": "true when all five §49 gates passed their own counts and nothing in any \
                            phase disagreed with a published claim",
            }),
        );
        manifest_body.insert(
            "not_claimed".to_string(),
            json!({
                "real_profitable_arbitrage": at(&self.real, &["verdicts", "real_profitable_arbitrage"])
                    .cloned()
                    .unwrap_or(Value::Null),
                "giwa_success_file": "absent by design — §60 forbids creating a fabricated one, and \
                                     §52 says an included receipt proves none of the four things a \
                                     success would need",
                "realized_profit": at(&record, &["realized_profit"]).cloned().unwrap_or(Value::Null),
                "profit_status": at(&record, &["profit_status"]).cloned().unwrap_or(Value::Null),
                "why_they_are_null": at(&record, &["blocked_reason"]).cloned().unwrap_or(Value::Null),
                "broadcast_by_this_gate": "none — no key was read, nothing was signed, no endpoint \
                                          was named (§51)",
            }),
        );
        manifest_body.insert(
            "git_commit_reading".to_string(),
            git.get("how_it_was_read").cloned().unwrap_or(Value::Null),
        );
        manifest_body.insert("rpc_reads".to_string(), json!(0usize));

        let body = Value::Object(manifest_body);
        self.write_json("manifest.json", body.clone());
        let digest = self.digest_of("manifest.json");
        self.check(
            "manifest",
            json!({
                "file": "manifest.json",
                "digest": digest,
                "s61_fields": fields.len(),
                "null_fields": absent.len(),
                "files_digesting": at(&body, &["file_digests", "files"])
                    .and_then(Value::as_array)
                    .map(|entries| entries.len()),
                "directory_recomputes": at(&body, &["verdict", "directory_recomputes"]),
            }),
        );
        body
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// One test, one pass over the directory: read, rebuild, replay, re-judge, recompute, write, and
/// finally assert that nothing disagreed.
///
/// Every phase records its own findings and keeps going, so a single run reports every defect in
/// the directory instead of stopping at whichever assertion happened to be written first. The one
/// assertion at the end is the only place a `false` is allowed to stop the run, and it names the
/// drift file rather than restating the numbers.
#[tokio::test]
async fn the_m10_evidence_directory_recomputes_itself() {
    let mut gate = Gate::new();

    let inventory = gate
        .inventory()
        .expect("§60's evidence directory can be listed");
    gate.check("inventory", inventory);
    gate.load_rows()
        .expect("the published scenario rows can be read");
    let _ = gate.read_run();
    let _ = gate.state_integrity();
    gate.d3_replay().await;
    gate.d4_rejudge();
    gate.plan_rebuild();
    let _ = gate.contract_abi();
    let _ = gate.runtime_bytecode();
    let _ = gate.lifecycle();
    let _ = gate.determinism();
    gate.readme();
    let _ = gate.manifest();

    let drift = gate.drift.clone();
    assert!(
        drift.is_empty(),
        "{} recomputed facts disagree with what {EVIDENCE_REL} publishes:\n{}\
         every line is also written into {EVIDENCE_REL}/recompute/deterministic.json's `drift` \
         table and {EVIDENCE_REL}/manifest.json's `verdict.drift`, with the phase and the row that \
         produced it.",
        drift.len(),
        drift
            .iter()
            .map(|entry| format!("  - {entry}\n"))
            .collect::<Vec<_>>()
            .join("")
    );
}

// @@CHUNK6D_END@@
