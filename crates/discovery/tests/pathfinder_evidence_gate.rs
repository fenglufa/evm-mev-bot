//! M9.3 §36/§37–§41/§53/§54 — the PathFinder read against M9.2's real graph, and the
//! committed evidence directory that records what it found.
//!
//! # What this file does, in order
//!
//! It rebuilds M9.2's `GraphSnapshot` from M9.2's own committed raw records — no network,
//! no clock, no re-scan — runs the bounded cycle search over it twice, and writes
//! `data/evidence/m9/m9.3/` from what that run produced. In default mode it writes nothing
//! and compares every committed byte against a fresh rebuild.
//!
//! ```text
//! M93_EVIDENCE_REFRESH=1 cargo test -p evm-discovery --test pathfinder_evidence_gate \
//!     -- --test-threads=1
//! cargo test -p evm-discovery --test pathfinder_evidence_gate -- --test-threads=1
//! ```
//!
//! # Why the graph is rebuilt rather than read back
//!
//! `GraphSnapshot` has no deserializer, deliberately: its edges are only meaningful as the
//! output of the state layer's own gates (§28 — the search re-checks none of that, because
//! the graph could not contain it). So this gate hydrates the rows M9.2 hydrated, calls the
//! same `integrate_at_target`, and takes the graph the builder hands back. Then it checks
//! that graph against M9.2's committed edge table, triple by triple: if the two topologies
//! disagree, this directory has no business claiming it read the milestone it names.
//!
//! # Why the recomputation appears twice, in two styles
//!
//! [`Case::load`] calls the crate. The tests under *independent recompute* below call no
//! `evm_pathfinder` and no `evm_discovery` function: they read the raw run documents and
//! M9.2's committed tables as `serde_json::Value`, re-derive every count, re-check every row
//! against the graph's own edge list, and re-derive each canonical key from the recorded edge
//! sequence with a string comparator. That second half is §54's "the producer cannot
//! self-certify", and [`an_injected_wrong_number_is_caught_by_the_recompute`] proves it bites.
//!
//! # What is never written
//!
//! The endpoint, any amount, and any profit. The search layer has no field to put one in and
//! this directory does not invent one: gross, net and realized profit are recorded as `N/A`
//! with the reason (§56).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

use evm_core::{BlockNumber, ChainId};
use evm_discovery::{integrate_at_target, GraphOutcome, Reconstruction, VerifiedPool};
use evm_graph::EdgeId;
use evm_graph::GraphSnapshot;
use evm_pathfinder::{find_cycles_traced, CycleCandidate, FeeStatus, PathFinderConfig};
use evm_protocol::Registry;

const EVIDENCE_REL: &str = "data/evidence/m9/m9.3";
/// The 80 pools M9.1 verified, read out of M9.1's own raw pass. §36 forbids re-discovery,
/// so this is the same input M9.2's gate used and the same denominator it committed.
const M91_PASS_REL: &str = "data/evidence/m9/m9.1/raw/pass-a.json";
/// M9.2's reconstruction rows for pass A: the per-pool selected `Sync` and the scan that
/// covers the gap up to the target block.
const M92_PASS_REL: &str = "data/evidence/m9/m9.2/raw/reconstruction-pass-a.json";
/// M9.2's published graph table, used twice: as the topology this run must agree with, and
/// as the independent witness for which edges carry an attested fee.
const M92_GRAPH_REL: &str = "data/evidence/m9/m9.2/graph-integration.json";

const RUN_A: &str = "raw/run-a.json";
const RUN_B: &str = "raw/run-b.json";
const RAW_FILES: [&str; 2] = [RUN_A, RUN_B];

const SUMMARY: &str = "summary.json";
const CYCLES: &str = "cycles.json";
const CANDIDATE_EDGES: &str = "candidate-edges.json";
const FEE_STATUS: &str = "fee-status.json";
const DETERMINISM: &str = "determinism.json";
const BENCHMARK: &str = "benchmark.json";
const CONTROLS: &str = "negative-controls.json";
const MANIFEST: &str = "manifest.json";
const README: &str = "README.md";
/// Everything the byte gate compares, plus the two raw documents it reads.
const TABLES: [&str; 9] = [
    SUMMARY,
    CYCLES,
    CANDIDATE_EDGES,
    FEE_STATUS,
    DETERMINISM,
    BENCHMARK,
    CONTROLS,
    MANIFEST,
    README,
];
/// The content tables the manifest carries digests for: itself and the README are excluded,
/// since a file cannot hash bytes that include its own hash.
const DIGESTED: [&str; 7] = [
    SUMMARY,
    CYCLES,
    CANDIDATE_EDGES,
    FEE_STATUS,
    DETERMINISM,
    BENCHMARK,
    CONTROLS,
];
/// Every file under this directory that a reader asks for by name, so one helper can put the
/// assembly run in front of any of them.
const COMMITTED: [&str; 11] = [
    SUMMARY,
    CYCLES,
    CANDIDATE_EDGES,
    FEE_STATUS,
    DETERMINISM,
    BENCHMARK,
    CONTROLS,
    MANIFEST,
    README,
    RUN_A,
    RUN_B,
];

/// §35, with the test that carries each control. A control naming a test nobody wrote is a
/// claim, so one test below greps the source for every name listed here.
const CONTROLS_RUN: [(&str, &str, &str); 8] = [
    (
        "NC1",
        "A --P1--> B --P1--> A emits nothing",
        "t07_same_pool_reuse_is_rejected",
    ),
    (
        "NC2",
        "A -> B -> A -> C -> A emits nothing",
        "t10_repeated_intermediate_token_is_rejected",
    ),
    (
        "NC3",
        "an open path A -> B -> C emits nothing",
        "t04_open_path_yields_no_candidates",
    ),
    (
        "NC4",
        "the three rotations of one cycle are one candidate",
        "t13_rotations_are_deduplicated",
    ),
    (
        "NC5",
        "a route and its reversal stay two candidates",
        "t14_reversal_is_preserved",
    ),
    (
        "NC6",
        "one unattested fee keeps a candidate enumerable and never tradable",
        "t18_one_none_is_incomplete, t22_incomplete_cannot_be_promoted",
    ),
    (
        "NC7",
        "a candidate carries the graph's target block and no other",
        "nc7_candidates_carry_the_graphs_target_block",
    ),
    (
        "NC8",
        "the same route at two target blocks is two identities",
        "nc8_snapshots_at_different_blocks_are_distinct",
    ),
];

const PATHFINDER_TESTS: &str = "crates/pathfinder/tests/pathfinder.rs";
const PATHFINDER_CARGO: &str = "crates/pathfinder/Cargo.toml";
const DISCOVERY_CARGO: &str = "crates/discovery/Cargo.toml";

/// The only production dependencies the search layer is allowed (§42: a reader can check
/// this by opening one file). Anything else would mean the topology layer reached for a node
/// client, a decoder, or an execution engine.
const ALLOWED_PATHFINDER_DEPENDENCIES: [&str; 4] = ["evm-core", "evm-graph", "serde", "thiserror"];

/// Field-shaped words that would mean this directory is calling a cycle candidate something
/// it is not (§38/§55). Prose may name the prohibition — a key with one of these shapes in it
/// could only be a claim.
const FORBIDDEN_FIELD_SHAPES: [&str; 10] = [
    "arbitrage_opportunit",
    "profitable_opportunit",
    "opportunities_found",
    "expected_profit",
    "optimal_amount",
    "gas_cost",
    "simulation_status",
    "execution_status",
    "amount_in",
    "amount_out",
];

/// Literals that would mean a key, a key-name, or the repo's own dummy key placeholder got
/// copied into a committed file. M9.3 needs none of the three, so any of them appearing here is
/// a defect rather than a citation. The test's own source carries them as the deny-list; the
/// check below only reads the evidence directory. This is a list of known strings, not a shape
/// heuristic: an endpoint digest is `0x` + 64 hex too, so "looks like a key" cannot be the rule.
/// The real test key is not any of these three — `target/secret_scan.py` reads it from the key
/// file at run time and searches for that literal across the whole tree.
const FORBIDDEN_SECRET_SHAPES: [&str; 3] = [
    "PRIVATE KEY",
    "GIWA_RPC_URL",
    "0x0000000000000000000000000000000000000000000000000000000000000001",
];

// ---------------------------------------------------------------------------
// reading the inputs
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct M91Pass {
    verified: Vec<VerifiedPool>,
}

#[derive(Deserialize)]
struct RawPass {
    chain_id: u64,
    target: u64,
    pools_input: usize,
    eth_getlogs_requests: usize,
    logs_returned: usize,
    rows: Vec<evm_discovery::PoolSyncAtTarget>,
}

static ASSEMBLED: std::sync::Once = std::sync::Once::new();

/// The two timed run documents of an assembly run, measured once per process. Wall time is
/// the only field a second measurement could change, and a table that quotes it must agree
/// with the bytes written beside it.
static MEASURED: std::sync::OnceLock<Vec<Value>> = std::sync::OnceLock::new();

fn assemble_once() {
    ASSEMBLED.call_once(|| {
        if refresh() {
            let case = Case::load();
            let tables = case.tables();
            case.commit(&tables);
        }
    });
}

fn refresh() -> bool {
    std::env::var("M93_EVIDENCE_REFRESH").is_ok()
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
        "{relative} is not readable. Assemble this directory first: M93_EVIDENCE_REFRESH=1 \
         cargo test -p evm-discovery --test pathfinder_evidence_gate -- --test-threads=1"
    )
}

/// One path for every read of this directory's own files: in an assembly run it assembles
/// first, so a test never reads a half-written directory, and in a gate run it writes nothing.
fn read_text(relative: &str) -> String {
    if COMMITTED.contains(&relative) {
        assemble_once();
    }
    let path = if relative.starts_with("data/") {
        workspace_root().join(relative)
    } else {
        evidence_dir().join(relative)
    };
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", missing(relative)))
}

