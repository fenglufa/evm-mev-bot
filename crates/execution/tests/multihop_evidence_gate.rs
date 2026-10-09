//! M11 §42–§44 — the independent recomputation of the multi-hop evidence directory.
//!
//! ```text
//! CC=clang CXX=clang++ CXXFLAGS="-include cstdint" \
//!   cargo test -p evm-execution --test multihop_evidence_gate -- --test-threads=1
//! ```
//!
//! # What this file is for
//!
//! [`multihop_evidence.rs`][../../simulation/tests/multihop_evidence.rs] assembled
//! `data/evidence/m11/`: it owns the fixtures, runs them in REVM, and writes down what it saw. A
//! gate that re-used that harness would be a receipt — the same code, the same state, the same
//! answer quoted back. So this file lives in a different crate, is compiled by a different
//! `cargo test` invocation, and reads **only the published JSON**. It imports no fixture builder,
//! no recipe constant, and no address literal from the simulation test.
//!
//! §43 names six things that "should be independently verifiable". Each is answered by a phase
//! that recomputes the figure from primitives rather than calling the code that published it:
//!
//! | §43 | figure | recomputed by |
//!|---|---|---|
//!| pricing | the quote | a constant-product fold written in this file (`fold`), over the reserves and fees the row itself publishes |
//!| optimizer | the grid, the ranking, the best point | the grid rebuilt from `domain` + `policy`, every point folded again, re-ranked by the rule in `better` |
//!| route identity | `RouteIdentity(…)` / `CanonicalKey(…)` | the minimum rotation of the trade-order edge list, re-spelled as a string here — `evm-graph` and `evm-pathfinder` are not dependencies of this crate, so the Debug form is rebuilt from published text |
//!| simulation summary | the whole `observed` block | a second REVM run, in this process, from the published dump path and the published calldata bytes |
//!| plan hash | `plan_hash` | `keccak256` over the published canonical text, plus a line-by-line rebuild of that text from the row's own `fields` |
//!| calldata hash | `calldata_hash` | `keccak256` over the published bytes, plus `decode_calldata` of those bytes against the published leg rows |
//!
//! §43 also forbids a gate that merely announces success: nothing here prints the word `PASS`.
//! Every phase prints the numbers it recomputed, beside the numbers it read, so a reader can see
//! what was compared rather than being told it matched.
//!
//! # Reads only
//!
//! This file writes **nothing**. It opens the evidence directory, the fixtures it names, and two
//! source files it counts as evidence about itself — and never opens any of them for writing. The
//! last phase proves it: all sixteen files are hashed before the first phase and again after the
//! last, and the two hash maps must be equal.
//!
//! # §44: no RPC
//!
//! No endpoint is read, no provider other than [`DumpStateProvider`] is constructed, and the
//! environment variable that would carry one is checked for absence in this process. The provider
//! type named by every row is re-measured here with `type_name` rather than quoted.
//!
//! # §41: what may not be inferred
//!
//! The controlled chains are measured. The real chain's question — §42's three questions — was
//! never asked by M11, so `real/*.json` says `UNKNOWN`, and this gate's job is to confirm that no
//! row quietly turned that into a zero or into a market verdict. A `CONTROLLED_FIXTURE` row that
//! claims a real-market profit, or a `real/` row whose verdict is anything but `UNKNOWN`, is a
//! drift line.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use alloy_primitives::{Address, Bytes, U256};
use serde_json::{json, Map, Value};

use evm_core::{BlockNumber, ChainId};
use evm_execution::LaneState;
use evm_protocol::{decode_calldata, ExecutorCall, ExecutorLeg};
use evm_simulation::executor::{run as run_executor, ExecutorOutcome, ExecutorRun};
use evm_simulation::gas::GasPricing;
use evm_simulation::request::EvmRules;
use evm_simulation::result::StepStatus;
use evm_simulation::state::{DumpStateProvider, StateDump, StateProvider};

const EVIDENCE_ROOT: &str = "data/evidence/m11";
const SCHEMA: &str = "m11-evidence-v1";
const MILESTONE: &str = "M11";
/// The test that assembled the directory. Quoted, never called: a gate that can regenerate its
/// own inputs cannot report a divergence in them.
const ASSEMBLED_BY: &str = "crates/simulation/tests/multihop_evidence.rs";
/// This file. Every row's `check_command` names it; the envelope must say so.
const CHECKED_BY: &str = "crates/execution/tests/multihop_evidence_gate.rs";

/// The fifteen files that carry a `rows` envelope plus the manifest, in the order the manifest's
/// own `tree` publishes them. `README.md` is text and is handled by the inventory phase.
const ROW_FILES: [&str; 14] = [
    "data/evidence/m11/pricing/recorded_2hop.json",
    "data/evidence/m11/pricing/declared_3hop.json",
    "data/evidence/m11/pricing/declared_4hop.json",
    "data/evidence/m11/optimizer/recorded_2hop.json",
    "data/evidence/m11/optimizer/declared_3hop.json",
    "data/evidence/m11/simulation/recorded_2hop.json",
    "data/evidence/m11/simulation/declared_3hop.json",
    "data/evidence/m11/risk/decision.json",
    "data/evidence/m11/lanes/lane_matrix.json",
    "data/evidence/m11/controlled/2hop/chain.json",
    "data/evidence/m11/controlled/3hop/chain.json",
    "data/evidence/m11/real/execution.json",
    "data/evidence/m11/real/failure.json",
    "data/evidence/m11/real/reconciliation.json",
];
const MANIFEST_FILE: &str = "data/evidence/m11/manifest.json";
const README_FILE: &str = "data/evidence/m11/README.md";

/// The writer's own command, quoted from the manifest so the inventory can check that every row
/// quotes the same one.
const ASSEMBLE_COMMAND: &str = "CC=clang CXX=clang++ CXXFLAGS=\"-include cstdint\" cargo test -p evm-simulation --test multihop_evidence -- --test-threads=1";
const CHECK_COMMAND: &str = "CC=clang CXX=clang++ CXXFLAGS=\"-include cstdint\" cargo test -p evm-execution --test multihop_evidence_gate -- --test-threads=1";

// ---------------------------------------------------------------------------
// Reading published fields
// ---------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn read_json(root: &Path, rel: &str) -> Result<Value, String> {
    let bytes = std::fs::read(root.join(rel)).map_err(|e| format!("reading {rel}: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parsing {rel}: {e}"))
}

fn read_text(root: &Path, rel: &str) -> Result<String, String> {
    std::fs::read_to_string(root.join(rel)).map_err(|e| format!("reading {rel}: {e}"))
}

/// `{bytes, keccak256}` in the writer's own spelling, so a published digest block can be compared
/// with a recomputed one as JSON values rather than field by field.
fn digest_of_file(root: &Path, rel: &str) -> Result<Value, String> {
    let bytes = std::fs::read(root.join(rel)).map_err(|e| format!("reading {rel}: {e}"))?;
    Ok(json!({
        "bytes": bytes.len(),
        "keccak256": format!("{:#x}", alloy_primitives::keccak256(&bytes)),
    }))
}

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

/// A fee figure. The directory spells these as decimal strings; a number is accepted too so this
/// gate does not invent a second reason for a run to refuse.
fn field_u128(v: &Value, key: &str) -> Result<u128, String> {
    let raw = v.get(key).ok_or_else(|| format!("field {key} is absent"))?;
    if let Some(text) = raw.as_str() {
        return text
            .parse::<u128>()
            .map_err(|_| format!("field {key} = {text:?} is not a decimal word"));
    }
    raw.as_u64()
        .map(u128::from)
        .ok_or_else(|| format!("field {key} is neither a decimal string nor a number"))
}

/// An amount that may be absent. `null` and a missing key are the same answer here, and neither is
/// a zero: §4 forbids a zero standing in for a run that delivered nothing.
fn optional_u256(v: &Value, key: &str) -> Result<Option<U256>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => Ok(Some(field_u256(v, key)?)),
    }
}

/// An amount. This directory spells every amount as a decimal string, and a number only where the
/// figure is a count or a block; accepting both spellings for an amount is what lets a row's
/// `input` and a run's `gas_used` be compared without a translation table.
fn field_u256(v: &Value, key: &str) -> Result<U256, String> {
    let raw = v.get(key).ok_or_else(|| format!("field {key} is absent"))?;
    if let Some(text) = raw.as_str() {
        if let Some(hex_body) = text.strip_prefix("0x") {
            return U256::from_str_radix(hex_body, 16)
                .map_err(|_| format!("field {key} = {text:?} is not an 0x-prefixed word"));
        }
        return U256::from_str_radix(text, 10)
            .map_err(|_| format!("field {key} = {text:?} is not a decimal word"));
    }
    raw.as_u64()
        .map(U256::from)
        .ok_or_else(|| format!("field {key} is neither a decimal string nor a number"))
}

fn field_addr(v: &Value, key: &str) -> Result<Address, String> {
    let text = field_str(v, key)?;
    // The directory spells every address lowercase (`hex::encode`), and this gate compares
    // published strings to each other, so parsing must not go through a checksum.
    addr_of(&text).ok_or_else(|| format!("field {key} = {text:?} is not a 20-byte address"))
}

/// The directory's own address spelling, so a value rebuilt here is byte-comparable with a value
/// read from a row.
fn addr_text(address: Address) -> String {
    format!("0x{}", hex::encode(address.as_slice()))
}

fn field_bytes(v: &Value, key: &str) -> Result<Bytes, String> {
    let text = field_str(v, key)?;
    let hex = text.strip_prefix("0x").unwrap_or(&text);
    hex::decode(hex)
        .map(Bytes::from)
        .map_err(|_| format!("field {key} = {text:?} is not hex bytes"))
}

fn dec(v: U256) -> String {
    v.to_string()
}

fn opt_dec(v: Option<U256>) -> Value {
    v.map(|v| Value::String(dec(v))).unwrap_or(Value::Null)
}

/// An `Option<U256>` in the words the canonical texts use: a missing amount is `none`, never a 0
/// standing in for a run that delivered nothing.
fn spell(amount: Option<U256>) -> String {
    amount.map(dec).unwrap_or_else(|| "none".to_string())
}

/// An address as the directory spells it: lowercase, `0x`-prefixed, never checksummed, so a value
/// parsed here re-renders byte-identically to the string it came from.
fn addr_of(text: &str) -> Option<Address> {
    let hex = text.strip_prefix("0x").unwrap_or(text);
    if hex.len() != 40 {
        return None;
    }
    let bytes = hex::decode(hex).ok()?;
    if bytes.len() != 20 {
        return None;
    }
    Some(Address::from_slice(&bytes))
}

/// The rows of an envelope, in a stable order. A row's key is its identity everywhere below; a
/// position in a JSON object is not.
fn rows_of(envelope: &Value, rel: &str) -> Result<BTreeMap<String, Value>, String> {
    let object = envelope
        .get("rows")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{rel} has no rows object"))?;
    let mut out = BTreeMap::new();
    for (key, value) in object {
        out.insert(key.clone(), value.clone());
    }
    Ok(out)
}

fn array(v: &Value, key: &str) -> Result<Vec<Value>, String> {
    v.get(key)
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| format!("field {key} is absent or is not an array"))
}

// ---------------------------------------------------------------------------
// §43: the pricing fold, written here rather than called from the pricing crate
// ---------------------------------------------------------------------------

/// One hop as the pricing row publishes it.
#[derive(Clone, Debug)]
struct Hop {
    pool: String,
    token_in: String,
    token_out: String,
    reserve_in: U256,
    reserve_out: U256,
    numerator: U256,
    denominator: U256,
}

impl Hop {
    fn from_published(row: &Value) -> Result<Hop, String> {
        let fee = row
            .get("fee")
            .ok_or_else(|| "a hop with no fee object is not a priceable hop".to_string())?;
        Ok(Hop {
            pool: field_str(row, "pool")?,
            token_in: field_str(row, "token_in")?,
            token_out: field_str(row, "token_out")?,
            reserve_in: field_u256(row, "reserve_in")?,
            reserve_out: field_u256(row, "reserve_out")?,
            numerator: field_u256(fee, "numerator")?,
            denominator: field_u256(fee, "denominator")?,
        })
    }
}

/// What the fold refuses to quote, named rather than folded into a zero.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Refusal {
    EmptySide,
    ZeroInput,
    FeeRatio,
    Overflow,
}

impl Refusal {
    fn text(&self) -> &'static str {
        match self {
            Refusal::EmptySide => "one side of the pool is empty",
            Refusal::ZeroInput => "a zero input buys nothing",
            Refusal::FeeRatio => "the fee ratio is not a fraction below one",
            Refusal::Overflow => "a product of the fold leaves 256 bits",
        }
    }
}

/// The constant-product output of one hop: the fee-retained input against the pooled side, floored.
///
/// Written as the three checked products the market makes rather than as a formula, because the
/// refusals are the point: a quote that silently wraps is worse than no quote.
fn fold_hop(input: U256, hop: &Hop) -> Result<U256, Refusal> {
    if hop.reserve_in.is_zero() || hop.reserve_out.is_zero() {
        return Err(Refusal::EmptySide);
    }
    if input.is_zero() {
        return Err(Refusal::ZeroInput);
    }
    if hop.denominator.is_zero() || hop.numerator > hop.denominator {
        return Err(Refusal::FeeRatio);
    }
    let retained = input.checked_mul(hop.numerator).ok_or(Refusal::Overflow)?;
    let base = hop
        .reserve_in
        .checked_mul(hop.denominator)
        .ok_or(Refusal::Overflow)?;
    let divisor = base.checked_add(retained).ok_or(Refusal::Overflow)?;
    let numerator = retained
        .checked_mul(hop.reserve_out)
        .ok_or(Refusal::Overflow)?;
    Ok(numerator / divisor)
}

/// The whole route. A hop that floors to zero is a market outcome, not an error: the walk
/// continues with zero and the remaining hops answer zero.
fn fold_route(hops: &[Hop], input: U256) -> Result<(U256, Vec<U256>), Refusal> {
    let mut current = input;
    let mut outputs = Vec::with_capacity(hops.len());
    for hop in hops {
        current = fold_hop(current, hop)?;
        outputs.push(current);
    }
    Ok((current, outputs))
}

/// `Gross` in the row's own two-word spelling: the state and the magnitude, never a signed number,
/// so a reader cannot mistake a loss for a negative gain.
fn gross_of(input: U256, output: U256) -> (String, U256) {
    if output > input {
        ("gain".to_string(), output - input)
    } else if output == input {
        ("even".to_string(), U256::ZERO)
    } else {
        ("loss".to_string(), input - output)
    }
}

/// Is `left` the better of two (input, output) points? The published rule is a three-part sort:
/// a gain beats an even result beats a loss; within a gain the larger amount wins; within a loss
/// the smaller one does; a tie falls to the smaller input.
fn better(left: (U256, U256), right: (U256, U256)) -> bool {
    fn rank(v: (U256, U256)) -> u8 {
        if v.1 > v.0 {
            0
        } else if v.1 == v.0 {
            1
        } else {
            2
        }
    }
    fn magnitude(v: (U256, U256)) -> U256 {
        if v.1 > v.0 {
            v.1 - v.0
        } else if v.1 < v.0 {
            v.0 - v.1
        } else {
            U256::ZERO
        }
    }
    match rank(left).cmp(&rank(right)) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => match magnitude(left).cmp(&magnitude(right)) {
            std::cmp::Ordering::Equal => left.0 < right.0,
            // Bigger is better only among gains; among losses it is worse.
            std::cmp::Ordering::Less => rank(left) == 2,
            std::cmp::Ordering::Greater => rank(left) == 0,
        },
    }
}

/// The best point of a set, by the rule above. `None` for an empty set — an empty set is what a
/// grid the fold refused entirely produces, and it must not be reported as a zero.
fn best_of(points: &[(U256, U256)]) -> Option<(U256, U256)> {
    let mut best: Option<(U256, U256)> = None;
    for point in points {
        let replace = match best {
            None => true,
            Some(current) => better(*point, current),
        };
        if replace {
            best = Some(*point);
        }
    }
    best
}

// ---------------------------------------------------------------------------
// §43: the route identity, rebuilt from published text
// ---------------------------------------------------------------------------

/// One directed edge, in the three lowercase hex strings every row uses. `evm-graph` is not a
/// dependency of this crate and stays that way: the identity is rebuilt from published fields, and
/// a rebuild that needed the graph crate would be the publisher quoting itself.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Edge {
    pool: String,
    token_in: String,
    token_out: String,
}

impl Edge {
    fn from_published(row: &Value) -> Result<Edge, String> {
        Ok(Edge {
            pool: field_str(row, "pool")?,
            token_in: field_str(row, "token_in")?,
            token_out: field_str(row, "token_out")?,
        })
    }
}

/// A cycle is the same route whichever position you start reading it at, so the identity is the
/// minimum rotation of the edge list. Reversal is deliberately *not* folded: a route that walks
/// a→b→c→a is not the route that walks a→c→b→a.
fn min_rotation(edges: &[Edge]) -> (Vec<Edge>, usize) {
    let mut best: Option<(Vec<Edge>, usize)> = None;
    for shift in 0..edges.len() {
        let rotated: Vec<Edge> = edges[shift..]
            .iter()
            .chain(edges[..shift].iter())
            .cloned()
            .collect();
        let takeover = match &best {
            None => true,
            Some((current, _)) => rotated.as_slice() < current.as_slice(),
        };
        if takeover {
            best = Some((rotated, shift));
        }
    }
    best.unwrap_or_else(|| (Vec::new(), 0))
}