fn read_json<T>(relative: &str) -> T
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_str(&read_text(relative))
        .unwrap_or_else(|err| panic!("{relative} does not parse: {err}"))
}

fn read_value(relative: &str) -> Value {
    let text = read_text(relative);
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("{relative} does not parse: {err}"))
}

fn source_text(relative: &str) -> String {
    let path = workspace_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// The endpoint as this directory is allowed to name it: the same digest rule the chain crate
/// uses (`crates/chain/src/rpc_trace.rs:933`), read from M7's committed census rather than
/// restated. M9.3 makes no request, so the digest is here only to keep the provenance chain
/// unbroken.
fn endpoint_digest() -> String {
    let census: Value = read_value("data/evidence/m7/census-pair-created.json");
    let url = census["_provenance"]["rpc_endpoint"]
        .as_str()
        .expect("M7's census names its endpoint");
    format!(
        "rpc-{}",
        &alloy_primitives::keccak256(url.as_bytes()).to_string()[2..18]
    )
}

fn digest(text: &str) -> String {
    alloy_primitives::keccak256(text.as_bytes()).to_string()
}

// ---------------------------------------------------------------------------
// rendering a candidate: the §39 schema, produced once and reused by every table
// ---------------------------------------------------------------------------

/// An address as this directory writes it: lowercase hex, so a string comparison is a byte
/// comparison and a checksum's capitalisation cannot decide whether two rows match.
fn hex_of(address: &alloy_primitives::Address) -> String {
    address.to_string().to_ascii_lowercase()
}

fn triple(pool: &str, token_in: &str, token_out: &str) -> String {
    format!("{pool}:{token_in}>{token_out}")
}

fn edge_triple(edge: &EdgeId) -> String {
    triple(
        &hex_of(&edge.pool.address),
        &hex_of(&edge.token_in.address),
        &hex_of(&edge.token_out.address),
    )
}

fn edge_row(edge: &EdgeId) -> Value {
    json!({
        "pool": hex_of(&edge.pool.address),
        "token_in": hex_of(&edge.token_in.address),
        "token_out": hex_of(&edge.token_out.address),
    })
}

/// The triple of an already-rendered edge row, in either of the two shapes this directory
/// reads: the flat `pool/token_in/token_out` row, and M9.2's nested `id.pool.address`.
fn row_triple(edge: &Value) -> String {
    match edge["id"].as_object() {
        Some(id) => triple(
            id["pool"]["address"].as_str().expect("pool address"),
            id["token_in"]["address"]
                .as_str()
                .expect("token_in address"),
            id["token_out"]["address"]
                .as_str()
                .expect("token_out address"),
        ),
        None => triple(
            edge["pool"].as_str().expect("pool"),
            edge["token_in"].as_str().expect("token_in"),
            edge["token_out"].as_str().expect("token_out"),
        ),
    }
}

fn fee_name(status: FeeStatus) -> &'static str {
    match status {
        FeeStatus::Complete => "complete",
        FeeStatus::Incomplete => "incomplete",
    }
}

/// The least rotation of a rendered edge list, joined as a canonical key. Because every
/// address is lowercase and fixed-width, string order here is byte order of
/// `(pool, token_in, token_out)` — the same order `EdgeId`'s derived `Ord` gives, which is
/// what the production key is defined over. That coincidence is the point: the independent
/// recompute can re-derive the key without calling the code that made it.
fn least_rotation(edges: &[Value]) -> String {
    let rendered: Vec<String> = edges.iter().map(row_triple).collect();
    let len = rendered.len();
    if len < 2 {
        return rendered.join("+");
    }
    let mut best = String::new();
    for shift in 0..len {
        let joined: Vec<&str> = (shift..shift + len)
            .map(|index| rendered[index % len].as_str())
            .collect();
        let candidate = joined.join("+");
        if shift == 0 || candidate < best {
            best = candidate;
        }
    }
    best
}

/// One candidate, in the shape §39 asks for. `canonical_key` is the joined edge sequence
/// below it, so the identity a reader can eyeball and the identity a machine can re-derive
/// are the same bytes, and neither is a hash of anything.
fn candidate_row(candidate: &CycleCandidate) -> Value {
    let edges: Vec<Value> = candidate.edges.iter().map(edge_row).collect();
    json!({
        "canonical_key": least_rotation(&edges),
        "chain_id": candidate.chain_id.0,
        "target_block": candidate.target_block.0,
        "start_token": hex_of(&candidate.start_token.address),
        "hop_count": candidate.hop_count,
        "edges": edges,
        "fee_status": fee_name(candidate.fee_status),
    })
}

// ---------------------------------------------------------------------------
// the run
// ---------------------------------------------------------------------------

/// One search over the rebuilt graph.
struct Fresh {
    rows: Vec<Value>,
    states_visited: usize,
    wall_ms: u64,
}

/// One edge of M9.2's committed graph, as its own table records it.
struct PublishedEdge {
    pool: String,
    token_in: String,
    token_out: String,
    fee: Value,
}

impl PublishedEdge {
    fn triple(&self) -> String {
        triple(&self.pool, &self.token_in, &self.token_out)
    }
}

struct Case {
    chain_id: u64,
    target: u64,
    graph_nodes: usize,
    graph_edges: usize,
    graph_pools: usize,
    max_hops: usize,
    /// The search called on this run's own rebuilt graph. In gate mode that is the third run
    /// over the same input, and it has to agree with the two recorded ones.
    fresh: Fresh,
    /// The two raw run documents: the assembly run's own measurements, or their committed
    /// bytes in a gate run.
    runs: Vec<Value>,
    /// M9.2's committed topology, in its own order — the cross-check and the fee census.
    published: Vec<PublishedEdge>,
}

impl Case {
    fn load() -> Self {
        let m91: M91Pass = read_json(M91_PASS_REL);
        let raw: RawPass = read_json(M92_PASS_REL);
        let chain_id = ChainId(raw.chain_id);
        assert!(!m91.verified.is_empty(), "M9.1's raw pass carries no pools");
        assert_eq!(
            raw.pools_input,
            m91.verified.len(),
            "the reconstruction's input set is not the pool set M9.1 verified"
        );
        assert_eq!(
            raw.rows.len(),
            raw.pools_input,
            "a pool in the input set has no reconstruction row"
        );

        let reconstruction = Reconstruction {
            chain_id,
            target: BlockNumber(raw.target),
            rows: raw.rows.clone(),
            chunks_scanned: raw.eth_getlogs_requests,
            logs_returned: raw.logs_returned,
        };
        let state = integrate_at_target(
            chain_id,
            &Registry::default(),
            &m91.verified,
            &reconstruction,
        )
        .expect("integrate M9.2's rows at its target");
        let graph = match &state.graph {
            GraphOutcome::Built(build) => &build.graph,
            GraphOutcome::NoStateApplied => panic!(
                "M9.2's own rows produce no applied state, so there is no graph to search and \
                 this directory would have nothing to say"
            ),
        };

        // The topology cross-check before anything is quoted: the graph this run searched has
        // to be the graph M9.2 published, edge triple for edge triple.
        let published: Vec<PublishedEdge> = read_value(M92_GRAPH_REL)["edges"]
            .as_array()
            .expect("M9.2's graph table lists its edges")
            .iter()
            .map(|edge| PublishedEdge {
                pool: edge["id"]["pool"]["address"]
                    .as_str()
                    .expect("pool address")
                    .to_string(),
                token_in: edge["id"]["token_in"]["address"]
                    .as_str()
                    .expect("token_in address")
                    .to_string(),
                token_out: edge["id"]["token_out"]["address"]
                    .as_str()
                    .expect("token_out address")
                    .to_string(),
                fee: edge["fee"].clone(),
            })
            .collect();
        let mine: Vec<String> = graph.edges().map(|edge| edge_triple(&edge.id)).collect();
        let theirs: Vec<String> = published.iter().map(PublishedEdge::triple).collect();
        assert_eq!(
            mine, theirs,
            "this run rebuilt a different topology than M9.2 committed"
        );

        let fresh = Self::search(graph);
        let runs = if refresh() {
            // An assembly run times the search, so it is timed exactly once per process: a
            // second `Case::load()` would measure a different wall time, and the tables it
            // builds would then disagree with the bytes the first load committed.
            MEASURED
                .get_or_init(|| {
                    vec![
                        Self::raw_run("A", graph, &fresh),
                        Self::raw_run("B", graph, &Self::search(graph)),
                    ]
                })
                .clone()
        } else {
            vec![read_value(RUN_A), read_value(RUN_B)]
        };

        Self {
            chain_id: raw.chain_id,
            target: raw.target,
            graph_nodes: graph.node_count(),
            graph_edges: graph.edge_count(),
            graph_pools: graph.pool_count(),
            max_hops: PathFinderConfig::default().max_hops,
            fresh,
            runs,
            published,
        }
    }

    /// Every two-hop closed route over M9.2's committed edge table, re-derived here without
    /// calling the search: an ordered pair `(A --e1--> B, B --e2--> A)` on distinct pools is a
    /// route, and folding the pair to its least rotation gives the same identity the production
    /// key uses. The returned count is the ordered-pair form — how many times that route appears
    /// depending on which end the walk starts at — which is what §20's depth boundary and NC4's
    /// rotation rule predict: one route, two ordered pairs, one canonical key.
    ///
    /// The enumeration reads M9.2's table, not this run's rebuilt graph, so the witness is a
    /// committed artifact from another milestone rather than this directory's own arithmetic.
    fn two_hop_routes_from_published_table(&self) -> (BTreeSet<String>, usize) {
        fn row(edge: &PublishedEdge) -> Value {
            json!({
                "pool": edge.pool.as_str(),
                "token_in": edge.token_in.as_str(),
                "token_out": edge.token_out.as_str(),
            })
        }
        let mut routes = BTreeSet::new();
        let mut ordered_pairs = 0usize;
        for first in &self.published {
            if first.token_in == first.token_out {
                continue;
            }
            for second in &self.published {
                if second.pool == first.pool
                    || second.token_in != first.token_out
                    || second.token_out != first.token_in
                    || second.token_in == second.token_out
                {
                    continue;
                }
                ordered_pairs += 1;
                routes.insert(least_rotation(&[row(first), row(second)]));
            }
        }
        (routes, ordered_pairs)
    }

    fn search(graph: &GraphSnapshot) -> Fresh {
        let started = std::time::Instant::now();
        let run = find_cycles_traced(graph, &PathFinderConfig::default())
            .expect("the default config is inside the bound by construction");
        let wall_ms = started.elapsed().as_millis() as u64;
        Fresh {
            rows: run.cycles.iter().map(candidate_row).collect(),
            states_visited: run.states_visited,
            wall_ms,
        }
    }

    fn raw_run(pass: &str, graph: &GraphSnapshot, fresh: &Fresh) -> Value {
        json!({
            "table": "raw-run",
            "pass": pass,
            "assembled_by": "crates/discovery/tests/pathfinder_evidence_gate.rs",
            "input_graph": {
                "rebuilt_from": [M91_PASS_REL, M92_PASS_REL],
                "checked_against": M92_GRAPH_REL,
                "chain_id": graph.chain_id().0,
                "target_block": graph.block_number().0,
                "nodes": graph.node_count(),
                "edges": graph.edge_count(),
                "pools": graph.pool_count(),
            },
            "config": { "max_hops": PathFinderConfig::default().max_hops },
            "states_visited": fresh.states_visited,
            "wall_ms": fresh.wall_ms,
            "cycles": fresh.rows,
        })
    }

    fn recorded(&self, pass: &str) -> &Value {
        let run = self
            .runs
            .iter()
            .find(|run| run["pass"].as_str() == Some(pass))
            .unwrap_or_else(|| panic!("no raw run documents pass {pass}"));
        assert_eq!(
            run["cycles"],
            Value::Array(self.fresh.rows.clone()),
            "raw run {pass} does not reproduce this run's candidates"
        );
        assert_eq!(
            run["states_visited"].as_u64(),
            Some(self.fresh.states_visited as u64),
            "raw run {pass} walked a different number of states"
        );
        run
    }

    fn wall_ms(&self, pass: &str) -> u64 {
        self.recorded(pass)["wall_ms"]
            .as_u64()
            .expect("a recorded wall time")
    }

    fn rows(&self) -> &[Value] {
        &self.fresh.rows
    }

    // -- aggregates ---------------------------------------------------------

    fn counted(&self, hop: usize) -> usize {
        self.rows()
            .iter()
            .filter(|row| row["hop_count"].as_u64() == Some(hop as u64))
            .count()
    }

    fn fee_count(&self, status: &str) -> usize {
        self.rows()
            .iter()
            .filter(|row| row["fee_status"].as_str() == Some(status))
            .count()
    }

    fn keys(&self) -> Vec<String> {
        self.rows()
            .iter()
            .map(|row| row["canonical_key"].as_str().expect("a key").to_string())
            .collect()
    }

    fn duplicate_cycles(&self) -> usize {
        let unique: BTreeSet<&str> = self
            .rows()
            .iter()
            .map(|row| row["canonical_key"].as_str().expect("a key"))
            .collect();
        self.rows().len() - unique.len()
    }

    /// Whether M9.2's table records an attested fee for this triple.
    fn fee_attested(&self, edge: &Value) -> bool {
        let wanted = row_triple(edge);
        self.published
            .iter()
            .any(|found| found.triple() == wanted && found.fee != Value::Null)
    }

    /// Which graph edges the candidates walk, and how often. An edge no candidate walks is a
    /// market this block's topology closes nowhere through — a statement about shape, and
    /// nothing about quality.
    fn edge_usage(&self) -> Vec<Value> {
        let mut usage: BTreeMap<String, usize> = BTreeMap::new();
        for row in self.rows() {
            for edge in row["edges"].as_array().expect("a candidate has edges") {
                *usage.entry(row_triple(edge)).or_insert(0) += 1;
            }
        }
        let mut rows: Vec<(String, Value)> = self
            .published
            .iter()
            .map(|edge| {
                let used = usage.get(&edge.triple()).copied().unwrap_or(0);
                (
                    edge.triple(),
                    json!({
                        "pool": edge.pool,
                        "token_in": edge.token_in,
                        "token_out": edge.token_out,
                        "candidates_using": used,
                        "fee_attested": edge.fee != Value::Null,
                    }),
                )
            })
            .collect();
        rows.sort_by(|left, right| left.0.cmp(&right.0));
        rows.into_iter().map(|(_, row)| row).collect()
    }

    fn pools_in_candidates(&self) -> usize {
        let mut pools: BTreeSet<&str> = BTreeSet::new();
        for row in self.rows() {
            for edge in row["edges"].as_array().expect("a candidate has edges") {
                pools.insert(edge["pool"].as_str().expect("a pool"));
            }
        }
        pools.len()
    }

    /// Per candidate, which of its edges M9.2's own table records as unattested.
    fn fee_rows(&self) -> Vec<Value> {
        self.rows()
            .iter()
            .map(|row| {
                let missing: Vec<String> = row["edges"]
                    .as_array()
                    .expect("a candidate has edges")
                    .iter()
                    .filter(|edge| !self.fee_attested(edge))
                    .map(row_triple)
                    .collect();
                json!({
                    "canonical_key": row["canonical_key"],
                    "hop_count": row["hop_count"],
                    "fee_status": row["fee_status"],
                    "edges_without_attested_fee": missing,
                })
            })
            .collect()
    }

    /// The evidence-side controls, counted from this run's rows. Zero is the only acceptable
    /// total, and the count is recorded rather than asserted away so a reader sees what was
    /// checked against how many rows.
    fn row_violations(&self) -> usize {
        let mut violations = 0usize;
        for row in self.rows() {
            let edges = row["edges"].as_array().expect("a candidate has edges");
            if row["start_token"] != edges[0]["token_in"]
                || edges.first().expect("an edge")["token_in"]
                    != edges.last().expect("an edge")["token_out"]
            {
                violations += 1;
            }
            if !edges
                .windows(2)
                .all(|pair| pair[0]["token_out"] == pair[1]["token_in"])
            {
                violations += 1;
            }
            let unique_over: Vec<BTreeSet<String>> = ["pool", "token_in"]
                .iter()
                .map(|field| {
                    edges
                        .iter()
                        .map(|edge| edge[*field].as_str().unwrap_or_default().to_string())
                        .collect()
                })
                .collect();
            for set in &unique_over {
                if set.len() != edges.len() {
                    violations += 1;
                }
            }
            if edges.len() < 2
                || edges.len() > 3
                || row["hop_count"].as_u64() != Some(edges.len() as u64)
            {
                violations += 1;
            }
            if !edges.iter().all(|edge| {
                self.published
                    .iter()
                    .any(|found| found.triple() == row_triple(edge))
            }) {
                violations += 1;
            }
            if row["target_block"] != json!(self.target) || row["chain_id"] != json!(self.chain_id)
            {
                violations += 1;
            }
            let expected = if edges.iter().all(|edge| self.fee_attested(edge)) {
                "complete"
            } else {
                "incomplete"
            };
            if row["fee_status"].as_str() != Some(expected)
                || least_rotation(edges) != row["canonical_key"].as_str().unwrap_or_default()
            {
                violations += 1;
            }
        }
        if self.keys().into_iter().collect::<BTreeSet<_>>().len() != self.rows().len() {
            violations += 1;
        }
        violations
    }

    fn runs_identical(&self) -> bool {
        self.recorded("A")["cycles"] == self.recorded("B")["cycles"]
    }

    // -- tables -------------------------------------------------------------

    fn provenance(&self) -> Value {
        json!({
            "asked": "§36–§41: which closed routes exist in M9.2's real target-block graph, how \
                      many are 2-hop and 3-hop, which have every fee proved, whether two runs of \
                      one graph and one config agree byte for byte, and how much walking and how \
                      long that took — with every number recomputable from the raw run documents \
                      beside it by a reader who calls no production helper.",
            "assembled_by": "crates/discovery/tests/pathfinder_evidence_gate.rs",
            "assemble_command": "M93_EVIDENCE_REFRESH=1 cargo test -p evm-discovery \
                                 --test pathfinder_evidence_gate -- --test-threads=1",
            "check_command": "cargo test -p evm-discovery --test pathfinder_evidence_gate \
                              -- --test-threads=1",
            "chain_id": self.chain_id,
            "endpoint": endpoint_digest(),
            "endpoint_link": "data/evidence/m7/census-pair-created.json#_provenance.rpc_endpoint",
            "endpoint_source": "the digest of the endpoint M7 and M9.1/M9.2 recorded. This \
                                milestone makes no request at all: the graph is rebuilt from \
                                committed JSON, so the digest appears here only to keep the \
                                provenance chain unbroken, and no URL is written anywhere in \
                                this directory.",
            "input_files": [M91_PASS_REL, M92_PASS_REL, M92_GRAPH_REL],
            "milestone": "M9.3",
            "no_key_in_this_file": true,
            "raw_records": RAW_FILES
                .iter()
                .map(|name| format!("{EVIDENCE_REL}/{name}"))
                .collect::<Vec<_>>(),
            "written_at": "the run, not the review: every count is read off the candidate rows \
                           beside it, and the one number that cannot repeat — the wall clock — \
                           lives in raw/ and is quoted rather than re-measured",
        })
    }