/// `RouteIdentity([EdgeId { … }, …])` / `CanonicalKey([…])`, spelled the way the Debug derive of
/// those types prints it, from the published chain id and the published edge fields.
fn identity_string(prefix: &str, chain: u64, edges: &[Edge]) -> String {
    let inner = edges
        .iter()
        .map(|edge| {
            format!(
                "EdgeId {{ pool: PoolId {{ chain_id: ChainId({chain}), address: {} }}, \
                 token_in: TokenId {{ chain_id: ChainId({chain}), address: {} }}, \
                 token_out: TokenId {{ chain_id: ChainId({chain}), address: {} }} }}",
                edge.pool, edge.token_in, edge.token_out
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{prefix}([{inner}])")
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

struct Gate {
    root: PathBuf,
    /// Every way a published figure failed to survive recomputation. The test asserts this is
    /// empty and prints each line, so a failure names the figure rather than the phase.
    drift: Vec<String>,
    /// rel path -> parsed JSON, for the sixteen files.
    files: BTreeMap<String, Value>,
    /// rel path -> digest, taken before the first phase runs.
    hashes_before: BTreeMap<String, Value>,
    /// One entry per phase, in order, for the printed table.
    report: Vec<(String, Value)>,
}

impl Gate {
    fn new() -> Self {
        Gate {
            root: workspace_root(),
            drift: Vec::new(),
            files: BTreeMap::new(),
            hashes_before: BTreeMap::new(),
            report: Vec::new(),
        }
    }

    fn note(&mut self, phase: &str, detail: impl Into<String>) {
        self.drift.push(format!("[{phase}] {}", detail.into()));
    }

    /// Record a phase's recomputed figures, returning the value so a caller can keep using it.
    fn check(&mut self, phase: &str, value: Value) {
        self.report.push((phase.to_string(), value));
    }

    fn file(&self, rel: &str) -> Result<&Value, String> {
        self.files
            .get(rel)
            .ok_or_else(|| format!("{rel} was never loaded"))
    }

    /// The rows of one published file, addressed as `<file>#<row id>` in every drift line.
    fn rows(&self, rel: &str) -> Result<BTreeMap<String, Value>, String> {
        rows_of(self.file(rel)?, rel)
    }

    fn row_key(rel: &str, id: &str) -> String {
        format!("{}#{}", rel.trim_start_matches("data/evidence/m11/"), id)
    }

    /// Compare two published values and record the difference by name, not by assertion, so one
    /// bad field does not hide the other nine.
    fn agree(&mut self, phase: &str, what: &str, published: &Value, recomputed: Value) {
        if published != &recomputed {
            self.note(
                phase,
                format!(
                    "{what}: published {} but recomputed {}",
                    compact(published),
                    compact(&recomputed)
                ),
            );
        }
    }

    fn agree_bool(&mut self, phase: &str, what: &str, published: &Value, recomputed: bool) {
        self.agree(phase, what, published, Value::Bool(recomputed));
    }
}

/// A value short enough to read in a terminal. Drift lines quote both sides of a mismatch, so a
/// reader can see what was compared without opening the JSON. The cut walks back to a character
/// boundary: these rows carry `§` and CJK prose, and a gate that panics while formatting a
/// mismatch reports nothing about it.
fn compact(value: &Value) -> String {
    let text = value.to_string();
    if text.len() <= 180 {
        return text;
    }
    let mut end = 170;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} bytes)", &text[..end], text.len())
}

/// A published digest block `{bytes, keccak256}` as one comparable string.
fn digest_pair(value: &Value) -> String {
    format!(
        "{} bytes {}",
        value.get("bytes").and_then(Value::as_u64).unwrap_or(0),
        value
            .get("keccak256")
            .and_then(Value::as_str)
            .unwrap_or("absent")
    )
}

/// The route's hops, as the pricing row publishes them.
fn hop_list(row: &Value) -> Result<Vec<Hop>, String> {
    array(row, "hops")?
        .iter()
        .map(Hop::from_published)
        .collect()
}

fn edge_list(row: &Value, key: &str) -> Result<Vec<Edge>, String> {
    array(row, key)?.iter().map(Edge::from_published).collect()
}

#[cfg(test)]
mod gate_compact_boundary {
    /// `compact` must not panic on a multi-byte character at the cut.
    #[test]
    fn cuts_at_a_char_boundary() {
        let wide = "§".repeat(400);
        let value = serde_json::Value::String(wide);
        let text = super::compact(&value);
        assert!(text.chars().count() < 220);
    }
}

// ---------------------------------------------------------------------------
// Phase 1 — the directory itself
// ---------------------------------------------------------------------------

impl Gate {
    /// Open every file the manifest's `tree` names, hash it, and check the envelope each row
    /// carries. This is the phase that decides whether the other ten are reading what they think
    /// they are reading.
    fn inventory(&mut self) -> Result<(), String> {
        let manifest = read_json(&self.root, MANIFEST_FILE)?;
        let tree = array(&manifest, "tree")?
            .iter()
            .map(|v| v.as_str().map(str::to_string).unwrap_or_default())
            .collect::<Vec<_>>();
        if tree.len() != 16 {
            self.note(
                "inventory",
                format!("the tree lists {} paths, not 16", tree.len()),
            );
        }

        // The directory on disk, walked rather than trusted.
        let mut on_disk = BTreeSet::new();
        walk(&self.root.join(EVIDENCE_ROOT), &self.root, &mut on_disk)?;
        let declared: BTreeSet<String> = tree.iter().cloned().collect();
        for extra in on_disk.difference(&declared) {
            self.note(
                "inventory",
                format!("{extra} is on disk and not in the tree"),
            );
        }
        for missing in declared.difference(&on_disk) {
            self.note(
                "inventory",
                format!("{missing} is in the tree and not on disk"),
            );
        }

        for rel in &declared {
            let digest = digest_of_file(&self.root, rel)?;
            self.hashes_before.insert(rel.clone(), digest.clone());
            if rel.ends_with(".json") && rel != MANIFEST_FILE {
                self.files.insert(rel.clone(), read_json(&self.root, rel)?);
            } else if rel == MANIFEST_FILE {
                self.files.insert(rel.clone(), manifest.clone());
            }
        }

        // §42's envelope: the same nine words on every row file, spelled the same way.
        let mut envelope_problems = 0usize;
        let mut row_total = 0usize;
        for rel in ROW_FILES {
            let envelope = self.file(rel)?.clone();
            let expected = [
                ("schema", json!(SCHEMA)),
                ("milestone", json!(MILESTONE)),
                ("evidence_root", json!(EVIDENCE_ROOT)),
                ("file", json!(rel)),
                ("assembled_by", json!(ASSEMBLED_BY)),
                ("checked_by", json!(CHECKED_BY)),
                ("assemble_command", json!(ASSEMBLE_COMMAND)),
                ("check_command", json!(CHECK_COMMAND)),
            ];
            for (key, want) in expected {
                if envelope.get(key) != Some(&want) {
                    self.note(
                        "inventory",
                        format!(
                            "{rel}: {key} is {} and not {}",
                            compact(&envelope[key]),
                            compact(&want)
                        ),
                    );
                    envelope_problems += 1;
                }
            }
            // `directory` is the path between the evidence root and the file, which is the one
            // field a reader uses to see that a file was not moved after it was written.
            let parent = rel
                .trim_start_matches(&format!("{EVIDENCE_ROOT}/"))
                .rsplit_once('/')
                .map(|(head, _)| head.to_string())
                .unwrap_or_default();
            if envelope.get("directory") != Some(&json!(parent)) {
                self.note(
                    "inventory",
                    format!(
                        "{rel}: directory is {}, not {parent:?}",
                        compact(&envelope["directory"])
                    ),
                );
                envelope_problems += 1;
            }
            let rows = rows_of(&envelope, rel)?;
            row_total += rows.len();

            // The manifest's per-file record must describe the file that is actually here.
            let entry = manifest["file_digests"]
                .get(rel)
                .ok_or_else(|| format!("the manifest lists no digest for {rel}"))?;
            self.agree(
                "inventory",
                &format!("{rel} digest"),
                &entry["digest"],
                digest_of_file(&self.root, rel)?,
            );
            self.agree(
                "inventory",
                &format!("{rel} row count"),
                &entry["rows"],
                Value::from(rows.len()),
            );
            let published_keys = array(entry, "row_keys")?
                .iter()
                .map(|v| v.as_str().map(str::to_string).unwrap_or_default())
                .collect::<BTreeSet<_>>();
            self.agree(
                "inventory",
                &format!("{rel} row identities"),
                &json!(published_keys.iter().collect::<Vec<_>>()),
                json!(rows.keys().collect::<Vec<_>>()),
            );
        }

        // The manifest's own accounting, recomputed from the files rather than read back.
        // `total_bytes` is scoped to the files the manifest digests (its `total_bytes_scope`), so
        // the recomputation sums exactly the paths `file_digests` names — the manifest's own bytes
        // are excluded on both sides, and the whole-tree figure is published beside it so the
        // exclusion is visible rather than assumed.
        let digested = manifest["file_digests"]
            .as_object()
            .ok_or_else(|| "manifest file_digests is not an object".to_string())?;
        let mut bytes_total = 0u64;
        for rel in digested.keys() {
            let bytes = self.hashes_before[rel]["bytes"]
                .as_u64()
                .ok_or_else(|| format!("{rel} is digested but not in the tree"))?;
            bytes_total += bytes;
        }
        let mut tree_total = 0u64;
        for rel in &declared {
            tree_total += self.hashes_before[rel]["bytes"]
                .as_u64()
                .ok_or_else(|| format!("{rel} has no byte count"))?;
        }
        self.agree(
            "inventory",
            "manifest total_bytes",
            &manifest["total_bytes"],
            Value::from(bytes_total),
        );
        self.agree(
            "inventory",
            "manifest files_written",
            &manifest["files_written"],
            Value::from(declared.len()),
        );
        if digested.len() + 1 != declared.len() {
            self.note(
                "inventory",
                "file_digests must cover the files a manifest can describe, itself excluded",
            );
        }
        if !manifest["total_bytes_scope"]
            .as_str()
            .unwrap_or_default()
            .contains("digest")
        {
            self.note(
                "inventory",
                "manifest total_bytes_scope does not say which files the figure counts",
            );
        }
        for key in [
            "schema",
            "milestone",
            "evidence_root",
            "directory",
            "file",
            "assembled_by",
            "checked_by",
            "assemble_command",
            "check_command",
        ] {
            let want = match key {
                "schema" => json!(SCHEMA),
                "milestone" => json!(MILESTONE),
                "evidence_root" => json!(EVIDENCE_ROOT),
                "file" => json!(MANIFEST_FILE),
                "directory" => json!("."),
                "assembled_by" => json!(ASSEMBLED_BY),
                "checked_by" => json!(CHECKED_BY),
                "assemble_command" => json!(ASSEMBLE_COMMAND),
                _ => json!(CHECK_COMMAND),
            };
            if manifest[key] != want {
                self.note(
                    "inventory",
                    format!("manifest {key} is {}", compact(&manifest[key])),
                );
            }
        }

        // §41's verdict and §44's count, read here so a later phase cannot be the only witness.
        self.agree(
            "inventory",
            "manifest verdict",
            &manifest["verdict"],
            json!("UNKNOWN"),
        );
        self.agree(
            "inventory",
            "manifest rpc_count",
            &manifest["rpc_count"],
            Value::from(0u64),
        );

        // The README is the only non-JSON file; it has to mention the gate it describes.
        let readme = read_text(&self.root, README_FILE)?;
        for phrase in [CHECKED_BY, ASSEMBLED_BY, "writes nothing", EVIDENCE_ROOT] {
            if !readme.contains(phrase) {
                self.note("inventory", format!("README.md never says {phrase:?}"));
            }
        }

        self.check(
            "1 inventory",
            json!({
                "files_in_tree": declared.len(),
                "files_on_disk": on_disk.len(),
                "row_files": ROW_FILES.len(),
                "rows_across_the_directory": row_total,
                "envelope_field_problems": envelope_problems,
                "total_bytes_recomputed": bytes_total,
                "total_bytes_published": manifest["total_bytes"],
                "bytes_of_the_whole_tree": tree_total,
                "verdict": manifest["verdict"],
                "rpc_count": manifest["rpc_count"],
            }),
        );
        Ok(())
    }
}

/// Every regular file under `dir`, as a workspace-relative path.
fn walk(dir: &Path, root: &Path, out: &mut BTreeSet<String>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("listing {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("reading an entry of {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            walk(&path, root, out)?;
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|_| format!("{} is not under the workspace root", path.display()))?
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR_STR, "/");
        out.insert(rel);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Phase 2 — pricing: the fold, the identity, and the ceiling
// ---------------------------------------------------------------------------

/// What one pricing row produced, kept for the later phases that have to join on it.
struct PricingFacts {
    ceiling: U256,
    input: U256,
    output: U256,
    hop_outputs: Vec<U256>,
    market_kind: String,
}

impl Gate {
    fn pricing(&mut self) -> Result<BTreeMap<String, PricingFacts>, String> {
        let mut out = BTreeMap::new();
        let mut table = Vec::new();
        for rel in [
            "data/evidence/m11/pricing/recorded_2hop.json",
            "data/evidence/m11/pricing/declared_3hop.json",
            "data/evidence/m11/pricing/declared_4hop.json",
        ] {
            for (id, row) in self.rows(rel)? {
                let key = Gate::row_key(rel, &id);
                let where_ = key.as_str();
                let chain = field_u64(&row, "chain_id")?;
                let hops = hop_list(&row)?;
                if hops.is_empty() {
                    self.note(
                        "pricing",
                        format!("{where_}: a pricing row with no hops prices nothing"),
                    );
                    continue;
                }
                let published = &row["published"];
                let recomputed = &row["recomputed_independently"];
                let route = &row["route"];
                let quote = &published["quote"];

                // 1. the fold, in this file, over the reserves and fees this row publishes.
                let input = field_u256(published, "input")?;
                let folded = fold_route(&hops, input);
                let (fold_output, fold_hops) = match folded {
                    Ok(pair) => pair,
                    Err(refusal) => {
                        self.note(
                            "pricing",
                            format!("{where_}: the fold refuses with {:?}", refusal.text()),
                        );
                        (U256::ZERO, Vec::new())
                    }
                };
                let published_hops = array(quote, "hops")?;
                if published_hops.len() != hops.len() {
                    self.note(
                        "pricing",
                        format!(
                            "{where_}: {} hops priced, {} declared",
                            published_hops.len(),
                            hops.len()
                        ),
                    );
                }
                let hop_texts = fold_hops.iter().map(|v| dec(*v)).collect::<Vec<_>>();
                let published_hop_texts = published_hops
                    .iter()
                    .map(|h| h["amount_out"].as_str().unwrap_or_default().to_string())
                    .collect::<Vec<_>>();
                self.agree(
                    "pricing",
                    &format!("{where_} hop outputs"),
                    &json!(published_hop_texts),
                    json!(hop_texts),
                );
                self.agree(
                    "pricing",
                    &format!("{where_} fold output"),
                    &quote["output"],
                    json!(dec(fold_output)),
                );
                self.agree(
                    "pricing",
                    &format!("{where_} recomputed hop outputs"),
                    &recomputed["hop_outputs"],
                    json!(hop_texts),
                );
                self.agree(
                    "pricing",
                    &format!("{where_} recomputed fold output"),
                    &recomputed["output"],
                    json!(dec(fold_output)),
                );

                // 2. gross: the state and the magnitude, never a signed number.
                let (state, amount) = gross_of(input, fold_output);
                self.agree(
                    "pricing",
                    &format!("{where_} gross"),
                    &published["gross"],
                    json!({"amount": dec(amount), "state": state}),
                );

                // 3. the identity: minimum rotation of the trade-order edges, re-spelled.
                let trade: Vec<Edge> = hops
                    .iter()
                    .map(|h| Edge {
                        pool: h.pool.clone(),
                        token_in: h.token_in.clone(),
                        token_out: h.token_out.clone(),
                    })
                    .collect();
                let (canonical, shift) = min_rotation(&trade);
                let rebuilt = identity_string("RouteIdentity", chain, &canonical);
                self.agree(
                    "pricing",
                    &format!("{where_} identity string"),
                    &published["identity"],
                    json!(rebuilt),
                );
                self.agree(
                    "pricing",
                    &format!("{where_} identity edges"),
                    &published["identity_edges"],
                    json!(edge_values(&canonical)),
                );
                self.agree(
                    "pricing",
                    &format!("{where_} recomputed identity edges"),
                    &recomputed["identity_edges"],
                    json!(edge_values(&canonical)),
                );
                self.agree_bool(
                    "pricing",
                    &format!("{where_} agrees.identity_with_the_rotation_minimum"),
                    &row["agrees"]["identity_with_the_rotation_minimum"],
                    true,
                );
                self.agree_bool(
                    "pricing",
                    &format!("{where_} agrees.quote_with_the_fold"),
                    &row["agrees"]["quote_with_the_fold"],
                    true,
                );
                // The control that makes the rotation mean something: starting the same cycle one
                // edge later must land on the same identity, and — for a cycle longer than one hop
                // — must not already have been canonical.
                if trade.len() > 1 {
                    let rotated = [&trade[1..], &trade[..1]].concat();
                    let (canonical_again, _) = min_rotation(&rotated);
                    if canonical_again != canonical {
                        self.note("pricing", format!("{where_}: rotating the trade order then canonicalising produced a different identity"));
                    }
                    if rotated == canonical {
                        self.note("pricing", format!("{where_}: the trade order is already its own minimum rotation at shift 1, so the rotation control proved nothing"));
                    }
                }
                if shift != 0 {
                    self.note("pricing", format!("{where_}: the published row starts at rotation {shift}, so its trade order is not the canonical one — check the joins downstream"));
                }

                // 4. the route's own claims.
                let ceiling = hops
                    .last()
                    .and_then(|h| h.reserve_out.checked_sub(U256::ONE));
                let ceiling = match ceiling {
                    Some(value) if value >= U256::ONE => value,
                    _ => {
                        self.note(
                            "pricing",
                            format!("{where_}: the exit pool cannot state an input ceiling"),
                        );
                        U256::ZERO
                    }
                };
                self.agree(
                    "pricing",
                    &format!("{where_} input ceiling"),
                    &route["input_upper_bound"],
                    json!(dec(ceiling)),
                );
                self.agree(
                    "pricing",
                    &format!("{where_} pools"),
                    &route["pools"],
                    json!(hops.iter().map(|h| h.pool.clone()).collect::<Vec<_>>()),
                );
                let mut tokens = vec![hops[0].token_in.clone()];
                tokens.extend(hops.iter().map(|h| h.token_out.clone()));
                self.agree(
                    "pricing",
                    &format!("{where_} tokens"),
                    &route["tokens"],
                    json!(tokens),
                );
                self.agree(
                    "pricing",
                    &format!("{where_} hop count"),
                    &route["hop_count"],
                    Value::from(hops.len()),
                );
                self.agree(
                    "pricing",
                    &format!("{where_} price state"),
                    &published["price_state"],
                    json!("quoted"),
                );

                // 5. §41's disclosure has to survive in the row that needs it.
                let kind = field_str(&row["market_claim"], "kind").unwrap_or_default();
                if hops.len() == 4 && !row["what_this_is"].is_string() {
                    self.note(
                        "pricing",
                        format!("{where_}: a four-hop row must say in words that it was never run"),
                    );
                }
                if hops.len() == 4
                    && !row["what_this_is"]
                        .as_str()
                        .unwrap_or("")
                        .contains("never run through the EVM")
                {
                    self.note(
                        "pricing",
                        format!("{where_}: the four-hop disclosure disappeared"),
                    );
                }

                out.insert(
                    rebuilt.clone(),
                    PricingFacts {
                        ceiling,
                        input,
                        output: fold_output,
                        hop_outputs: fold_hops,
                        market_kind: kind.clone(),
                    },
                );
                table.push(json!({
                    "row": where_,
                    "hops": hops.len(),
                    "chain_id": chain,
                    "input": dec(input),
                    "output_recomputed": dec(fold_output),
                    "output_published": quote["output"],
                    "gross_recomputed": format!("{state}:{amount}"),
                    "ceiling_recomputed": dec(ceiling),
                    "ceiling_published": route["input_upper_bound"],
                    "rotation_shift": shift,
                    "market_claim_kind": kind,
                }));
            }
        }
        self.check("2 pricing", json!({ "rows": table }));
        Ok(out)
    }
}

/// An edge list in the object form every row publishes it.
fn edge_values(edges: &[Edge]) -> Vec<Value> {
    edges
        .iter()
        .map(|e| json!({"pool": e.pool, "token_in": e.token_in, "token_out": e.token_out}))
        .collect()
}

// ---------------------------------------------------------------------------
// Phase 3 — the optimizer: the grid, the re-ranking, and what the grid covered
// ---------------------------------------------------------------------------

/// The grid the published domain and policy describe, rebuilt point by point.
struct Grid {
    points: Vec<U256>,
    coarse_points: u64,
    strategy: &'static str,
    termination: &'static str,
    anchor: Option<U256>,
}

/// `ceil(width / coarse_points)`, spelled so that the last coarse point lands on or before the
/// far end of the domain however wide it is.
fn ceil_div(width: U256, parts: u64) -> Option<U256> {
    let parts = U256::from(parts);
    if parts.is_zero() {
        return None;
    }
    width
        .checked_add(parts)?
        .checked_sub(U256::ONE)?
        .checked_div(parts)
}

impl Gate {
    /// The grid a row's `domain` + `policy` imply, walked in the order the search walked it: the
    /// coarse lattice first, then the refinement window in full — including the anchor, which the
    /// search priced twice and the published curve therefore lists twice.
    fn rebuild_grid(
        hops: &[Hop],
        domain_min: U256,
        domain_max: U256,
        policy: &Value,
    ) -> Result<Grid, String> {
        let coarse_points = field_u64(policy, "coarse_points")?;
        let exhaustive_limit = field_u64(policy, "exhaustive_limit")?;
        let refine_span = field_u64(policy, "refine_span")?;
        let width = domain_max
            .checked_sub(domain_min)
            .and_then(|span| span.checked_add(U256::ONE))
            .ok_or_else(|| "the domain width leaves 256 bits".to_string())?;

        let mut priced: Vec<(U256, U256)> = Vec::new();
        let consider = |input: U256| {
            fold_route(hops, input)
                .ok()
                .map(|(output, _)| (input, output))
        };

        let walkable = u64::try_from(width)
            .ok()
            .filter(|count| *count <= exhaustive_limit);
        let mut grid = Grid {
            points: Vec::new(),
            coarse_points: 0,
            strategy: "exhaustive",
            termination: "domainexhausted",
            anchor: None,
        };
        match walkable {
            Some(count) => {
                for step in 0..count {
                    let input = domain_min
                        .checked_add(U256::from(step))
                        .ok_or_else(|| "a grid point leaves 256 bits".to_string())?;
                    grid.points.push(input);
                    if let Some(point) = consider(input) {
                        priced.push(point);
                    }
                }
            }
            None => {
                let step = ceil_div(width, coarse_points).ok_or_else(|| {
                    "the coarse step is not computable from this policy".to_string()
                })?;
                let mut input = domain_min;
                let mut sampled = 0u64;
                while sampled < coarse_points && input <= domain_max {
                    grid.points.push(input);
                    if let Some(point) = consider(input) {
                        priced.push(point);
                    }
                    sampled += 1;
                    grid.coarse_points = sampled;
                    match input.checked_add(step) {
                        Some(next) => input = next,
                        None => break,
                    }
                }
                if let Some(anchor) = best_of(&priced) {
                    grid.anchor = Some(anchor.0);
                    let span = U256::from(refine_span);
                    let window_low = anchor.0.saturating_sub(span).max(domain_min);
                    let window_high = anchor.0.saturating_add(span).min(domain_max);
                    let window_width = window_high
                        .checked_sub(window_low)
                        .and_then(|delta| delta.checked_add(U256::ONE))
                        .ok_or_else(|| "the refinement window is not measurable".to_string())?;
                    let steps = u64::try_from(window_width)
                        .map_err(|_| "the refinement window does not fit a counter".to_string())?;
                    for step in 0..steps {
                        let input = window_low
                            .checked_add(U256::from(step))
                            .ok_or_else(|| "a refinement point leaves 256 bits".to_string())?;
                        grid.points.push(input);
                        if let Some(point) = consider(input) {
                            priced.push(point);
                        }
                    }
                }
                grid.strategy = "coarsethenrefine";
                grid.termination = "windowrefined";
            }
        }
        Ok(grid)
    }

    /// Every optimizer row: rebuild the grid, fold every point again, re-rank, and compare the
    /// whole curve element by element. A curve that agrees only at its endpoints is not a curve.
    fn optimizer(&mut self, pricing: &BTreeMap<String, PricingFacts>) -> Result<(), String> {
        let mut table = Vec::new();
        for rel in [
            "data/evidence/m11/optimizer/recorded_2hop.json",
            "data/evidence/m11/optimizer/declared_3hop.json",
        ] {
            for (id, row) in self.rows(rel)? {
                let key = Gate::row_key(rel, &id);
                let where_ = key.as_str();
                let hops = hop_list(&row)?;
                let published = &row["published"];
                let recomputed = &row["recomputed_independently"];

                let search_min = field_u256(published, "search_min")?;
                let search_max = field_u256(published, "search_max")?;
                let policy = &published["policy"];

                // The domain the search was allowed to ask about: the requested window, clipped by
                // the route's own ceiling, with a zero lower bound replaced by one.
                let facts = pricing.get(&field_str(&row, "route_identity")?);
                let ceiling = match facts {
                    Some(facts) => facts.ceiling,
                    None => {
                        self.note(
                            "optimizer",
                            format!("{where_}: its route identity matches no pricing row, so its ceiling is unknown"),
                        );
                        continue;
                    }
                };
                let low = if search_min.is_zero() {
                    U256::ONE
                } else {
                    search_min
                };
                let high = search_max.min(ceiling);
                self.agree(
                    "optimizer",
                    &format!("{where_} domain min"),
                    &published["domain_min"],
                    json!(dec(low)),
                );
                self.agree(
                    "optimizer",
                    &format!("{where_} domain max"),
                    &published["domain_max"],
                    json!(dec(high)),
                );
                let width = match high
                    .checked_sub(low)
                    .and_then(|span| span.checked_add(U256::ONE))
                {
                    Some(width) => width,
                    None => {
                        self.note(
                            "optimizer",
                            format!("{where_}: the domain is empty, so it has no width to publish"),
                        );
                        continue;
                    }
                };
                self.agree(
                    "optimizer",
                    &format!("{where_} domain width"),
                    &published["domain_width"],
                    json!(dec(width)),
                );

                let grid = match Gate::rebuild_grid(&hops, low, high, policy) {
                    Ok(grid) => grid,
                    Err(e) => {
                        self.note("optimizer", format!("{where_}: {e}"));
                        continue;
                    }
                };
                self.agree(
                    "optimizer",
                    &format!("{where_} strategy"),
                    &published["strategy"],
                    json!(grid.strategy),
                );
                self.agree(
                    "optimizer",
                    &format!("{where_} strategy this run took"),
                    &published["strategy_this_run_took"],
                    json!(grid.strategy),
                );
                self.agree(
                    "optimizer",
                    &format!("{where_} termination"),
                    &published["termination"],
                    json!(grid.termination),
                );
                self.agree(
                    "optimizer",
                    &format!("{where_} grid points"),
                    &recomputed["grid_points_rebuilt"],
                    Value::from(grid.points.len()),
                );
                if grid.coarse_points > 0 {
                    self.agree(
                        "optimizer",
                        &format!("{where_} coarse points"),
                        &recomputed["coarse_points_rebuilt"],
                        Value::from(grid.coarse_points),
                    );
                }

                // Fold every grid point again; a refusal is counted, never priced as zero.
                let mut curve: Vec<Value> = Vec::new();
                let mut evaluated: Vec<(U256, U256)> = Vec::new();
                let mut refusals = 0u64;
                for input in &grid.points {
                    match fold_route(&hops, *input) {
                        Ok((output, _)) => {
                            let (state, amount) = gross_of(*input, output);
                            evaluated.push((*input, output));
                            curve.push(json!({
                                "input": dec(*input),
                                "output": dec(output),
                                "gross": {"amount": dec(amount), "state": state},
                            }));
                        }
                        Err(_) => refusals += 1,
                    }
                }
                self.agree(
                    "optimizer",
                    &format!("{where_} evaluations"),
                    &published["evaluations"],
                    Value::from(evaluated.len()),
                );
                self.agree(
                    "optimizer",
                    &format!("{where_} refusals"),
                    &published["refusals"],
                    Value::from(refusals as usize),
                );
                self.agree(
                    "optimizer",
                    &format!("{where_} points the fold refused"),
                    &recomputed["points_the_fold_refused"],
                    Value::from(refusals as usize),
                );
                if evaluated.len() as u64 + refusals != grid.points.len() as u64 {
                    self.note("optimizer", format!("{where_}: the priced points and the refusals do not add up to the grid"));
                }

                // The whole curve, element by element, against both published copies of it.
                for (name, source) in [
                    (
                        "published curve",
                        array(published, "curve_this_file_asked_the_pipeline_for"),
                    ),
                    ("recomputed curve", array(recomputed, "curve")),
                ] {
                    match source {
                        Ok(entries) => {
                            if entries.len() != curve.len() {
                                self.note(
                                    "optimizer",
                                    format!(
                                        "{where_}: {name} has {} points, this file folded {}",
                                        entries.len(),
                                        curve.len()
                                    ),
                                );
                            }
                            for (index, (entry, mine)) in
                                entries.iter().zip(curve.iter()).enumerate()
                            {
                                if entry != mine {
                                    self.note(
                                        "optimizer",
                                        format!(
                                            "{where_}: {name}[{index}] is {} and not {}",
                                            compact(entry),
                                            compact(mine)
                                        ),
                                    );
                                    break;
                                }
                            }
                        }
                        Err(e) => self.note("optimizer", format!("{where_}: {e}")),
                    }
                }

                // The best point, by this file's own ranking.
                let best = match best_of(&evaluated) {
                    Some(best) => best,
                    None => {
                        self.note(
                            "optimizer",
                            format!(
                                "{where_}: nothing in the grid priced, so there is no best to name"
                            ),
                        );
                        continue;
                    }
                };
                let (state, amount) = gross_of(best.0, best.1);
                self.agree(
                    "optimizer",
                    &format!("{where_} best input"),
                    &published["best_input"],
                    json!(dec(best.0)),
                );
                self.agree(
                    "optimizer",
                    &format!("{where_} best output"),
                    &published["best_output"],
                    json!(dec(best.1)),
                );
                self.agree(
                    "optimizer",
                    &format!("{where_} best input rebuilt"),
                    &recomputed["best_input"],
                    json!(dec(best.0)),
                );
                self.agree(
                    "optimizer",
                    &format!("{where_} best output rebuilt"),
                    &recomputed["best_output"],
                    json!(dec(best.1)),
                );
                let best_profit = if state == "gain" {
                    dec(amount)
                } else {
                    dec(U256::ZERO)
                };
                self.agree(
                    "optimizer",
                    &format!("{where_} best profit"),
                    &published["best_profit"],
                    json!(best_profit),
                );
                self.agree(
                    "optimizer",
                    &format!("{where_} gross"),
                    &published["gross"],
                    json!({"amount": dec(amount), "state": state}),
                );
                self.agree(
                    "optimizer",
                    &format!("{where_} gross rebuilt"),
                    &recomputed["best_profit"],
                    json!({"amount": dec(amount), "state": state}),
                );
                if let Some(anchor) = grid.anchor {
                    self.agree(
                        "optimizer",
                        &format!("{where_} refinement anchor"),
                        &recomputed["refinement_anchor_this_file_found"],
                        json!(dec(anchor)),
                    );
                }

                // What the search actually covered: only an exhaustive walk that reached the
                // route's own ceiling may say it saw the domain.
                let covered = grid.strategy == "exhaustive" && high >= ceiling;
                self.agree(
                    "optimizer",
                    &format!("{where_} covered the route domain"),
                    &published["covered_the_route_domain"],
                    Value::from(covered),
                );
                for (flag, value) in [
                    (
                        "best_input",
                        best.0 == field_u256(published, "best_input").unwrap_or(U256::ZERO),
                    ),
                    (
                        "best_output",
                        best.1 == field_u256(published, "best_output").unwrap_or(U256::ZERO),
                    ),
                ] {
                    self.agree_bool(
                        "optimizer",
                        &format!("{where_} agrees.{flag}"),
                        &row["agrees"][flag],
                        value,
                    );
                }
                self.agree_bool(
                    "optimizer",
                    &format!("{where_} agrees.every_rebuilt_point_was_attempted"),
                    &row["agrees"]["every_rebuilt_point_was_attempted"],
                    true,
                );

                table.push(json!({
                    "row": where_,
                    "hops": hops.len(),
                    "domain": [dec(low), dec(high)],
                    "grid_points": grid.points.len(),
                    "coarse_points": grid.coarse_points,
                    "anchor": grid.anchor.map(dec).unwrap_or_else(|| "n/a".to_string()),
                    "evaluations": evaluated.len(),
                    "refusals": refusals,
                    "strategy": grid.strategy,
                    "best": format!("{} -> {}", dec(best.0), dec(best.1)),
                    "gross": format!("{state}:{amount}"),
                    "ceiling": dec(ceiling),
                    "covered_the_route_domain": covered,
                }));
            }
        }
        self.check("3 optimizer", json!({ "rows": table }));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Phase 4 — §43: the EVM answers the same bytes again, in this process
// ---------------------------------------------------------------------------

/// `0x`-prefixed lowercase bytes, the directory's own spelling.
fn hex_bytes(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

/// keccak256 as the directory prints it, so a published hash can be compared as a string.
fn keccak_hex(bytes: &[u8]) -> String {
    format!("{:#x}", alloy_primitives::keccak256(bytes))
}

/// What one run answered, rebuilt from a run this file issued rather than quoted from the row
/// that publishes it.
///
/// `run_spec` and `observed` are carried in the evidence directory's own JSON spelling, so the
/// comparison is a whole-object one: a row whose `gas_used` is right and whose `charge` is wrong
/// still fails, and [`Gate::agree`] names the key that differs instead of the phase it lives in.
#[derive(Clone)]
struct Replay {
    run_spec: Value,
    observed: Value,
    /// The reserve rows on their own, for a later phase that asks whether the market moved.
    reserves: Value,
    /// §24's word for the ending, as a simulation row spells it.
    status_word: &'static str,
    /// §5's word for the same ending inside an execution plan.
    plan_status_word: &'static str,
    gas_used: u64,
    gas_limit: u64,
    delivered: Option<U256>,
    market_moved: bool,
    block_number: u64,
    block_hash: String,
    recipient: String,
    input_token: String,
    amount_in: U256,
    min_final_amount: U256,
    calldata: String,
    calldata_hash: String,
    changed_slots: usize,
    balance_rows: usize,
    balance_rows_unchanged: usize,
    /// Changed slots counted per pool address, for the rows that claim residue by address rather
    /// than by a total nobody can re-ask.
    changed_slots_in: BTreeMap<String, usize>,
}

/// [`GasPricing`] from the published fields.
///
/// The run is rebuilt from these rather than from the `pricing_debug` word beside them: §43 asks
/// for a re-execution, and a `Debug` string is not a number a caller can pass. The word stays in
/// the row so a reader can check this gate's reconstruction against what the run itself printed.
fn pricing_of(pricing: &Value) -> Result<GasPricing, String> {
    let kind = field_str(pricing, "kind")?;
    match kind.as_str() {
        "eip1559" => Ok(GasPricing::Eip1559 {
            priority_fee_per_gas: field_u128(pricing, "priority_fee_per_gas")?,
            provenance: field_str(pricing, "provenance")?,
        }),
        "legacy" => Ok(GasPricing::Legacy {
            gas_price: field_u128(pricing, "gas_price")?,
            provenance: field_str(pricing, "provenance")?,
        }),
        "unresolved" => Ok(GasPricing::Unresolved {
            reason: field_str(pricing, "reason")?,
        }),
        other => Err(format!(
            "published pricing kind {other:?} is a model this gate would have to invent; §29 \
             forbids one"
        )),
    }
}

/// [`EvmRules`] from the published word, refusing to run a fixture under a guessed ruleset.
fn rules_of(text: &str) -> Result<EvmRules, String> {
    Ok(match text {
        "Shanghai" => EvmRules::Shanghai,
        "Cancun" => EvmRules::Cancun,
        "Prague" => EvmRules::Prague,
        "Osaka" => EvmRules::Osaka,
        other => {
            return Err(format!(
                "published rules {other:?} is not a ruleset this gate knows"
            ))
        }
    })
}

/// The [`ExecutorRun`] a published `spec` block describes.
///
/// The call is decoded from the published calldata bytes, never re-encoded from the published
/// `call` object: if this file assembled the fields itself, a bug in the field set would be
/// reproduced by its own author and the run would agree with a transaction nobody can send. The
/// decoded call is then compared against those fields, so the two spellings still have to agree —
/// in the other direction.
fn run_of(spec: &Value) -> Result<ExecutorRun, String> {
    let calldata = field_bytes(spec, "calldata")?;
    let call =
        decode_calldata(&calldata).map_err(|e| format!("decoding the published calldata: {e}"))?;
    Ok(ExecutorRun {
        chain_id: ChainId(field_u64(spec, "chain_id")?),
        priced_at: BlockNumber(field_u64(spec, "priced_at_block")?),
        state_source: field_str(spec, "state_source")?,
        executor: field_addr(spec, "executor")?,
        operator: field_addr(spec, "operator")?,
        call,
        gas_limit: field_u64(spec, "gas_limit")?,
        rules: rules_of(&field_str(spec, "rules")?)?,
        pricing: pricing_of(&spec["pricing"])?,
        endowment: optional_u256(spec, "endowment")?,
    })
}

/// The published `call` object against the call decoded from the published bytes.
fn call_problems(call: &ExecutorCall, published: &Value) -> Vec<String> {
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
                "the published bytes decode as {}, which is not a multi-hop execute call",
                other.signature()
            ));
            return problems;
        }
    };
    if field_str(published, "signature").ok().as_deref() != Some(call.signature()) {
        problems.push(format!(
            "signature: published {}, decoded {:?}",
            compact(&published["signature"]),
            call.signature()
        ));
    }
    if field_str(published, "selector").ok().as_deref() != Some(&hex_bytes(&call.selector())) {
        problems.push(format!(
            "selector: published {}, decoded {}",
            compact(&published["selector"]),
            hex_bytes(&call.selector())
        ));
    }
    for (name, decoded, published_value) in [
        ("amount_in", amount_in, &published["amount_in"]),
        (
            "min_final_amount",
            min_final,
            &published["min_final_amount"],
        ),
    ] {
        if field_u256(published, name).ok() != Some(decoded) {
            problems.push(format!(
                "{name}: decoded {}, published {}",
                dec(decoded),
                compact(published_value)
            ));
        }
    }
    for (key, decoded) in [("input_token", input_token), ("recipient", recipient)] {
        if field_addr(published, key).ok() != Some(decoded) {
            problems.push(format!(
                "{key}: decoded {}, published {}",
                addr_text(decoded),
                compact(&published[key])
            ));
        }
    }
    let published_legs = published["legs"].as_array().cloned().unwrap_or_default();
    if published_legs.len() != legs.len() {
        problems.push(format!(
            "legs: the decoded call carries {}, the published object lists {}",
            legs.len(),
            published_legs.len()
        ));
        return problems;
    }
    for (index, (leg, published)) in legs.iter().zip(published_legs.iter()).enumerate() {
        for key in ["pool", "token_in", "token_out"] {
            if field_addr(published, key).ok()
                != Some(
                    [leg.pool, leg.token_in, leg.token_out][match key {
                        "pool" => 0,
                        "token_in" => 1,
                        _ => 2,
                    }],
                )
            {
                problems.push(format!(
                    "leg[{index}].{key}: decoded {}, published {}",
                    addr_text(leg.pool),
                    compact(&published[key])
                ));
            }
        }
        for key in ["amount_in", "amount_out", "min_amount_out"] {
            let decoded = match key {
                "amount_in" => leg.amount_in,
                "amount_out" => leg.amount_out,
                _ => leg.min_amount_out,
            };
            if field_u256(published, key).ok() != Some(decoded) {
                problems.push(format!(
                    "leg[{index}].{key}: decoded {}, published {}",
                    dec(decoded),
                    compact(&published[key])
                ));
            }
        }
    }
    problems
}

/// The published `pricing` block, in the writer's spelling, from the model this gate rebuilt.
fn pricing_view(pricing: &GasPricing) -> Value {
    match pricing {
        GasPricing::Eip1559 {
            priority_fee_per_gas,
            provenance,
        } => json!({
            "kind": "eip1559",
            "priority_fee_per_gas": priority_fee_per_gas.to_string(),
            "provenance": provenance,
        }),
        GasPricing::Legacy {
            gas_price,
            provenance,
        } => json!({
            "kind": "legacy",
            "gas_price": gas_price.to_string(),
            "provenance": provenance,
        }),
        GasPricing::Unresolved { reason } => json!({ "kind": "unresolved", "reason": reason }),
    }
}

/// The published `call` block, from the call decoded out of the published bytes.
fn call_view(call: &ExecutorCall) -> Value {
    match call {
        ExecutorCall::Execute {
            legs,
            input_token,
            amount_in,
            min_final_amount,
            recipient,
        } => json!({
            "signature": call.signature(),
            "selector": hex_bytes(&call.selector()),
            "legs": legs.iter().map(|leg| json!({
                "pool": addr_text(leg.pool),
                "token_in": addr_text(leg.token_in),
                "token_out": addr_text(leg.token_out),
                "amount_in": dec(leg.amount_in),
                "amount_out": dec(leg.amount_out),
                "min_amount_out": dec(leg.min_amount_out),
            })).collect::<Vec<_>>(),
            "input_token": addr_text(*input_token),
            "amount_in": dec(*amount_in),
            "min_final_amount": dec(*min_final_amount),
            "recipient": addr_text(*recipient),
        }),
        other => json!({ "signature": other.signature() }),
    }
}

impl Replay {
    /// The run's answer, converted into the two blocks a simulation row publishes.
    ///
    /// `pools` are the addresses the row asked about by name; a per-pool slot count is only
    /// meaningful for a pool somebody can point at, so the row's own list is what gets counted.
    fn of(outcome: &ExecutorOutcome, run: &ExecutorRun, pools: &[String]) -> Replay {
        let reserves: Vec<Value> = outcome
            .reserves
            .iter()
            .map(|row| {
                json!({
                    "pool": addr_text(row.pool),
                    "before": [dec(row.before.reserve0), dec(row.before.reserve1)],
                    "after": [dec(row.after.reserve0), dec(row.after.reserve1)],
                    "changed": row.changed(),
                })
            })
            .collect();
        let balances: Vec<Value> = outcome
            .balances
            .iter()
            .map(|row| {
                json!({
                    "token": addr_text(row.token),
                    "holder": addr_text(row.holder),
                    "before": dec(row.before),
                    "after": dec(row.after),
                    "movement": format!("{:?}", row.movement()),
                })
            })
            .collect();
        let observed = json!({
            "status": format!("{:?}", outcome.status),
            "succeeded": outcome.succeeded(),
            "delivered": opt_dec(outcome.delivered),
            "gas_used": outcome.gas_used,
            "gas_limit": outcome.gas_limit,
            "charge": format!("{:?}", outcome.charge),
            "contract_error": outcome.contract_error.clone(),
            "revert_kind": outcome.revert_kind,
            "revert_reason": outcome.revert().map(|data| data.reason().to_string()),
            "market_moved": outcome.market_moved(),
            "logs": outcome.logs.len(),
            "reserve_rows": reserves.clone(),
            "balance_rows": balances,
            "changed_slots": outcome.state_changes.slots.len(),
        });
        let run_spec = json!({
            "chain_id": run.chain_id.0,
            "priced_at_block": run.priced_at.0,
            "state_source": run.state_source,
            "executor": addr_text(run.executor),
            "operator": addr_text(run.operator),
            "recipient": addr_text(run.operator),
            "gas_limit": run.gas_limit,
            "rules": format!("{:?}", run.rules),
            "pricing": pricing_view(&run.pricing),
            "pricing_debug": format!("{:?}", run.pricing),
            "endowment": opt_dec(run.endowment),
            "call": call_view(&run.call),
            "calldata": hex_bytes(run.call.encode().as_ref()),
            "calldata_len": run.call.encode().len(),
        });
        let mut changed_slots_in = BTreeMap::new();
        for pool in pools {
            let count = match addr_of(pool) {
                Some(address) => outcome.changed_slots_in(address).len(),
                // A pool the row names but this gate cannot name is itself the finding: the row
                // counted something on an address no reader can point at.
                None => usize::MAX,
            };
            changed_slots_in.insert(pool.clone(), count);
        }
        let status_word = match &outcome.status {
            StepStatus::Success => {
                if outcome.delivered.is_some() {
                    "delivered"
                } else {
                    "no_return"
                }
            }
            StepStatus::Reverted(_) => "reverted",
            StepStatus::OutOfGas => "out_of_gas",
            StepStatus::Halted(_) => "halted",
        };
        let plan_status_word = match &outcome.status {
            StepStatus::Success => "succeeded",
            StepStatus::Reverted(_) => "reverted",
            StepStatus::OutOfGas | StepStatus::Halted(_) => "not_run",
        };
        Replay {
            run_spec,
            reserves: Value::Array(reserves),
            observed,
            status_word,
            plan_status_word,
            gas_used: outcome.gas_used,
            gas_limit: outcome.gas_limit,
            delivered: outcome.delivered,
            market_moved: outcome.market_moved(),
            block_number: outcome.block.number.0,
            block_hash: format!("{:#x}", outcome.block.hash),
            recipient: addr_text(outcome.recipient),
            input_token: addr_text(outcome.input_token),
            amount_in: outcome.amount_in,
            min_final_amount: outcome.min_final_amount,
            calldata: hex_bytes(outcome.calldata.as_ref()),
            calldata_hash: keccak_hex(outcome.calldata.as_ref()),
            changed_slots: outcome.state_changes.slots.len(),
            balance_rows: outcome.balances.len(),
            balance_rows_unchanged: outcome.balances.iter().filter(|row| !row.changed()).count(),
            changed_slots_in,
        }
    }
}

impl Gate {
    /// Issue the run a row describes. The state is the committed dump the row names, loaded
    /// through the published recipe and nothing else: no overrides, no node, no in-memory edit
    /// beyond the endowment the spec itself carries.
    async fn replay_run(
        &mut self,
        where_: &str,
        spec: &Value,
        state: &Value,
        pools: &[String],
    ) -> Result<Replay, String> {
        let fixture_rel = field_str(state, "fixture_file")?;
        let source = field_str(state, "state_source_string")?;
        // Independence starts at the bytes on disk: a digest that does not match the file this
        // gate is about to load would mean the row describes a state nobody can read back.
        let digest = digest_of_file(&self.root, &fixture_rel)?;
        self.agree(
            "simulation",
            &format!("{where_} fixture digest"),
            &state["fixture"],
            digest,
        );
        if let Ok(additions) = field_str(state, "additions_file") {
            match digest_of_file(&self.root, &additions) {
                Ok(digest) => self.agree(
                    "simulation",
                    &format!("{where_} additions digest"),
                    &state["additions_digest"],
                    digest,
                ),
                Err(e) => self.note("simulation", e),
            }
        }
        if field_str(spec, "state_source").ok().as_deref() != Some(source.as_str()) {
            self.note(
                "simulation",
                format!(
                    "{where_}: spec.state_source and state.state_source_string are two \
                         different labels for one run"
                ),
            );
        }
        let run = run_of(spec)?;
        for problem in call_problems(&run.call, &spec["call"]) {
            self.note("simulation", format!("{where_}: {problem}"));
        }
        let dump = StateDump::from_file(&self.root.join(&fixture_rel))
            .map_err(|e| format!("reading {fixture_rel}: {e}"))?;
        let provider: Arc<dyn StateProvider> =
            Arc::new(DumpStateProvider::new(dump, source.clone()));
        let outcome = run_executor(provider, &run)
            .await
            .map_err(|e| format!("the rebuilt run refused: {e}"))?;
        Ok(Replay::of(&outcome, &run, pools))
    }

    /// The two simulation rows, plus §39's rollback run, replayed and re-spelled.
    async fn simulations(
        &mut self,
        pricing: &BTreeMap<String, PricingFacts>,
    ) -> Result<BTreeMap<String, Replay>, String> {
        let mut by_identity: BTreeMap<String, Replay> = BTreeMap::new();
        let mut table = Vec::new();
        for rel in [
            "data/evidence/m11/simulation/recorded_2hop.json",
            "data/evidence/m11/simulation/declared_3hop.json",
        ] {
            for (id, row) in self.rows(rel)? {
                let key = Gate::row_key(rel, &id);
                let where_ = key.as_str();
                let published = &row["published"];
                let spec = &published["spec"];
                let state = &published["state"];
                let opportunity = &published["simulated_opportunity"];
                let legs = array(&spec["call"], "legs")?;
                let pools = legs
                    .iter()
                    .map(|leg| field_str(leg, "pool"))
                    .collect::<Result<Vec<_>, _>>()?;
                let replay = match self.replay_run(where_, spec, state, &pools).await {
                    Ok(replay) => replay,
                    Err(e) => {
                        self.note("simulation", format!("{where_}: {e}"));
                        continue;
                    }
                };
                self.agree(
                    "simulation",
                    &format!("{where_} observed block"),
                    &published["observed"],
                    replay.observed.clone(),
                );
                self.agree(
                    "simulation",
                    &format!("{where_} run spec block"),
                    spec,
                    replay.run_spec.clone(),
                );

                // §44: what this run cost the network. The published figure belongs to the
                // assembling process, and the number it claims is zero; what this gate proves is
                // that a run of the same bytes issues no request either, because the provider it
                // built has no URL to read — recorded below as what this process sees.
                self.agree(
                    "simulation",
                    &format!("{where_} rpc count"),
                    &row["rpc"]["rpc_count"],
                    json!(0u64),
                );
                self.agree(
                    "simulation",
                    &format!("{where_} provider type"),
                    &row["rpc"]["basis"]["provider_type"],
                    json!("evm_simulation::state::DumpStateProvider"),
                );
                self.agree(
                    "simulation",
                    &format!("{where_} state built from"),
                    &row["rpc"]["basis"]["constructed_from"],
                    json!(field_str(state, "fixture_file")?),
                );

                // The two hashes the row claims it re-took, taken a third time here — over the
                // published bytes and over the published text, not over anything this file made.
                let published_calldata = field_bytes(spec, "calldata")?;
                let hash_of_published_bytes = keccak_hex(published_calldata.as_ref());
                let hash_of_published_text =
                    keccak_hex(field_str(opportunity, "canonical_text")?.as_bytes());
                self.agree(
                    "simulation",
                    &format!("{where_} calldata hash of the published bytes"),
                    &row["recomputed_independently"]["calldata_hash_of_the_published_bytes"],
                    json!(hash_of_published_bytes.clone()),
                );
                self.agree(
                    "simulation",
                    &format!("{where_} identity hash of the published text"),
                    &row["recomputed_independently"]
                        ["identity_hash_of_the_published_canonical_text"],
                    json!(hash_of_published_text.clone()),
                );
                self.agree_bool(
                    "simulation",
                    &format!("{where_} calldata hash agrees"),
                    &row["recomputed_independently"]["calldata_hash_agrees"],
                    hash_of_published_bytes == field_str(opportunity, "calldata_hash")?,
                );
                self.agree_bool(
                    "simulation",
                    &format!("{where_} identity hash agrees"),
                    &row["recomputed_independently"]["identity_hash_agrees"],
                    hash_of_published_text == field_str(opportunity, "identity_hash")?,
                );
                self.agree_bool(
                    "simulation",
                    &format!("{where_} request bytes match"),
                    &row["agrees"]["request_bytes_with_the_recorded_bytes"],
                    hex_bytes(published_calldata.as_ref()) == replay.calldata,
                );
                self.agree_bool(
                    "simulation",
                    &format!("{where_} calldata hash matches the figure"),
                    &row["agrees"]["calldata_hash_with_the_published_figure"],
                    replay.calldata_hash == field_str(opportunity, "calldata_hash")?,
                );

                // The flat summary, field by field, against the run this gate issued.
                self.agree(
                    "simulation",
                    &format!("{where_} status"),
                    &opportunity["status"],
                    json!(replay.status_word),
                );
                self.agree(
                    "simulation",
                    &format!("{where_} chain id"),
                    &opportunity["chain_id"],
                    json!(field_u64(spec, "chain_id")?),
                );
                self.agree(
                    "simulation",
                    &format!("{where_} simulation block"),
                    &opportunity["simulation_block"],
                    json!(replay.block_number),
                );
                self.agree(
                    "simulation",
                    &format!("{where_} simulation block hash"),
                    &opportunity["simulation_block_hash"],
                    json!(replay.block_hash.clone()),
                );
                self.agree(
                    "simulation",
                    &format!("{where_} gas used"),
                    &opportunity["gas_used"],
                    json!(replay.gas_used),
                );
                self.agree(
                    "simulation",
                    &format!("{where_} gas charge"),
                    &opportunity["gas_charge"],
                    replay.observed["charge"].clone(),
                );
                self.agree(
                    "simulation",
                    &format!("{where_} input amount"),
                    &opportunity["input_amount"],
                    json!(dec(replay.amount_in)),
                );
                self.agree(
                    "simulation",
                    &format!("{where_} min final output"),
                    &opportunity["min_final_output"],
                    json!(dec(replay.min_final_amount)),
                );
                self.agree(
                    "simulation",
                    &format!("{where_} final amount"),
                    &opportunity["final_amount"],
                    opt_dec(replay.delivered),
                );
                self.agree_bool(
                    "simulation",
                    &format!("{where_} market moved"),
                    &opportunity["market_moved"],
                    replay.market_moved,
                );

                // §43's strongest form: the canonical text is rebuilt here from the pricing rows
                // and this run's own answer, then hashed. The published identity hash is a number
                // only this reconstruction can produce.
                let edges = edge_list(&spec["call"], "legs")?;
                let (canonical, _) = min_rotation(&edges);
                let identity =
                    identity_string("RouteIdentity", field_u64(spec, "chain_id")?, &canonical);
                let facts = match pricing.get(&identity) {
                    Some(facts) => facts,
                    None => {
                        self.note(
                            "simulation",
                            format!(
                                "{where_}: its route matches no pricing row, so its priced \
                                     figures have no witness here"
                            ),
                        );
                        continue;
                    }
                };
                let gross_text = match replay.delivered {
                    Some(delivered) => {
                        let (state, amount) = gross_of(replay.amount_in, delivered);
                        format!("{state}:{amount}")
                    }
                    None => "none".to_string(),
                };
                let leg_lines = legs
                    .iter()
                    .enumerate()
                    .map(|(index, leg)| -> Result<String, String> {
                        Ok(format!(
                            "leg[{index}]|pool={}|token_in={}|token_out={}|amount_in={}|\
                             amount_out={}|min_amount_out={}",
                            field_str(leg, "pool")?,
                            field_str(leg, "token_in")?,
                            field_str(leg, "token_out")?,
                            field_str(leg, "amount_in")?,
                            field_str(leg, "amount_out")?,
                            field_str(leg, "min_amount_out")?,
                        ))
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join("\n");
                let route_line = format!(
                    "{} hops:{}",
                    canonical.len(),
                    canonical
                        .iter()
                        .map(|edge| format!("{}/{}>{}", edge.pool, edge.token_in, edge.token_out))
                        .collect::<Vec<_>>()
                        .join("|")
                );
                let text = format!(
                    "m11-multi-hop-simulation\nchain_id={}\nblock={}\nblock_hash={}\n\
                     executor={}\noperator={}\nrecipient={}\ninput_token={}\n\
                     input_amount={}\npriced_input={}\npriced_output={}\n\
                     legs={}\nmin_final_output={}\ngas_limit={}\ngas_used={}\n\
                     status={}\nfinal_amount={}\ngross={}\ncalldata_hash={}\nroute={}\n{}",
                    field_u64(spec, "chain_id")?,
                    replay.block_number,
                    replay.block_hash,
                    field_str(spec, "executor")?,
                    field_str(spec, "operator")?,
                    replay.recipient,
                    replay.input_token,
                    dec(replay.amount_in),
                    dec(facts.input),
                    dec(facts.output),
                    legs.len(),
                    dec(replay.min_final_amount),
                    replay.gas_limit,
                    replay.gas_used,
                    replay.status_word,
                    spell(replay.delivered),
                    gross_text,
                    replay.calldata_hash,
                    route_line,
                    leg_lines,
                );
                self.agree(
                    "simulation",
                    &format!("{where_} canonical text"),
                    &opportunity["canonical_text"],
                    json!(text.clone()),
                );
                self.agree(
                    "simulation",
                    &format!("{where_} identity hash of the rebuilt text"),
                    &opportunity["identity_hash"],
                    json!(keccak_hex(text.as_bytes())),
                );
                self.agree(
                    "simulation",
                    &format!("{where_} identity"),
                    &opportunity["identity"],
                    json!(format!(
                        "m11-sim-{}-{}-{}",
                        field_u64(spec, "chain_id")?,
                        replay.block_number,
                        keccak_hex(text.as_bytes())
                    )),
                );

                // What the run delivered against what the arithmetic promised, both re-derived.
                let same_route = &published["pricing_quotes_the_same_route"];
                self.agree(
                    "simulation",
                    &format!("{where_} quoted hop outputs"),
                    &same_route["quoted_hop_outputs"],
                    Value::Array(facts.hop_outputs.iter().map(|v| json!(dec(*v))).collect()),
                );
                self.agree(
                    "simulation",
                    &format!("{where_} delivered"),
                    &same_route["delivered"],
                    opt_dec(replay.delivered),
                );
                self.agree(
                    "simulation",
                    &format!("{where_} priced against delivered"),
                    &same_route["priced_against_delivered"],
                    match replay.delivered {
                        Some(delivered) => {
                            let (state, amount) = gross_of(delivered, facts.output);
                            json!({ "amount": dec(amount), "state": state })
                        }
                        None => Value::Null,
                    },
                );
                self.agree_bool(
                    "simulation",
                    &format!("{where_} matches pricing"),
                    &opportunity["matches_pricing"],
                    replay.delivered == Some(facts.output),
                );
                self.agree(
                    "simulation",
                    &format!("{where_} route agrees with pricing"),
                    &opportunity["route_agrees_with_pricing"],
                    json!("agrees"),
                );

                by_identity.insert(field_str(opportunity, "identity_hash")?, replay.clone());
                table.push(json!({
                    "row": key,
                    "identity": opportunity["identity"],
                    "fixture": state["fixture_file"],
                    "calldata_hash": replay.calldata_hash,
                    "status": replay.status_word,
                    "gas_used": replay.gas_used,
                    "delivered": opt_dec(replay.delivered),
                    "changed_slots": replay.changed_slots,
                    "reserve_rows_moved": replay.reserves.as_array().map(|rows| rows
                        .iter()
                        .filter(|row| row["changed"] == Value::Bool(true))
                        .count()),
                }));
            }
        }
        self.rollback_run().await?;
        self.check("4 simulation", json!({
            "layer": "revm",
            "rows_replayed": table.len(),
            "identity_of_a_run": "the committed fixture file's keccak256 plus the published \
                                  calldata keccak256; a row's position in a directory is not an \
                                  identity",
            "state_loading": "StateDump::from_file(fixture_file) → DumpStateProvider::new(dump, \
                              state_source_string). No overrides: the published fixture is the \
                              whole composed dump, so a row that quietly re-assembled it in \
                              memory would not be the state this gate re-ran.",
            "comparison": "the whole rebuilt observed block and the whole rebuilt run spec, plus \
                           the canonical text re-spelled from the pricing rows and this run's own \
                           answer, hashed here",
            "this_process_has_an_rpc_endpoint_variable": std::env::var("GIWA_RPC_URL").is_ok(),
            "runs": table,
        }));
        Ok(by_identity)
    }

    /// §39's third-leg revert: the run that refuses, and the residue it is claimed to leave.
    async fn rollback_run(&mut self) -> Result<(), String> {
        let rel = "data/evidence/m11/controlled/3hop/chain.json";
        for (id, row) in self.rows(rel)? {
            if row.get("spec").is_none() {
                continue;
            }
            let key = Gate::row_key(rel, &id);
            let where_ = key.as_str();
            let spec = &row["spec"];
            let state = &row["state"];
            let pools = array(&row["changed_slots_in_the_three_pools"], "pools")?
                .iter()
                .map(|pool| {
                    pool.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| "a pool entry that is not an address string".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            let replay = match self.replay_run(where_, spec, state, &pools).await {
                Ok(replay) => replay,
                Err(e) => {
                    self.note("rollback", format!("{where_}: {e}"));
                    continue;
                }
            };
            self.agree(
                "rollback",
                &format!("{where_} run spec block"),
                spec,
                replay.run_spec.clone(),
            );
            self.agree(
                "rollback",
                &format!("{where_} calldata"),
                &row["calldata"],
                json!(replay.calldata),
            );
            self.agree(
                "rollback",
                &format!("{where_} status"),
                &row["status"],
                replay.observed["status"].clone(),
            );
            self.agree(
                "rollback",
                &format!("{where_} revert reason"),
                &row["revert_reason"],
                replay.observed["revert_reason"].clone(),
            );
            self.agree(
                "rollback",
                &format!("{where_} contract error"),
                &row["contract_error"],
                replay.observed["contract_error"].clone(),
            );
            self.agree(
                "rollback",
                &format!("{where_} delivered"),
                &row["delivered"],
                opt_dec(replay.delivered),
            );
            self.agree(
                "rollback",
                &format!("{where_} reserve rows"),
                &row["reserve_rows"],
                replay.reserves.clone(),
            );
            self.agree_bool(
                "rollback",
                &format!("{where_} market moved"),
                &row["market_moved"],
                replay.market_moved,
            );
            self.agree(
                "rollback",
                &format!("{where_} balance rows"),
                &row["balance_rows"],
                json!(replay.balance_rows),
            );
            self.agree(
                "rollback",
                &format!("{where_} unchanged balance rows"),
                &row["balance_rows_unchanged"],
                json!(replay.balance_rows_unchanged),
            );
            let rebuilt: Vec<Value> = pools
                .iter()
                .map(|pool| {
                    json!(replay
                        .changed_slots_in
                        .get(pool)
                        .copied()
                        .unwrap_or(usize::MAX))
                })
                .collect();
            self.agree(
                "rollback",
                &format!("{where_} changed slots per pool"),
                &row["changed_slots_in_the_three_pools"]["counts"],
                Value::Array(rebuilt.clone()),
            );
            // §39's claim is a residue-free revert, so the counts this gate rebuilt have to be
            // zero as well as equal to the published ones — one without the other is a different
            // finding.
            let residue_free = rebuilt.iter().all(|count| count == &json!(0));
            self.agree_bool(
                "rollback",
                &format!("{where_} verdict of no residue"),
                &json!(true),
                residue_free,
            );
            self.check(
                "4b rollback",
                json!({
                    "row": key,
                    "status": replay.observed["status"],
                    "delivered": opt_dec(replay.delivered),
                    "reserve_rows_rebuilt": replay.reserves.as_array().map(|rows| rows.len()),
                    "changed_slots_per_pool": rebuilt,
                }),
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Phase 5 — the plan layer: the canonical text, its hash, and the bytes it binds
// ---------------------------------------------------------------------------

/// The four identities one controlled chain row claims, kept for the phase that joins the files.
#[derive(Clone, Debug)]
struct ChainFacts {
    plan_hash: String,
    calldata_hash: String,
    route_id: String,
    sim_identity: String,
    /// §30's `m11-sim-…` string, spelled here from the block facts the run answered with.
    simulation_id: String,
    market_label: String,
    declared_addresses: Vec<String>,
}

/// `RealState { source: "…" }`, the `Debug` spelling the row publishes for §34's funding statement.
///
/// An `Overridden { … }` is refused here on purpose rather than spelled into the text: a plan whose
/// sender was funded by a state override cannot be an execution plan at all, so a row that
/// publishes one has to fail this gate instead of being quietly rendered.
fn real_state_source(text: &str) -> Result<String, String> {
    text.strip_prefix("RealState { source: \"")
        .and_then(|body| body.strip_suffix("\" }"))
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "funding is spelled {text:?}, which is not a real-chain-state \
                               statement this gate can read"
            )
        })
}

/// `TokenSettled { token: 0x…, reason: "…" }` → the token and the reason, the two pieces §34's
/// split-denomination line re-spells.
fn token_settled(text: &str) -> Result<(String, String), String> {
    let body = text
        .strip_prefix("TokenSettled { token: ")
        .ok_or_else(|| format!("denomination is spelled {text:?}, which this gate cannot read"))?;
    let (token, rest) = body
        .split_once(", reason: \"")
        .ok_or_else(|| format!("denomination {text:?} has no reason field"))?;
    let reason = rest
        .strip_suffix("\" }")
        .ok_or_else(|| format!("denomination {text:?} does not close"))?;
    Ok((token.to_string(), reason.to_string()))
}

/// The plan's canonical text, rebuilt from the row's own `fields` with the simulation's answer
/// bound to the run this gate issued.
///
/// This is the gate's own spelling of §5's hash pre-image: no production type is imported and no
/// production function is called, so `plan_hash` is only reproducible if this file's reading of the
/// fields matches what the plan asserted about itself — and `outcome=`, `gas_used=`,
/// `proved_gas_limit=` and `final_amount=` come from a REVM run this process started.
fn plan_text(fields: &Value, replay: &Replay) -> Result<String, String> {
    let simulation = &fields["simulation"];
    let validity = &fields["validity"];
    let profit = &fields["profit"];
    let mut text = format!(
        "m10-arbitrage-execution-plan\nchain_id={}\nexecutor={}\nsender={}\nrecipient={}\n\
         input_token={}\ninput_amount={}\nlegs={}\nmin_final_output={}\n",
        field_u64(fields, "chain_id")?,
        field_str(fields, "executor")?,
        field_str(fields, "sender")?,
        field_str(fields, "recipient")?,
        field_str(fields, "input_token")?,
        field_str(fields, "input_amount")?,
        field_u64(fields, "legs")?,
        field_str(fields, "min_final_output")?,
    );
    for (index, leg) in array(fields, "leg_rows")?.iter().enumerate() {
        text.push_str(&format!(
            "leg[{index}]|pool={}|token_in={}|token_out={}|amount_in={}|amount_out={}|\
             min_amount_out={}|derivation={}\n",
            field_str(leg, "pool")?,
            field_str(leg, "token_in")?,
            field_str(leg, "token_out")?,
            field_str(leg, "amount_in")?,
            field_str(leg, "amount_out")?,
            field_str(leg, "min_amount_out")?,
            field_str(leg, "derivation")?,
        ));
    }
    text.push_str(&format!(
        "validity|simulated_at={}|max_block_age={}|provenance={}\n",
        field_u64(validity, "simulated_at_block")?,
        field_u64(validity, "max_block_age")?,
        field_str(validity, "provenance")?,
    ));
    let outcome = match field_str(simulation, "outcome")?.as_str() {
        "succeeded" => {
            let delivered = replay.delivered.ok_or_else(|| {
                "the plan says the simulation succeeded and the run this gate issued returned \
                 nothing"
                    .to_string()
            })?;
            format!(
                "succeeded|gas_used={}|proved_gas_limit={}|final_amount={}",
                replay.gas_used,
                replay.gas_limit,
                dec(delivered)
            )
        }
        other => {
            return Err(format!(
                "the plan's outcome is {other:?}, which this gate has no run to bind to"
            ))
        }
    };
    let funding_source = real_state_source(&field_str(simulation, "funding")?)?;
    let market = field_str(simulation, "market")?;
    let market_evidence = field_str(simulation, "market_evidence")?;
    text.push_str(&format!(
        "simulation|correlation_id={}|block={}|hash={}|state={}|simulation_id={}|outcome={}|\
         funding=real chain state: {funding_source}|market={market} — {market_evidence}\n",
        field_str(simulation, "correlation_id")?,
        field_u64(simulation, "block_number")?,
        field_str(simulation, "block_hash")?,
        field_str(simulation, "state_fingerprint")?,
        field_str(simulation, "simulation_id")?,
        outcome,
    ));
    let (token, reason) = token_settled(&field_str(profit, "denomination")?)?;
    text.push_str(&format!(
        "profit|denomination=split: profit in {token} and cost in native ETH; \
         no single-denomination net is claimed ({reason})|required_final_balance={}|provenance={}\n",
        field_str(profit, "required_final_balance")?,
        field_str(profit, "provenance")?,
    ));
    Ok(text)
}

impl Gate {
    /// §41's label, derived from the recording the directory names rather than copied from the
    /// row's prose.
    ///
    /// An address the archive-node recording carries is market state; an address only the declared
    /// additions carry is topology this milestone wrote down. That distinction is the whole content
    /// of `REAL_MARKET` versus `CONTROLLED_FIXTURE`, so it is checkable by a reader who never
    /// executes anything: open the recording, look the address up.
    fn market_claim(
        &mut self,
        where_: &str,
        state: &Value,
        edges: &[Edge],
    ) -> (String, Vec<String>) {
        let recording_rel = match field_str(state, "base_recording") {
            Ok(rel) => rel,
            Err(e) => {
                self.note("plan", format!("{where_}: {e}"));
                return (String::new(), Vec::new());
            }
        };
        let dump = match StateDump::from_file(&self.root.join(&recording_rel)) {
            Ok(dump) => dump,
            Err(e) => {
                self.note("plan", format!("{where_}: reading {recording_rel}: {e}"));
                return (String::new(), Vec::new());
            }
        };
        let mut declared = Vec::new();
        let mut pools_recorded = 0usize;
        for edge in edges {
            for (address, is_pool) in [
                (&edge.pool, true),
                (&edge.token_in, false),
                (&edge.token_out, false),
            ] {
                let present = dump.accounts.contains_key(address);
                if is_pool && present {
                    pools_recorded += 1;
                }
                if !present && !declared.contains(address) {
                    declared.push(address.clone());
                }
            }
        }
        let plan_label = if declared.is_empty() {
            "REAL_MARKET"
        } else {
            "CONTROLLED_FIXTURE"
        };
        // The pricing rows' three kinds split on one question: does any pool of this route exist in
        // the recording at all, and if so does every address the route touches.
        let pricing_label = if pools_recorded == 0 {
            "DECLARED_SYNTHETIC_GRAPH"
        } else if declared.is_empty() {
            "REAL_MARKET_POOLS_ON_A_CONTROLLED_FIXTURE_STATE"
        } else {
            "CONTROLLED_FIXTURE"
        };
        (format!("{plan_label}|{pricing_label}"), declared)
    }

    /// One controlled chain row: its plan text and hash, the bytes the plan binds, the identities
    /// the binding publishes, and §41's label re-derived from the recording.
    #[allow(clippy::too_many_lines)]
    fn plan_stage(
        &mut self,
        where_: &str,
        stages: &Value,
        replay: &Replay,
        pricing: &BTreeMap<String, PricingFacts>,
    ) -> Result<ChainFacts, String> {
        let plan = &stages["plan"];
        let fields = &plan["fields"];
        let binding = &stages["binding"];
        let simulation_fields = &fields["simulation"];
        let chain = field_u64(fields, "chain_id")?;

        // §5's word for an ending inside a plan is not §24's word for the same ending in a
        // simulation row: `reverted` becomes `not_run` for a run that never started. The plan may
        // only claim `succeeded` if the run this gate issued says so.
        self.agree(
            "plan",
            &format!("{where_} plan outcome word"),
            &simulation_fields["outcome"],
            json!(replay.plan_status_word),
        );

        let rebuilt = match plan_text(fields, replay) {
            Ok(text) => text,
            Err(e) => {
                self.note("plan", format!("{where_}: {e}"));
                return Err(e);
            }
        };
        let published_text = field_str(plan, "canonical_text")?;
        self.agree(
            "plan",
            &format!("{where_} canonical text"),
            &json!(published_text.clone()),
            json!(rebuilt.clone()),
        );
        let hash_of_published_text = keccak_hex(published_text.as_bytes());
        let hash_of_rebuilt_text = keccak_hex(rebuilt.as_bytes());
        self.agree(
            "plan",
            &format!("{where_} plan hash of the published text"),
            &plan["plan_hash"],
            json!(hash_of_published_text.clone()),
        );
        self.agree(
            "plan",
            &format!("{where_} plan hash of the rebuilt text"),
            &plan["plan_hash"],
            json!(hash_of_rebuilt_text.clone()),
        );

        // The bytes. Re-encoded from the plan's own leg rows, so §23's determinism is shown by
        // this file producing the calldata rather than by trusting the hex on disk; then decoded
        // back, so the fields the bytes carry are the fields the plan claims.
        let calldata = field_bytes(plan, "calldata")?;
        let call = decode_calldata(&calldata)
            .map_err(|e| format!("{where_}: decoding the plan calldata: {e}"))?;
        let legs = array(fields, "leg_rows")?;
        let rebuilt_call = ExecutorCall::Execute {
            legs: legs
                .iter()
                .map(|leg| -> Result<ExecutorLeg, String> {
                    Ok(ExecutorLeg {
                        pool: field_addr(leg, "pool")?,
                        token_in: field_addr(leg, "token_in")?,
                        token_out: field_addr(leg, "token_out")?,
                        amount_in: field_u256(leg, "amount_in")?,
                        amount_out: field_u256(leg, "amount_out")?,
                        min_amount_out: field_u256(leg, "min_amount_out")?,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            input_token: field_addr(fields, "input_token")?,
            amount_in: field_u256(fields, "input_amount")?,
            min_final_amount: field_u256(fields, "min_final_output")?,
            recipient: field_addr(fields, "recipient")?,
        };
        self.agree(
            "plan",
            &format!("{where_} plan bytes re-encoded from the leg rows"),
            &json!(hex_bytes(calldata.as_ref())),
            json!(hex_bytes(rebuilt_call.encode().as_ref())),
        );
        self.agree(
            "plan",
            &format!("{where_} plan calldata hash"),
            &plan["calldata_hash"],
            json!(keccak_hex(calldata.as_ref())),
        );
        self.agree_bool(
            "plan",
            &format!("{where_} plan bytes are the run's bytes"),
            &json!(true),
            hex_bytes(calldata.as_ref()) == replay.calldata,
        );
        // The plan's leg rows, re-rendered as the call they encode.
        let as_call = json!({
            "legs": legs.iter().map(|leg| json!({
                "pool": leg["pool"], "token_in": leg["token_in"], "token_out": leg["token_out"],
                "amount_in": leg["amount_in"], "amount_out": leg["amount_out"],
                "min_amount_out": leg["min_amount_out"],
            })).collect::<Vec<_>>(),
            "input_token": fields["input_token"],
            "amount_in": fields["input_amount"],
            "min_final_amount": fields["min_final_output"],
            "recipient": fields["recipient"],
        });
        let decoded = call_view(&call);
        for key in [
            "legs",
            "input_token",
            "amount_in",
            "min_final_amount",
            "recipient",
        ] {
            self.agree(
                "plan",
                &format!("{where_} plan call field {key}"),
                &as_call[key],
                decoded[key].clone(),
            );
        }

        // route_id: the pools in trade order, then the tokens the route steps through.
        let pools = legs
            .iter()
            .map(|leg| field_str(leg, "pool"))
            .collect::<Result<Vec<_>, _>>()?;
        let mut tokens = vec![field_str(&legs[0], "token_in")?];
        for leg in &legs {
            tokens.push(field_str(leg, "token_out")?);
        }
        self.agree(
            "plan",
            &format!("{where_} token transitions"),
            &plan["token_transitions"],
            json!(tokens.clone()),
        );
        let route_id = format!("m10-{chain}-{}-{}", pools.join(">"), tokens.join(">"));
        self.agree(
            "plan",
            &format!("{where_} route id"),
            &plan["route_id"],
            json!(route_id.clone()),
        );

        // §30's binding: one plan, one route, one set of bytes, across the three stages that name
        // them independently.
        for key in [
            "priced_route_id",
            "simulated_route_id",
            "execution_route_id",
        ] {
            self.agree(
                "plan",
                &format!("{where_} binding {key}"),
                &binding[key],
                json!(route_id.clone()),
            );
        }
        self.agree(
            "plan",
            &format!("{where_} binding plan hash"),
            &binding["plan_hash"],
            plan["plan_hash"].clone(),
        );
        self.agree(
            "plan",
            &format!("{where_} binding plan hash is the recomputed one"),
            &binding["plan_hash"],
            json!(hash_of_rebuilt_text.clone()),
        );
        self.agree(
            "plan",
            &format!("{where_} binding calldata hashes agree"),
            &binding["execution_calldata_hash"],
            plan["calldata_hash"].clone(),
        );
        self.agree(
            "plan",
            &format!("{where_} simulated calldata hash"),
            &binding["simulation_calldata_hash"],
            json!(replay.calldata_hash.clone()),
        );
        self.agree_bool(
            "plan",
            &format!("{where_} binding says bound"),
            &binding["bound"],
            true,
        );

        // The simulation identity the plan carries: the row's `simulation_id` field is the
        // simulation's canonical-text hash, and §30's `m11-sim-…` string is built from it.
        let sim_identity = field_str(simulation_fields, "simulation_id")?;
        self.agree(
            "plan",
            &format!("{where_} plan simulation id equals the run's identity"),
            &json!(sim_identity.clone()),
            json!(field_str(
                &stages["simulation"]["published"]["simulated_opportunity"],
                "identity_hash"
            )?),
        );
        let simulation_id = format!("m11-sim-{chain}-{}-{sim_identity}", replay.block_number);
        self.agree(
            "plan",
            &format!("{where_} binding simulation id"),
            &binding["simulation_id"],
            json!(simulation_id.clone()),
        );

        // §41: the label, derived from the recording. The chain row's own pricing stage points at
        // the standalone pricing file rather than repeating a kind, so the comparison the gate can
        // actually make is between this derivation and the kind the pricing row published.
        let edges = edge_list(&stages["search"], "edges")?;
        let (label, declared) =
            self.market_claim(where_, &stages["simulation"]["published"]["state"], &edges);
        let (plan_label, pricing_label) = label.split_once('|').unwrap_or(("", ""));
        self.agree(
            "plan",
            &format!("{where_} plan market label"),
            &simulation_fields["market"],
            json!(plan_label),
        );
        self.check(
            "plan",
            json!({
                "row": where_,
                "pricing_market_claim_in_the_chain_row": stages["pricing"]["market_claim"]["kind"],
                "addresses_absent_from_the_recording": declared.clone(),
            }),
        );
        // The search stage: the edges the cycle was found over, canonicalised the same way the
        // pricing rows' identities are.
        let search = &stages["search"];
        let (canonical, _) = min_rotation(&edges);
        self.agree(
            "plan",
            &format!("{where_} search canonical key"),
            &search["canonical_key"],
            json!(identity_string("CanonicalKey", chain, &canonical)),
        );
        self.agree(
            "plan",
            &format!("{where_} search identity from edges"),
            &search["identity_recomputed_from_the_edges"],
            json!(edge_values(&canonical)),
        );
        self.agree(
            "plan",
            &format!("{where_} search hop count"),
            &search["hop_count"],
            Value::from(edges.len()),
        );
        self.agree(
            "plan",
            &format!("{where_} search start token"),
            &search["start_token"],
            json!(edges
                .first()
                .map(|e| e.token_in.clone())
                .unwrap_or_default()),
        );
        let identity = identity_string("RouteIdentity", chain, &canonical);
        let facts = pricing.get(&identity).ok_or_else(|| {
            format!(
                "{where_}: its cycle matches no pricing row, so the plan has no arithmetic to \
                    be checked against"
            )
        })?;

        // The pricing row's §41 kind, re-derived here from the recording it ran on. A pricing row
        // that claimed real-market pools while naming addresses the recording never saw would be
        // the §41 violation this gate exists to catch, and the chain row's own pricing stage is
        // only a pointer, so the comparison has to be made against the file it points at.
        self.agree(
            "plan",
            &format!("{where_} pricing market kind"),
            &json!(facts.market_kind.clone()),
            json!(pricing_label),
        );

        // The optimizer stage, against the pricing fold this gate recomputed in phase 2.
        let optimizer = &stages["optimizer"];
        self.agree(
            "plan",
            &format!("{where_} optimizer best input"),
            &optimizer["best_input"],
            json!(dec(facts.input)),
        );
        self.agree(
            "plan",
            &format!("{where_} optimizer best output"),
            &optimizer["best_output"],
            json!(dec(facts.output)),
        );
        self.agree(
            "plan",
            &format!("{where_} optimizer domain"),
            &optimizer["domain"],
            json!([dec(facts.input), dec(facts.input)]),
        );

        // The risk stage: §26's comparisons, re-made from the run's own figures.
        let risk = &stages["risk"];
        let figures = &risk["figures"];
        let input = replay.amount_in;
        let delivered = replay.delivered.ok_or_else(|| {
            format!("{where_}: the run delivered nothing, so no floor can be asked")
        })?;
        let gross = delivered
            .checked_sub(input)
            .ok_or_else(|| format!("{where_}: the delivery is below the input"))?;
        self.agree(
            "plan",
            &format!("{where_} risk input"),
            &figures["input_amount"],
            json!(dec(input)),
        );
        self.agree(
            "plan",
            &format!("{where_} risk delivered"),
            &figures["delivered"],
            json!(dec(delivered)),
        );
        self.agree(
            "plan",
            &format!("{where_} risk gross"),
            &figures["gross_profit"],
            json!(dec(gross)),
        );
        self.agree(
            "plan",
            &format!("{where_} risk gross by arithmetic"),
            &risk["gross_by_arithmetic"],
            json!(dec(gross)),
        );
        self.agree_bool(
            "plan",
            &format!("{where_} risk clears the floor"),
            &risk["clears_the_floor"],
            delivered > input
                && gross >= field_u256(figures, "minimum_gross_profit")?
                && replay.gas_used <= field_u64(figures, "maximum_gas")?
                && replay.min_final_amount <= delivered,
        );
        // The gas bill, re-derived from the recorded header rather than read out of the run: §26
        // refuses a plan whose charge the gate cannot recompute.
        let recording_rel = field_str(
            &stages["simulation"]["published"]["state"],
            "base_recording",
        )?;
        let dump = StateDump::from_file(&self.root.join(&recording_rel))
            .map_err(|e| format!("{where_}: reading {recording_rel}: {e}"))?;
        let base_fee = dump
            .header
            .and_then(|header| header.base_fee_per_gas)
            .ok_or_else(|| format!("{where_}: the recording carries no base fee"))?;
        let priority = field_u128(
            &stages["simulation"]["published"]["spec"]["pricing"],
            "priority_fee_per_gas",
        )?;
        let wei = u128::from(replay.gas_used)
            .checked_mul(
                base_fee
                    .checked_add(priority)
                    .ok_or_else(|| "the fee overflows".to_string())?,
            )
            .ok_or_else(|| "the gas bill overflows".to_string())?;
        self.agree(
            "plan",
            &format!("{where_} risk gas charge"),
            &figures["gas_charge_wei"],
            json!(wei.to_string()),
        );

        Ok(ChainFacts {
            plan_hash: field_str(plan, "plan_hash")?,
            calldata_hash: field_str(plan, "calldata_hash")?,
            route_id,
            sim_identity,
            simulation_id,
            market_label: label,
            declared_addresses: declared,
        })
    }

    /// The two controlled chains, end to end: the row's own run replayed, then every stage in the
    /// ladder checked against this gate's arithmetic and this gate's EVM.
    async fn chain_ladder(
        &mut self,
        pricing: &BTreeMap<String, PricingFacts>,
    ) -> Result<BTreeMap<String, ChainFacts>, String> {
        let mut out = BTreeMap::new();
        let mut table = Vec::new();
        for rel in [
            "data/evidence/m11/controlled/2hop/chain.json",
            "data/evidence/m11/controlled/3hop/chain.json",
        ] {
            for (id, row) in self.rows(rel)? {
                if row.get("stages").is_none() {
                    continue;
                }
                let key = Gate::row_key(rel, &id);
                let where_ = key.as_str();
                let stages = &row["stages"];
                let published = &stages["simulation"]["published"];
                let legs = array(&published["spec"]["call"], "legs")?;
                let pools = legs
                    .iter()
                    .map(|leg| field_str(leg, "pool"))
                    .collect::<Result<Vec<_>, _>>()?;
                let replay = match self
                    .replay_run(where_, &published["spec"], &published["state"], &pools)
                    .await
                {
                    Ok(replay) => replay,
                    Err(e) => {
                        self.note("plan", format!("{where_}: {e}"));
                        continue;
                    }
                };
                self.agree(
                    "plan",
                    &format!("{where_} chain observed block"),
                    &published["observed"],
                    replay.observed.clone(),
                );
                let facts = match self.plan_stage(where_, stages, &replay, pricing) {
                    Ok(facts) => facts,
                    Err(e) => {
                        self.note("plan", format!("{where_}: {e}"));
                        continue;
                    }
                };
                // The row's own contract-answer summary, re-derived from the replay.
                let answered = &row["the_contract_answered"];
                self.agree(
                    "plan",
                    &format!("{where_} answered status"),
                    &answered["status"],
                    json!(replay.status_word),
                );
                self.agree(
                    "plan",
                    &format!("{where_} answered gas"),
                    &answered["gas_used"],
                    json!(replay.gas_used),
                );
                self.agree(
                    "plan",
                    &format!("{where_} answered delivery"),
                    &answered["delivered"],
                    opt_dec(replay.delivered),
                );
                self.agree(
                    "plan",
                    &format!("{where_} answered quote"),
                    &answered["quoted_output"],
                    json!(dec(replay.min_final_amount)),
                );
                self.agree_bool(
                    "plan",
                    &format!("{where_} answered floor met"),
                    &answered["min_final_output_met"],
                    replay
                        .delivered
                        .is_some_and(|v| v >= replay.min_final_amount),
                );
                self.agree(
                    "plan",
                    &format!("{where_} answered reserve rows moved"),
                    &answered["reserve_rows_moved"],
                    Value::from(replay.reserves.as_array().map_or(0, |rows| {
                        rows.iter()
                            .filter(|row| row["changed"] == Value::Bool(true))
                            .count()
                    })),
                );
                // §41's disclaimer has to still be in the row that carries it.
                let not_claimed = array(&row, "not_claimed")?;
                let text = not_claimed
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" | ");
                self.agree_bool(
                    "plan",
                    &format!("{where_} disclaims a real-market verdict"),
                    &json!(true),
                    text.contains("real-market verdict"),
                );
                out.insert(facts.sim_identity.clone(), facts.clone());
                table.push(json!({
                    "row": where_,
                    "plan_hash": facts.plan_hash,
                    "calldata_hash": facts.calldata_hash,
                    "simulation_id": facts.simulation_id,
                    "market_label": facts.market_label,
                    "addresses_not_in_the_recording": facts.declared_addresses,
                    "gas_used": replay.gas_used,
                    "delivered": opt_dec(replay.delivered),
                }));
            }
        }
        self.check("5 plan", json!({
            "layer": "execution plan",
            "rows": table.len(),
            "hash_of_a_plan": "keccak256 over the canonical text this file re-spelled from the \
                               row's own `fields`, with the simulation outcome bound to the run \
                               this gate issued — not the plan crate's hash function",
            "market_claim": "derived by looking every route address up in the recording the row \
                             names as its base_recording; an address the recording does not carry \
                             is declared topology (§41)",
            "runs": table,
        }));
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Phase 6 — risk: §26–§29's comparisons, re-made over the figures the row carries
// ---------------------------------------------------------------------------

/// A number that may be absent, in the JSON spelling the rows use for the two block facts: a real
/// height, or `null` because the caller had not read one. `null` is not zero (§4).
fn field_u64_opt(v: &Value, key: &str) -> Result<Option<u64>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => Ok(Some(field_u64(v, key)?)),
    }
}

/// The four numbers inside a `GasCharge`'s `Debug` word, read back out of it.
///
/// The row publishes the charge as production prints it, and a printed charge is not a number a
/// caller can pass — so the figure is taken from the words and then checked against itself: the
/// bill has to be the burn times the two fees the same sentence names. A word this gate cannot
/// read is recorded as drift rather than skipped.
fn charge_figures(text: &str) -> Result<(u64, u128, u128, u128), String> {
    let number = |label: &str| -> Result<u128, String> {
        let at = text
            .find(label)
            .ok_or_else(|| format!("the charge word carries no {label}"))?;
        let tail = &text[at + label.len()..];
        let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits
            .parse::<u128>()
            .map_err(|_| format!("{label} is not a number in {text:?}"))
    };
    let used = number("gas_used: ")
        .and_then(|v| u64::try_from(v).map_err(|_| "the burn leaves u64".to_string()))?;
    let effective = number("effective_gas_price: ")?;
    let wei = number("wei: ")?;
    let priority = number("priority_fee_per_gas: ")?;
    let base = if text.contains("base_fee_per_gas: Some(") {
        number("base_fee_per_gas: Some(")?
    } else {
        0
    };
    Ok((
        used,
        effective,
        base.checked_add(priority)
            .ok_or_else(|| "the fee overflows".to_string())?,
        wei,
    ))
}

impl Gate {
    /// Every risk row: the six comparisons re-made from the figures beside them, the acceptance
    /// that follows from those six, and the §46 rejection arm answered the same way.
    fn risk(&mut self, pricing: &BTreeMap<String, PricingFacts>) -> Result<(), String> {
        let rel = "data/evidence/m11/risk/decision.json";
        let mut table = Vec::new();
        for (id, row) in self.rows(rel)? {
            let key = Gate::row_key(rel, &id);
            let where_ = key.as_str();
            let policy = &row["policy"];
            let facts = &row["market_facts"];
            let run = &row["run"];
            let published = &row["published"];
            let recomputed = &row["recomputed_independently"];

            let input = field_u256(&row, "input_amount")?;
            let delivered = optional_u256(run, "final_amount")?;
            let gas_used = field_u64(run, "gas_used")?;
            let simulation_block = field_u64(run, "simulation_block")?;
            let guard = field_u256(run, "min_final_output")?;
            let floor = field_u256(policy, "minimum_gross_profit")?;
            let maximum_gas = field_u64(policy, "maximum_gas")?;
            let maximum_simulation_age = field_u64(policy, "maximum_simulation_age")?;
            let maximum_state_age = field_u64(policy, "maximum_state_age")?;
            let head = field_u64_opt(facts, "head")?;
            let state_version = field_u64_opt(facts, "state_version")?;

            // §27's first question is whether the run delivered at all; a row whose ending is not
            // `delivered` has no profit to compare, and the row that says so must not be accepted.
            let status = field_str(run, "status")?;
            let gross = delivered.map(|value| value.saturating_sub(input));
            let simulation_age = head.map(|head| head.saturating_sub(simulation_block));
            let state_age = state_version.map(|version| version.saturating_sub(simulation_block));
            let clears = gross.map(|value| value >= floor).unwrap_or(false);
            let within_gas = gas_used <= maximum_gas;
            let within_simulation_age = simulation_age
                .map(|age| age <= maximum_simulation_age)
                .unwrap_or(false);
            let within_state_age = state_age
                .map(|age| age <= maximum_state_age)
                .unwrap_or(false);
            let executor_matches = field_str(run, "executor")? == field_str(policy, "executor")?;
            let chain_matches = field_u64(run, "chain_id")? == field_u64(policy, "chain_id")?;
            let guard_met = delivered.map(|value| value >= guard).unwrap_or(false);
            let accepted = status == "delivered"
                && !input.is_zero()
                && clears
                && within_gas
                && within_simulation_age
                && within_state_age
                && executor_matches
                && chain_matches
                && guard_met;

            self.agree(
                "risk",
                &format!("{where_} gross by arithmetic"),
                &recomputed["gross_profit"],
                json!(spell(gross)),
            );
            self.agree_bool(
                "risk",
                &format!("{where_} clears the profit floor"),
                &recomputed["clears_the_profit_floor"],
                clears,
            );
            self.agree_bool(
                "risk",
                &format!("{where_} within the gas ceiling"),
                &recomputed["within_the_gas_ceiling"],
                within_gas,
            );
            self.agree(
                "risk",
                &format!("{where_} simulation age"),
                &recomputed["simulation_age"],
                simulation_age.map(Value::from).unwrap_or(Value::Null),
            );
            self.agree_bool(
                "risk",
                &format!("{where_} within the simulation age"),
                &recomputed["within_the_simulation_age"],
                within_simulation_age,
            );
            self.agree(
                "risk",
                &format!("{where_} state age"),
                &recomputed["state_age"],
                state_age.map(Value::from).unwrap_or(Value::Null),
            );
            self.agree_bool(
                "risk",
                &format!("{where_} within the state age"),
                &recomputed["within_the_state_age"],
                within_state_age,
            );
            self.agree_bool(
                "risk",
                &format!("{where_} executor matches"),
                &recomputed["executor_matches"],
                executor_matches,
            );
            self.agree_bool(
                "risk",
                &format!("{where_} chain matches"),
                &recomputed["chain_matches"],
                chain_matches,
            );
            self.agree_bool(
                "risk",
                &format!("{where_} guard met by the delivery"),
                &recomputed["guard_is_met_by_the_delivery"],
                guard_met,
            );
            self.agree_bool(
                "risk",
                &format!("{where_} accepted"),
                &published["accepted"],
                accepted,
            );
            self.agree(
                "risk",
                &format!("{where_} decision"),
                &published["decision"],
                json!(if accepted { "accept" } else { "reject" }),
            );

            // An acceptance carries its figures; a rejection carries the check that refused and no
            // figures at all. Publishing a filled-in acceptance beside `reject` would be §28's
            // boundary crossed in words.
            if accepted {
                let figures = &published["figures"];
                self.agree(
                    "risk",
                    &format!("{where_} figures input"),
                    &figures["input_amount"],
                    json!(dec(input)),
                );
                self.agree(
                    "risk",
                    &format!("{where_} figures delivered"),
                    &figures["delivered"],
                    opt_dec(delivered),
                );
                self.agree(
                    "risk",
                    &format!("{where_} figures gross"),
                    &figures["gross_profit"],
                    json!(spell(gross)),
                );
                self.agree(
                    "risk",
                    &format!("{where_} figures floor"),
                    &figures["minimum_gross_profit"],
                    json!(dec(floor)),
                );
                self.agree(
                    "risk",
                    &format!("{where_} figures guard"),
                    &figures["min_final_output"],
                    json!(dec(guard)),
                );
                self.agree(
                    "risk",
                    &format!("{where_} figures gas used"),
                    &figures["gas_used"],
                    Value::from(gas_used),
                );
                self.agree(
                    "risk",
                    &format!("{where_} figures ceiling"),
                    &figures["maximum_gas"],
                    Value::from(maximum_gas),
                );
                self.agree(
                    "risk",
                    &format!("{where_} figures block"),
                    &figures["simulation_block"],
                    Value::from(simulation_block),
                );
                self.agree(
                    "risk",
                    &format!("{where_} figures head"),
                    &figures["head"],
                    head.map(Value::from).unwrap_or(Value::Null),
                );
                self.agree(
                    "risk",
                    &format!("{where_} figures state version"),
                    &figures["state_version"],
                    state_version.map(Value::from).unwrap_or(Value::Null),
                );
                self.agree(
                    "risk",
                    &format!("{where_} check is empty"),
                    &published["check"],
                    Value::Null,
                );
                self.agree(
                    "risk",
                    &format!("{where_} reason is empty"),
                    &published["reason"],
                    Value::Null,
                );
            } else {
                self.agree(
                    "risk",
                    &format!("{where_} figures absent"),
                    &published["figures"],
                    Value::Null,
                );
                if published["check"].is_null() {
                    self.note("risk", format!("{where_}: a rejection that names no check cannot be grouped by the rule that bit"));
                }
            }

            // The bill, read back out of the charge's own words and required to be the burn times
            // the two fees the same sentence names (§24: beside the gross, never inside it).
            let charge = field_str(run, "gas_charge").unwrap_or_default();
            match charge_figures(&charge) {
                Ok((used_word, effective, expected_price, wei)) => {
                    if used_word != gas_used {
                        self.note("risk", format!("{where_}: the charge says {used_word} gas and the row says {gas_used}"));
                    }
                    if effective != expected_price {
                        self.note("risk", format!("{where_}: the charge's price {effective} is not base plus tip {expected_price}"));
                    }
                    let bill = u128::from(gas_used)
                        .checked_mul(effective)
                        .map(|bill| bill.to_string());
                    if bill != Some(wei.to_string()) {
                        self.note(
                            "risk",
                            format!(
                                "{where_}: the charge's wei {wei} is not {} × {effective}",
                                gas_used
                            ),
                        );
                    }
                    let published_bill = published["figures"]["gas_charge_wei"]
                        .as_str()
                        .map(str::to_string);
                    if published_bill != Some(wei.to_string()) && accepted {
                        self.note(
                            "risk",
                            format!(
                                "{where_}: the acceptance quotes {:?} and the charge says {}",
                                published_bill, wei
                            ),
                        );
                    }
                }
                Err(e) => self.note("risk", format!("{where_}: {e}")),
            }

            // §41's route, spelled the way production's `Debug` derive spells it, has to be the
            // same cycle the pricing rows priced — the identity is the join between these two files.
            let edges = field_str(&row, "route_identity")?;
            match parse_debug_identity(&edges) {
                Ok((chain, parsed)) => {
                    let policy_chain = field_u64(policy, "chain_id")?;
                    if chain != policy_chain {
                        self.note("risk", format!("{where_}: the route is on chain {chain} and the policy is on {policy_chain}"));
                    }
                    let (canonical, _) = min_rotation(&parsed);
                    if canonical != parsed {
                        self.note("risk", format!("{where_}: the published route identity is not the canonical rotation of its own edges"));
                    }
                    let identity = identity_string("RouteIdentity", chain, &canonical);
                    if identity != edges {
                        self.note(
                            "risk",
                            format!("{where_}: the identity re-spells as {identity}"),
                        );
                    }
                    match pricing.get(&identity) {
                        None => self.note("risk", format!("{where_}: its route matches no pricing row, so the floor was asked of an unpriced cycle")),
                        Some(facts) => {
                            if facts.input != input {
                                self.note("risk", format!("{where_}: the row's input {} is not the priced point {}", dec(input), dec(facts.input)));
                            }
                        }
                    }
                }
                Err(e) => self.note("risk", format!("{where_}: {e}")),
            }

            table.push(json!({
                "row": where_,
                "case": row["case"],
                "input": dec(input),
                "delivered": opt_dec(delivered),
                "gross_recomputed": spell(gross),
                "floor": dec(floor),
                "accepted_recomputed": accepted,
                "decision_published": published["decision"],
                "check_published": published["check"],
            }));
        }
        self.check("6 risk", json!({
            "layer": "risk",
            "rows": table.len(),
            "comparisons": "the six §26–§29 makes, re-made here from the figures the row carries, \
                            plus the delivery and the guard. A rejection row is expected to have \
                            no figures beside it.",
            "charge": "gas_used × (base + tip), read out of the charge's own words",
            "runs": table,
        }));
        Ok(())
    }
}

/// A `RouteIdentity([EdgeId { pool: PoolId { chain_id: ChainId(N), address: 0x… }, …])` word,
/// parsed back into the edges it names.
///
/// Only the shape this milestone publishes is accepted: an unexpected word refuses rather than
/// guessing, because a risk row whose route this gate cannot read is a row whose floor question
/// cannot be checked.
fn parse_debug_identity(text: &str) -> Result<(u64, Vec<Edge>), String> {
    let body = text
        .strip_prefix("RouteIdentity([")
        .and_then(|body| body.strip_suffix("])"))
        .ok_or_else(|| "the route identity is not spelled as a RouteIdentity list".to_string())?;
    let mut chain: Option<u64> = None;
    let mut edges = Vec::new();
    for chunk in body
        .split("EdgeId { ")
        .filter(|chunk| !chunk.trim().is_empty())
    {
        let take = |label: &str| -> Result<(u64, String), String> {
            let at = chunk
                .find(label)
                .ok_or_else(|| format!("an edge without a {label} field"))?;
            let tail = &chunk[at + label.len()..];
            // Each label ends inside `ChainId(`, so the chain number is the run of digits that
            // follows it — not the digits after the next `(`, which is a later field's chain.
            let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
            let chain: u64 = digits
                .parse()
                .map_err(|_| format!("{label} carries no chain number"))?;
            let address_at = tail
                .find("address: 0x")
                .ok_or_else(|| format!("{label} carries no address"))?;
            let address = tail[address_at + "address: ".len()..]
                .chars()
                .take(42)
                .collect::<String>();
            addr_of(&address)
                .ok_or_else(|| format!("{label} address {address:?} is not 20 bytes"))?;
            Ok((chain, address))
        };
        let (pool_chain, pool) = take("pool: PoolId { chain_id: ChainId(")?;
        let (in_chain, token_in) = take("token_in: TokenId { chain_id: ChainId(")?;
        let (out_chain, token_out) = take("token_out: TokenId { chain_id: ChainId(")?;
        if pool_chain != in_chain || in_chain != out_chain {
            return Err("one edge names three different chains".to_string());
        }
        match chain {
            None => chain = Some(pool_chain),
            Some(seen) if seen != pool_chain => {
                return Err("the route spans two chains".to_string())
            }
            _ => {}
        }
        edges.push(Edge {
            pool,
            token_in,
            token_out,
        });
    }
    let chain = chain.ok_or_else(|| "an empty route identity names no cycle".to_string())?;
    Ok((chain, edges))
}

// ---------------------------------------------------------------------------
// Phase 7 — lanes: §32's arrows and §35's additions, re-checked step by step
// ---------------------------------------------------------------------------

/// The lane this gate is tracking, one per `lane_id` the ledger opened.
#[derive(Clone, Debug)]
struct LaneTrack {
    candidate_id: String,
    state: LaneState,
    moves: usize,
    nonce: Option<(String, u64, String)>,
    capital: Option<U256>,
    /// The amount this lane reserved at some point, kept after the reservation is handed back: a
    /// settled lane's capital is gone from it and lives in the pool's settled half.
    claimed: Option<U256>,
    plan_hash: Option<String>,
    simulation_id: Option<String>,
}

/// A state word as the rows spell it, resolved through production's own list rather than a table
/// written here: `code()` is the spelling the rows were built from, so a word that maps to no
/// state is a row this gate cannot follow, and that is said out loud.
fn lane_state(word: &str) -> Result<LaneState, String> {
    LaneState::ALL
        .into_iter()
        .find(|state| state.code() == word)
        .ok_or_else(|| format!("{word:?} is not one of §32's thirteen states"))
}

/// The domain object, as opposed to a reservation pair: only the pool carries a capacity.
fn is_domain(value: &Value) -> bool {
    value.get("capacity").is_some()
}

impl Gate {
    /// The lane matrix: every step's capital addition re-added here, every arrow looked up in
    /// §32's table, and the ledger's end reconciled against its own beginning.
    ///
    /// The ledger is not replayed — the script would be the writer's script again, and a second
    /// copy of a mistake agrees with the first. What is checked instead is what a mistake cannot
    /// survive: the pool's two halves summing to the capacity at every step, an arrow that is in
    /// the table, and a reservation that no other lane also holds.
    fn lanes(&mut self) -> Result<(), String> {
        let rel = "data/evidence/m11/lanes/lane_matrix.json";
        let mut table = Vec::new();
        for (id, row) in self.rows(rel)? {
            let key = Gate::row_key(rel, &id);
            let where_ = key.as_str();
            let domain = &row["domain"];
            let domain_id = field_str(domain, "domain_id")?;
            let capacity = field_u256(domain, "capacity_at_open")?;
            let claim = field_u256(domain, "amount_each_lane_claims")?;

            let mut lanes: BTreeMap<u64, LaneTrack> = BTreeMap::new();
            let mut last_winner: Option<u64> = None;
            let mut previous_settled = U256::ZERO;
            let mut capital_steps = 0usize;
            let mut arrow_steps = 0usize;
            let mut refusal_arms: BTreeSet<String> = BTreeSet::new();

            for (index, step) in array(&row, "steps")?.iter().enumerate() {
                let spot = format!("{where_} step {index}");
                // The lane snapshot is read before the pool: `lane_after` and `capital_after` are
                // two views of one action, and the pool's reserved half is only recomputable once
                // the reservation the same step just granted is visible to it.
                for field in ["lane", "lane_after"] {
                    let value = &step[field];
                    if !value.is_object() {
                        continue;
                    }
                    let lane_id = field_u64(value, "lane_id")?;
                    let state = lane_state(&field_str(value, "state")?)?;
                    let track = match lanes.get(&lane_id) {
                        Some(track) => track.clone(),
                        None => LaneTrack {
                            candidate_id: field_str(value, "candidate_id")?,
                            state: LaneState::Created,
                            moves: 0,
                            nonce: None,
                            capital: None,
                            claimed: None,
                            plan_hash: None,
                            simulation_id: None,
                        },
                    };
                    if track.candidate_id != field_str(value, "candidate_id")? {
                        self.note(
                            "lanes",
                            format!("{spot}: lane {lane_id} changes candidate mid-ledger"),
                        );
                    }
                    let moved = state != track.state;
                    arrow_steps += 1;
                    if moved {
                        if !track.state.allows(state) {
                            self.note(
                                "lanes",
                                format!(
                                    "{spot}: {} → {} is not an arrow in §32's table",
                                    track.state.code(),
                                    state.code()
                                ),
                            );
                        }
                        // §34's promise is about a refusal: the rule that says no moves the lane
                        // nowhere. A control arm that ends a lane by handing its pair back is not a
                        // refusal, so the test is whether this step published a refusal at all, not
                        // whether it published a control name.
                        if step.get("refusal").is_some() {
                            self.note(
                                "lanes",
                                format!(
                                    "{spot}: a refusal moved lane {lane_id} to {}",
                                    state.code()
                                ),
                            );
                        }
                    } else if step.get("action").is_some()
                        && step["action"] != "open"
                        && step["action"] != "settle"
                    {
                        let expected = field_str(step, "to").ok();
                        if let Some(to) = expected {
                            self.note(
                                "lanes",
                                format!(
                                    "{spot}: the step advances to {to} and the lane is still in {}",
                                    state.code()
                                ),
                            );
                        }
                    }
                    let history = array(value, "history")?;
                    if history.len() != track.moves + usize::from(moved) {
                        self.note("lanes", format!("{spot}: lane {lane_id} has {} history entries and this gate counted {}", history.len(), track.moves + usize::from(moved)));
                    }
                    if let Some(last) = history.last() {
                        if field_str(last, "to")? != state.code() {
                            self.note("lanes", format!("{spot}: lane {lane_id}'s history ends at {} and its state is {}", last["to"], state.code()));
                        }
                        if moved && field_str(last, "from")? != track.state.code() {
                            self.note("lanes", format!("{spot}: lane {lane_id}'s newest arrow starts from {} and this gate left it in {}", last["from"], track.state.code()));
                        }
                        if let Some(note) = step.get("note").and_then(Value::as_str) {
                            if field_str(last, "note")? != note {
                                self.note("lanes", format!("{spot}: the step's note and the history entry's note differ"));
                            }
                        }
                    }
                    let mut next = track.clone();
                    next.state = state;
                    next.moves += usize::from(moved);
                    if let Some(nonce) = value.get("nonce") {
                        if nonce.is_object() {
                            next.nonce = Some((
                                field_str(nonce, "signer")?,
                                field_u64(nonce, "nonce")?,
                                field_str(nonce, "stage")?,
                            ));
                        }
                    }
                    if let Some(capital) = value.get("capital") {
                        if capital.is_object() {
                            let amount = field_u256(capital, "amount")?;
                            next.capital = Some(amount);
                            next.claimed = Some(amount);
                        } else {
                            next.capital = None;
                        }
                    }
                    for (field, slot) in [
                        ("plan_hash", &mut next.plan_hash),
                        ("simulation_id", &mut next.simulation_id),
                    ] {
                        match &value[field] {
                            Value::Null => {}
                            value => {
                                let text = value
                                    .as_str()
                                    .map(str::to_string)
                                    .ok_or_else(|| format!("{spot}: {field} is not a string"))?;
                                if slot.is_some() && slot.as_deref() != Some(text.as_str()) {
                                    self.note(
                                        "lanes",
                                        format!(
                                            "{spot}: lane {lane_id} changes its {field} mid-ledger"
                                        ),
                                    );
                                }
                                *slot = Some(text);
                            }
                        }
                    }
                    // §34's pairing: a lane past the reservation holds both halves, and a lane that
                    // ended by something other than a block holds neither.
                    // §34's pairing, read off production's own predicate: a lane from `reserved`
                    // through `included` holds both halves, a settled lane has committed its number
                    // and given its capital back, and a lane that ended any other way holds neither.
                    if let Some(nonce) = value.get("nonce") {
                        if nonce.is_null() {
                            next.nonce = None;
                        }
                    }
                    if let Some(capital) = value.get("capital") {
                        if capital.is_null() {
                            next.capital = None;
                        }
                    }
                    let holds_both = state.holds_nonce();
                    if holds_both != next.capital.is_some() {
                        self.note("lanes", format!("{spot}: lane {lane_id} is in {} and the capital it carries says otherwise", state.code()));
                    }
                    let holds_a_number = holds_both || state == LaneState::Settled;
                    if holds_a_number != next.nonce.is_some() {
                        self.note("lanes", format!("{spot}: lane {lane_id} is in {} and the nonce it carries says otherwise", state.code()));
                    }
                    if state == LaneState::Settled {
                        let stage = next
                            .nonce
                            .as_ref()
                            .map(|(_, _, stage)| stage.as_str())
                            .unwrap_or("absent");
                        if stage != "committed" {
                            self.note("lanes", format!("{spot}: a settled lane's nonce stage is {stage:?}, which §34 says is commit"));
                        }
                    }
                    if state.is_terminal()
                        && state != LaneState::Settled
                        && (next.nonce.is_some() || next.capital.is_some())
                    {
                        self.note(
                            "lanes",
                            format!(
                                "{spot}: a lane that ended in {} still carries a reservation",
                                state.code()
                            ),
                        );
                    }
                    lanes.insert(lane_id, next);
                }

                for field in ["capital", "capital_after"] {
                    let value = &step[field];
                    if !value.is_object() || !is_domain(value) {
                        continue;
                    }
                    capital_steps += 1;
                    let available = field_u256(value, "available_capital")?;
                    let reserved = field_u256(value, "reserved_capital")?;
                    let stated = field_u256(value, "available_plus_reserved")?;
                    self.agree(
                        "lanes",
                        &format!("{spot} sums to capacity"),
                        &value["sums_to_capacity_recomputed"],
                        Value::Bool(available + reserved == capacity),
                    );
                    self.agree_bool(
                        "lanes",
                        &format!("{spot} invariant holds"),
                        &value["invariant_holds"],
                        available + reserved == capacity,
                    );
                    self.agree(
                        "lanes",
                        &format!("{spot} the addition"),
                        &value["available_plus_reserved"],
                        json!(dec(available + reserved)),
                    );
                    self.agree(
                        "lanes",
                        &format!("{spot} capacity"),
                        &value["capacity"],
                        json!(dec(capacity)),
                    );
                    self.agree(
                        "lanes",
                        &format!("{spot} domain id"),
                        &value["domain_id"],
                        json!(domain_id.clone()),
                    );
                    if stated != available + reserved {
                        self.note("lanes", format!("{spot}: the pair is stated as {stated} and the two halves add to {}", available + reserved));
                    }
                    let settled = field_u256(value, "settled_input")?;
                    if settled < previous_settled {
                        self.note(
                            "lanes",
                            format!(
                                "{spot}: settled capital went backwards from {} to {}",
                                dec(previous_settled),
                                dec(settled)
                            ),
                        );
                    }
                    previous_settled = settled;
                    let mut held = U256::ZERO;
                    let mut spent = U256::ZERO;
                    for track in lanes.values() {
                        if let Some(amount) = track.capital {
                            held = held.checked_add(amount).ok_or_else(|| {
                                format!("{spot}: the reservations leave 256 bits")
                            })?;
                        }
                        if track.state == LaneState::Settled {
                            // A settled lane has handed its reservation back to the pool, so what it
                            // settled is the amount it once claimed rather than what it still carries.
                            if let Some(amount) = track.claimed {
                                spent = spent.checked_add(amount).ok_or_else(|| {
                                    format!("{spot}: the settled capital leaves 256 bits")
                                })?;
                            }
                        }
                    }
                    if available + held != capacity {
                        self.note("lanes", format!("{spot}: {available} available plus the {} this gate sees reserved by a lane is not the capacity", dec(held)));
                    }
                    if settled != spent {
                        self.note("lanes", format!("{spot}: the pool says {} settled and the settled lanes carry {spent}", dec(settled)));
                    }
                }

                if let Some(arm) = step.get("control").and_then(Value::as_str) {
                    if !refusal_arms.insert(arm.to_string()) {
                        self.note(
                            "lanes",
                            format!("{spot}: the control arm {arm} appears twice"),
                        );
                    }
                    // Two shapes name their rule. A refusal names the code it refused with; an arm
                    // that ends a lane by handing the pair back names the reason and the disposition
                    // instead. A control step of either shape that says neither is a step this gate
                    // cannot attribute to a rule.
                    let refusal = step.get("refusal").is_some();
                    let named = if refusal {
                        !step
                            .get("refusal_code")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .is_empty()
                    } else {
                        step.get("reason").and_then(Value::as_str).is_some()
                            && step.get("disposition").and_then(Value::as_str).is_some()
                    };
                    if !named {
                        let asked = if refusal {
                            "a refusal code"
                        } else {
                            "a reason and a disposition"
                        };
                        self.note("lanes", format!("{spot}: {arm} ran without naming {asked}"));
                    }
                }
                if let Some(arm) = step.get("control_positive_arm").and_then(Value::as_str) {
                    refusal_arms.insert(arm.to_string());
                    // The grant is only evidence if the lane is shown holding what was granted.
                    if !step["lane_after"].is_object() {
                        self.note(
                            "lanes",
                            format!("{spot}: {arm} granted a pair and shows no lane holding it"),
                        );
                    }
                }
                if let Some(lanes_simulating) = step.get("simulating_lanes") {
                    let counted = lanes
                        .iter()
                        .filter(|(_, track)| track.state == LaneState::Simulating)
                        .map(|(lane_id, _)| Value::from(*lane_id))
                        .collect::<Vec<_>>();
                    self.agree(
                        "lanes",
                        &format!("{spot} simulating lanes"),
                        lanes_simulating,
                        json!(counted),
                    );
                    let held = lanes
                        .iter()
                        .filter(|(_, track)| track.nonce.is_some())
                        .count();
                    self.agree(
                        "lanes",
                        &format!("{spot} lanes holding a nonce"),
                        &step["lanes_holding_a_nonce"],
                        Value::from(held),
                    );
                }
                if step.get("action").and_then(Value::as_str) == Some("choose_winner") {
                    let standings = array(step, "standings")?;
                    let mut best: Option<(U256, u64)> = None;
                    for entry in &standings {
                        let gain = field_u256(entry, "gross_gain")?;
                        let lane_id = field_u64(entry, "lane_id")?;
                        let take = match &best {
                            None => true,
                            Some((top, top_lane)) => {
                                gain > *top || (gain == *top && lane_id < *top_lane)
                            }
                        };
                        if take {
                            best = Some((gain, lane_id));
                        }
                    }
                    let winner = best.map(|(_, lane_id)| lane_id);
                    self.agree(
                        "lanes",
                        &format!("{spot} winner"),
                        &step["winner"],
                        winner.map(Value::from).unwrap_or(Value::Null),
                    );
                    if winner.is_none() {
                        self.note(
                            "lanes",
                            format!("{spot}: a ranking over no standings selects a lane"),
                        );
                    }
                    last_winner = winner.or(last_winner);
                }
            }

            let final_block = &row["final"];
            let published_lanes = array(final_block, "lanes")?;
            if published_lanes.len() != lanes.len() {
                self.note(
                    "lanes",
                    format!(
                        "{where_}: the ledger ended with {} lanes and published {}",
                        lanes.len(),
                        published_lanes.len()
                    ),
                );
            }
            for published in &published_lanes {
                let lane_id = field_u64(published, "lane_id")?;
                let Some(track) = lanes.get(&lane_id) else {
                    self.note(
                        "lanes",
                        format!("{where_}: lane {lane_id} appears in the end state and in no step"),
                    );
                    continue;
                };
                self.agree(
                    "lanes",
                    &format!("{where_} lane {lane_id} state"),
                    &published["state"],
                    json!(track.state.code()),
                );
                // The end-of-ledger history is compared as a count: the entries themselves are
                // checked one by one against this gate's own transitions at every step above.
                let published_history = array(published, "history")?;
                self.agree(
                    "lanes",
                    &format!("{where_} lane {lane_id} history length"),
                    &json!(published_history.len()),
                    json!(track.moves),
                );
            }
            let outstanding = lanes
                .iter()
                .filter(|(_, track)| track.nonce.is_some())
                .count();
            self.agree(
                "lanes",
                &format!("{where_} outstanding nonces"),
                &final_block["outstanding_nonces"],
                Value::from(outstanding),
            );
            self.agree(
                "lanes",
                &format!("{where_} winner"),
                &final_block["winner"],
                last_winner.map(Value::from).unwrap_or(Value::Null),
            );
            let capital = &final_block["capital"];
            let recomputed = &row["recomputed_independently"];
            let available = field_u256(capital, "available_capital")?;
            let reserved = field_u256(capital, "reserved_capital")?;
            self.agree(
                "lanes",
                &format!("{where_} end capacity"),
                &recomputed["capacity"],
                json!(dec(capacity)),
            );
            self.agree(
                "lanes",
                &format!("{where_} end addition"),
                &recomputed["available_plus_reserved_at_the_end"],
                json!(dec(available + reserved)),
            );
            self.agree_bool(
                "lanes",
                &format!("{where_} end sums to capacity"),
                &recomputed["sums_to_capacity"],
                available + reserved == capacity,
            );
            let spent = lanes
                .iter()
                .filter(|(_, track)| track.state == LaneState::Settled)
                .filter_map(|(_, track)| track.claimed)
                .fold(U256::ZERO, |total, amount| total.saturating_add(amount));
            self.agree(
                "lanes",
                &format!("{where_} settled input"),
                &recomputed["settled_input"],
                json!(dec(spent)),
            );
            let held_by = lanes
                .iter()
                .find(|(_, track)| track.state == LaneState::Settled && track.nonce.is_some())
                .map(|(lane_id, _)| Value::from(*lane_id))
                .unwrap_or(Value::Null);
            self.agree(
                "lanes",
                &format!("{where_} nonce held by the settled lane"),
                &recomputed["nonce_held_by_the_settled_lane"],
                held_by,
            );
            let mut claims = 0usize;
            let mut live_reservations = 0usize;
            for track in lanes.values() {
                if let Some(amount) = track.claimed {
                    if amount != claim {
                        self.note("lanes", format!("{where_}: lane {} reserved {} and the domain says each lane claims {}", track.state.code(), dec(amount), dec(claim)));
                    }
                    claims += 1;
                }
                if track.capital.is_some() {
                    live_reservations += 1;
                }
            }
            table.push(json!({
                "row": where_,
                "lanes": lanes.len(),
                "steps": array(&row, "steps")?.len(),
                "capital_objects": capital_steps,
                "lane_objects": arrow_steps,
                "refusal_arms": refusal_arms.iter().cloned().collect::<Vec<_>>(),
                "outstanding_nonces": outstanding,
                "winner": final_block["winner"],
                "available": capital["available_capital"],
                "reserved": capital["reserved_capital"],
                "lanes_that_ever_reserved": claims,
                "lanes_holding_a_reservation_at_the_end": live_reservations,
            }));
        }
        self.check("7 lanes", json!({
            "layer": "lanes",
            "arrows": "each transition looked up in §32's table through LaneState::allows, the \
                       production rule the ledger itself applies",
            "capital": "available plus reserved re-added at every step that carries the pool, and \
                        checked against the reservations this gate sees held by lanes",
            "rows": table.len(),
            "runs": table,
        }));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Phase 8 — one record, two files
// ---------------------------------------------------------------------------

/// Where a chain row's stage copy is allowed to differ from the standalone row it copies:
/// `what_this_is` describes the file it sits in, and the copy's `market_claim` is a pointer at the
/// standalone row instead of a second market claim — §41 keeps one claim about one market in one
/// home, so a copy that re-spelled the claim could drift from its original unnoticed.
const COPY_PROSE: [&str; 2] = ["market_claim", "what_this_is"];

/// The pointer every chain row publishes in place of a market claim.
const MARKET_POINTER: &str = "see pricing/ for this route's market claim";

const PRICING_FILES: [&str; 3] = [
    "data/evidence/m11/pricing/recorded_2hop.json",
    "data/evidence/m11/pricing/declared_3hop.json",
    "data/evidence/m11/pricing/declared_4hop.json",
];
const OPTIMIZER_FILES: [&str; 2] = [
    "data/evidence/m11/optimizer/recorded_2hop.json",
    "data/evidence/m11/optimizer/declared_3hop.json",
];
const SIMULATION_FILES: [&str; 2] = [
    "data/evidence/m11/simulation/recorded_2hop.json",
    "data/evidence/m11/simulation/declared_3hop.json",
];
const RISK_FILE: &str = "data/evidence/m11/risk/decision.json";
const LANES_FILE: &str = "data/evidence/m11/lanes/lane_matrix.json";
const CHAIN_FILES: [&str; 2] = [
    "data/evidence/m11/controlled/2hop/chain.json",
    "data/evidence/m11/controlled/3hop/chain.json",
];
const REAL_FILES: [&str; 3] = [
    "data/evidence/m11/real/execution.json",
    "data/evidence/m11/real/failure.json",
    "data/evidence/m11/real/reconciliation.json",
];

/// `value` with `keys` dropped, so two files' copies of one record can be compared as whole
/// objects instead of field by field, with only the prose that legitimately differs excluded.
fn minus(value: &Value, keys: &[&str]) -> Value {
    let mut object = value.as_object().cloned().unwrap_or_else(Map::new);
    for key in keys {
        object.remove(*key);
    }
    Value::Object(object)
}

/// The part of `full` that `copy` claims to hold. A shortened copy is comparable, but only on the
/// keys it actually carries; a key the copy has and the original lacks reappears as `null` here so
/// the drift line names it instead of silently passing.
fn subset(copy: &Value, full: &Value) -> Value {
    let mut object = Map::new();
    if let Some(keys) = copy.as_object() {
        for key in keys.keys() {
            object.insert(key.clone(), full.get(key).cloned().unwrap_or(Value::Null));
        }
    }
    Value::Object(object)
}

impl Gate {
    /// §43's last requirement: the six figures are not enough on their own if the same record is
    /// told twice in two files and the two tellings disagree. Each controlled chain row carries a
    /// copy of the pricing, optimizer, simulation and risk records; this phase finds each copy's
    /// original by the identity the record itself publishes — a route's `RouteIdentity` text, a
    /// run's `identity_hash` — and compares the two as whole objects, then joins both to the REVM
    /// run this gate issued in its own process.
    ///
    /// Nothing here is addressed by position: a row is found by the hash of its own run, and a row
    /// id whose trailing component is not that hash (or not the plan hash, for a chain row) is
    /// itself reported as drift.
    #[allow(clippy::too_many_lines)]
    fn joins(
        &mut self,
        sims: &BTreeMap<String, Replay>,
        chain: &BTreeMap<String, ChainFacts>,
    ) -> Result<(), String> {
        // A standalone row a chain row cited, so the phase can tell an uncited record from an
        // unexplained one.
        let mut cited: BTreeSet<String> = BTreeSet::new();
        let mut table = Vec::new();

        for rel in CHAIN_FILES {
            for (id, row) in self.rows(rel)? {
                if row.get("stages").is_none() {
                    continue;
                }
                let where_ = Gate::row_key(rel, &id);
                let stages = &row["stages"];
                let opportunity = &stages["simulation"]["published"]["simulated_opportunity"];
                let sim_hash = field_str(opportunity, "identity_hash")?;
                let identity = field_str(&stages["pricing"]["published"], "identity")?;
                let facts = match chain.get(&sim_hash) {
                    Some(facts) => facts,
                    None => {
                        self.note(
                            "joins",
                            format!("{where_}: the plan phase kept no facts for run {sim_hash}"),
                        );
                        continue;
                    }
                };
                let (_, pricing_label) = facts.market_label.split_once('|').unwrap_or(("", ""));

                // The row's identity is its plan hash, spelled into the row id it was filed under.
                let tail = id.rsplit('-').next().unwrap_or_default().to_string();
                self.agree(
                    "joins",
                    &format!("{where_} row id names its plan hash"),
                    &json!(tail),
                    json!(facts.plan_hash.clone()),
                );

                let mut pricing_cited = Value::Null;
                let mut hits = Vec::new();
                for prel in PRICING_FILES {
                    for (pid, prow) in self.rows(prel)? {
                        if prow["published"]["identity"].as_str() == Some(identity.as_str()) {
                            hits.push((Gate::row_key(prel, &pid), prow));
                        }
                    }
                }
                if hits.len() != 1 {
                    self.note(
                        "joins",
                        format!(
                            "{where_}: {identity} is the identity of {} pricing rows, so the \
                                 chain row's copy has no single original to be checked against",
                            hits.len()
                        ),
                    );
                } else {
                    let (key, prow) = hits.remove(0);
                    cited.insert(key.clone());
                    pricing_cited = json!(key);
                    self.agree(
                        "joins",
                        &format!("{where_} pricing stage against its pricing row"),
                        &minus(&stages["pricing"], &COPY_PROSE),
                        minus(&prow, &COPY_PROSE),
                    );
                    self.agree(
                        "joins",
                        &format!("{where_} pricing stage is a pointer, not a second claim"),
                        &stages["pricing"]["market_claim"]["kind"],
                        json!(MARKET_POINTER),
                    );
                    // §41, one more time and from the other side: the claim the pricing row
                    // publishes has to be the label this gate derived by looking the route's
                    // addresses up in the recording the run loaded.
                    self.agree(
                        "joins",
                        &format!("{where_} pricing row's market kind against the recording"),
                        &prow["market_claim"]["kind"],
                        json!(pricing_label),
                    );
                }

                let mut simulation_cited = Value::Null;
                let mut hits = Vec::new();
                for srel in SIMULATION_FILES {
                    for (sid, srow) in self.rows(srel)? {
                        if srow["published"]["simulated_opportunity"]["identity_hash"].as_str()
                            == Some(sim_hash.as_str())
                        {
                            hits.push((Gate::row_key(srel, &sid), srow));
                        }
                    }
                }
                if hits.len() != 1 {
                    self.note(
                        "joins",
                        format!(
                            "{where_}: run {sim_hash} is filed under {} simulation rows",
                            hits.len()
                        ),
                    );
                } else {
                    let (key, srow) = hits.remove(0);
                    cited.insert(key.clone());
                    simulation_cited = json!(key.clone());
                    self.agree(
                        "joins",
                        &format!("{key} row id names its own run"),
                        &json!(key.rsplit('-').next().unwrap_or_default()),
                        json!(sim_hash.clone()),
                    );
                    self.agree(
                        "joins",
                        &format!("{where_} simulation stage against its simulation row"),
                        &minus(&stages["simulation"], &["what_this_is"]),
                        minus(&srow, &["what_this_is"]),
                    );
                    match sims.get(&sim_hash) {
                        Some(replay) => self.agree(
                            "joins",
                            &format!("{where_} copy, row and this gate's EVM are one run"),
                            &stages["simulation"]["published"]["observed"],
                            replay.observed.clone(),
                        ),
                        None => self.note(
                            "joins",
                            format!(
                                "{where_}: this gate issued no run under {sim_hash}, so the \
                                     simulation has two tellings and no third"
                            ),
                        ),
                    }
                }

                // The search stage and the pricing stage must be the same route told twice — the
                // edge list the cycle was found over, and the identity the quote is filed under.
                let edges = edge_list(&stages["search"], "edges")?;
                let (canonical, _) = min_rotation(&edges);
                let route_chain = field_u64(&stages["search"], "chain_id")?;
                self.agree(
                    "joins",
                    &format!("{where_} search edges re-spell the pricing identity"),
                    &stages["pricing"]["published"]["identity"],
                    json!(identity_string("RouteIdentity", route_chain, &canonical)),
                );

                // The optimizer family holds several policies over one route, so the join is on the
                // five figures the chain row says its candidate was searched under.
                let opt = &stages["optimizer"];
                let domain = array(opt, "domain")?;
                let mut on_the_route = 0usize;
                let mut exact: Vec<String> = Vec::new();
                for orel in OPTIMIZER_FILES {
                    for (oid, orow) in self.rows(orel)? {
                        if orow["route_identity"].as_str() != Some(identity.as_str()) {
                            continue;
                        }
                        on_the_route += 1;
                        let published = &orow["published"];
                        let same = published["best_input"] == opt["best_input"]
                            && published["best_output"] == opt["best_output"]
                            && published["strategy"] == opt["strategy"]
                            && published["evaluations"] == opt["evaluations"]
                            && published["domain_min"] == domain[0]
                            && published["domain_max"] == domain[1];
                        if same {
                            exact.push(Gate::row_key(orel, &oid));
                        }
                    }
                }
                let optimizer_cited = if on_the_route == 0 {
                    self.note(
                        "joins",
                        format!("{where_}: no optimizer row carries this route's identity"),
                    );
                    Value::Null
                } else if exact.len() != 1 {
                    self.note(
                        "joins",
                        format!(
                            "{where_}: {} optimizer rows on this route carry its five figures \
                                 ({exact:?}), so the search the candidate was run under is not \
                                 identifiable",
                            exact.len()
                        ),
                    );
                    Value::Null
                } else {
                    let key = exact.remove(0);
                    cited.insert(key.clone());
                    json!(key)
                };

                // Risk: a run can be judged more than once (an accept and the negative controls
                // planted beside it), so the join is on the accept — the decision the plan stage
                // bound into the plan.
                let mut on_the_run = 0usize;
                let mut accepts: Vec<(String, Value)> = Vec::new();
                for (rid, rrow) in self.rows(RISK_FILE)? {
                    if !rid.ends_with(&format!("-{sim_hash}")) {
                        continue;
                    }
                    on_the_run += 1;
                    if rrow["published"]["decision"].as_str() == Some("accept") {
                        accepts.push((Gate::row_key(RISK_FILE, &rid), rrow));
                    }
                }
                if accepts.len() != 1 {
                    self.note(
                        "joins",
                        format!(
                            "{where_}: {on_the_run} risk rows end in this run's identity and \
                                 {} of them accept; a plan binds one decision",
                            accepts.len()
                        ),
                    );
                } else {
                    let (key, rrow) = accepts.remove(0);
                    cited.insert(key.clone());
                    self.agree(
                        "joins",
                        &format!("{where_} risk stage against {key}"),
                        &stages["risk"]["figures"],
                        subset(&stages["risk"]["figures"], &rrow["published"]["figures"]),
                    );
                    self.agree(
                        "joins",
                        &format!("{where_} risk floor flag against {key}"),
                        &stages["risk"]["clears_the_floor"],
                        rrow["recomputed_independently"]["clears_the_profit_floor"].clone(),
                    );
                    self.agree(
                        "joins",
                        &format!("{where_} risk gross against {key}"),
                        &stages["risk"]["gross_by_arithmetic"],
                        rrow["recomputed_independently"]["gross_profit"].clone(),
                    );
                    if let Some(replay) = sims.get(&sim_hash) {
                        let run = &rrow["run"];
                        self.agree(
                            "joins",
                            &format!("{key} gas against this gate's EVM"),
                            &run["gas_used"],
                            json!(replay.gas_used),
                        );
                        self.agree(
                            "joins",
                            &format!("{key} delivery against this gate's EVM"),
                            &run["final_amount"],
                            opt_dec(replay.delivered),
                        );
                        self.agree(
                            "joins",
                            &format!("{key} status against this gate's EVM"),
                            &run["status"],
                            json!(replay.status_word),
                        );
                        self.agree(
                            "joins",
                            &format!("{key} guard against this gate's EVM"),
                            &run["min_final_output"],
                            json!(dec(replay.min_final_amount)),
                        );
                        self.agree(
                            "joins",
                            &format!("{key} block against this gate's EVM"),
                            &run["simulation_block"],
                            json!(replay.block_number),
                        );
                        self.agree(
                            "joins",
                            &format!("{key} executor against this gate's EVM"),
                            &run["executor"],
                            replay.run_spec["executor"].clone(),
                        );
                    }
                }

                table.push(json!({
                    "row": where_,
                    "run_identity": sim_hash,
                    "pricing_row": pricing_cited,
                    "simulation_row": simulation_cited,
                    "optimizer_row": optimizer_cited,
                    "optimizers_on_the_route": on_the_route,
                    "risk_rows_on_the_run": on_the_run,
                    "plan_hash": facts.plan_hash,
                    "calldata_hash": facts.calldata_hash,
                    "route_id": facts.route_id,
                }));
            }
        }

        // The lane matrix is the one record in this directory that carries no run: its lanes cite
        // placeholder plan hashes. A lane that named a controlled chain's plan would be claiming a
        // run the matrix never published, so the non-join is checked rather than assumed.
        let mut lane_plans = BTreeSet::new();
        for (lid, lrow) in self.rows(LANES_FILE)? {
            let where_ = Gate::row_key(LANES_FILE, &lid);
            for step in array(&lrow, "steps")? {
                if let Some(plan) = step
                    .get("lane")
                    .and_then(|lane| lane.get("plan_hash"))
                    .and_then(Value::as_str)
                {
                    lane_plans.insert(plan.to_string());
                    if chain.values().any(|facts| facts.plan_hash == plan) {
                        self.note(
                            "joins",
                            format!(
                                "{where_}: a lane carries {plan}, which is a controlled \
                                     chain's plan hash, while the lane publishes no run"
                            ),
                        );
                    }
                }
            }
        }

        // A pricing or simulation record that no chain row cites has to be explained by the
        // manifest's own `not_measured` list; otherwise the directory publishes an arithmetic
        // nobody ran end to end without saying so.
        let manifest = self.file(MANIFEST_FILE)?.clone();
        let not_measured = array(&manifest, "not_measured")?
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let mut orphans = Vec::new();
        for rel in PRICING_FILES.into_iter().chain(SIMULATION_FILES) {
            let named_in_the_manifest = not_measured.contains(rel);
            for (id, _) in self.rows(rel)? {
                let key = Gate::row_key(rel, &id);
                if !cited.contains(&key) && !named_in_the_manifest {
                    orphans.push(key);
                }
            }
        }
        for key in &orphans {
            self.note(
                "joins",
                format!("{key} is cited by no controlled chain and named by no not_measured item"),
            );
        }

        self.check("8 joins", json!({
            "layer": "one record, two files",
            "rule": "a chain row's stage copy must equal the standalone row it points at, whole, \
                     except the file's own prose; a copy is found by the route identity or the \
                     run's identity hash it publishes, never by a position in a directory",
            "three_way": "the chain copy, the standalone row and the observed block of the run \
                          this gate issued in this process must be one record",
            "lane_plan_hashes": lane_plans.len(),
            "uncited_records": orphans,
            "rows": table,
        }));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Phase 9 — what the directory refuses to answer
// ---------------------------------------------------------------------------

/// Every JSON number under `node`. A `real/` row that carries one carries a verdict, and §41 says
/// this milestone has none: an unasked question does not have an answer of `0`.
fn numbers_in(node: &Value) -> usize {
    match node {
        Value::Number(_) => 1,
        Value::Array(items) => items.iter().map(numbers_in).sum(),
        Value::Object(object) => object.values().map(numbers_in).sum(),
        _ => 0,
    }
}

impl Gate {
    /// §42's three questions about the real chain, and §41's boundary drawn on every row that is
    /// not real market state. This is the phase that fails if a future assembly turns `UNKNOWN`
    /// into a figure or copies a controlled row's profit into a real row: it asks each `real/` row
    /// for its verdict, for the reason it has one, for the files it points a reader at, and for a
    /// number it must not carry.
    fn unknown(&mut self) -> Result<(), String> {
        let mut questions = Vec::new();
        let mut table = Vec::new();
        for rel in REAL_FILES {
            for (id, row) in self.rows(rel)? {
                let where_ = Gate::row_key(rel, &id);
                self.agree(
                    "unknown",
                    &format!("{where_} verdict"),
                    &row["verdict"],
                    json!("UNKNOWN"),
                );
                let mut words = Vec::new();
                for key in [
                    "what_this_is",
                    "what_would_answer_it",
                    "not_measured_because",
                    "why_not_zero",
                ] {
                    let text = field_str(&row, key)?;
                    if text.trim().is_empty() {
                        self.note("unknown", format!("{where_}: {key} says nothing"));
                    }
                    words.push(key.to_string());
                }
                let why = field_str(&row, "why_not_zero")?;
                if !why.contains("§41") || !why.contains("0 is a verdict") {
                    self.note(
                        "unknown",
                        format!("{where_}: why_not_zero does not name §41 or that 0 is a verdict"),
                    );
                }
                let question = field_str(&row, "what_this_is")?;
                if questions.contains(&question) {
                    self.note(
                        "unknown",
                        format!("{where_} asks a question another real row already asks"),
                    );
                }
                questions.push(question);
                let numbers = numbers_in(&row);
                if numbers != 0 {
                    self.note(
                        "unknown",
                        format!(
                            "{where_}: an unasked question is answered with {numbers} number(s)"
                        ),
                    );
                }
                let mut named = 0usize;
                let mut present = 0usize;
                match row["evidence_that_does_exist_for_the_mechanism"].as_object() {
                    None => self.note(
                        "unknown",
                        format!("{where_} names no evidence for the mechanism"),
                    ),
                    Some(object) => {
                        for (label, path) in object {
                            if label == "note" {
                                continue;
                            }
                            let text = path.as_str().unwrap_or_default();
                            named += 1;
                            if !text.is_empty() && self.root.join(text).exists() {
                                present += 1;
                            } else {
                                self.note(
                                    "unknown",
                                    format!(
                                        "{where_}: {label} points at {text:?}, which is not a \
                                             file a reader can open"
                                    ),
                                );
                            }
                        }
                    }
                }
                table.push(json!({
                    "row": where_,
                    "verdict": row["verdict"],
                    "fields": words,
                    "numbers_in_the_row": numbers,
                    "evidence_named": named,
                    "evidence_that_opens": present,
                }));
            }
        }
        if questions.len() != 3 {
            self.note(
                "unknown",
                format!(
                    "§42 asks three questions of the real chain; the directory asks {}",
                    questions.len()
                ),
            );
        }

        // The manifest's own disclaimers, and the reasons behind each unmeasured item.
        let manifest = self.file(MANIFEST_FILE)?.clone();
        let means = field_str(&manifest, "verdict_means")?;
        if !means.contains("§41") || !means.contains("zero") {
            self.note(
                "unknown",
                "manifest verdict_means does not say why the answer is not reported as zero",
            );
        }
        let not_claimed = array(&manifest, "not_claimed")?
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>()
            .join(" | ");
        for phrase in [
            "real-market",
            "CONTROLLED_FIXTURE",
            "transaction hash",
            "private key",
        ] {
            if !not_claimed.contains(phrase) {
                self.note(
                    "unknown",
                    format!("the manifest disclaims nothing about {phrase:?}, which §41 and §50 require"),
                );
            }
        }
        for (index, item) in array(&manifest, "not_measured")?.iter().enumerate() {
            let name = field_str(item, "item")?;
            if field_str(item, "why")?.trim().is_empty() {
                self.note(
                    "unknown",
                    format!("not_measured[{index}] ({name}) gives no reason"),
                );
            }
            let text = item.to_string();
            if !text.contains("data/evidence/")
                && !text.contains("crates/")
                && !text.contains("fixtures/")
            {
                self.note(
                    "unknown",
                    format!(
                        "not_measured[{index}] ({name}) points a reader at nothing they can check"
                    ),
                );
            }
        }

        // §41's other half: a row that is not real market state has to say both what it does prove
        // and what it does not, and the one row over recorded pools has to carry the reading §41
        // forbids as well as the attestation behind it.
        for rel in PRICING_FILES {
            for (id, row) in self.rows(rel)? {
                let where_ = Gate::row_key(rel, &id);
                let claim = &row["market_claim"];
                let kind = field_str(claim, "kind")?;
                let required: &[&str] = match kind.as_str() {
                    "CONTROLLED_FIXTURE" | "DECLARED_SYNTHETIC_GRAPH" => {
                        &["proves", "does_not_prove"]
                    }
                    "REAL_MARKET_POOLS_ON_A_CONTROLLED_FIXTURE_STATE" => {
                        &["attested_by", "forbidden_reading"]
                    }
                    "REAL_MARKET" => &["attested_by"],
                    other => {
                        self.note(
                            "unknown",
                            format!(
                                "{where_}: market kind {other:?} is one this gate has no rule for"
                            ),
                        );
                        &[]
                    }
                };
                for key in required {
                    match claim.get(*key) {
                        None => self.note(
                            "unknown",
                            format!("{where_}: a {kind} row carries no {key}"),
                        ),
                        Some(value) if value.as_str().unwrap_or_default().trim().is_empty() => {
                            self.note("unknown", format!("{where_}: {key} says nothing"))
                        }
                        Some(_) => {}
                    }
                }
            }
        }

        let readme = read_text(&self.root, README_FILE)?;
        for phrase in ["UNKNOWN", "§41", "§42", "rpc_count", "real-market verdict"] {
            if !readme.contains(phrase) {
                self.note("unknown", format!("README.md never says {phrase:?}"));
            }
        }

        self.check("9 unknown", json!({
            "layer": "§41's boundary",
            "real_questions": questions.len(),
            "rule": "a real/ row may name files a reader can open and say why the answer is not \
                     zero; it may not carry a number",
            "rows": table,
        }));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Phase 10 — §44, §48 and §50 over the whole directory, not row by row
// ---------------------------------------------------------------------------

/// Every value filed under the key `wanted` anywhere under `node`, with the path it was found at
/// and a copy of the object carrying it — the directory-wide form of a field check, so this gate
/// can ask how many `rpc_count` fields exist at all rather than only what the two rows it knows
/// about publish. Values are copied out so a scan over fifteen parsed files does not have to keep
/// all fifteen alive while the phase reports.
fn dig(node: &Value, wanted: &str, path: &str, out: &mut Vec<(String, Value, Value)>) {
    match node {
        Value::Object(object) => {
            for (key, value) in object {
                let here = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}/{key}")
                };
                if key == wanted {
                    out.push((here.clone(), value.clone(), node.clone()));
                }
                dig(value, wanted, &here, out);
            }
        }
        Value::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                dig(value, wanted, &format!("{path}/{index}"), out);
            }
        }
        _ => {}
    }
}

/// Every string under `node` the predicate keeps, with its path.
fn strings_under(node: &Value, path: &str, keep: &dyn Fn(&str) -> bool, out: &mut Vec<String>) {
    match node {
        Value::String(text) => {
            if keep(text) {
                out.push(format!("{path}: {}", compact(&json!(text))));
            }
        }
        Value::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                strings_under(value, &format!("{path}/{index}"), keep, out);
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                strings_under(value, &format!("{path}/{key}"), keep, out);
            }
        }
        _ => {}
    }
}

/// §50's key shape: sixty-four bare lower-case hex characters.
///
/// Two shapes are deliberately *not* included, and `secret_shape_control` measures both rather than
/// assuming them. A `0x`-prefixed 64-hex string is the shape every published digest has — this
/// directory carries one under keys like `plan_hash` and `keccak256` — so widening the rule to that
/// form would bury a real key under a hundred false witnesses. And because the rule demands
/// lower-case, a hex string with any capital letter passes unseen: the control counts that
/// population in the directory too, so the scan's blind spots are published, not inferred.
fn is_secret_shape(text: &str) -> bool {
    text.len() == 64
        && text.chars().all(|c| c.is_ascii_hexdigit())
        && text.chars().all(|c| !c.is_ascii_uppercase())
}

/// Every non-integer JSON number under `node` — §48 forbids a float in the financial core, and the
/// evidence directory is where one would surface if the code that wrote it had one.
fn floats_under(node: &Value, path: &str, out: &mut Vec<String>) {
    match node {
        Value::Number(number) if number.is_f64() => {
            out.push(format!("{path}: {number}"));
        }
        Value::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                floats_under(value, &format!("{path}/{index}"), out);
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                floats_under(value, &format!("{path}/{key}"), out);
            }
        }
        _ => {}
    }
}

impl Gate {
    /// §44 (no network), §48 (no float) and §50 (no secret), asked of all sixteen files at once.
    ///
    /// The endpoint variable is allowed to be *named* — every simulation row proves its own run
    /// cost no request by declaring the variable absent — so the check is a form check on one
    /// corpus: each mention of `"GIWA_RPC_URL"` must sit in the `"name"` field of an
    /// `endpoint_variable` block that also says it was not present. A keyword filter alone would
    /// either pass a real endpoint or fail a witness of absence.
    fn boundaries(&mut self) -> Result<(), String> {
        let provider = std::any::type_name::<DumpStateProvider>();
        let mut floats = Vec::new();
        let mut endpoints = Vec::new();
        let mut secrets = Vec::new();
        let mut rpc_counts = Vec::new();
        let mut providers = Vec::new();
        let mut flags = Vec::new();
        let mut mentions = 0usize;
        let mut witnesses = 0usize;
        let mut parsed = 0usize;

        for rel in self.hashes_before.keys() {
            let text = read_text(&self.root, rel)?;
            mentions += text.matches("\"GIWA_RPC_URL\"").count();
            witnesses += text.matches("\"name\": \"GIWA_RPC_URL\"").count();
            if !rel.ends_with(".json") {
                continue;
            }
            parsed += 1;
            let value = read_json(&self.root, rel)?;
            floats_under(&value, rel, &mut floats);
            strings_under(
                &value,
                rel,
                &|text| {
                    ["http://", "https://", "ws://", "wss://"]
                        .iter()
                        .any(|scheme| text.contains(scheme))
                },
                &mut endpoints,
            );
            strings_under(&value, rel, &is_secret_shape, &mut secrets);
            dig(&value, "rpc_count", rel, &mut rpc_counts);
            dig(&value, "provider_type", rel, &mut providers);
            dig(&value, "present_in_the_assembling_process", rel, &mut flags);
        }

        if mentions != witnesses {
            self.note(
                "rpc",
                format!(
                    "the endpoint variable is mentioned {mentions} times across the directory and \
                     declared absent {witnesses} times, so a mention is not a witness"
                ),
            );
        }
        let mut nonzero = Vec::new();
        for (path, value, _) in &rpc_counts {
            if *value != json!(0u64) {
                nonzero.push(format!("{path}: {}", compact(value)));
            }
        }
        let mut wrong_provider = Vec::new();
        for (path, value, _) in &providers {
            if value.as_str() != Some(provider) {
                wrong_provider.push(format!("{path}: {}", compact(value)));
            }
        }
        let mut endpoint_present = Vec::new();
        for (path, value, parent) in &flags {
            let named = parent
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if *value != json!(false) || named != "GIWA_RPC_URL" {
                endpoint_present.push(format!("{path}: {named}={}", compact(value)));
            }
        }
        for line in &nonzero {
            self.note("rpc", format!("{line} is not zero"));
        }
        for line in &wrong_provider {
            self.note(
                "rpc",
                format!(
                    "{line} is not the provider type this process loaded a state from ({provider})"
                ),
            );
        }
        for line in &endpoint_present {
            self.note("rpc", format!("{line} says an endpoint was present"));
        }
        for line in &endpoints {
            self.note("rpc", format!("{line} is a network endpoint (§44)"));
        }
        for line in &floats {
            self.note("float", format!("{line} is a floating-point number (§48)"));
        }
        for line in &secrets {
            self.note(
                "secret",
                format!(
                    "{line} is 64 bare lower-case hex characters, the shape of a key literal (§50)"
                ),
            );
        }

        self.check(
            "10 boundaries",
            json!({
                "layer": "§44 / §48 / §50",
                "files_scanned": self.hashes_before.len(),
                "json_files_parsed": parsed,
                "rpc_count_fields": rpc_counts.len(),
                "rpc_count_nonzero": nonzero,
                "provider_type_fields": providers.len(),
                "provider_type_of_this_process": provider,
                "endpoint_variable_mentions": mentions,
                "endpoint_variable_witnesses": witnesses,
                "endpoint_present_flags": flags.len(),
                "endpoint_present": endpoint_present,
                "floats": floats,
                "endpoints": endpoints,
                "secret_shaped_strings": secrets,
                "this_gate_holds_an_endpoint_variable": std::env::var("GIWA_RPC_URL").is_ok(),
            }),
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// §50 — the key-shape scan's positive control
// ---------------------------------------------------------------------------

#[cfg(test)]
mod secret_shape_control {
    //! Phase 10 reports zero key-shaped strings in the evidence directory. A zero from a scan no one
    //! has watched fire is a receipt, not a measurement, so §50 asks for a positive control: plant a
    //! key-shaped string and ask the *same* predicate the gate runs — `is_secret_shape` reached
    //! through `strings_under` — whether it sees it, at the path it was planted at.
    //!
    //! The planted string is built at run time. Sixty-four hex characters written as a source literal
    //! anywhere in `crates/execution/tests/` would trip M6's
    //! `no_private_key_is_written_into_the_code`, and the shape phase 10 hunts in the evidence is
    //! exactly the shape that guard hunts in the source.
    //!
    //! The control is two-sided. It has to catch the planted key, and it has to keep *not* catching
    //! the `0x`-prefixed 64-hex form, because that is how every published digest in this directory
    //! is spelled; widening the rule to it would trade one real key for a hundred false witnesses.
    use super::*;

    /// The planted key shape: the character `1`, sixty-four times. Not a key.
    fn planted() -> String {
        "1".repeat(64)
    }

    /// Phase 10's scan, over an in-memory tree.
    fn scan(node: &Value, path: &str, keep: &dyn Fn(&str) -> bool) -> Vec<String> {
        let mut out = Vec::new();
        strings_under(node, path, keep, &mut out);
        out
    }

    /// The manifest, with a sibling key added to its top-level object. Nothing is written to disk:
    /// the tree this operates on is the parsed copy, so the planted value can never reach evidence.
    fn manifest_with(key: &str, value: &str) -> Value {
        let mut tree = read_json(&workspace_root(), MANIFEST_FILE).expect("manifest readable");
        tree.as_object_mut()
            .expect("manifest object")
            .insert(key.to_string(), json!(value));
        tree
    }

    /// The JSON files the manifest's own `tree` names — the set phase 10 walks. Coverage is asserted
    /// here: a control that silently scanned nothing could report a clean directory forever.
    fn tree_files() -> Vec<String> {
        let manifest = read_json(&workspace_root(), MANIFEST_FILE).expect("manifest readable");
        let files = array(&manifest, "tree")
            .expect("manifest tree")
            .iter()
            .map(|rel| rel.as_str().unwrap_or_default().to_string())
            .filter(|rel| rel.ends_with(".json"))
            .collect::<Vec<_>>();
        for rel in ROW_FILES {
            assert!(
                files.iter().any(|published| published == rel),
                "the manifest tree does not publish {rel}, so this control would be scanning a \
                 different directory than the gate scans"
            );
        }
        files
    }

    /// Every witness phase 10's scan would report for `keep`, across the whole directory.
    fn witnesses(keep: &dyn Fn(&str) -> bool) -> Vec<String> {
        let root = workspace_root();
        let mut out = Vec::new();
        for rel in tree_files() {
            let value = read_json(&root, &rel).expect("evidence file readable");
            strings_under(&value, &rel, keep, &mut out);
        }
        out
    }

    /// The shape phase 10 deliberately does *not* hunt: `0x` plus sixty-four lower-case hex, which is
    /// how every digest this directory publishes is spelled.
    fn is_digest_shape(text: &str) -> bool {
        text.len() == 66
            && text.starts_with("0x")
            && text[2..].chars().all(|c| c.is_ascii_hexdigit())
    }

    #[test]
    fn a_planted_key_shape_is_seen_at_the_path_it_was_planted_at() {
        let plain = read_json(&workspace_root(), MANIFEST_FILE).expect("manifest readable");
        let before = scan(&plain, MANIFEST_FILE, &is_secret_shape);
        assert!(
            before.is_empty(),
            "the manifest already carries a key-shaped string, so this control cannot show a delta: \
             {before:?}"
        );

        let after = manifest_with("planted_secret", &planted());
        let hits = scan(&after, MANIFEST_FILE, &is_secret_shape);
        assert_eq!(
            hits.len(),
            1,
            "the planted key shape did not survive the scan that phase 10 runs: {hits:?}"
        );
        assert!(
            hits[0].starts_with(&format!("{MANIFEST_FILE}/planted_secret:")),
            "the scan caught something, but not at the planted path: {hits:?}"
        );
    }

    #[test]
    fn the_digest_shape_it_rejects_is_the_digest_shape_the_directory_publishes() {
        let with_digest = manifest_with("planted_digest", &format!("0x{}", planted()));
        assert!(
            scan(&with_digest, MANIFEST_FILE, &is_secret_shape).is_empty(),
            "a 0x-prefixed digest was counted as a key literal, which would make phase 10 fire on \
             every hash row in the directory"
        );

        let digests = witnesses(&is_digest_shape);
        assert!(
            !digests.is_empty(),
            "no 0x-prefixed digest was found in the directory, so the reason the key rule stays \
             narrow has disappeared"
        );
    }

    #[test]
    fn the_rule_is_the_shape_and_nothing_adjoining_it() {
        let mixed = format!("A{}", "1".repeat(63));
        assert!(is_secret_shape(&planted()));
        assert!(is_secret_shape(&"deadbeef".repeat(8)));
        for text in [
            "1".repeat(63),
            "1".repeat(65),
            "1".repeat(128),
            "g".repeat(64),
            " ".repeat(64),
            format!("0x{}", planted()),
            "DEADBEEF".repeat(8),
            mixed,
        ] {
            assert!(
                !is_secret_shape(&text),
                "the rule fired on {}…, which is not the shape §50 names",
                &text[..24.min(text.len())]
            );
        }
    }

    #[test]
    fn the_directory_holds_the_key_shape_under_neither_casing() {
        let any_case = |text: &str| text.len() == 64 && text.chars().all(|c| c.is_ascii_hexdigit());
        let strict = witnesses(&is_secret_shape);
        let loose = witnesses(&any_case);
        assert!(
            strict.is_empty(),
            "phase 10's published count is not what the directory holds: {strict:?}"
        );
        assert!(
            loose.is_empty(),
            "the narrow rule is not what keeps this directory clean — a case-insensitive scan finds \
             {loose:?}, so a key literal with a capital letter in it would pass unseen"
        );
    }
}

// ---------------------------------------------------------------------------
// Phase 11 — the gate's own footprint
// ---------------------------------------------------------------------------

impl Gate {
    /// The proof the header promises: every file hashed before phase 1 and again here, and the two
    /// maps compared. One differing byte anywhere would mean this gate wrote into its own inputs.
    ///
    /// The same comparison carries the determinism half this gate can witness: the sixteen digests
    /// taken at the start of eleven phases of re-reading, re-hashing and re-executing are the
    /// digests taken at the end, so the directory the phases reasoned over was one stable set of
    /// bytes. The writer's second assembly is a claim in `manifest.json` about a process this gate
    /// cannot enter, so it is published rather than verified.
    fn wrote_nothing(&mut self) -> Result<(), String> {
        let mut now = BTreeMap::new();
        let mut bytes = 0u64;
        for rel in self.hashes_before.keys() {
            let digest = digest_of_file(&self.root, rel)?;
            bytes += digest["bytes"]
                .as_u64()
                .ok_or_else(|| format!("{rel} has no byte count"))?;
            now.insert(rel.clone(), digest);
        }
        let mut changed = Vec::new();
        for (rel, before) in &self.hashes_before {
            match now.get(rel) {
                None => changed.push(format!("{rel} is gone; phase 1 read {rel}: {before}")),
                Some(after) if after != before => changed.push(format!(
                    "{rel} is not the file this gate opened: was {}, now {}",
                    digest_pair(before),
                    digest_pair(after)
                )),
                Some(_) => {}
            }
        }
        for rel in now.keys() {
            if !self.hashes_before.contains_key(rel) {
                changed.push(format!("{rel} appeared while the gate was running"));
            }
        }
        for line in &changed {
            self.note("wrote nothing", line.clone());
        }

        let manifest = self.file(MANIFEST_FILE)?.clone();
        let determinism = &manifest["determinism"];
        self.agree(
            "wrote nothing",
            "manifest assemblies_in_this_run",
            &determinism["assemblies_in_this_run"],
            json!(2u64),
        );
        self.agree_bool(
            "wrote nothing",
            "manifest byte_identical",
            &determinism["byte_identical"],
            true,
        );
        for key in ["method", "cross_process"] {
            if field_str(determinism, key)?.trim().is_empty() {
                self.note("wrote nothing", format!("determinism.{key} says nothing"));
            }
        }
        // The same scope `total_bytes` states in the manifest: the digested files, this file
        // excluded. `bytes` above is the whole tree, published beside it so the exclusion stays
        // visible here rather than only in the manifest's prose.
        let digested = manifest["file_digests"]
            .as_object()
            .ok_or_else(|| "manifest file_digests is not an object".to_string())?;
        let mut digested_bytes = 0u64;
        for rel in digested.keys() {
            let entry = now
                .get(rel)
                .ok_or_else(|| format!("{rel} is digested and no longer on disk"))?;
            digested_bytes += entry["bytes"]
                .as_u64()
                .ok_or_else(|| format!("{rel} has no byte count"))?;
        }
        self.agree(
            "wrote nothing",
            "manifest total_bytes",
            &manifest["total_bytes"],
            json!(digested_bytes),
        );

        self.check("11 wrote nothing", json!({
            "layer": "this gate's own footprint",
            "files_rehashed": now.len(),
            "bytes_of_the_tree_rehashed": bytes,
            "bytes_rehashed_over_the_digested_files": digested_bytes,
            "files_digested": digested.len(),
            "digests_taken_in_phase_1": self.hashes_before.len(),
            "changed_during_the_run": changed,
            "proves": "the sixteen files are byte-identical between the first phase and the last, \
                       so the eleven phases reasoned over a directory they never touched; the \
                       writer's second assembly is published below and not verified here, because \
                       it happened in another process",
            "determinism_claim": determinism,
        }));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The gate, end to end
// ---------------------------------------------------------------------------

/// §42–§44 as one run: eleven phases, in the order that lets each read what the one before
/// recomputed. Nothing here prints the word `PASS` — the printed table is this gate's recomputed
/// figures beside the published ones, and the assertion is the drift list, whose lines each name a
/// file, a row and a figure.
#[tokio::test]
async fn the_evidence_directory_recomputes() {
    let mut gate = Gate::new();

    gate.inventory()
        .unwrap_or_else(|e| panic!("phase 1 could not read the directory: {e}"));
    let pricing = gate
        .pricing()
        .unwrap_or_else(|e| panic!("phase 2 could not re-fold the pricing rows: {e}"));
    gate.optimizer(&pricing)
        .unwrap_or_else(|e| panic!("phase 3 could not rebuild the search: {e}"));
    let sims = gate
        .simulations(&pricing)
        .await
        .unwrap_or_else(|e| panic!("phase 4 could not replay the runs: {e}"));
    let chain = gate
        .chain_ladder(&pricing)
        .await
        .unwrap_or_else(|e| panic!("phase 5 could not rebuild the plans: {e}"));
    gate.risk(&pricing)
        .unwrap_or_else(|e| panic!("phase 6 could not re-make the risk comparisons: {e}"));
    gate.lanes()
        .unwrap_or_else(|e| panic!("phase 7 could not walk the lane ledger: {e}"));
    gate.joins(&sims, &chain)
        .unwrap_or_else(|e| panic!("phase 8 could not join the files: {e}"));
    gate.unknown()
        .unwrap_or_else(|e| panic!("phase 9 could not read the unknowns: {e}"));
    gate.boundaries()
        .unwrap_or_else(|e| panic!("phase 10 could not scan the directory: {e}"));
    gate.wrote_nothing()
        .unwrap_or_else(|e| panic!("phase 11 could not re-hash the directory: {e}"));

    for (phase, value) in &gate.report {
        println!("== {phase}");
        match serde_json::to_string_pretty(value) {
            Ok(text) => println!("{text}"),
            Err(e) => println!("<a figure this file could not render: {e}>"),
        }
    }
    let drift = gate.drift.clone();
    assert!(
        drift.is_empty(),
        "{} published figure(s) did not survive recomputation:\n{}",
        drift.len(),
        drift.join("\n")
    );
}