    fn summary_table(&self) -> String {
        let total = self.rows().len();
        let duplicates = self.duplicate_cycles();
        let usage = self.edge_usage();
        let used = usage
            .iter()
            .filter(|row| row["candidates_using"].as_u64().unwrap_or(0) > 0)
            .count();
        pretty(
            SUMMARY,
            json!({
                "table": "summary",
                "_provenance": self.provenance(),

                "chain_id": self.chain_id,
                "target_block": self.target,
                "graph_nodes": self.graph_nodes,
                "graph_edges": self.graph_edges,
                "graph_pools": self.graph_pools,
                "max_hops": self.max_hops,

                "total_cycles": total,
                "cycles_2hop": self.counted(2),
                "cycles_3hop": self.counted(3),

                "fee_complete": self.fee_count("complete"),
                "fee_incomplete": self.fee_count("incomplete"),

                "canonical_unique": duplicates == 0,
                "duplicate_cycles": duplicates,

                "search_wall_ms": self.wall_ms("A"),
                "dfs_states_visited": self.fresh.states_visited,

                "terminology": {
                    "what_is_counted": "cycle candidates — closed routes in one block's topology",
                    "what_is_not_counted": "a candidate is not an opportunity, not a quote, and \
                                            not a count of money. total_cycles above counts shapes \
                                            in a graph and is never a number of trades (§38/§55).",
                },
                "profit": {
                    "gross_profit": "N/A",
                    "net_profit": "N/A",
                    "realized_profit": "N/A",
                    "reason": "§56: not evaluated at this stage. N/A and 0 are different \
                               statements — a zero would claim the search priced these routes and \
                               found them worthless, and the search reads no reserves, no fee \
                               values and no gas.",
                },
                "safety": {
                    "rpc_calls": 0,
                    "signatures": 0,
                    "broadcasts": 0,
                    "real_arbitrage_executed": 0,
                    "state_mutations": 0,
                    "flashblock_reads": 0,
                    "pathfinder_production_dependencies": production_dependencies_of(PATHFINDER_CARGO),
                    "pathfinder_dependencies_allowed": ALLOWED_PATHFINDER_DEPENDENCIES,
                    "discovery_production_dependencies_name_pathfinder": dependencies_of(DISCOVERY_CARGO, "dependencies")
                        .contains(&"evm-pathfinder".to_string()),
                    "how_this_is_checkable": "§42/§50: this block reads the two Cargo.toml files \
                                              at assembly time. The search crate links four \
                                              crates, none of which can open a socket; discovery \
                                              depends on it only as a dev-dependency, so no \
                                              pipeline edge was created here.",
                },
                "graph_read": {
                    "edges_used_by_at_least_one_candidate": used,
                    "edges_used_by_no_candidate": usage.len() - used,
                    "distinct_pools_in_candidates": self.pools_in_candidates(),
                    "note": "an edge with no candidate is a market this block's topology closes \
                             nowhere through within the hop bound — a statement about shape, not \
                             about quality.",
                },
                "verdict": if total > 0 { "CYCLE_CANDIDATES_FOUND" } else { "NO_CYCLE_CANDIDATES" },
                "verdict_rule": "`CYCLE_CANDIDATES_FOUND` requires the search to return at least \
                                 one well-formed candidate over the rebuilt M9.2 graph. Neither \
                                 verdict says anything about profit, fillability, or the fees \
                                 being proved; §58's Fee and Safety groups are graded from \
                                 `fee-status.json`, `safety` above, and the static scans.",
                "not_claimed": [
                    "that any candidate is profitable, or close to it",
                    "that any candidate can be filled at this block's reserves",
                    "that any fee on any candidate is known",
                    "that a deeper bound would find more, or that three hops is enough",
                    format!("that this graph is current: it is block {} as M9.2 rebuilt it", self.target).as_str(),
                ],
            }),
        )
    }

    fn cycles_table(&self) -> String {
        pretty(
            CYCLES,
            json!({
                "table": "cycles",
                "_provenance": self.provenance(),
                "row_identity": "§12: chain_id + target_block + the ordered edge sequence under \
                                 canonical_key. No row position, no list index, no hash.",
                "ordering_rule": "canonical_key ascending — the order the search returns, \
                                  restated rather than re-sorted, so a reader can check the \
                                  output arrived sorted (§25).",
                "fields": {
                    "canonical_key": "the route's least rotation, one `pool:token_in>token_out` \
                                      per edge, joined by `+`",
                    "chain_id": "the graph's chain",
                    "target_block": "the graph's target block, copied onto every candidate",
                    "start_token": "the token spent first and received last, as the canonical \
                                    rotation reads it",
                    "hop_count": "edges walked, always 2..=3 in v0.1",
                    "edges": "directed edges in trade order, identities only — no reserves, no amounts",
                    "fee_status": "`complete` only if every edge's pool has an attested fee; \
                                   `incomplete` otherwise, and never a guessed rate",
                },
                "counts": {
                    "total": self.rows().len(),
                    "two_hop": self.counted(2),
                    "three_hop": self.counted(3),
                    "fee_complete": self.fee_count("complete"),
                    "fee_incomplete": self.fee_count("incomplete"),
                },
                "rows": self.rows(),
            }),
        )
    }

    fn candidate_edges_table(&self) -> String {
        let usage = self.edge_usage();
        let used = usage
            .iter()
            .filter(|row| row["candidates_using"].as_u64().unwrap_or(0) > 0)
            .count();
        let busiest = usage
            .iter()
            .map(|row| row["candidates_using"].as_u64().unwrap_or(0))
            .max()
            .unwrap_or(0);
        pretty(
            CANDIDATE_EDGES,
            json!({
                "table": "candidate-edges",
                "_provenance": self.provenance(),
                "row_identity": "one row per directed graph edge, keyed `pool:token_in>token_out`, \
                                 taken from M9.2's committed edge list rather than from this \
                                 run's rebuild order, then sorted by that key",
                "totals": {
                    "graph_edges": usage.len(),
                    "edges_in_at_least_one_candidate": used,
                    "edges_in_no_candidate": usage.len() - used,
                    "busiest_edge": busiest,
                    "distinct_pools_in_candidates": self.pools_in_candidates(),
                    "graph_pools": self.graph_pools,
                },
                "rule": "a direction no candidate walks is not a skipped pool and not a refused \
                         one: its two ends do not come back around within three hops. The search \
                         never reads reserves, so `fee_attested` is the only per-edge fact beyond \
                         identity — and it is M9.2's, not this run's.",
                "rows": usage,
            }),
        )
    }

    fn fee_status_table(&self) -> String {
        let attested = self
            .published
            .iter()
            .filter(|edge| edge.fee != Value::Null)
            .count();
        let edges = self.published.len();
        let incomplete = self.fee_count("incomplete");
        pretty(
            FEE_STATUS,
            json!({
                "table": "fee-status",
                "_provenance": self.provenance(),
                "witness": format!("the fee column is read from {M92_GRAPH_REL}, one field per \
                                    edge — M9.2's published bytes, not this run's rebuild. A fee \
                                    the search believed would have to be a fee the graph \
                                    recorded, and there is no other door into this table."),
                "graph_edges": edges,
                "graph_edges_with_attested_fee": attested,
                "graph_edges_without_attested_fee": edges - attested,
                "candidate_rows": {
                    "total": self.rows().len(),
                    "complete": self.fee_count("complete"),
                    "incomplete": incomplete,
                },
                "rule": "§16–§18: Some is proved and None is unknown. Unknown is never read as \
                         zero and never defaulted to 997/1000, so a route touching one \
                         unattested pool is enumerated, reported, and unusable by anything that \
                         prices.",
                "consequence_for_this_directory": format!(
                    "every candidate here is incomplete, because the graph M9.2 published carries \
                     no attested fee on any of its {edges} edges. That is NC6 at real scale, and \
                     it is the reason nothing downstream may read this table as a list of trades: \
                     {incomplete} of {} findings cannot be priced without inventing a number.",
                    self.rows().len()
                ),
                "rows": self.fee_rows(),
            }),
        )
    }

    fn determinism_table(&self) -> String {
        let a = self.recorded("A");
        let b = self.recorded("B");
        let keys_a = run_keys(a);
        let keys_b = run_keys(b);
        let json_a = serde_json::to_string(&a["cycles"]).expect("rows serialize");
        let json_b = serde_json::to_string(&b["cycles"]).expect("rows serialize");
        pretty(
            DETERMINISM,
            json!({
                "table": "determinism",
                "_provenance": self.provenance(),
                "runs": [format!("{EVIDENCE_REL}/{RUN_A}"), format!("{EVIDENCE_REL}/{RUN_B}")],
                "same_input": {
                    "chain_id": a["input_graph"]["chain_id"] == b["input_graph"]["chain_id"],
                    "target_block": a["input_graph"]["target_block"]
                        == b["input_graph"]["target_block"],
                    "graph_edges": a["input_graph"]["edges"] == b["input_graph"]["edges"],
                    "config": a["config"] == b["config"],
                },
                "compared": {
                    "candidate_count": a["cycles"].as_array().map(Vec::len)
                        == b["cycles"].as_array().map(Vec::len),
                    "candidate_order": keys_a == keys_b,
                    "canonical_keys": keys_a == keys_b,
                    "candidate_json": json_a == json_b,
                },
                "rule": "§40: exact equality on all four, not a count. Two runs of one graph and \
                         one config that differ in any position are a nondeterministic search, \
                         and a matching length would hide it.",
                "candidate_json_digest": {
                    "run_a": digest(&json_a),
                    "run_b": digest(&json_b),
                    "equal": json_a == json_b,
                },
                "fresh_rebuild_vs_recorded_runs": {
                    "cycles": Value::Array(self.rows().to_vec()) == a["cycles"],
                    "states_visited": self.fresh.states_visited
                        == b["states_visited"].as_u64().unwrap_or_default() as usize,
                    "note": "a gate run calls the search a third time. Its candidate list and its \
                             DFS state count have to equal both recorded runs; only its wall time \
                             is allowed to differ, and it is therefore not compared.",
                },
                "wall_ms": { "run_a": a["wall_ms"], "run_b": b["wall_ms"] },
            }),
        )
    }

    fn benchmark_table(&self) -> String {
        let states_per_candidate = if self.rows().is_empty() {
            Value::Null
        } else {
            json!((self.fresh.states_visited as f64) / (self.rows().len() as f64))
        };
        pretty(
            BENCHMARK,
            json!({
                "table": "benchmark",
                "_provenance": self.provenance(),
                "graph_node_count": self.graph_nodes,
                "graph_edge_count": self.graph_edges,
                "graph_pool_count": self.graph_pools,
                "max_hops": self.max_hops,
                "candidate_count": self.rows().len(),
                "cycles_2hop": self.counted(2),
                "cycles_3hop": self.counted(3),
                "wall_ms": {
                    "run_a": self.wall_ms("A"),
                    "run_b": self.wall_ms("B"),
                    "measured_on": "the assembly run. A gate run re-measures and is not compared \
                                    against these bytes, which is exactly why the number lives \
                                    in raw/ and every table quotes it instead of claiming it.",
                },
                "dfs_states_visited": self.fresh.states_visited,
                "states_per_candidate": states_per_candidate,
                "interpretation": "§43: no latency target is set for M9.3 and none is implied by \
                                  this row. The first goals are correctness, determinism and a \
                                  bounded search; `dfs_states_visited` is here so a future slow \
                                  run can be read without changing what the search returns.",
                "no_speculative_optimization": "§44–§47: no memoization, no global cache, no RPC \
                                                cache, no parallel DFS, no unsafe, no arena, no \
                                                SIMD. The graph is borrowed, not cloned, and a \
                                                GraphEdge is never copied into a candidate — only \
                                                its EdgeId.",
            }),
        )
    }

    fn controls_table(&self) -> String {
        pretty(
            CONTROLS,
            json!({
                "table": "negative-controls",
                "_provenance": self.provenance(),
                "unit_tests": CONTROLS_RUN
                    .iter()
                    .map(|(id, claim, tests)| json!({
                        "id": id,
                        "claim": claim,
                        "tests": tests,
                        "source_file": PATHFINDER_TESTS,
                    }))
                    .collect::<Vec<_>>(),
                "recomputed_over_the_real_graph": {
                    "rows_checked": self.rows().len(),
                    "rule": "§53/§54: the same rules are re-derived from the committed rows by a \
                             test that calls no search and no discovery function — a candidate \
                             that broke one of them would fail this table, not only the unit test \
                             that produced it.",
                    "checks": [
                        "every route closes on its start token",
                        "every consecutive pair is adjacent",
                        "no pool appears twice in one route",
                        "no token is stood on twice before the route closes",
                        "hop_count is 2 or 3 and equals the edge count",
                        "every edge is in the graph M9.2 published",
                        "chain_id and target_block equal the graph's on every row",
                        "canonical_key is the least rotation of the row's own edges",
                        "canonical keys are unique across rows",
                        "fee_status follows M9.2's fee column, edge by edge",
                    ],
                    "violations": self.row_violations(),
                },
                "what_no_control_covers": "none of these says a candidate is worth trading. NC6 is \
                                           the closest, and its finding runs the other way: a \
                                           route with an unproved fee is a shape, not an offer.",
            }),
        )
    }

    fn manifest_table(&self, tables: &BTreeMap<String, String>) -> String {
        let files: Vec<Value> = DIGESTED
            .iter()
            .map(|name| {
                let text = tables
                    .get(*name)
                    .unwrap_or_else(|| panic!("no table was built for {name}"));
                json!({
                    "file": name,
                    "bytes": text.len(),
                    "digest": digest(text),
                })
            })
            .collect();
        let raw_files: Vec<Value> = self
            .runs
            .iter()
            .map(|run| {
                let name = if run["pass"].as_str() == Some("A") {
                    RUN_A
                } else {
                    RUN_B
                };
                let text = pretty_raw(run);
                json!({
                    "file": name,
                    "bytes": text.len(),
                    "digest": digest(&text),
                    "cycles_recorded": run["cycles"].as_array().map(Vec::len),
                    "wall_ms": run["wall_ms"],
                })
            })
            .collect();
        pretty(
            MANIFEST,
            json!({
                "table": "manifest",
                "_provenance": self.provenance(),
                "files": files,
                "raw_files": raw_files,
                "questions_answered": [
                    "which closed routes exist in M9.2's target-block graph, and how many of each depth",
                    "which of them have every fee proved, edge by edge rather than by assumption",
                    "whether two runs of one graph and one config agree byte for byte",
                    "how much walking the bounded search did to answer, and how long it took",
                    "whether this milestone touched a node, a key or a broadcast — and how a reader checks that without trusting this file",
                ],
                "negative_controls_run_here": "§35's eight controls are unit tests in \
                                               crates/pathfinder/tests/pathfinder.rs. This \
                                               directory re-derives the same rules from its own \
                                               rows (`negative-controls.json#recomputed_over_the_\
                                               real_graph`) and greps the source for every test \
                                               name it quotes.",
            }),
        )
    }

    fn readme(&self, tables: &BTreeMap<String, String>) -> String {
        let listing = tables
            .iter()
            .filter(|(name, _)| name.as_str() != README)
            .map(|(name, text)| format!("- `{name}` — {} 字节\n", text.len()))
            .collect::<String>();
        let edges = self.published.len();
        let incomplete = self.fee_count("incomplete");
        format!(
            "# M9.3 证据目录（有界环搜索：GraphSnapshot → CycleCandidate[]）\n\n\
             ## 一句话（白话版）\n\n\
             M9.2 交出的是某一区块上「哪些池子的价格是成立的」。M9.3 只做一件事：在这些池子\
             连成的市场图上，把**能绕回起点的闭环路线**全部数出来——最多 {hops} 跳，同一个池子\
             不许走两遍，中途站过的代币不许再站。产出是 {cycles} 条**环候选**（cycle candidates），\
             不是 {cycles} 个套利机会：这一层不看储备、不算金额、不出利润，它只回答\
             「图里有几条路能绕回来」。\n\n\
             ## 这次到底证明了什么\n\n\
             | 问题 | 数字 | 逐条证据 |\n|---|---|---|\n\
             | 读的图（M9.2 目标块） | {nodes} 代币 / {edges} 有向边 / {pools} 池 | \
             `summary.json#graph_*`，拓扑逐边对照 `{m92_graph}` |\n\
             | 环候选总数 | {cycles} | `cycles.json`，每条一行 |\n\
             | 其中 2 跳 / 3 跳 | {two} / {three} | `cycles.json#counts` |\n\
             | 费率全已举证 / 至少一边未举证 | {complete} / {incomplete} | `fee-status.json` |\n\
             | 同一规范键重复出现 | {duplicates} | `summary.json#duplicate_cycles` |\n\
             | 两次运行是否逐字节相同 | {identical} | `determinism.json` |\n\
             | DFS 状态数 / 墙钟耗时 | {states} / {wall} ms | `benchmark.json` |\n\
             | 被至少一条候选走到的边 | {used} / {edges} | `candidate-edges.json#totals` |\n\n\
             两个等式必须成立：`{cycles} = {two} + {three}`，`{complete} + {incomplete} = {cycles}`。\
             它们由这个目录自己算出来，不是抄来的。\n\n\
             ## 为什么「费率全未举证」不是坏消息\n\n\
             M9.2 留下的图里，{edges} 条边的 `fee` 都是 `null`——这条链上没人替这些池子举证过费率。\
             所以本目录 {cycles} 条候选**全部**是 `incomplete`。这不是搜索失败，恰恰是它应当给出的\
             答案：任务书 §16–§18 要求「未知」永远不许被读成 0、也不许默认成 997/1000，\
             而 §35 的 NC6 要求「带未举证费率的候选照样被枚举，但一条都不能进财务层」。\
             这一格等于把 NC6 在真实规模上又跑了一遍：{incomplete} 条可枚举、{complete} 条可用。\n\n\
             ## 表格清单\n\n{listing}\
             ## 原始记录\n\n\
             `raw/` 是装配那一次跑的完整输出（候选逐条 + 状态数 + 墙钟毫秒），\
             `manifest.json#raw_files` 逐个列字节数与摘要。图本身不重复存一份：它由\n\
             `{m91_pass}` 与 `{m92_pass}` 用生产函数重建，再逐边对照 `{m92_graph}`；\
             重建不一致，这个目录就没资格说自己读的是 M9.2 的图（§36）。\n\n\
             ## 重算与门禁\n\n\
             ```text\n\
             M93_EVIDENCE_REFRESH=1 cargo test -p evm-discovery --test pathfinder_evidence_gate -- --test-threads=1\n\
             cargo test -p evm-discovery --test pathfinder_evidence_gate -- --test-threads=1\n\
             ```\n\n\
             第一条重建并写入；第二条只读比对，任何一格对不上就失败。此外\n\
             `the_independent_recompute_agrees_with_every_committed_number` 不调用任何 pathfinder \
             / discovery 函数，把 `raw/` 与 M9.2 已提交的表当 JSON 读进来逐项重算；\
             `an_injected_wrong_number_is_caught_by_the_recompute` 分别篡改原始记录与证人表\
             （删一条候选、把一条候选的规范键换成它自己的另一个轮转、给某条路线走过的每条边都补上\
             已举证费率、删掉某条候选真正走过的那条边），确认四类缺陷各自都会让门变红——只有前一道\
             门绿的目录不能算被验过。\n\n\
             ## 这个目录没有说的事\n\n\
             - 没有任何一条候选被声称「有利润」「可成交」：gross / net / realized profit 一律记 \
             `N/A`（§56——写 `0` 是另一句话，意思是「算过了，不值钱」）。\n\
             - 术语固定：本目录的计数叫 **cycle candidates（环候选）**，不叫 arbitrage \
             opportunities，也不叫 profitable opportunities（§38/§55）。有测试逐表扫描字段名形状，\
             防止任何像「机会 / 金额 /  gas / 模拟状态」的字段被塞进候选里。\n\
             - 全程 0 次 RPC：本目录的输入全是仓库里已提交的 JSON；寻路 crate 的生产依赖只有 \
             {deps} 四个，没有一个能开网络（测试逐行读那两个 `Cargo.toml` 核对，§42）。\n\
             - discovery crate 只在 dev-dependencies 里引用 pathfinder，生产流水线没有任何新增接线\
             （§50：candidate → risk → sign → submit 这条链在本里程碑不存在）。\n\
             - 端点在本目录只以摘要 `{endpoint}` 出现，URL 字面量一个都没有（有测试逐文件检查）。\n",
            hops = self.max_hops,
            cycles = self.rows().len(),
            nodes = self.graph_nodes,
            edges = edges,
            pools = self.graph_pools,
            two = self.counted(2),
            three = self.counted(3),
            complete = self.fee_count("complete"),
            incomplete = incomplete,
            duplicates = self.duplicate_cycles(),
            identical = self.runs_identical(),
            states = self.fresh.states_visited,
            wall = self.wall_ms("A"),
            used = self
                .edge_usage()
                .iter()
                .filter(|row| row["candidates_using"].as_u64().unwrap_or(0) > 0)
                .count(),
            m91_pass = M91_PASS_REL,
            m92_pass = M92_PASS_REL,
            m92_graph = M92_GRAPH_REL,
            listing = listing,
            deps = ALLOWED_PATHFINDER_DEPENDENCIES.join(" / "),
            endpoint = endpoint_digest(),
        )
    }

    fn tables(&self) -> BTreeMap<String, String> {
        let mut tables = BTreeMap::new();
        tables.insert(SUMMARY.to_string(), self.summary_table());
        tables.insert(CYCLES.to_string(), self.cycles_table());
        tables.insert(CANDIDATE_EDGES.to_string(), self.candidate_edges_table());
        tables.insert(FEE_STATUS.to_string(), self.fee_status_table());
        tables.insert(DETERMINISM.to_string(), self.determinism_table());
        tables.insert(BENCHMARK.to_string(), self.benchmark_table());
        tables.insert(CONTROLS.to_string(), self.controls_table());
        let manifest = self.manifest_table(&tables);
        tables.insert(MANIFEST.to_string(), manifest.clone());
        let mut for_readme = tables.clone();
        for_readme.insert(MANIFEST.to_string(), manifest);
        let readme = self.readme(&for_readme);
        tables.insert(README.to_string(), readme);
        tables
    }

    /// Write in refresh mode; in gate mode the caller compares against the bytes.
    fn commit(&self, tables: &BTreeMap<String, String>) {
        if !refresh() {
            return;
        }
        let directory = evidence_dir();
        std::fs::create_dir_all(directory.join("raw")).expect("the evidence directory exists");
        for run in &self.runs {
            let name = if run["pass"].as_str() == Some("A") {
                RUN_A
            } else {
                RUN_B
            };
            std::fs::write(directory.join(name), pretty_raw(run))
                .unwrap_or_else(|err| panic!("write {name}: {err}"));
        }
        for (name, text) in tables {
            std::fs::write(directory.join(name), text)
                .unwrap_or_else(|err| panic!("write {name}: {err}"));
        }
    }
}

/// A count read out of a JSON field. A number that is not a number fails here rather
/// than silently comparing as unequal to zero.
fn count_of(value: &Value) -> u64 {
    value.as_u64().expect("a count")
}

/// The canonical keys of one recorded run, in the order that run produced them.
fn run_keys(run: &Value) -> Vec<&str> {
    run["cycles"]
        .as_array()
        .expect("a run of cycles")
        .iter()
        .map(|row| row["canonical_key"].as_str().expect("a key"))
        .collect()
}

/// A raw run document rendered with the same two-space printing as the tables, so the
/// manifest's digest of it is a digest of its committed bytes.
fn pretty_raw(run: &Value) -> String {
    let mut text = serde_json::to_string_pretty(run).expect("a raw run serializes");
    text.push('\n');
    text
}

/// The `[dependencies]` block of a crate's own `Cargo.toml`, as bare package names. A claim
/// about what can reach the network should be read off the file that decides it, not restated.
fn dependencies_of(relative: &str, section: &str) -> Vec<String> {
    let text = source_text(relative);
    let mut names: Vec<String> = Vec::new();
    let header = format!("[{section}]");
    let mut inside = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            inside = trimmed == header;
            continue;
        }
        if !inside || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = trimmed.split_once(['.', '=']) {
            let name = name.trim();
            if !name.is_empty() {
                names.push(name.to_string());
            }
        }
    }
    names
}

fn production_dependencies_of(relative: &str) -> Vec<String> {
    dependencies_of(relative, "dependencies")
}

/// The one JSON shape every table shares: two-space pretty printing with a trailing newline,
/// so a byte comparison is a comparison of content rather than of a formatter's mood.
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

// ---------------------------------------------------------------------------
// the byte gate
// ---------------------------------------------------------------------------

#[test]
fn every_committed_table_matches_a_fresh_rebuild() {
    let case = Case::load();
    let tables = case.tables();
    assert_eq!(
        tables.len(),
        TABLES.len(),
        "one committed file per table name"
    );
    let mut mismatches: Vec<&str> = Vec::new();
    for name in TABLES {
        let built = tables
            .get(name)
            .unwrap_or_else(|| panic!("no table was built for {name}"));
        let committed = read_text(name);
        if &committed != built {
            mismatches.push(name);
        }
    }
    assert!(
        mismatches.is_empty(),
        "these committed files are not what a fresh rebuild produces: {mismatches:?}. \
         Re-assemble with M93_EVIDENCE_REFRESH=1 only if the code changed, not to make a \
         failure go away."
    );
}

#[test]
fn the_run_read_the_graph_m9_2_published() {
    let case = Case::load();
    assert_eq!(
        case.published.len(),
        case.graph_edges,
        "the rebuilt graph's edge count is not M9.2's"
    );
    let mut invented = 0usize;
    for row in case.rows() {
        for edge in row["edges"].as_array().expect("a candidate has edges") {
            let wanted = row_triple(edge);
            if !case.published.iter().any(|found| found.triple() == wanted) {
                invented += 1;
            }
        }
    }
    assert_eq!(invented, 0, "the search produced an edge no graph carries");
}

/// §4 item 8 / §13: the two-hop routes M9.2's committed edge table contains are exactly the
/// two-hop candidates this search canonicalized. Nothing was added, nothing was lost. The
/// enumeration reads M9.2's published file and never calls the search, so the witness is another
/// milestone's artifact, not this directory's own arithmetic — and it is the only witness shape
/// available here: `crates/discovery/tests/reconstruction_evidence_gate.rs`'s boundary test
/// forbids discovery's manifest from listing `evm-opportunity` even as a dev-dependency, so M3's
/// own enumerator cannot be called from this crate.
///
/// `ordered_pairs` is the same walk with rotation folded away: one route appears once per end it
/// is started from, which is the count M3's identity keeps separate (§20). Comparing it against
/// the committed rows checks the committed set has one candidate per route — the equality
/// `ordered_pairs == 2 * enumerated_routes` would be true by construction and is not asserted.
#[test]
fn the_two_hop_candidates_are_exactly_the_routes_in_m9_2s_table() {
    let case = Case::load();
    let cycles = read_value(CYCLES);
    let rows = cycles["rows"].as_array().expect("candidate rows");
    let two_hop: BTreeSet<String> = rows
        .iter()
        .filter(|row| row["hop_count"].as_u64() == Some(2))
        .map(|row| row["canonical_key"].as_str().expect("a key").to_string())
        .collect();
    assert_eq!(
        two_hop.len() as u64,
        count_of(&cycles["counts"]["two_hop"]),
        "the depth count is not read off the rows it counts"
    );
    let (enumerated, ordered_pairs) = case.two_hop_routes_from_published_table();
    assert!(
        !enumerated.is_empty(),
        "an enumeration over zero routes would agree with everything"
    );
    assert_eq!(
        enumerated, two_hop,
        "M9.2's published topology and this search disagree about which two-hop routes exist"
    );
    assert_eq!(
        ordered_pairs,
        2 * two_hop.len(),
        "each two-hop cycle has two ends to start from, and the committed set carries one \
         candidate per cycle (§20)"
    );
}

#[test]
fn candidates_are_bound_to_the_graph_and_its_block() {
    let case = Case::load();
    assert!(
        !case.rows().is_empty(),
        "the real graph has to be searched, and a directory over zero rows says nothing"
    );
    assert_eq!(case.row_violations(), 0);
    assert_eq!(case.duplicate_cycles(), 0);
    assert_eq!(
        case.counted(2) + case.counted(3),
        case.rows().len(),
        "every candidate is 2- or 3-hop, so the depth split is a partition"
    );
    assert_eq!(
        case.fee_count("complete") + case.fee_count("incomplete"),
        case.rows().len(),
        "fee status is a partition too, and there is no third state"
    );
    let keys = case.keys();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(
        keys, sorted,
        "the output arrives in canonical key order (§25)"
    );
}

#[test]
fn pathfinder_introduces_no_rpc_and_no_pipeline_edge() {
    let deps = production_dependencies_of(PATHFINDER_CARGO);
    assert_eq!(
        deps,
        ALLOWED_PATHFINDER_DEPENDENCIES
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>(),
        "{PATHFINDER_CARGO}'s production dependencies are what §42 can be checked against"
    );
    for name in &deps {
        assert!(
            !name.contains("chain") && !name.contains("live") && !name.contains("reqwest"),
            "{name} could reach a node"
        );
    }
    let discovery = dependencies_of(DISCOVERY_CARGO, "dependencies");
    assert!(
        !discovery.contains(&"evm-pathfinder".to_string()),
        "§50: the search must not be wired into discovery's production dependencies"
    );
    let dev = dependencies_of(DISCOVERY_CARGO, "dev-dependencies");
    assert!(
        dev.contains(&"evm-pathfinder".to_string()),
        "this gate is the only consumer, and it is a test"
    );
}

#[test]
fn every_quoted_negative_control_names_a_test_that_exists() {
    let source = source_text(PATHFINDER_TESTS);
    let mut quoted = 0usize;
    for (id, _claim, tests) in CONTROLS_RUN {
        for name in tests.split(',').map(|part| part.trim()) {
            quoted += 1;
            assert!(
                source.contains(&format!("fn {name}(")),
                "{id} quotes `{name}`, which is not a test in {PATHFINDER_TESTS}"
            );
        }
    }
    assert_eq!(quoted, 9, "the eight controls plus NC6's second test");
}

#[test]
fn no_candidate_is_named_an_opportunity() {
    let case = Case::load();
    let mut tables = case.tables();
    for (name, text) in std::mem::take(&mut tables) {
        for forbidden in FORBIDDEN_FIELD_SHAPES {
            assert!(
                !text.contains(forbidden),
                "{name} carries the field-shaped word `{forbidden}` — §38/§55 keep a cycle \
                 candidate and an opportunity two different things"
            );
        }
        assert!(
            !text.contains("found profit") && !text.contains("pathfinder found"),
            "{name} claims the search found money"
        );
    }
    for run in &case.runs {
        let text = pretty_raw(run);
        for forbidden in FORBIDDEN_FIELD_SHAPES {
            assert!(!text.contains(forbidden), "a raw run carries {forbidden}");
        }
    }
}

#[test]
fn the_directory_names_no_endpoint_url_and_no_key_material() {
    let digest = endpoint_digest();
    let url = {
        let census: Value = read_value("data/evidence/m7/census-pair-created.json");
        census["_provenance"]["rpc_endpoint"]
            .as_str()
            .expect("M7's census names its endpoint")
            .to_string()
    };
    assert_ne!(url, digest, "the digest is not the URL");
    for name in COMMITTED {
        let text = read_text(name);
        assert!(
            !text.contains(&url),
            "{name} carries the endpoint URL, which no committed file may do"
        );
        assert!(
            !text.contains("http://") && !text.contains("https://"),
            "{name} carries a URL scheme"
        );
        for forbidden in FORBIDDEN_SECRET_SHAPES {
            assert!(!text.contains(forbidden), "{name} carries {forbidden}");
        }
    }
}

// ---------------------------------------------------------------------------
// the independent recompute — no production helper is called below this line
// ---------------------------------------------------------------------------

/// Every aggregate §38 asks for, computed from a raw run document and M9.2's committed edge
/// table alone.
fn recompute(run: &Value, published: &[Value]) -> Value {
    let cycles = run["cycles"].as_array().expect("a run of cycles");
    let census: BTreeMap<String, bool> = published
        .iter()
        .map(|edge| (row_triple(edge), edge["fee"] != Value::Null))
        .collect();
    let graph_edges: BTreeSet<String> = census.keys().cloned().collect();
    let mut two = 0usize;
    let mut three = 0usize;
    let mut complete = 0usize;
    let mut duplicates = 0usize;
    let mut malformed = 0usize;
    let mut edges_not_in_the_graph = 0usize;
    let mut keys: BTreeSet<String> = BTreeSet::new();
    for row in cycles {
        let edges = row["edges"].as_array().expect("a candidate has edges");
        match edges.len() {
            2 => two += 1,
            3 => three += 1,
            _ => malformed += 1,
        }
        if edges
            .iter()
            .all(|edge| *census.get(&row_triple(edge)).unwrap_or(&false))
        {
            complete += 1;
        }
        for edge in edges {
            if !graph_edges.contains(&row_triple(edge)) {
                edges_not_in_the_graph += 1;
            }
        }
        let key = row["canonical_key"].as_str().expect("a key").to_string();
        if !keys.insert(key.clone()) {
            duplicates += 1;
        }
        if least_rotation(edges) != key {
            malformed += 1;
        }
        if row["hop_count"].as_u64() != Some(edges.len() as u64) {
            malformed += 1;
        }
    }
    json!({
        "total_cycles": cycles.len(),
        "cycles_2hop": two,
        "cycles_3hop": three,
        "fee_complete": complete,
        "fee_incomplete": cycles.len() - complete,
        "canonical_unique": duplicates == 0,
        "duplicate_cycles": duplicates,
        "rows_outside_the_hop_bound": malformed,
        "edges_outside_the_published_graph": edges_not_in_the_graph,
        "dfs_states_visited": run["states_visited"],
        "graph_nodes": run["input_graph"]["nodes"],
        "graph_edges": run["input_graph"]["edges"],
        "graph_pools": run["input_graph"]["pools"],
        "target_block": run["input_graph"]["target_block"],
        "chain_id": run["input_graph"]["chain_id"],
        "max_hops": run["config"]["max_hops"],
    })
}

#[test]
fn the_independent_recompute_agrees_with_every_committed_number() {
    let run_a = read_value(RUN_A);
    let published = read_value(M92_GRAPH_REL);
    let edges = published["edges"].as_array().expect("M9.2's edges").clone();
    let recomputed = recompute(&run_a, &edges);
    let summary = read_value(SUMMARY);
    for field in [
        "total_cycles",
        "cycles_2hop",
        "cycles_3hop",
        "fee_complete",
        "fee_incomplete",
        "canonical_unique",
        "duplicate_cycles",
        "dfs_states_visited",
        "graph_nodes",
        "graph_edges",
        "graph_pools",
        "target_block",
        "chain_id",
        "max_hops",
    ] {
        assert_eq!(
            recomputed[field], summary[field],
            "the committed `{field}` is not what the raw run and M9.2's edge table give \
             ({} vs {})",
            recomputed[field], summary[field]
        );
    }
    assert_eq!(
        recomputed["rows_outside_the_hop_bound"],
        json!(0),
        "a row broke the hop bound, its own edge count, or its own canonical rotation"
    );
    assert_eq!(
        recomputed["edges_outside_the_published_graph"],
        json!(0),
        "a candidate walks an edge M9.2 never published"
    );

    let cycles = read_value(CYCLES);
    let rows = cycles["rows"].as_array().expect("candidate rows");
    assert_eq!(
        rows.len() as u64,
        count_of(&recomputed["total_cycles"]),
        "the recompute and the published table disagree on how many candidates exist"
    );
    assert_eq!(
        count_of(&cycles["counts"]["total"]),
        rows.len() as u64,
        "a table's count field disagrees with its own rows"
    );
    assert_eq!(
        count_of(&cycles["counts"]["two_hop"]) + count_of(&cycles["counts"]["three_hop"]),
        rows.len() as u64,
        "the depth split is a partition of the rows"
    );

    let mut seen: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        let edges = row["edges"].as_array().expect("a candidate has edges");
        let key = row["canonical_key"].as_str().expect("a key");
        assert!(
            seen.insert(key.to_string()),
            "{key} appears twice in cycles.json"
        );
        assert_eq!(
            least_rotation(edges),
            key,
            "{key} is not its own route's least rotation"
        );
        assert_eq!(row["hop_count"].as_u64(), Some(edges.len() as u64));
        assert!(
            edges.len() >= 2 && edges.len() <= 3,
            "{key} breaks the hop bound"
        );
        assert_eq!(row["target_block"], summary["target_block"], "{key}");
        assert_eq!(row["chain_id"], summary["chain_id"], "{key}");
        assert_eq!(row["start_token"], edges[0]["token_in"], "{key}");
        assert_eq!(
            edges.first().expect("an edge")["token_in"],
            edges.last().expect("an edge")["token_out"],
            "{key} does not close"
        );
        for pair in edges.windows(2) {
            assert_eq!(
                pair[0]["token_out"], pair[1]["token_in"],
                "{key} is not adjacent"
            );
        }
        let pools: BTreeSet<&str> = edges
            .iter()
            .map(|edge| edge["pool"].as_str().expect("a pool"))
            .collect();
        assert_eq!(pools.len(), edges.len(), "{key} re-uses a pool");
        let entered: BTreeSet<&str> = edges
            .iter()
            .map(|edge| edge["token_in"].as_str().expect("a token"))
            .collect();
        assert_eq!(
            entered.len(),
            edges.len(),
            "{key} stands on a token twice before closing"
        );
        let attested = edges
            .iter()
            .all(|edge| published_attested(edges_root(&published), edge));
        assert_eq!(
            row["fee_status"].as_str(),
            Some(if attested { "complete" } else { "incomplete" }),
            "{key} reports a fee status its own edges do not support"
        );
    }

    let usage = read_value(CANDIDATE_EDGES);
    let mut expected: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows {
        for edge in row["edges"].as_array().expect("edges") {
            *expected.entry(row_triple(edge)).or_insert(0) += 1;
        }
    }
    let usage_rows = usage["rows"].as_array().expect("usage rows");
    assert_eq!(
        usage["totals"]["graph_edges"].as_u64(),
        Some(edges_root(&published).len() as u64),
        "one row per published graph edge"
    );
    assert_eq!(usage_rows.len(), edges_root(&published).len());
    let mut leftovers = expected.clone();
    for row in usage_rows {
        let wanted = row_triple(row);
        assert_eq!(
            row["candidates_using"].as_u64().unwrap_or_default() as usize,
            leftovers.remove(&wanted).unwrap_or_default(),
            "{wanted}'s usage is not the number of candidate rows that walk it"
        );
    }
    assert!(
        leftovers.is_empty(),
        "a candidate walks an edge no usage row accounts for: {leftovers:?}"
    );
    let used: usize = usage_rows
        .iter()
        .filter(|row| row["candidates_using"].as_u64().unwrap_or_default() > 0)
        .count();
    assert_eq!(
        usage["totals"]["edges_in_at_least_one_candidate"].as_u64(),
        Some(used as u64)
    );
    assert_eq!(
        usage["totals"]["edges_in_no_candidate"].as_u64(),
        Some(usage_rows.len() as u64 - used as u64)
    );

    let fee = read_value(FEE_STATUS);
    let published_edges = edges_root(&published);
    assert_eq!(
        fee["graph_edges"].as_u64(),
        Some(published_edges.len() as u64),
        "the census denominator is M9.2's edge count"
    );
    assert_eq!(
        fee["graph_edges_with_attested_fee"]
            .as_u64()
            .unwrap_or_default(),
        published_edges
            .iter()
            .filter(|edge| edge["fee"] != Value::Null)
            .count() as u64,
        "the attested count is read from M9.2's own fee column"
    );
    assert_eq!(fee["candidate_rows"]["complete"], summary["fee_complete"]);
    assert_eq!(
        fee["candidate_rows"]["incomplete"],
        summary["fee_incomplete"]
    );
    assert_eq!(
        fee["rows"].as_array().expect("fee rows").len(),
        rows.len(),
        "one fee row per candidate"
    );
    for row in fee["rows"].as_array().expect("fee rows") {
        let gaps = row["edges_without_attested_fee"]
            .as_array()
            .expect("a gap list");
        assert_eq!(
            row["fee_status"].as_str(),
            Some(if gaps.is_empty() {
                "complete"
            } else {
                "incomplete"
            }),
            "a fee row's own gap list contradicts its status"
        );
    }

    let determinism = read_value(DETERMINISM);
    let run_b = read_value(RUN_B);
    assert_eq!(
        run_a["cycles"], run_b["cycles"],
        "the two raw runs disagree"
    );
    for key in [
        "candidate_count",
        "candidate_order",
        "canonical_keys",
        "candidate_json",
    ] {
        assert_eq!(
            determinism["compared"][key],
            json!(true),
            "§40 wants exact equality on {key}"
        );
    }
    assert_eq!(
        determinism["candidate_json_digest"]["equal"],
        json!(true),
        "the digest of the two candidate lists is the same bytes"
    );

    let benchmark = read_value(BENCHMARK);
    assert_eq!(benchmark["candidate_count"], summary["total_cycles"]);
    assert_eq!(benchmark["cycles_2hop"], summary["cycles_2hop"]);
    assert_eq!(benchmark["cycles_3hop"], summary["cycles_3hop"]);
    assert_eq!(
        benchmark["wall_ms"]["run_a"], run_a["wall_ms"],
        "the benchmark quotes the recorded run"
    );
    assert_eq!(benchmark["graph_node_count"], summary["graph_nodes"]);
    assert_eq!(benchmark["graph_edge_count"], summary["graph_edges"]);

    let controls = read_value(CONTROLS);
    assert_eq!(
        controls["recomputed_over_the_real_graph"]["violations"],
        json!(0),
        "the evidence-side controls were not clean"
    );
    assert_eq!(
        controls["recomputed_over_the_real_graph"]["rows_checked"], summary["total_cycles"],
        "the controls were counted over the same number of rows the summary claims"
    );

    let manifest = read_value(MANIFEST);
    for entry in manifest["files"].as_array().expect("manifest files") {
        let name = entry["file"].as_str().expect("a file name").to_string();
        let text = read_text(&name);
        assert_eq!(
            entry["digest"].as_str(),
            Some(digest(&text).as_str()),
            "{name}'s committed bytes are not what the manifest digests"
        );
        assert_eq!(entry["bytes"].as_u64(), Some(text.len() as u64), "{name}");
    }
    for entry in manifest["raw_files"]
        .as_array()
        .expect("manifest raw files")
    {
        let name = entry["file"].as_str().expect("a file name").to_string();
        let text = read_text(&name);
        assert_eq!(
            entry["digest"].as_str(),
            Some(digest(&text).as_str()),
            "{name}'s committed bytes are not what the manifest digests"
        );
        assert_eq!(entry["bytes"].as_u64(), Some(text.len() as u64), "{name}");
        assert_eq!(
            entry["cycles_recorded"].as_u64(),
            Some(
                read_value(&name)["cycles"]
                    .as_array()
                    .expect("cycles")
                    .len() as u64
            ),
            "{name}'s recorded row count is not its own"
        );
    }
}

/// The edge array of M9.2's committed graph table.
fn edges_root(published: &Value) -> &[Value] {
    published["edges"]
        .as_array()
        .expect("M9.2's graph table lists its edges")
}

/// Whether M9.2's committed table records an attested fee for this candidate edge.
fn published_attested(published_edges: &[Value], edge: &Value) -> bool {
    let wanted = row_triple(edge);
    published_edges
        .iter()
        .any(|found| row_triple(found) == wanted && found["fee"] != Value::Null)
}

#[test]
fn an_injected_wrong_number_is_caught_by_the_recompute() {
    let run = read_value(RUN_A);
    let published = read_value(M92_GRAPH_REL);
    let edges = edges_root(&published).to_vec();
    let honest = recompute(&run, &edges);
    let summary = read_value(SUMMARY);
    assert_eq!(
        honest["total_cycles"], summary["total_cycles"],
        "the honest recompute matches the committed summary before any tamper is claimed to bite"
    );

    // Drop a candidate: every count that depends on the row list has to move.
    let mut fewer = run.clone();
    fewer["cycles"].as_array_mut().expect("cycles").remove(0);
    let after = recompute(&fewer, &edges);
    assert_ne!(
        after["total_cycles"], honest["total_cycles"],
        "the recompute does not count rows, so it could not have caught anything"
    );
    assert_ne!(
        after, honest,
        "a tampered run must not recompute to the committed numbers"
    );

    // Re-key one candidate with a different rotation of its own edges: the least-rotation
    // check has to notice, because the key is a property of the route rather than of the
    // walk that found it. Rotating the *edges* instead would prove nothing — every rotation
    // of a cycle shares its set of rotations, so the minimum cannot move.
    let mut rotated = run.clone();
    {
        let rows = fewer_of(&mut rotated);
        let first = rows[0].as_object_mut().expect("a row");
        let rendered: Vec<String> = first["edges"]
            .as_array()
            .expect("edges")
            .iter()
            .map(row_triple)
            .collect();
        let mut shifted = rendered.clone();
        shifted.rotate_left(1);
        let other_rotation = shifted.join("+");
        assert_ne!(
            other_rotation,
            first["canonical_key"].as_str().expect("a key"),
            "a one-edge cycle has no second rotation; the fixture would need a real one"
        );
        first.insert("canonical_key".to_string(), Value::String(other_rotation));
    }
    let after_rotation = recompute(&rotated, &edges);
    assert_ne!(
        after_rotation["rows_outside_the_hop_bound"], honest["rows_outside_the_hop_bound"],
        "a route whose recorded key is no longer its least rotation is not caught"
    );

    // The edges of one candidate, so the two tamper controls below cannot be spent on an
    // edge the search never used. Every one of the graph's edges is unattested in the honest
    // data, so attesting a single edge would move nothing: the control has to close a whole
    // route, which is the only way `fee_complete` can leave zero.
    let walked: Vec<String> = run["cycles"]
        .as_array()
        .expect("cycles")
        .first()
        .and_then(|row| row["edges"].as_array())
        .expect("a candidate with edges")
        .iter()
        .map(row_triple)
        .collect();
    let indices = walked
        .iter()
        .map(|wanted| {
            edges
                .iter()
                .position(|edge| row_triple(edge) == *wanted)
                .unwrap_or_else(|| panic!("{wanted} is not in M9.2's table"))
        })
        .collect::<Vec<usize>>();

    // Attest every fee on that route: that one candidate has to cross into `complete`. The
    // control is only possible because the honest row is incomplete — said out loud, so a
    // future dataset where it is already complete fails here instead of silently passing.
    let honest_first = run["cycles"][0]["fee_status"]
        .as_str()
        .expect("a fee status");
    assert_eq!(
        honest_first, "incomplete",
        "the fee tamper needs a route that is not already complete"
    );
    let mut tampered_edges = edges.clone();
    for index in &indices {
        tampered_edges[*index]
            .as_object_mut()
            .expect("an edge")
            .insert(
                "fee".to_string(),
                json!({"numerator": 997, "denominator": 1000}),
            );
    }
    let after_fee = recompute(&run, &tampered_edges);
    assert_eq!(
        after_fee["fee_complete"],
        json!(count_of(&honest["fee_complete"]) + 1),
        "attesting every edge of an incomplete route must move exactly one candidate: {}",
        walked.join("+")
    );
    assert_ne!(
        after_fee["fee_incomplete"], honest["fee_incomplete"],
        "the fee split is not a partition of the rows"
    );

    // Delete one walked edge from the witness: the candidate that walked it becomes unfounded.
    let mut short_edges = edges.clone();
    let removed = short_edges.remove(indices[0]);
    let after_missing = recompute(&run, &short_edges);
    assert_ne!(
        after_missing["edges_outside_the_published_graph"],
        honest["edges_outside_the_published_graph"],
        "the cross-check never looks at whether the edge exists"
    );
    assert_eq!(
        row_triple(&removed),
        walked[0],
        "the deleted edge is not the walked one: {}",
        walked[0]
    );
}

/// The candidate rows of a run document, as a mutable array.
fn fewer_of(run: &mut Value) -> &mut Vec<Value> {
    run["cycles"].as_array_mut().expect("cycles")
}

#[test]
fn the_verdict_is_read_off_the_rows_not_a_claim() {
    let summary = read_value(SUMMARY);
    let rows = read_value(CYCLES)["rows"]
        .as_array()
        .expect("candidate rows")
        .len();
    assert_eq!(
        summary["verdict"].as_str(),
        Some(if rows == 0 {
            "NO_CYCLE_CANDIDATES"
        } else {
            "CYCLE_CANDIDATES_FOUND"
        }),
        "the verdict has to follow the row count in the same directory"
    );
    for field in ["gross_profit", "net_profit", "realized_profit"] {
        assert_eq!(
            summary["profit"][field].as_str(),
            Some("N/A"),
            "§56: a stage that priced nothing reports {field} as N/A, never 0"
        );
    }
    for field in [
        "rpc_calls",
        "signatures",
        "broadcasts",
        "real_arbitrage_executed",
    ] {
        assert_eq!(summary["safety"][field], json!(0), "{field}");
    }
}
