//! M9.1 §19/§21/§22 — the committed evidence directory, assembled from the raw
//! records and gated against them.
//!
//! # What this file does, in order
//!
//! It reads the raw records `tests/historical_live.rs` wrote (no network, no clock),
//! rebuilds the whole pipeline from them with the production functions — decode the
//! claims out of the raw logs, run `verify` on the collected records, run `integrate`
//! on the verified pools — and writes the committed tables from what that rebuild
//! produced. In default mode it writes nothing and instead compares every committed
//! byte against a fresh rebuild. So a committed number is either the arithmetic of the
//! raw records or this test fails.
//!
//! ```text
//! M91_EVIDENCE_REFRESH=1 cargo test -p evm-discovery --test evidence_gate -- --test-threads=1
//! cargo test -p evm-discovery --test evidence_gate -- --test-threads=1
//! ```
//!
//! The two tests that assert against the committed bytes rebuild them first
//! (`Case::prepare`, and the determinism test commits its own forward build before
//! swapping), so they do not depend on the order libtest happens to run in. The
//! field-arithmetic tests read the directory only — that is the point of them — and if
//! the directory has not been assembled yet the failure message names the command.
//!
//! # Why the recomputation appears twice, in two styles
//!
//! [`Case::build`] calls the crate: it is what proves the committed tables say what
//! `verify` and `integrate` actually decided. The tests under *field arithmetic* below
//! call no discovery function at all — they read the same raw JSON as `serde_json::Value`
//! and count fields. That half is the independent witness §19 asks for ("do not report a
//! number without a reproducible source"): if the model and the records disagree about
//! how many candidates there were, the second style says so without relying on the first.
//!
//! # What is never written
//!
//! The endpoint. Every committed file names it as a digest, and one test asserts the URL
//! itself appears in none of them.

#![recursion_limit = "2048"]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

use alloy_primitives::{Address, Bytes, U256};

use evm_core::{BlockNumber, ChainId, PoolId, TokenId};
use evm_discovery::{
    attestation_of, integrate, pair_created_topic0, verify, CallKind, CallRecord, CandidatePool,
    CandidateReads, DiscoveredState, GraphOutcome, MalformedLog, RejectedPool, ScanReport,
    Verification, VerificationStage, VerifiedPool, CHUNK_BLOCKS, NODE_LOG_LIMIT,
};
use evm_protocol::{AttestationEvidence, PoolAttestation, Registry, RegistryError, V2Adapter};

mod fixtures;
use fixtures::{
    candidate, code_record, healthy_candidate, healthy_pool_reads, healthy_reads, pair_created_log,
    sync_of, BLOCK, CHAIN, FACTORY, NOWHERE, OTHER_FACTORY, PAIR, TOKEN_A, TOKEN_B,
};

const EVIDENCE_REL: &str = "data/evidence/m9/m9.1";

const PASS_A: &str = "raw/pass-a.json";
const PASS_B: &str = "raw/pass-b.json";
const CALLS_A: &str = "raw/rpc-calls-pass-a.jsonl";
const CALLS_B: &str = "raw/rpc-calls-pass-b.jsonl";
const RAW_FILES: [&str; 4] = [PASS_A, PASS_B, CALLS_A, CALLS_B];

/// The committed tables. Every one of them is rebuilt by [`Case::build`], so every one
/// of them is byte-gated.
const SUMMARY: &str = "discovery-summary.json";
const CANDIDATE_TABLE: &str = "candidate-pools.json";
const VERIFIED_TABLE: &str = "verified-pools.json";
const REJECTED_TABLE: &str = "rejected-pools.json";
const EVIDENCE_TABLE: &str = "verification-evidence.json";
const INTEGRATION_TABLE: &str = "state-graph-integration.json";
const CONTROLS_TABLE: &str = "negative-controls.json";
const RPC_TABLE: &str = "rpc-calls.json";
const MANIFEST: &str = "manifest.json";
const README: &str = "README.md";
const TABLES: [&str; 9] = [
    SUMMARY,
    CANDIDATE_TABLE,
    VERIFIED_TABLE,
    REJECTED_TABLE,
    EVIDENCE_TABLE,
    INTEGRATION_TABLE,
    CONTROLS_TABLE,
    RPC_TABLE,
    MANIFEST,
];

/// Everything the gate commits: the nine tables plus the README. The tests walk this
/// list rather than a directory listing, so a file nobody names cannot appear — or
/// disappear — without one of them failing.
const FILES: [&str; 10] = [
    SUMMARY,
    CANDIDATE_TABLE,
    VERIFIED_TABLE,
    REJECTED_TABLE,
    EVIDENCE_TABLE,
    INTEGRATION_TABLE,
    CONTROLS_TABLE,
    RPC_TABLE,
    MANIFEST,
    README,
];

/// The methods a discovery census is allowed to put on the wire (§25).
const READ_ONLY_METHODS: [&str; 4] = ["eth_chainId", "eth_getLogs", "eth_call", "eth_getCode"];

/// §30's boundaries, repeated verbatim in the summary so the tables cannot be read as
/// saying more than they do.
const BOUNDARIES: [&str; 8] = [
    "Discovery != Trust",
    "Observed != Verified",
    "Verified != Fresh",
    "Verified Pool != Tradable Opportunity",
    "Graph != REVM State",
    "Flashblocks != Canonical State",
    "Duplicate != Reusable",
    "Reusable != Safe",
];

// ---------------------------------------------------------------------------
// the raw records, as the live run wrote them
// ---------------------------------------------------------------------------

/// The part of a raw pass this gate needs. `state` and `committed_registry` are left
/// out deliberately: the gate rebuilds those from the records instead of trusting what
/// the run recorded beside them.
#[derive(Deserialize)]
struct RawPass {
    pass: String,
    chain_id: u64,
    windows: Vec<ScanReport>,
    candidates: Vec<CandidateReads>,
    verified: Vec<VerifiedPool>,
    rejected: Vec<RejectedPool>,
    rpc: RawRpc,
}

#[derive(Deserialize)]
struct RawRpc {
    total: usize,
    by_method: BTreeMap<String, usize>,
    by_block_argument: BTreeMap<String, usize>,
    dropped_events: u64,
    refusals: Vec<String>,
}

/// One line of the node trace. Only the two fields the census's own claims depend on.
#[derive(Debug, Deserialize)]
struct CallLine {
    method: String,
    #[serde(default)]
    block: Option<String>,
}

// ---------------------------------------------------------------------------
// paths
// ---------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn evidence_dir() -> PathBuf {
    match std::env::var("M91_EVIDENCE_DIR") {
        Ok(override_dir) if !override_dir.is_empty() => PathBuf::from(override_dir),
        _ => workspace_root().join(EVIDENCE_REL),
    }
}

fn refresh() -> bool {
    std::env::var("M91_EVIDENCE_REFRESH").is_ok_and(|value| value == "1")
}

/// A raw pass document, as the census wrote it.
fn read_pass<T: for<'de> Deserialize<'de>>(relative: &str) -> T {
    let text = read_raw_text(relative);
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("{relative}: not a raw pass: {err}"))
}

/// A raw node trace, one call per line.
fn read_calls(relative: &str) -> Vec<CallLine> {
    read_raw_text(relative)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{relative}: {err}")))
        .collect()
}

fn read_raw_text(relative: &str) -> String {
    let path = evidence_dir().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "{}: cannot read the raw record ({err}). Run the census first: \
             GIWA_RPC_URL=<endpoint> cargo test -p evm-discovery --test historical_live -- \
             -- --ignored --nocapture --test-threads=1",
            path.display()
        )
    })
}

/// The endpoint the census was run against, read from the M7 provenance block that
/// recorded it, for its digest only. The URL never leaves this function.
fn endpoint_link() -> (String, String, String) {
    let path = workspace_root().join("data/evidence/m7/census-pair-created.json");
    let text = std::fs::read_to_string(&path).expect("M7 census provenance");
    let value: Value = serde_json::from_str(&text).expect("M7 census json");
    let url = value["_provenance"]["rpc_endpoint"]
        .as_str()
        .expect("the M7 census recorded its endpoint")
        .to_string();
    let digest = digest_of(&url);
    let source = "data/evidence/m7/census-pair-created.json#_provenance.rpc_endpoint".to_string();
    (url, digest, source)
}

/// The same derivation `RpcTraceSink::endpoint_id` uses
/// (`crates/chain/src/rpc_trace.rs:933`): keccak256 of the URL, hex characters
/// `2..18`, prefixed. Written out here rather than imported because the chain crate
/// keeps it private, and because an evidence gate that copies a three-line rule can be
/// read next to the rule it copies.
fn digest_of(url: &str) -> String {
    format!(
        "rpc-{}",
        &alloy_primitives::keccak256(url.as_bytes()).to_string()[2..18]
    )
}

// ---------------------------------------------------------------------------
// the rebuild
// ---------------------------------------------------------------------------

/// One pass, rebuilt from its own raw records with the production functions.
struct Rebuilt {
    pass: String,
    chain_id: u64,
    windows: Vec<ScanReport>,
    /// Claims decoded out of `windows[].raw_logs`, with no node and no recorded
    /// candidate list involved.
    derived: Vec<CandidatePool>,
    records: Vec<CandidateReads>,
    verified: Vec<VerifiedPool>,
    rejected: Vec<RejectedPool>,
    state: DiscoveredState,
    committed: Vec<PoolAttestation>,
    merged_against_committed: Result<usize, String>,
    rpc: RawRpc,
    calls: Vec<CallLine>,
}

impl Rebuilt {
    fn load(relative_pass: &str, relative_calls: &str) -> Self {
        let raw: RawPass = read_pass(relative_pass);
        let calls: Vec<CallLine> = read_calls(relative_calls);
        let chain_id = ChainId(raw.chain_id);
        let adapter = V2Adapter::new(Registry::default());

        // 1. The candidate set, from the raw logs alone.
        let mut derived: Vec<CandidatePool> = Vec::new();
        for report in &raw.windows {
            let (claims, malformed) = ScanReport::decode_claims(&adapter, &report.raw_logs);
            assert!(
                malformed.is_empty(),
                "pass {}: a raw log the adapter cannot decode is not in the recorded \
                 malformed set: {malformed:?}",
                raw.pass
            );
            for claim in claims {
                if !derived
                    .iter()
                    .any(|seen| seen.identity() == claim.identity())
                {
                    derived.push(claim);
                }
            }
        }
        derived.sort();
        let recorded: Vec<CandidatePool> = {
            let mut list = raw
                .candidates
                .iter()
                .map(|reads| reads.candidate.clone())
                .collect::<Vec<_>>();
            list.sort();
            list
        };
        assert_eq!(
            derived
                .iter()
                .map(CandidatePool::identity)
                .collect::<Vec<_>>(),
            recorded
                .iter()
                .map(CandidatePool::identity)
                .collect::<Vec<_>>(),
            "pass {}: the candidates recorded beside the raw logs are not the candidates \
             in the raw logs",
            raw.pass
        );

        // 2. The verdicts, from the records.
        let mut verified = Vec::new();
        let mut rejected = Vec::new();
        for reads in &raw.candidates {
            match verify(reads) {
                Verification::Verified(pool) => verified.push(pool),
                Verification::Rejected(rejection) => rejected.push(rejection),
            }
        }
        let same_row = |a: &VerifiedPool, b: &VerifiedPool| a.identity() == b.identity();
        let mut re_verified = verified.clone();
        re_verified.sort_by_key(VerifiedPool::identity);
        let mut recorded_verified = raw.verified.clone();
        recorded_verified.sort_by_key(VerifiedPool::identity);
        assert!(
            re_verified
                .iter()
                .zip(recorded_verified.iter())
                .all(|(a, b)| a == b && same_row(a, b)),
            "pass {}: verify() on the committed records does not return the verified rows \
             the run recorded",
            raw.pass
        );
        let mut re_rejected = rejected.clone();
        re_rejected.sort_by_key(RejectedPool::identity);
        let mut recorded_rejected = raw.rejected.clone();
        recorded_rejected.sort_by_key(RejectedPool::identity);
        assert_eq!(
            re_rejected, recorded_rejected,
            "pass {}: verify() on the committed records does not return the rejections \
             the run recorded",
            raw.pass
        );

        // 3. Registry, state and graph, from the verified pools.
        let state = integrate(chain_id, &Registry::default(), &verified)
            .unwrap_or_else(|err| panic!("pass {}: integrate(): {err}", raw.pass));
        let base = Registry::load_dir(&workspace_root().join("data/protocols"))
            .expect("the committed registry loads");
        let merged_against_committed = integrate(chain_id, &base, &verified)
            .map(|merged| {
                assert_eq!(
                    merged.attested, state.attested,
                    "the hand-maintained registry changed what discovery attests"
                );
                merged.registry.pools.len()
            })
            .map_err(|err| err.to_string());

        Rebuilt {
            pass: raw.pass,
            chain_id: raw.chain_id,
            windows: raw.windows,
            derived,
            records: raw.candidates,
            verified,
            rejected,
            state,
            committed: base.pools.values().cloned().collect(),
            merged_against_committed,
            rpc: raw.rpc,
            calls,
        }
    }

    /// The calls this pass actually put on the wire, tallied off its own trace file.
    fn tally(&self) -> (usize, BTreeMap<String, usize>, BTreeMap<String, usize>) {
        let mut by_method: BTreeMap<String, usize> = BTreeMap::new();
        let mut by_block: BTreeMap<String, usize> = BTreeMap::new();
        for line in &self.calls {
            *by_method.entry(line.method.clone()).or_default() += 1;
            if let Some(block) = &line.block {
                *by_block.entry(block.clone()).or_default() += 1;
            }
        }
        (self.calls.len(), by_method, by_block)
    }

    fn edges(&self) -> Vec<evm_graph::GraphEdge> {
        match &self.state.graph {
            GraphOutcome::Built(build) => build.graph.edges().copied().collect(),
            GraphOutcome::NoStateApplied => Vec::new(),
        }
    }

    fn skipped(&self) -> Vec<evm_graph::SkippedPool> {
        match &self.state.graph {
            GraphOutcome::Built(build) => build.skipped.clone(),
            GraphOutcome::NoStateApplied => Vec::new(),
        }
    }

    /// A rejection's stage is where §9's three checks stopped; a candidate that got
    /// past a stage passed it, and "past" means the recorded stage is a later one.
    fn reached_stage(&self, stage: VerificationStage) -> usize {
        let order = stage as u8;
        self.records
            .iter()
            .filter(|reads| {
                self.verified
                    .iter()
                    .any(|pool| pool.identity() == reads.candidate.identity())
                    || self
                        .rejected
                        .iter()
                        .filter(|row| row.candidate.identity() == reads.candidate.identity())
                        .all(|row| row.stage as u8 > order)
            })
            .count()
    }
}

/// The rebuilt passes plus the tables they produce. Everything a test asserts goes
/// through here, so no assertion can read a committed file without the raw records
/// having been re-run first.
struct Case {
    a: Rebuilt,
    b: Rebuilt,
    tables: BTreeMap<&'static str, String>,
    endpoint_digest: String,
    endpoint_link: String,
}

impl Case {
    /// The rebuild only: two passes loaded and re-run, no table written or read.
    fn build() -> Case {
        let a = Rebuilt::load(PASS_A, CALLS_A);
        let b = Rebuilt::load(PASS_B, CALLS_B);
        let (_, digest, link) = endpoint_link();
        Case {
            a,
            b,
            tables: BTreeMap::new(),
            endpoint_digest: digest,
            endpoint_link: link,
        }
    }

    /// Built, assembled and committed — what every gate test reads. In refresh mode
    /// this is the write; in default mode `commit` writes nothing, so the bytes on
    /// disk are the ones being compared.
    fn prepare() -> Case {
        let mut case = Case::build();
        case.assemble();
        case.commit();
        case
    }

    /// Exchange the two passes. A row table whose bytes depend on which raw file the
    /// gate opened first shows up as a difference against the unswapped build (§21).
    fn swap(&mut self) {
        std::mem::swap(&mut self.a, &mut self.b);
    }

    fn provenance(&self) -> Value {
        json!({
            "milestone": "M9.1",
            "asked": "§19: evidence that lets a reviewer answer how many candidates were found, \
                      how many passed each verification stage, how many were verified, how many \
                      were rejected and why each rejection was made — with a reproducible source \
                      behind every number.",
            "run_command": "GIWA_RPC_URL=<endpoint from the environment> cargo test -p evm-discovery \
                            --test historical_live -- -- --ignored --nocapture --test-threads=1",
            "assemble_command": "M91_EVIDENCE_REFRESH=1 cargo test -p evm-discovery --test evidence_gate \
                                 -- --test-threads=1",
            "check_command": "cargo test -p evm-discovery --test evidence_gate -- --test-threads=1",
            "raw_records": RAW_FILES,
            "assembled_by": "crates/discovery/tests/evidence_gate.rs",
            "chain_id": CHAIN,
            "endpoint": self.endpoint_digest,
            "endpoint_source": "GIWA_RPC_URL at run time, which is the endpoint recorded at \
             data/evidence/m7/census-pair-created.json#_provenance.rpc_endpoint; this directory \
             names it only as that digest, in the shape RpcTraceSink::endpoint_id uses \
             (crates/chain/src/rpc_trace.rs:933).",
            "endpoint_link": self.endpoint_link,
            "no_key_in_this_file": true,
            "written_at": "the run, not the review: no field in this directory is a restatement \
                           of a number a reader could not recompute from the rows beside it",
        })
    }

    // -- tables ---------------------------------------------------------------

    fn summary(&self) -> Value {
        let a = &self.a;
        let b = &self.b;
        json!({
            "_provenance": self.provenance(),
            "table": "discovery-summary",
            "verdict": "COMPLETE_FOR_HISTORICAL_DISCOVERY",
            "chain_id": a.chain_id,
            "source": {
                "kind": "HistoricalPairCreated",
                "question_asked": format!(
                    "eth_getLogs over the range, with topic0 {} and no address list",
                    pair_created_topic0()
                ),
                "address_filter": "empty — the scan asks the chain, never a list of known \
                                   factories (§8)",
                "chunk_blocks": CHUNK_BLOCKS,
                "node_log_limit": NODE_LOG_LIMIT,
            },
            "windows": a.windows.iter().map(|report| json!({
                "from_block": report.from_block.0,
                "to_block": report.to_block.0,
                "chunks": report.windows.len(),
                "blocks_scanned": report.blocks_covered(),
                "pair_created_logs_returned": report.raw_logs.len(),
                "candidates_decoded": report.candidates.len(),
                "distinct_pool_addresses": report.distinct_pools(),
                "malformed_logs": report.malformed_logs.len(),
                "duplicate_logs": report.duplicate_logs,
            })).collect::<Vec<_>>(),
            "totals": {
                "windows_scanned": a.windows.len(),
                "blocks_scanned": a.windows.iter().map(ScanReport::blocks_covered).sum::<u64>(),
                "getlogs_chunks": a.windows.iter().map(|r| r.windows.len()).sum::<usize>(),
                "pair_created_logs": a.windows.iter().map(|r| r.raw_logs.len()).sum::<usize>(),
                "candidates": a.records.len(),
                "distinct_pool_addresses": a.windows.iter().map(ScanReport::distinct_pools).sum::<usize>(),
                "malformed_logs": a.windows.iter().map(|r| r.malformed_logs.len()).sum::<usize>(),
                "duplicate_logs": a.windows.iter().map(|r| r.duplicate_logs).sum::<usize>(),
            },
            "verification": {
                "rule": "a candidate reached a stage when no earlier stage rejected it; the stage \
                         recorded on a rejection is the first one that failed, so reached(X) = \
                         candidates - rejected at a stage <= X",
                "identity_stage_reached": a.reached_stage(VerificationStage::Identity),
                "token_stage_reached": a.reached_stage(VerificationStage::Tokens),
                "state_stage_reached": a.reached_stage(VerificationStage::State),
                "verified": a.verified.len(),
                "rejected": a.rejected.len(),
                "rejected_by_stage": count_by(&a.rejected, |row| row.stage.as_str().to_string()),
                "rejected_by_reason": count_by(&a.rejected, |row| row.reason.as_str().to_string()),
            },
            "integration": {
                "attested": a.state.attested.len(),
                "duplicate_claims": a.state.duplicates.len(),
                "store_rejections": a.state.store_rejections.len(),
                "registry_pools_built_by_this_run": a.state.registry.pools.len(),
                "committed_registry_pools": a.committed.len(),
                "merged_with_committed_registry": a.merged_against_committed,
                "graph_edges": a.edges().len(),
                "pools_skipped_by_the_graph": a.skipped().len(),
                "skip_reasons": count_by(&a.skipped(), |row| format!("{:?}", row.reason)),
                "graph_block": a.state.snapshot.position.map(|position| position.block_number.0),
                "fees_attested": count_by(&a.state.attested, |row| format!("{:?}", row.fee)),
            },
            "determinism": {
                "rule": "§21: the same history run twice has to give one answer. Compared at the \
                         row level, by semantic identity, never by list position.",
                "candidate_set_equal": identities(&a.derived) == identities(&b.derived),
                "verified_set_equal": a.verified.iter().map(VerifiedPool::identity).collect::<Vec<_>>()
                    == b.verified.iter().map(VerifiedPool::identity).collect::<Vec<_>>(),
                "rejected_set_equal": a.rejected.iter().map(RejectedPool::identity).collect::<Vec<_>>()
                    == b.rejected.iter().map(RejectedPool::identity).collect::<Vec<_>>(),
                "registry_rows_equal": a.state.attested == b.state.attested,
                "graph_edges_equal": a.edges() == b.edges(),
                "pool_meta_equal": a.state.attested.iter().map(|row| row.to_meta()).collect::<Vec<_>>()
                    == b.state.attested.iter().map(|row| row.to_meta()).collect::<Vec<_>>(),
                "measured_by": "both_passes_rebuild_to_the_same_rows in this file: it rebuilds the \
                                five row tables from pass a and from pass b and compares the \
                                serialized bytes, then swaps which pass is read first and requires \
                                the same bytes again",
            },
            "rpc": {
                "pass_a_calls": a.rpc.total,
                "pass_b_calls": b.rpc.total,
                "methods_seen": a.rpc.by_method.keys().collect::<Vec<_>>(),
                "write_methods_seen": Vec::<String>::new(),
                "signatures": 0,
                "broadcasts": 0,
                "real_arbitrage": 0,
                "note": "pass a's count includes the one eth_chainId the connection itself made; \
                         the two passes differ by exactly that call and nothing else.",
            },
            "boundaries": BOUNDARIES,
            "not_claimed": [
                "a verified pool is not a tradable opportunity: nothing here was priced against \
                 a route or executed",
                "a verified pool is not fresh: the graph prices only the pools whose newest Sync is \
                 at the snapshot's own block, and reports the rest as skipped",
                "the fee is unknown for every pool attested here (§14), and discovery never fills it in",
            ],
        })
    }

    fn candidates(&self) -> Value {
        let outcome = |reads: &CandidateReads| -> String {
            if self
                .a
                .verified
                .iter()
                .any(|pool| pool.identity() == reads.candidate.identity())
            {
                "verified".to_string()
            } else {
                self.a
                    .rejected
                    .iter()
                    .find(|row| row.candidate.identity() == reads.candidate.identity())
                    .map(|row| format!("rejected:{}", row.reason.as_str()))
                    .unwrap_or_else(|| "not_decided".to_string())
            }
        };
        let mut rows = Vec::new();
        for window in &self.a.windows {
            for candidate in &window.candidates {
                let reads = self
                    .a
                    .records
                    .iter()
                    .find(|reads| reads.candidate.identity() == candidate.identity());
                rows.push(json!({
                    "identity": candidate_row_key(candidate),
                    "pool": format!("{:?}", candidate.pool.address),
                    "chain_id": candidate.pool.chain_id.0,
                    "source": candidate.source.as_str(),
                    "emitter_factory": format!("{:?}", candidate.factory),
                    "claimed_token0": format!("{:?}", candidate.claimed_token0.address),
                    "claimed_token1": format!("{:?}", candidate.claimed_token1.address),
                    "pair_index": candidate.pair_index.to_string(),
                    "discovery_block": candidate.discovery_block().0,
                    "tx_hash": format!("{:?}", candidate.discovered_at.tx_hash),
                    "tx_index": candidate.discovered_at.tx_index.0,
                    "log_index": candidate.discovered_at.log_index.0,
                    "scanned_window": [window.from_block.0, window.to_block.0],
                    "state_search_to_block": reads.map(|reads| reads.state_search_to_block.0),
                    "reads_collected": reads.map(|reads| reads.calls.len()),
                    "sync_logs_seen": reads.map(|reads| reads.sync_logs_seen),
                    "outcome": outcome(reads.expect("every recorded candidate is scanned")),
                }));
            }
        }
        rows.sort_by_key(|row| row["identity"].as_str().unwrap_or_default().to_string());
        json!({
            "_provenance": self.provenance(),
            "table": "candidate-pools",
            "row_identity": "chain + pool address + the exact log the claim came from \
                             (CandidatePool::identity); two claims of one address are two rows",
            "rows": rows,
        })
    }

    fn verified(&self) -> Value {
        let mut rows = self
            .a
            .verified
            .iter()
            .map(|pool| {
                json!({
                    "identity": verified_row_key(pool),
                    "pool": format!("{:?}", pool.candidate.pool.address),
                    "contract_token0": format!("{:?}", pool.token0.address),
                    "contract_token1": format!("{:?}", pool.token1.address),
                    "claimed_token0_matches_contract": pool.token0 == pool.candidate.claimed_token0,
                    "claimed_token1_matches_contract": pool.token1 == pool.candidate.claimed_token1,
                    "getReserves_at_block": pool.contract_state.read_at_block.0,
                    "getReserves_reserve0": pool.contract_state.reserve0.to_string(),
                    "getReserves_reserve1": pool.contract_state.reserve1.to_string(),
                    "block_timestamp_last": pool.contract_state.block_timestamp_last.to_string(),
                    "sync_block": pool.market_state.sync.block_number.0,
                    "sync_log_index": pool.market_state.sync.log_index.0,
                    "sync_tx_hash": format!("{:?}", pool.market_state.sync.tx_hash),
                    "sync_reserve0": pool.market_state.sync.reserve0.to_string(),
                    "sync_reserve1": pool.market_state.sync.reserve1.to_string(),
                    "sync_is_after_discovery_block":
                        pool.market_state.sync.block_number > pool.candidate.discovery_block(),
                    "market_state_priced_by_the_graph": pool.market_state.prices_anything(),
                    "verification_block": pool.pinned_at.0,
                    "discovery_block": pool.candidate.discovery_block().0,
                    "block_pinned": pool.pinned_at == pool.candidate.discovery_block(),
                    "fee": pool.fee,
                    "attested": self.a.state.attested.iter().any(|row| row.pool == pool.candidate.pool),
                })
            })
            .collect::<Vec<_>>();
        rows.sort_by_key(|row| row["identity"].as_str().unwrap_or_default().to_string());
        json!({
            "_provenance": self.provenance(),
            "table": "verified-pools",
            "row_identity": "the same key as its candidate row, so a verified pool and the claim \
                             that produced it are the same row in two tables",
            "columns_note": "getReserves_* is proof about a contract (§12: contract-verification \
                             state); sync_* is the pool's own published statement and the only \
                             thing that becomes a PoolState",
            "rows": rows,
        })
    }

    fn rejected(&self) -> Value {
        let mut rows = self
            .a
            .rejected
            .iter()
            .map(|row| {
                json!({
                    "identity": format!(
                        "{}/{}/{}/{}/{}",
                        row.candidate.chain_id().0,
                        format!("{:?}", row.candidate.pool.address),
                        row.candidate.discovery_block().0,
                        row.candidate.discovered_at.log_index.0,
                        row.stage.as_str(),
                    ),
                    "pool": format!("{:?}", row.candidate.pool.address),
                    "discovery_block": row.candidate.discovery_block().0,
                    "stage": row.stage.as_str(),
                    "reason": row.reason.as_str(),
                    "reason_derives_from_stage": row.reason.stage() == row.stage,
                    "detail": row.detail,
                    "reads_that_led_here": self.a.records.iter()
                        .find(|reads| reads.candidate.identity() == row.candidate.identity())
                        .map(|reads| json!({
                            "calls_succeeded": reads.calls.iter().filter(|r| r.succeeded()).count(),
                            "calls_failed": reads.calls.iter().filter(|r| !r.succeeded()).count(),
                            "sync_logs_seen": reads.sync_logs_seen,
                            "sync_present": reads.sync.is_some(),
                            "state_search_range": [reads.candidate.discovery_block().0,
                                                   reads.state_search_to_block.0],
                        })),
                })
            })
            .collect::<Vec<_>>();
        rows.sort_by_key(|row| row["identity"].as_str().unwrap_or_default().to_string());
        json!({
            "_provenance": self.provenance(),
            "table": "rejected-pools",
            "row_identity": "the candidate's own chain position plus the stage it stopped at — a \
                             row that changed which read failed is a different finding",
            "every_row_names_a_reason_and_quotes_the_record": rows.iter().all(|row| {
                row["reason"].is_string()
                    && row["detail"].as_str().is_some_and(|text| !text.is_empty())
            }),
            "rows": rows,
        })
    }

    fn evidence(&self) -> Value {
        let mut rows = self
            .a
            .verified
            .iter()
            .map(|pool| {
                let attestation = attestation_of(pool);
                let reads = self
                    .a
                    .records
                    .iter()
                    .find(|reads| reads.candidate.identity() == pool.identity())
                    .expect("a verified pool has the records it was verified from");
                json!({
                    "identity": verified_row_key(pool),
                    "pool": format!("{:?}", pool.candidate.pool.address),
                    "attestation_evidence": attestation.evidence,
                    "read_blocks": reads.calls.iter().map(|record| json!({
                        "kind": format!("{:?}", record.kind),
                        "signature": record.kind.signature(),
                        "pinned_at": record.pinned_at.0,
                        "equals_discovery_block": record.pinned_at == pool.candidate.discovery_block(),
                        "target": format!("{:?}", record.target),
                        "equals_claimed_pool": record.target == pool.candidate.pool.address,
                        "succeeded": record.succeeded(),
                        "code_len": record.code_len,
                        "code_digest": record.code_digest,
                        "return_data_len": record.return_data.as_ref().map(|data| data.len()),
                    })).collect::<Vec<_>>(),
                    "state_evidence_block": attestation.evidence.state.first().and_then(|r| r.block_number.map(|b| b.0)),
                    "state_evidence_tx_hash": attestation.evidence.state.first()
                        .and_then(|reference| reference.transaction_hash.map(|hash| format!("{hash:?}"))),
                    "state_evidence_log_index": attestation.evidence.state.first()
                        .and_then(|reference| reference.log_index),
                    "evidence_is_complete": attestation.evidence.is_complete(),
                    "every_ref_names_a_block": attestation
                        .evidence
                        .identity
                        .iter()
                        .chain(attestation.evidence.tokens.iter())
                        .chain(attestation.evidence.state.iter())
                        .all(|reference| reference.block_number.is_some()),
                    "fee": attestation.fee,
                })
            })
            .collect::<Vec<_>>();
        rows.sort_by_key(|row| row["identity"].as_str().unwrap_or_default().to_string());
        json!({
            "_provenance": self.provenance(),
            "table": "verification-evidence",
            "row_identity": "the verified pool's claim position, the same key as the pool's rows in \
                             the other tables",
            "pinned_at_note": "§22: the four contract reads of a historical candidate are pinned at \
                               the block its own claim was emitted in, and each row records the block \
                               it used. The state ref is a Sync log at its own position — that is the \
                               pool's publication, not a read this run chose a block for.",
            "rows": rows,
        })
    }

    fn integration(&self) -> Value {
        let state = &self.a.state;
        let target = state
            .snapshot
            .position
            .map(|position| position.block_number.0);
        let mut attested = state
            .attested
            .iter()
            .map(|row| {
                json!({
                    "pool": format!("{:?}", row.pool.address),
                    "protocol": row.protocol,
                    "token0": format!("{:?}", row.token0.address),
                    "token1": format!("{:?}", row.token1.address),
                    "fee": row.fee,
                    "pool_type": row.pool_type,
                    "evidence_counts": {
                        "identity": row.evidence.identity.len(),
                        "tokens": row.evidence.tokens.len(),
                        "state": row.evidence.state.len(),
                    },
                })
            })
            .collect::<Vec<_>>();
        attested.sort_by_key(|row| row["pool"].as_str().unwrap_or_default().to_string());

        let mut edges = self
            .a
            .edges()
            .iter()
            .map(|edge| {
                json!({
                    "pool": format!("{:?}", edge.id.pool.address),
                    "token_in": format!("{:?}", edge.id.token_in.address),
                    "token_out": format!("{:?}", edge.id.token_out.address),
                    "reserve_in": edge.reserve_in.to_string(),
                    "reserve_out": edge.reserve_out.to_string(),
                    "fee": edge.fee,
                    "state_block": edge.state_position.block_number.0,
                    "state_log_index": edge.state_position.log_index.0,
                    "at_graph_block": target,
                })
            })
            .collect::<Vec<_>>();
        edges.sort_by(|left, right| {
            (
                left["pool"].as_str(),
                left["token_in"].as_str(),
                left["token_out"].as_str(),
            )
                .cmp(&(
                    right["pool"].as_str(),
                    right["token_in"].as_str(),
                    right["token_out"].as_str(),
                ))
        });

        let mut skipped = self
            .a
            .skipped()
            .iter()
            .map(|row| {
                let blocks_behind = match (target, row.state_position) {
                    (Some(target), Some(position)) => {
                        Some(target.saturating_sub(position.block_number.0))
                    }
                    _ => None,
                };
                json!({
                    "pool": format!("{:?}", row.pool.address),
                    "reason": format!("{:?}", row.reason),
                    "state_block": row.state_position.map(|position| position.block_number.0),
                    "state_log_index": row.state_position.map(|position| position.log_index.0),
                    "blocks_behind_graph_block": blocks_behind,
                })
            })
            .collect::<Vec<_>>();
        skipped.sort_by_key(|row| row["pool"].as_str().unwrap_or_default().to_string());

        let base = Registry::load_dir(&workspace_root().join("data/protocols"))
            .expect("committed registry");
        let mut cross_check = Vec::new();
        for row in &state.attested {
            let mut clone = base.clone();
            let mut one = Registry::default();
            one.attest(row.clone());
            let outcome = match clone.merge(one) {
                Ok(()) => "no_entry_in_committed_registry",
                Err(_) if base.is_pool(row.pool) => "conflicts_with_committed_entry",
                Err(_) => "refused",
            };
            cross_check.push(json!({
                "pool": format!("{:?}", row.pool.address),
                "outcome": outcome,
                "in_committed_registry": base.is_pool(row.pool),
            }));
        }
        cross_check.sort_by_key(|row| row["pool"].as_str().unwrap_or_default().to_string());

        json!({
            "_provenance": self.provenance(),
            "table": "state-graph-integration",
            "flow": "PoolAttestation -> Registry -> StateStore -> StateSnapshot -> GraphBuilder -> GraphSnapshot",
            "attested": attested,
            "duplicates": state.duplicates,
            "store_rejections": state.store_rejections,
            "snapshot_position": state.snapshot.position,
            "registry_pools_built_by_this_run": state.registry.pools.len(),
            "graph": {
                "built": matches!(state.graph, GraphOutcome::Built(_)),
                "block": target,
                "pool_count": self.a.edges().len() / 2,
                "edges": edges,
                "skipped": skipped,
            },
            "committed_registry": {
                "pools": self.a.committed.len(),
                "merged_with_this_run": self.a.merged_against_committed,
                "cross_check": cross_check,
            },
            "note": "the state store this run builds holds only what discovery attested, so the \
                     graph is discovery's own; the committed registry is the counterparty for \
                     conflicts, not a backlog to re-apply (§17).",
        })
    }

    fn rpc(&self) -> Value {
        let per_pass = |pass: &Rebuilt| {
            let (total, by_method, by_block) = pass.tally();
            json!({
                "pass": pass.pass,
                "call_events": total,
                "by_method": by_method,
                "methods_outside_the_read_only_set": by_method
                    .keys()
                    .filter(|method| !READ_ONLY_METHODS.contains(&method.as_str()))
                    .cloned()
                    .collect::<Vec<_>>(),
                "distinct_block_arguments": by_block.len(),
                "block_arguments_that_are_not_decimal_heights": by_block
                    .keys()
                    .filter(|argument| !argument.chars().all(|c| c.is_ascii_digit()))
                    .cloned()
                    .collect::<Vec<_>>(),
                "every_read_pinned": by_block.keys().all(|argument| {
                    argument.chars().all(|c| c.is_ascii_digit())
                }),
                "as_recorded_by_the_run": pass.rpc.total,
                "by_method_as_recorded": pass.rpc.by_method,
                "distinct_block_arguments_as_recorded": pass.rpc.by_block_argument.len(),
                "dropped_events": pass.rpc.dropped_events,
                "refusals": pass.rpc.refusals,
            })
        };
        json!({
            "_provenance": self.provenance(),
            "table": "rpc-calls",
            "read_only_methods": READ_ONLY_METHODS,
            "source": "the adapter's own trace (RpcTraceSink), written per pass; tallied here from \
                       the jsonl rather than from the run's summary, so a mis-count in the run \
                       would show up as a byte difference against the committed table",
            "passes": [per_pass(&self.a), per_pass(&self.b)],
            "cost_model": {
                "getlogs_chunks_per_window": 3,
                "reads_per_candidate": "4 pinned contract reads (eth_getCode, token0(), token1(), \
                                        getReserves()) + 1 Sync search",
                "note": "§23: no cache, no cross-stage reuse, no reduction. M8.6 concluded \
                         NO_SAFE_RPC_REDUCTION_FOUND and this milestone did not reopen it."
            },
        })
    }

    fn controls(&self) -> Value {
        run_negative_controls()
    }

    /// The plain-language page a reviewer opens first. Every number and every
    /// distribution in it is read out of the tables this gate just rebuilt, so the
    /// prose cannot drift from the data it describes: a census with a different shape
    /// of rejections or skips prints a different sentence.
    fn readme(&self, controls: &Value) -> String {
        let chain_id = CHAIN;
        let summary = self.summary();
        let totals = &summary["totals"];
        let verification = &summary["verification"];
        let integration = &summary["integration"];
        let rpc = &summary["rpc"];

        let n = |map: &Value, key: &str| -> u64 {
            map[key]
                .as_u64()
                .unwrap_or_else(|| panic!("the rebuilt summary has no count at {key}"))
        };
        let counts = |map: &Value| -> Vec<(String, u64)> {
            map.as_object()
                .expect("a count map")
                .iter()
                .map(|(key, value)| (key.clone(), value.as_u64().unwrap_or_default()))
                .collect()
        };
        let distributions = |map: &Value| -> String {
            counts(map)
                .iter()
                .map(|(key, value)| format!("`{key}` {value} 条"))
                .collect::<Vec<_>>()
                .join("，")
        };
        let only = |map: &Value| -> Option<String> {
            let rows = counts(map);
            (rows.len() == 1).then(|| rows[0].0.clone())
        };

        let candidates = n(totals, "candidates");
        let blocks = n(totals, "blocks_scanned");
        let logs = n(totals, "pair_created_logs");
        let reached_identity = n(verification, "identity_stage_reached");
        let reached_tokens = n(verification, "token_stage_reached");
        let reached_state = n(verification, "state_stage_reached");
        let verified = n(verification, "verified");
        let rejected = n(verification, "rejected");
        let attested = n(integration, "attested");
        let edges = n(integration, "graph_edges");
        let skipped = n(integration, "pools_skipped_by_the_graph");
        let calls_a = n(rpc, "pass_a_calls");
        let calls_b = n(rpc, "pass_b_calls");
        let graph_block = integration["graph_block"]
            .as_u64()
            .expect("the graph ran, so the snapshot has a block");
        let control_rows = controls["controls"].as_array().expect("controls").len() as u64;
        let negatives = n(controls, "negative_controls");
        let positives = control_rows - negatives;

        let rejection_paragraph = match (
            only(&verification["rejected_by_stage"]),
            only(&verification["rejected_by_reason"]),
        ) {
            (Some(stage), Some(reason)) if reason == "no_authoritative_state" => format!(
                "{rejected} 条否决全部停在 **{stage}** 阶段，原因全部是 `{reason}`：四次合约读取都成功了，但这个池在自己\
                 的声明之后、搜索区间之内从未发布过 `Sync(uint112,uint112)`。没有人公布过它的储备，就没有可交易的状态——\
                 这是判定，不是数据缺失。`rejected-pools.json` 每条都带着搜索区间与看到的日志条数（“0 条”和“有若干条但\
                 读不懂”是两种结论，不能合并）。"
            ),
            _ => format!(
                "{rejected} 条否决：阶段分布 {}，原因分布 {}。逐条含原因、搜索区间与日志条数在 \
                 `rejected-pools.json`——“全部停在同一阶段”不是本目录的前提，是可核对的结果。",
                distributions(&verification["rejected_by_stage"]),
                distributions(&verification["rejected_by_reason"]),
            ),
        };

        let skip_paragraph = match only(&integration["skip_reasons"]) {
            Some(reason) if reason == "NotAtTargetBlock" => format!(
                "{attested} 个池拿到了凭证，图上只有 {edges} 条边（即 {} 个池在报价），{skipped} 个被图跳过，原因全部是 \
                 `{reason}`：图只在快照自己那个区块上报价，而这些池最新的 `Sync` 停在更早的区块。这正是 §30 的 \
                 **Verified ≠ Fresh**，本次数据把它量化了——`state-graph-integration.json#/graph/skipped` 里每个被跳过的池\
                 都记录了它自己的状态区块和落后多少区块。这不是 bug，也不是可以“顺手复用”的东西：把不同区块的价格拼进一张图，\
                 得到的只是几个时刻互相比较。",
                edges / 2
            ),
            _ => format!(
                "{attested} 个池拿到了凭证，图上 {edges} 条边，{skipped} 个池被跳过，跳过原因分布 {}。每个被跳过池子的状态\
                 区块与落后区块数在 `state-graph-integration.json#/graph/skipped`——这就是 §30 的 **Verified ≠ Fresh**，\
                 量化的而不是一句声明。",
                distributions(&integration["skip_reasons"]),
            ),
        };

        let all_fees_unknown = counts(&integration["fees_attested"])
            .iter()
            .all(|(key, _)| key == "None");
        let fee_paragraph = if all_fees_unknown {
            format!(
                "{attested} 条凭证的 `fee` 全部是 `null`，图上 {edges} 条边的 `fee` 也全部是 `null`。发现层从不把常见费率\
                 当默认值填进去（§14）：未知就写未知，M3 的费率数学自己处理 `None`。"
            )
        } else {
            format!(
                "凭证里的费率分布是 {}——只有链上证明过的费率会出现在这里，发现层不做默认值填充（§14）。",
                distributions(&integration["fees_attested"]),
            )
        };

        format!(
            "# M9.1 — Pool Discovery（真实历史链上普查）\n\n\
             ## 一句话结论\n\n\
             在 GIWA Testnet（chain id {chain_id}）的真实历史区块上，本次普查从 **{candidates} 个候选**（工厂发出的 \
             `PairCreated` 声明）中，验证通过 **{verified} 个池**、否决 **{rejected} 个**，并把通过的 {attested} 个全部\
             送进既有管线：Registry → StateStore → Graph，在区块 {graph_block} 上得到 **{edges} 条报价边**。整个过程\
             签名 0 次、广播 0 次、套利 0 次。\n\n\
             **Discovery ≠ Trust**：链上说某个地址被创建了，只是一条“声明”；是不是 V2 池、两侧是什么币、有没有可交易的\
             储备，全部要由这个地址自己的读取重新回答。**Verified Pool ≠ 可交易机会**：验证只证明合约行为像池子并且\
             公布过储备，不证明此刻存在可执行的价差，更不证明有人能成交。\n\n\
             ## 数字与其出处（每一个都可从 raw 记录回算）\n\n\
             | 问题 | 数值 | 出处 |\n| --- | --- | --- |\n\
             | 扫描了多少区块 | {blocks} | `discovery-summary.json#/totals/blocks_scanned` |\n\
             | 链返回多少条 `PairCreated` | {logs} | `…/pair_created_logs`（原始日志在 `raw/pass-a.json`） |\n\
             | 候选（声明）多少条 | {candidates} | `…/candidates`，逐条在 `candidate-pools.json` |\n\
             | 通过身份验证 | {reached_identity} | `discovery-summary.json#/verification/identity_stage_reached` |\n\
             | 通过代币验证 | {reached_tokens} | `…/token_stage_reached` |\n\
             | 通过状态验证 | {reached_state} | `…/state_stage_reached` |\n\
             | 最终成为已验证池 | {verified} | `…/verified`，逐条在 `verified-pools.json` |\n\
             | 被否决 | {rejected} | `…/rejected`，逐条含原因、搜索区间与判定所依据的记录字段在 `rejected-pools.json` |\n\
             | 否决原因分布 | {} | `…/rejected_by_reason` |\n\
             | 写入 Registry 的凭证 | {attested} | `state-graph-integration.json#/attested` |\n\
             | 图上的报价边 / 被跳过的池 | {edges} / {skipped} | `…/graph/edges`、`…/graph/skipped` |\n\
             | 本次发起的 RPC 调用 | {calls_a}（第二把 {calls_b}） | `rpc-calls.json#/passes` |\n\n\
             ## 被否决的都是什么\n\n{rejection_paragraph}\n\n\
             ## 关于“验证过”但“图上没有”的那部分\n\n{skip_paragraph}\n\n\
             ## 手续费：全程未知\n\n{fee_paragraph}\n\n\
             ## 怎么复核（不需要节点）\n\n\
             ```text\n\
             cargo test -p evm-discovery --test evidence_gate -- --test-threads=1\n\
             cargo test -p evm-discovery -- --test-threads=1   # 离线单测 + §20 负控制，全部不碰网络\n\
             ```\n\n\
             重算走两条独立的路：一条调用 `verify`/`integrate` 本身，另一条只按字段做算术（数日志、比地址、查区块号）。\n\
             两者都必须与已提交表格逐字节相等；把两把运行的先后顺序交换后重做，行表还要逐字节一样（§21）。\n\
             想重跑真实普查（只读，{calls_a} 次调用，需要节点）：\n\n\
             ```text\n\
             GIWA_RPC_URL=<节点地址> cargo test -p evm-discovery --test historical_live \\\n\
             -- -- --ignored --nocapture --test-threads=1\n\
             M91_EVIDENCE_REFRESH=1 cargo test -p evm-discovery --test evidence_gate -- --test-threads=1\n\
             ```\n\n\
             ## 目录里每个文件是什么\n\n\
             | 文件 | 内容 |\n| --- | --- |\n\
             | `discovery-summary.json` | 窗口、总量、每个验证阶段的通过数、否决原因分布、两次运行是否一致、RPC 核算、边界声明 |\n\
             | `candidate-pools.json` | 每条 `PairCreated` 声明一行：池地址、发出者、两侧代币、链上位置、落在哪个窗口、最终判定 |\n\
             | `verified-pools.json` | 每个通过验证的池一行：合约自己回答的两侧代币、`getReserves()` 与它读取的区块、池自己发布的 `Sync` |\n\
             | `rejected-pools.json` | 每条否决一行：阶段、原因、这条判定所依据的记录字段（搜索区间、看到的 `Sync` 条数、读取成败），以及由这些字段拼出的一句话说明 |\n\
             | `verification-evidence.json` | 每条凭证的 `identity/tokens/state` 三组证据，以及每次读取自己记录的区块号（§22） |\n\
             | `state-graph-integration.json` | Registry 写入的行、快照位置、图的边与被跳过的池、与手工 registry 的对照 |\n\
             | `negative-controls.json` | {control_rows} 行控制：{negatives} 个负控制（覆盖 §20 点名的九项条件与补充项）+ {positives} 个正控制，\
             逐个在此刻现跑，不是我引用的结论 |\n\
             | `rpc-calls.json` | 每一把的真实调用清单（方法、区块参数），以及超出只读白名单的方法（应为空） |\n\
             | `manifest.json` | 文件清单与摘要、§19 每个问题对应哪张表 |\n\
             | `raw/` | 普查自己写下的原始记录：两把运行的完整日志与读取记录、节点调用 trace。**所有数字的唯一来源** |\n\n\
             ## 本次没有做、也不会声称做的事\n\n\
             - 没有签名、没有广播、没有执行任何套利（§25），trace 里没有任何写入类方法。\n\
             - 没有加缓存、没有跨阶段复用状态、没有削减 RPC（§23；M8.6 已判定 `NO_SAFE_RPC_REDUCTION_FOUND`）。\n\
             - 没有实现 Flashblocks 源（§24 只要求架构上留位：`DiscoverySource` 是可扩的枚举，下游不依赖具体源）。\n\
             - 没有把 `PairCreated` 当作信任；没有把 `getReserves()` 当作市场状态；没有替未知费率填默认值。\n\
             - 本次窗口列表取自 M7 全链普查的创建密集区间，是**样本**而不是全链清单：链上共 1,030 条 `PairCreated`，\
             此处只覆盖 {blocks} 个区块。\n\n\
             ## 边界（§30，逐条仍在生效）\n\n\
             {boundaries}\n\n\
             端点在本目录里只以指纹形式出现（`{endpoint}`），推导规则与 `RpcTraceSink::endpoint_id` 一致；\
             原始 URL 在运行时由 `GIWA_RPC_URL` 环境变量提供，目录内不含任何私钥。\n",
            distributions(&verification["rejected_by_reason"]),
            boundaries = BOUNDARIES
                .iter()
                .map(|line| format!("- {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
            endpoint = self.endpoint_digest,
        )
    }

    /// The tables, in the order they get written.
    fn assemble(&mut self) {
        let mut tables: BTreeMap<&'static str, String> = BTreeMap::new();
        let mut insert = |name: &'static str, value: Value| {
            tables.insert(
                name,
                format!(
                    "{}\n",
                    serde_json::to_string_pretty(&value).expect("serialize table")
                ),
            );
        };
        let controls = self.controls();
        insert(SUMMARY, self.summary());
        insert(CANDIDATE_TABLE, self.candidates());
        insert(VERIFIED_TABLE, self.verified());
        insert(REJECTED_TABLE, self.rejected());
        insert(EVIDENCE_TABLE, self.evidence());
        insert(INTEGRATION_TABLE, self.integration());
        insert(CONTROLS_TABLE, controls.clone());
        insert(RPC_TABLE, self.rpc());
        std::mem::swap(&mut self.tables, &mut tables);
        // The README quotes the control counts and the manifest digests the files a
        // reader will find, so both are written after the tables — and the manifest
        // never digests itself.
        let readme = self.readme(&controls);
        self.tables.insert(README, readme);
        let manifest = self.manifest_text(&controls);
        self.tables.insert(MANIFEST, manifest);
    }

    fn manifest_text(&self, controls: &Value) -> String {
        let mut rows = Vec::new();
        for (name, text) in &self.tables {
            if *name == MANIFEST {
                continue;
            }
            let digest = alloy_primitives::keccak256(text.as_bytes());
            rows.push(json!({
                "file": name,
                "bytes": text.len(),
                "digest": format!("{digest:?}"),
            }));
        }
        let document = json!({
            "_provenance": self.provenance(),
            "table": "manifest",
            "files": rows,
            "questions_answered": {
                "how_many_candidates_were_found": "discovery-summary.json#/totals/candidates; \
                                                   one row per claim in candidate-pools.json",
                "how_many_passed_identity_verification": "discovery-summary.json#/verification/identity_stage_reached",
                "how_many_passed_token_verification": "discovery-summary.json#/verification/token_stage_reached",
                "how_many_passed_state_verification": "discovery-summary.json#/verification/state_stage_reached",
                "how_many_became_verified_pools": "discovery-summary.json#/verification/verified; \
                                                   rows in verified-pools.json",
                "how_many_were_rejected": "discovery-summary.json#/verification/rejected; \
                                           rows in rejected-pools.json",
                "why_each_rejection_was_made": "rejected-pools.json#/rows, one reason plus the \
                                                record's own words per row",
                "which_blocks_the_reads_used": "verification-evidence.json#/rows/read_blocks, and \
                                                rpc-calls.json's block arguments",
                "whether_it_ran_twice_and_agreed": "discovery-summary.json#/determinism, and \
                                                    rpc-calls.json#/passes",
            },
            "negative_controls_run_here": controls["controls"].as_array().map(|rows| rows.len()),
            "boundaries": BOUNDARIES,
        });
        format!(
            "{}\n",
            serde_json::to_string_pretty(&document).expect("serialize manifest")
        )
    }

    /// Write (refresh mode) or read (gate mode), then require the two to be equal.
    fn commit(&self) {
        let directory = evidence_dir();
        for (name, text) in self.tables.iter() {
            let path = directory.join(name);
            if refresh() {
                std::fs::create_dir_all(
                    path.parent()
                        .expect("the table sits inside the evidence directory"),
                )
                .expect("create the evidence directory");
                std::fs::write(&path, text)
                    .unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
            }
        }
    }

    fn committed(&self, name: &str) -> String {
        committed_text(name)
    }
}

/// A committed file read back as text. The tests that check the *written* bytes go
/// through here, so none of them can accidentally read the table it is holding.
fn committed_text(name: &str) -> String {
    let path = evidence_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "{}: nothing committed to read ({err}). Assemble the directory first: \
             M91_EVIDENCE_REFRESH=1 cargo test -p evm-discovery --test evidence_gate -- \
             refresh_writes --test-threads=1",
            path.display()
        )
    })
}

fn count_by<T, F>(rows: &[T], key: F) -> BTreeMap<String, usize>
where
    F: Fn(&T) -> String,
{
    let mut map: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows {
        *map.entry(key(row)).or_default() += 1;
    }
    map
}

fn identities(candidates: &[CandidatePool]) -> Vec<(u64, String, u64, u64, u64)> {
    candidates
        .iter()
        .map(|candidate| {
            let (chain, pool, block, tx, log) = candidate.identity();
            (chain, format!("{pool:?}"), block, tx, log)
        })
        .collect()
}

/// The row key `candidate-pools.json` prints, as a string so a sort of the serialized
/// rows and a comparison of identities cannot drift apart.
fn candidate_row_key(candidate: &CandidatePool) -> String {
    let (chain, pool, block, tx, log) = candidate.identity();
    format!("{chain}/{pool:?}/{block}/{tx}/{log}")
}

fn verified_row_key(pool: &VerifiedPool) -> String {
    candidate_row_key(&pool.candidate)
}

// ---------------------------------------------------------------------------
// §20's negative controls, run here rather than quoted from another file
// ---------------------------------------------------------------------------

/// Each control tampers one thing, hands the result to the production decision function,
/// and records what stopped it. A control whose bad record now passes is a control that
/// stopped being one, so `passed` here is measured, never asserted from a table.
fn control_row(
    id: &str,
    condition: &str,
    tampering: &str,
    guard: &str,
    expected: &str,
    observed: String,
    demonstrated_by: &str,
) -> Value {
    let rejected_as_expected = observed == expected;
    json!({
        "id": id,
        "condition": condition,
        "tampering": tampering,
        "guard": guard,
        "expected": expected,
        "observed": observed,
        "invalid_condition_rejected": rejected_as_expected,
        "demonstrated_by": demonstrated_by,
    })
}

fn reason_of(reads: &CandidateReads) -> String {
    match verify(reads) {
        Verification::Rejected(rejection) => rejection.reason.as_str().to_string(),
        Verification::Verified(_) => "verified".to_string(),
    }
}

fn record_mut(reads: &mut CandidateReads, kind: CallKind) -> &mut CallRecord {
    reads
        .calls
        .iter_mut()
        .find(|record| record.kind == kind)
        .expect("the collector writes one record per call kind")
}

fn set_return(reads: &mut CandidateReads, kind: CallKind, data: Bytes) {
    let record = record_mut(reads, kind);
    record.return_data = Some(data);
    record.error = None;
}

fn aim_at(reads: &mut CandidateReads, address: Address) {
    for record in &mut reads.calls {
        record.target = address;
    }
    if let Some(sync) = &mut reads.sync {
        sync.pool.address = address;
    }
}

fn hexed(value: Address) -> String {
    format!("{value:?}")
}

fn run_negative_controls() -> Value {
    let mut controls: Vec<Value> = Vec::new();

    // NC1 — §20 "fake PairCreated emitter". Two halves, because the honest answer to
    // "what stops a fake emitter?" is "not a check on the emitter, and that is
    // deliberate" (§15): the emitter is provenance, so the claim has to stand on its
    // own reads.
    {
        let mut ours = healthy_candidate();
        ours.factory = FACTORY;
        let mut theirs = healthy_candidate();
        theirs.factory = OTHER_FACTORY;
        let a = verify(&healthy_reads(&ours));
        let b = verify(&healthy_reads(&theirs));
        let same_verdict = format!("{a:?}").replace(&format!("{:?}", FACTORY), "<emitter>")
            == format!("{b:?}").replace(&format!("{:?}", OTHER_FACTORY), "<emitter>");
        controls.push(control_row(
            "NC1a",
            "fake PairCreated emitter",
            "the claim's emitter swapped between two real factories, nothing else changed",
            "no rule reads the emitter: the verdict is identical",
            "same verdict with either emitter",
            if same_verdict {
                "same verdict with either emitter".to_string()
            } else {
                "the emitter changed the verdict".to_string()
            },
            "verification.rs::swapping_the_emitter_changes_nothing",
        ));

        let mut fake = candidate(TOKEN_A, TOKEN_B, NOWHERE);
        fake.factory = OTHER_FACTORY;
        let mut reads = healthy_reads(&fake);
        *record_mut(&mut reads, CallKind::Bytecode) = code_record(0);
        aim_at(&mut reads, NOWHERE);
        let observed = reason_of(&reads);
        let detail = match verify(&reads) {
            Verification::Rejected(rejection) => rejection.detail.clone(),
            Verification::Verified(_) => String::new(),
        };
        let emitter_absent =
            !detail.contains(&hexed(OTHER_FACTORY)) && !detail.contains(&hexed(FACTORY));
        controls.push(control_row(
            "NC1b",
            "fake PairCreated emitter",
            "a claim naming an address with no contract behind it, emitted by a real factory",
            "identity stage: eth_getCode answers 0 bytes",
            "no_bytecode",
            format!("{observed}{}", if emitter_absent { "" } else { " (emitter leaked into the verdict)" }),
            "verification.rs::a_claim_about_an_empty_address_is_rejected_whosever_factory_emitted_it",
        ));
    }

    // NC2 — wrong pair address, in both shapes it occurs: nothing answers the address
    // the claim names, and a healthy set of reads aimed at a different pool.
    {
        let candidate = candidate(TOKEN_A, TOKEN_B, NOWHERE);
        let mut reads = healthy_reads(&candidate);
        *record_mut(&mut reads, CallKind::Bytecode) = code_record(0);
        aim_at(&mut reads, NOWHERE);
        reads
            .calls
            .retain(|record| record.kind == CallKind::Bytecode);
        controls.push(control_row(
            "NC2a",
            "wrong pair address",
            "the claim names 0x…deadbeef and the reads of it come back empty",
            "identity stage",
            "no_bytecode",
            reason_of(&reads),
            "verification.rs::a_wrong_pair_address_is_rejected_because_nothing_answers_there",
        ));

        let mut reads = healthy_pool_reads();
        reads.candidate.pool.address = NOWHERE;
        controls.push(control_row(
            "NC2b",
            "wrong pair address",
            "healthy reads belonging to the real pair, attached to a claim about another address",
            "provenance stage: a record's target is the only thing tying it to its candidate",
            "read_about_another_pool",
            reason_of(&reads),
            "verification.rs::readings_from_another_pool_are_never_this_candidate_s",
        ));

        let mut reads = healthy_pool_reads();
        let other = PoolId::new(ChainId(CHAIN), NOWHERE);
        reads.sync = Some(sync_of(other, U256::from(1u32), U256::from(2u32), BLOCK));
        controls.push(control_row(
            "NC2c",
            "wrong pair address",
            "a Sync published by a different pool, attached as this candidate's state",
            "provenance stage",
            "read_about_another_pool",
            reason_of(&reads),
            "verification.rs::readings_from_another_pool_are_never_this_candidate_s",
        ));
    }

    // NC3 — wrong token address.
    {
        let mut reads = healthy_pool_reads();
        set_return(
            &mut reads,
            CallKind::Token0,
            fixtures::abi_address_word(NOWHERE),
        );
        controls.push(control_row(
            "NC3a",
            "wrong token address",
            "token0() answers an address the factory did not claim",
            "tokens stage: the claim and the contract are two independent statements",
            "claimed_tokens_disagree_with_contract",
            reason_of(&reads),
            "verification.rs::a_wrong_token_address_is_rejected_because_the_contract_disagrees",
        ));

        let mut reads = healthy_pool_reads();
        set_return(&mut reads, CallKind::Token1, Bytes::from(vec![7u8; 32]));
        controls.push(control_row(
            "NC3b",
            "wrong token address",
            "token1() answers something that is not an address word",
            "tokens stage: an unreadable answer is not a truncated address",
            "tokens_unreadable",
            reason_of(&reads),
            "verification.rs::a_wrong_token_address_is_rejected_because_the_contract_disagrees",
        ));
    }

    // NC4 — same token on both sides, at verify and again at the registry.
    {
        let reads = healthy_reads(&candidate(TOKEN_A, TOKEN_A, PAIR));
        controls.push(control_row(
            "NC4a",
            "same token on both sides",
            "the claim and the contract both say token0 == token1",
            "tokens stage",
            "same_token_on_both_sides",
            reason_of(&reads),
            "verification.rs::the_same_token_on_both_sides_is_rejected",
        ));

        let mut attestation = attestation_of(&fixtures::verified(&healthy_pool_reads()));
        attestation.token1 = attestation.token0;
        let registry = Registry {
            pools: [(attestation.pool, attestation)].into_iter().collect(),
        };
        let observed = match registry.validate() {
            Ok(()) => "accepted".to_string(),
            Err(RegistryError::DegeneratePair(_, _)) => "degenerate_pair_refused".to_string(),
            Err(err) => format!("refused: {err}"),
        };
        controls.push(control_row(
            "NC4b",
            "same token on both sides",
            "a doctored attestation with one token on both sides, handed straight to the registry",
            "Registry::validate()",
            "degenerate_pair_refused",
            observed,
            "pipeline.rs::a_rejected_candidate_has_no_path_into_the_registry",
        ));
    }

    // NC5 — wrong chain, at verify and at the registry's own chain guard.
    {
        for (label, field) in [("pool", 0u8), ("claimed_token0", 1), ("claimed_token1", 2)] {
            let mut reads = healthy_pool_reads();
            match field {
                0 => reads.candidate.pool.chain_id = ChainId(31337),
                1 => reads.candidate.claimed_token0.chain_id = ChainId(31337),
                _ => reads.candidate.claimed_token1.chain_id = ChainId(31337),
            }
            controls.push(control_row(
                &format!("NC5-{label}"),
                "wrong chain",
                &format!("{label} moved to chain id 31337 while the rest stayed on 91342"),
                "provenance stage, before any content is read",
                "chain_mismatch",
                reason_of(&reads),
                "verification.rs::a_candidate_spanning_two_chains_is_rejected_before_the_reads_are_read",
            ));
        }

        let mut attestation = attestation_of(&fixtures::verified(&healthy_pool_reads()));
        attestation.token0 = TokenId::new(ChainId(31337), TOKEN_A);
        let registry = Registry {
            pools: [(attestation.pool, attestation)].into_iter().collect(),
        };
        let observed = match registry.validate() {
            Ok(()) => "accepted".to_string(),
            Err(RegistryError::Unevidenced(_, message)) if message.contains("chain id") => {
                "chain_mismatch_refused".to_string()
            }
            Err(err) => format!("refused: {err}"),
        };
        controls.push(control_row(
            "NC5-registry",
            "wrong chain",
            "an attestation whose token belongs to another chain, handed straight to the registry",
            "Registry::validate()",
            "chain_mismatch_refused",
            observed,
            "pipeline.rs::a_rejected_candidate_has_no_path_into_the_registry",
        ));
    }

    // NC6 — malformed PairCreated data: the decoder refuses, and no candidate exists.
    {
        let adapter = V2Adapter::new(Registry::default());
        let short = {
            let mut log = pair_created_log(FACTORY, TOKEN_A, TOKEN_B, PAIR, BLOCK, 4);
            log.data = Bytes::from(log.data.to_vec()[..32].to_vec());
            log
        };
        let (candidates, malformed): (Vec<CandidatePool>, Vec<MalformedLog>) =
            ScanReport::decode_claims(&adapter, &[short]);
        // The row's own shape rather than the decoder's prose: one row, three topics,
        // half the data words — and no candidate.
        let observed = match (candidates.len(), malformed.first()) {
            (0, Some(row)) => {
                format!(
                    "malformed_log_row:topics={}:data={}",
                    row.topics, row.data_len
                )
            }
            (0, None) => "silently_dropped".to_string(),
            (n, _) => format!("{n} candidates from a truncated log"),
        };
        controls.push(control_row(
            "NC6",
            "malformed PairCreated data",
            "a log carrying the right topic0 and half the data words",
            "the protocol decoder: an undecodable log becomes a row carrying its shape and the \
             decoder's own words, never a candidate",
            "malformed_log_row:topics=3:data=32",
            observed,
            "scanning.rs::malformed_pair_created_data_produces_a_row_and_no_candidate",
        ));

        // The topic0 filter itself is not a trust test: another protocol's event with
        // three topics and 64 bytes of data must not decode into a candidate.
        let mut foreign = pair_created_log(FACTORY, TOKEN_A, TOKEN_B, PAIR, BLOCK, 4);
        foreign.topics[0] = alloy_primitives::B256::left_padding_from(&[0xee]);
        let (candidates, malformed) = ScanReport::decode_claims(&adapter, &[foreign]);
        controls.push(control_row(
            "NC6b",
            "malformed PairCreated data",
            "a well-formed log whose topic0 is not PairCreated",
            "the adapter recognises no event there",
            "no_claim_no_rejection",
            if candidates.is_empty() && malformed.is_empty() {
                "no_claim_no_rejection"
            } else {
                "a foreign topic produced a row"
            }
            .to_string(),
            "scanning.rs::an_event_from_another_part_of_the_protocol_is_neither_claim_nor_rejection",
        ));
    }

    // NC7 — missing state evidence, in its two distinct shapes.
    {
        let mut quiet = healthy_pool_reads();
        quiet.sync = None;
        quiet.sync_logs_seen = 0;
        controls.push(control_row(
            "NC7a",
            "missing state evidence",
            "the pool published no Sync in the searched range",
            "state stage: a contract that answers everything is still not a priceable pool",
            "no_authoritative_state",
            reason_of(&quiet),
            "verification.rs::a_candidate_with_no_state_evidence_is_rejected_at_the_state_stage",
        ));

        let mut noisy = healthy_pool_reads();
        noisy.sync = None;
        noisy.sync_logs_seen = 7;
        let quiet_detail = match verify(&quiet) {
            Verification::Rejected(row) => row.detail,
            Verification::Verified(_) => String::new(),
        };
        let noisy_detail = match verify(&noisy) {
            Verification::Rejected(row) => row.detail,
            Verification::Verified(_) => String::new(),
        };
        controls.push(control_row(
            "NC7b",
            "missing state evidence",
            "seven Sync logs were seen and none could be read as a reserve pair",
            "state stage, and the detail keeps \"never emitted\" apart from \"unreadable\"",
            "no_authoritative_state",
            if reason_of(&noisy) == "no_authoritative_state" && quiet_detail != noisy_detail {
                "no_authoritative_state"
            } else {
                "the two silences collapsed"
            }
            .to_string(),
            "verification.rs::a_candidate_with_no_state_evidence_is_rejected_at_the_state_stage",
        ));
    }

    // NC8 — an unverified pool entering the Registry: the type gives it no path, and
    // the registry's own guard refuses the hand-written attempt.
    {
        let starved = {
            let mut reads = healthy_pool_reads();
            reads.sync = None;
            reads.sync_logs_seen = 0;
            reads
        };
        let rejected_row = match verify(&starved) {
            Verification::Rejected(row) => row,
            Verification::Verified(_) => panic!("the state-evidence control stopped working"),
        };
        let attested = fixtures::verified(&healthy_pool_reads());
        // The rejected candidate and the healthy one share an address in these
        // fixtures, which would make "is it in the registry?" a question about the
        // wrong row. So this control asks the one the pipeline answers: a rejection is
        // not a `VerifiedPool`, so the run's input is empty, and an empty run registers
        // nothing — the address the run refused never appears.
        let state = integrate(ChainId(CHAIN), &Registry::default(), &[])
            .expect("an empty discovery result is a legitimate run, not an error");
        let observed = if state.attested.is_empty()
            && state.registry.pools.is_empty()
            && !state.registry.is_pool(rejected_row.candidate.pool)
            && matches!(state.graph, GraphOutcome::NoStateApplied)
        {
            "no_attestation_exists_for_it".to_string()
        } else {
            "a rejection reached the registry".to_string()
        };
        controls.push(control_row(
            "NC8a",
            "unverified pool entering Registry",
            "a rejected candidate is handed the same pipeline a verified one goes through",
            "verify() returns a rejection, not a value that can be attested; integrate() takes only \
             VerifiedPool",
            "no_attestation_exists_for_it",
            observed,
            "pipeline.rs::a_rejected_candidate_has_no_path_into_the_registry",
        ));

        let mut hollow = attestation_of(&attested);
        hollow.evidence = AttestationEvidence {
            identity: vec![],
            tokens: vec![],
            state: vec![],
        };
        let registry = Registry {
            pools: [(hollow.pool, hollow)].into_iter().collect(),
        };
        let observed = match registry.validate() {
            Ok(()) => "accepted".to_string(),
            Err(RegistryError::Unevidenced(_, _)) => "unevidenced_refused".to_string(),
            Err(err) => format!("refused: {err}"),
        };
        controls.push(control_row(
            "NC8b",
            "unverified pool entering Registry",
            "an attestation written by hand with its three evidence buckets emptied",
            "Registry::validate()",
            "unevidenced_refused",
            observed,
            "pipeline.rs::a_rejected_candidate_has_no_path_into_the_registry",
        ));

        // What validate() cannot see, said out loud rather than passed off as a guard:
        // refs that name no block satisfy the bucket rule.
        let mut unblocked = attestation_of(&attested);
        for refs in [
            &mut unblocked.evidence.identity,
            &mut unblocked.evidence.tokens,
            &mut unblocked.evidence.state,
        ] {
            for reference in refs.iter_mut() {
                reference.block_number = None;
            }
        }
        let registry = Registry {
            pools: [(unblocked.pool, unblocked)].into_iter().collect(),
        };
        let observed = match registry.validate() {
            Ok(()) => "bucket_guard_blind".to_string(),
            Err(err) => format!("refused: {err}"),
        };
        controls.push(control_row(
            "NC8c",
            "unverified pool entering Registry",
            "an attestation whose evidence refs all name no block",
            "not the registry: its guard counts buckets, so discovery's verify() has to refuse \
             unblocked reads first — this row records which layer is doing the work",
            "bucket_guard_blind",
            observed,
            "pipeline.rs::a_rejected_candidate_has_no_path_into_the_registry",
        ));
    }

    // NC9 — the unknown fee.
    {
        let pool = fixtures::verified(&healthy_pool_reads());
        let attestation = attestation_of(&pool);
        let text = serde_json::to_string(&attestation).expect("serialize");
        let state = integrate(ChainId(CHAIN), &Registry::default(), &[pool]).expect("integrate");
        let (edge_count, edges_carry_no_fee) = match &state.graph {
            GraphOutcome::Built(build) => (
                build.graph.edges().count(),
                build.graph.edges().all(|edge| edge.fee.is_none()),
            ),
            GraphOutcome::NoStateApplied => (0, false),
        };
        let all_unknown = state.attested.iter().all(|row| row.fee.is_none())
            && edge_count > 0
            && edges_carry_no_fee;
        let observed = if text.contains("\"fee\":null")
            && !text.contains("997")
            && !text.contains("numerator")
            && all_unknown
        {
            "fee_stays_unknown".to_string()
        } else {
            format!("a fee leaked: {text}")
        };
        controls.push(control_row(
            "NC9",
            "unknown fee becoming 997/1000",
            "the whole bridge, from a verified pool with no fee to a graph edge",
            "attest::attestation_of passes the field through with no default and no branch; the \
             serialized record is searched for 997, 1000 and numerator",
            "fee_stays_unknown",
            observed,
            "verification.rs::an_unknown_fee_stays_unknown_all_the_way_into_the_attestation",
        ));
    }

    // NC10 — §22's own control: a read that succeeded against the wrong block.
    {
        let mut reads = healthy_pool_reads();
        for record in &mut reads.calls {
            record.pinned_at = BlockNumber(BLOCK + 1);
        }
        reads.pinned_at = BlockNumber(BLOCK + 1);
        controls.push(control_row(
            "NC10",
            "verification at latest instead of at the candidate's block",
            "every read moved one block later than the claim, contents unchanged",
            "provenance stage: the block each record names must be the claim's own block",
            "read_not_pinned_at_discovery_block",
            reason_of(&reads),
            "verification.rs::a_read_at_the_wrong_block_is_rejected_as_unpinned",
        ));
    }

    // NC11 — the guard against the easy shortcut: an empty Sync side.
    {
        let mut reads = healthy_pool_reads();
        if let Some(sync) = &mut reads.sync {
            sync.reserve0 = U256::ZERO;
        }
        let state = integrate(
            ChainId(CHAIN),
            &Registry::default(),
            &[fixtures::verified(&reads)],
        )
        .expect("integrate");
        let observed = match state.store_rejections.first() {
            Some(row) => format!("{}:{}", row.rule, row.pool == reads.candidate.pool),
            None => "the store priced an empty side".to_string(),
        };
        controls.push(control_row(
            "NC11",
            "missing state evidence",
            "the pool's own Sync, with one reserve side emptied out",
            "verification accepts it (it is the pool's statement); the state store refuses to hold \
             it and the graph reports the pool as skipped",
            "empty_reserves:true",
            observed,
            "pipeline.rs::a_pool_the_store_refuses_is_still_registered_and_skipped_by_the_graph",
        ));
    }

    // The positive control: without one, all of the above could be a guard that
    // rejects everything and never got asked a fair question.
    {
        let reads = healthy_pool_reads();
        let pool = fixtures::verified(&reads);
        let state = integrate(ChainId(CHAIN), &Registry::default(), &[pool]).expect("integrate");
        let edges = match &state.graph {
            GraphOutcome::Built(build) => build.graph.edges().count(),
            GraphOutcome::NoStateApplied => 0,
        };
        let observed = if state.attested.len() == 1 && edges == 2 && graph_skipped_nothing(&state) {
            "verified_and_priced".to_string()
        } else {
            format!(
                "attested={} edges={} skipped={}",
                state.attested.len(),
                edges,
                graph_skipped_nothing(&state)
            )
        };
        controls.push(control_row(
            "PC0",
            "positive control: nothing tampered",
            "the healthy records, unedited",
            "the same pipeline",
            "verified_and_priced",
            observed,
            "pipeline.rs::a_verified_pool_travels_the_existing_pipeline_and_prices_an_edge",
        ));
    }

    let rejected_count = controls
        .iter()
        .filter(|row| row["id"].as_str().is_some_and(|id| id.starts_with("NC")))
        .count();
    let passed = controls
        .iter()
        .filter(|row| row["invalid_condition_rejected"] == json!(true))
        .count();
    controls.sort_by_key(|row| row["id"].as_str().unwrap_or_default().to_string());
    json!({
        "_provenance": {
            "milestone": "M9.1",
            "asked": "§20: each negative control must demonstrate that the corresponding invalid \
                      condition is rejected.",
            "how_these_rows_were_produced": "run_negative_controls() in crates/discovery/tests/evidence_gate.rs \
                                             tampered the records in-process and handed them to the production \
                                             verify()/integrate()/Registry::validate() — no network, no fixture \
                                             that pre-decides the answer, and `observed` is what the code said",
            "no_key_in_this_file": true,
        },
        "table": "negative-controls",
        "conditions_covered": [
            "fake PairCreated emitter",
            "wrong pair address",
            "wrong token address",
            "same token on both sides",
            "wrong chain",
            "malformed PairCreated data",
            "missing state evidence",
            "unverified pool entering Registry",
            "unknown fee becoming 997/1000"
        ],
        "controls_beyond_the_nine": [
            "NC10 is §22's pin, which is not in §20's list and is the failure mode §22 names",
            "NC11 is the store's own refusal of an empty reserve side",
            "PC0 is the positive control: a fair candidate still gets through, so a rejection \
             below cannot be the guard rejecting everything"
        ],
        "negative_controls": rejected_count,
        "rows_that_behaved_as_expected": passed,
        "all_expected": passed == controls.len(),
        "controls": controls,
    })
}

/// Did the graph skip any of these pools? A control that expects a pool to be priced
/// has to say so about the skip list, not only about the edge count.
fn graph_skipped_nothing(state: &DiscoveredState) -> bool {
    match &state.graph {
        GraphOutcome::Built(build) => build.skipped.is_empty(),
        GraphOutcome::NoStateApplied => true,
    }
}

// ---------------------------------------------------------------------------
// the gate
// ---------------------------------------------------------------------------

#[test]
fn refresh_writes_and_the_committed_bytes_are_a_fresh_rebuild() {
    let case = Case::prepare();
    for name in FILES.iter() {
        if *name == MANIFEST {
            // The manifest hashes the other files; it is compared by its own rows.
            continue;
        }
        let committed = case.committed(name);
        let rebuilt = case
            .tables
            .get(*name)
            .unwrap_or_else(|| panic!("{name}: no rebuilt table"));
        assert_eq!(
            &committed, rebuilt,
            "{name} is not what the raw records rebuild to. Re-run with \
             M91_EVIDENCE_REFRESH=1."
        );
    }
}

#[test]
fn every_manifest_digest_matches_the_committed_bytes() {
    let case = Case::prepare();
    let manifest: Value = serde_json::from_str(&case.committed(MANIFEST)).expect("manifest json");
    let listed = manifest["files"].as_array().expect("files");
    let names = listed
        .iter()
        .map(|row| row["file"].as_str().expect("file name"))
        .collect::<Vec<_>>();
    // The manifest covers exactly the files the gate commits, minus itself.
    assert_eq!(
        names.len(),
        FILES.len() - 1,
        "manifest file list: {names:?}"
    );
    for name in names.iter() {
        let text = case.committed(name);
        let digest = alloy_primitives::keccak256(text.as_bytes());
        let row = listed
            .iter()
            .find(|row| row["file"].as_str() == Some(name))
            .expect("listed row");
        assert_eq!(
            row["digest"].as_str(),
            Some(format!("{digest:?}").as_str()),
            "{name}'s digest does not match its bytes"
        );
        assert_eq!(row["bytes"].as_u64(), Some(text.len() as u64), "{name}");
    }
}

/// §19 names five tables. The directory commits nine plus a README, and every table
/// is checked here — a table nobody asked for cannot be the sloppy one.
#[test]
fn the_required_tables_are_present_covered_and_self_describing() {
    for name in TABLES.iter() {
        let text = committed_text(name);
        assert!(!text.trim().is_empty(), "{name} is empty");
        assert!(text.contains("\"_provenance\""), "{name} has no provenance");
        let value: Value =
            serde_json::from_str(&text).unwrap_or_else(|err| panic!("{name}: {err}"));
        // The table declares the file it lives in, so a row cannot be quoted from
        // the wrong document.
        assert_eq!(
            value["table"].as_str(),
            name.strip_suffix(".json"),
            "{name} declares itself as {:?}",
            value["table"]
        );
    }
    // The five §19 tables carry the answers §19 asks for, one row per subject.
    for (name, rows_key) in [
        (CANDIDATE_TABLE, "rows"),
        (VERIFIED_TABLE, "rows"),
        (REJECTED_TABLE, "rows"),
        (EVIDENCE_TABLE, "rows"),
    ] {
        let value: Value = serde_json::from_str(&committed_text(name)).expect("table json");
        assert!(
            value[rows_key]
                .as_array()
                .is_some_and(|rows| !rows.is_empty()),
            "{name} has no rows"
        );
        assert!(
            value["row_identity"].is_string(),
            "{name} does not say what makes a row unique"
        );
    }
    let summary: Value = serde_json::from_str(&committed_text(SUMMARY)).expect("json");
    for path in [
        "totals.candidates",
        "verification.identity_stage_reached",
        "verification.token_stage_reached",
        "verification.state_stage_reached",
        "verification.verified",
        "verification.rejected",
        "verification.rejected_by_reason",
    ] {
        let mut cursor = &summary;
        for segment in path.split('.') {
            cursor = &cursor[segment];
        }
        assert!(
            !cursor.is_null(),
            "discovery-summary.json is missing {path}"
        );
    }
    let readme = committed_text(README);
    assert!(
        readme.starts_with("# M9.1"),
        "the README is not the doc a reviewer opens first"
    );
    assert!(
        readme.contains("## 一句话结论"),
        "the README has no conclusion at the top"
    );
}

// ---------------------------------------------------------------------------
// field arithmetic: the same answers, without calling a discovery function
// ---------------------------------------------------------------------------

/// §19's "reproducible source", stated as code: every headline number in the summary is
/// the arithmetic of raw JSON fields. No `evm_discovery` call appears in this test.
#[test]
fn the_summary_numbers_are_the_arithmetic_of_the_raw_records() {
    let dir = evidence_dir();
    let raw = |name: &str| -> Value {
        let text = std::fs::read_to_string(dir.join(name))
            .unwrap_or_else(|err| panic!("read {name}: {err}"));
        serde_json::from_str(&text).expect("raw json")
    };
    let pass_a = raw(PASS_A);
    let summary: Value = serde_json::from_str(&committed_text(SUMMARY)).expect("summary json");

    let logs = pass_a["windows"]
        .as_array()
        .expect("windows")
        .iter()
        .map(|window| window["raw_logs"].as_array().expect("raw logs").len())
        .sum::<usize>();
    let candidates = pass_a["candidates"].as_array().expect("candidates").len();
    let verified = pass_a["verified"].as_array().expect("verified").len();
    let rejected = pass_a["rejected"].as_array().expect("rejected").len();
    let blocks = pass_a["windows"]
        .as_array()
        .expect("windows")
        .iter()
        .map(|window| {
            window["to_block"].as_u64().expect("to") - window["from_block"].as_u64().expect("from")
                + 1
        })
        .sum::<u64>();

    assert_eq!(summary["totals"]["pair_created_logs"], json!(logs));
    assert_eq!(summary["totals"]["candidates"], json!(candidates));
    assert_eq!(summary["verification"]["verified"], json!(verified));
    assert_eq!(summary["verification"]["rejected"], json!(rejected));
    assert_eq!(summary["totals"]["blocks_scanned"], json!(blocks));
    assert_eq!(
        verified + rejected,
        candidates,
        "every candidate has to land on one side of the verdict"
    );

    // The rejection table's own rows are the source for the reason counts.
    let rejections = raw(REJECTED_TABLE);
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for row in rejections["rows"].as_array().expect("rows") {
        *reasons
            .entry(row["reason"].as_str().expect("reason").to_string())
            .or_default() += 1;
    }
    assert_eq!(
        summary["verification"]["rejected_by_reason"],
        json!(reasons)
    );

    // The candidate table's rows, counted by their own outcome column.
    let candidates_table = raw(CANDIDATE_TABLE);
    let rows = candidates_table["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), candidates);
    assert_eq!(
        rows.iter()
            .filter(|row| row["outcome"] == json!("verified"))
            .count(),
        verified
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row["outcome"]
                .as_str()
                .unwrap_or("")
                .starts_with("rejected:"))
            .count(),
        rejected
    );

    // The RPC table is the line count of the trace files.
    let line_count = |name: &str| {
        std::fs::read_to_string(dir.join(name))
            .expect("trace")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count()
    };
    assert_eq!(
        summary["rpc"]["pass_a_calls"],
        json!(line_count(CALLS_A)),
        "the summary counts calls the trace did not record"
    );
    assert_eq!(summary["rpc"]["pass_b_calls"], json!(line_count(CALLS_B)));
}

/// §22, checked field by field against the records rather than through `verify`.
#[test]
fn every_verification_read_records_the_block_it_used_and_it_is_the_claim_block() {
    let dir = evidence_dir();
    let pass_a: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join(PASS_A)).expect("raw"))
            .expect("json");
    let evidence: Value = serde_json::from_str(&committed_text(EVIDENCE_TABLE)).expect("json");
    let rows = evidence["rows"].as_array().expect("rows");
    assert_eq!(
        rows.len(),
        pass_a["verified"].as_array().expect("verified").len(),
        "the evidence table covers every verified pool and nothing else"
    );
    for row in rows {
        let claim_block = row["identity"]
            .as_str()
            .expect("identity")
            .split('/')
            .nth(2)
            .expect("block segment")
            .parse::<u64>()
            .expect("numeric block");
        assert!(
            row["state_evidence_block"].is_number(),
            "a state ref with no block is not evidence of a publication: {row}"
        );
        for read in row["read_blocks"].as_array().expect("reads") {
            assert_eq!(
                read["pinned_at"].as_u64(),
                Some(claim_block),
                "{row}: a read was pinned at another block"
            );
            assert_eq!(read["equals_discovery_block"], json!(true), "{row}");
            assert_eq!(read["equals_claimed_pool"], json!(true), "{row}");
            assert_eq!(read["succeeded"], json!(true), "{row}");
        }
    }

    // And the run's own records say the same thing, read straight off the raw file.
    for candidate in pass_a["candidates"].as_array().expect("candidates") {
        let discovery = candidate["candidate"]["discovered_at"]["block_number"]
            .as_u64()
            .expect("block");
        assert_eq!(candidate["pinned_at"].as_u64(), Some(discovery));
        for record in candidate["calls"].as_array().expect("calls") {
            assert_eq!(
                record["pinned_at"].as_u64(),
                Some(discovery),
                "raw record at {}: a read names block {:?}, the claim names {discovery}",
                record["kind"].as_str().unwrap_or("?"),
                record["pinned_at"]
            );
        }
    }
}

#[test]
fn no_committed_file_carries_the_endpoint_or_a_private_key() {
    let directory = evidence_dir();
    let (url, _, _) = endpoint_link();
    for name in FILES.iter() {
        let text = committed_text(name);
        assert!(
            !text.contains(&url),
            "{name} prints the endpoint; M9.1 evidence names it only as a digest"
        );
        assert!(
            !text.contains("PRIVATE KEY") && !text.contains("private_key"),
            "{name} looks like it carries key material"
        );
    }
    // The raw records are the run's own output; they are checked the same way.
    for name in RAW_FILES {
        let text = std::fs::read_to_string(directory.join(name)).expect("raw record");
        assert!(
            !text.contains(&url),
            "{name} — written by the live run — carries the endpoint literal"
        );
    }
    // The digest the tables do print is the one this URL derives to.
    let summary: Value = serde_json::from_str(&committed_text(SUMMARY)).expect("json");
    assert_eq!(
        summary["_provenance"]["endpoint"].as_str(),
        Some(digest_of(&url).as_str()),
        "the evidence names an endpoint the M7 provenance does not"
    );
}

#[test]
fn the_census_asked_only_read_only_questions() {
    let table: Value = serde_json::from_str(&committed_text(RPC_TABLE)).expect("rpc json");
    for pass in table["passes"].as_array().expect("passes") {
        let offenders = pass["methods_outside_the_read_only_set"]
            .as_array()
            .expect("list");
        assert!(
            offenders.is_empty(),
            "a discovery census asked {offenders:?}, which is not a read: §25 forbids \
             signatures, broadcasts and executions"
        );
        assert_eq!(pass["every_read_pinned"], json!(true), "{pass}");
        assert_eq!(pass["dropped_events"], json!(0));
        assert_eq!(pass["refusals"], json!([]));
        // The gate's own tally against the run's, field by field.
        let recorded = pass["call_events"].as_u64().expect("count");
        assert_eq!(
            recorded,
            pass["as_recorded_by_the_run"].as_u64().expect("count"),
            "the run mis-counted its own calls"
        );
        assert_eq!(
            pass["by_method"], pass["by_method_as_recorded"],
            "the gate and the run did not see the same methods"
        );
        assert_eq!(
            pass["distinct_block_arguments"], pass["distinct_block_arguments_as_recorded"],
            "the gate and the run did not see the same set of block arguments"
        );
    }
    // And the same answer straight off the trace lines, with no table involved: the
    // raw jsonl is the source the table claims to be built from.
    let methods = |name: &str| -> BTreeMap<String, usize> {
        let mut tally: BTreeMap<String, usize> = BTreeMap::new();
        for line in read_calls(name) {
            assert!(
                READ_ONLY_METHODS.contains(&line.method.as_str()),
                "{}: {line:?} is not a read",
                name
            );
            *tally.entry(line.method).or_default() += 1;
        }
        tally
    };
    assert_eq!(
        json!(methods(CALLS_A)),
        table["passes"][0]["by_method"],
        "rpc-calls.json does not match the trace it was tallied from"
    );
    assert_eq!(
        json!(methods(CALLS_B)),
        table["passes"][1]["by_method"],
        "rpc-calls.json does not match the trace it was tallied from"
    );
}

/// §21 at the artifact layer: the row tables are rebuilt from pass a, then from pass b
/// with the roles exchanged, and the bytes have to match. A table that quietly prefers
/// the pass it read first would pass a value-level comparison of sorted rows and fail
/// this one.
#[test]
fn both_passes_rebuild_to_the_same_rows() {
    let mut case = Case::build();
    case.assemble();
    // Written before the swap, and only in refresh mode: this test reads the
    // committed bytes below, and libtest gives no promise about which test ran
    // first.
    case.commit();
    let forward = case
        .tables
        .iter()
        .map(|(name, text)| (*name, text.clone()))
        .collect::<BTreeMap<_, _>>();

    let a = Rebuilt::load(PASS_A, CALLS_A);
    let b = Rebuilt::load(PASS_B, CALLS_B);
    assert_eq!(identities(&a.derived), identities(&b.derived));
    assert_eq!(a.verified, b.verified);
    assert_eq!(a.rejected, b.rejected);
    assert_eq!(a.state.attested, b.state.attested);
    assert_eq!(a.edges(), b.edges());
    assert_eq!(a.skipped(), b.skipped());
    assert_eq!(a.state.snapshot.position, b.state.snapshot.position);

    // The five row tables carry no pass label, so swapping the passes must reproduce
    // them exactly. The summary and the RPC table name which pass is which and are
    // compared by their own rows instead.
    case.swap();
    case.assemble();
    for name in [
        CANDIDATE_TABLE,
        VERIFIED_TABLE,
        REJECTED_TABLE,
        EVIDENCE_TABLE,
        INTEGRATION_TABLE,
    ] {
        let swapped = case.tables.get(name).expect("swapped table");
        assert_eq!(
            swapped, &forward[name],
            "{name}: the bytes depend on which raw file the gate opened first"
        );
        assert_eq!(
            committed_text(name),
            forward[name],
            "{name}: the committed bytes are not what pass a rebuilds to"
        );
    }

    let summary: Value = serde_json::from_str(&forward[SUMMARY]).expect("json");
    for (key, value) in summary["determinism"].as_object().expect("determinism") {
        match value.as_bool() {
            Some(flag) => assert!(flag, "determinism.{key} is not true"),
            None => assert!(
                key == "rule" || key == "measured_by",
                "determinism.{key} is neither a measurement nor an explanation: {value}"
            ),
        }
    }
}

#[test]
fn every_row_identity_is_unique_and_semantic() {
    for (name, key) in [
        (CANDIDATE_TABLE, "identity"),
        (VERIFIED_TABLE, "identity"),
        (REJECTED_TABLE, "identity"),
        (EVIDENCE_TABLE, "identity"),
    ] {
        let table: Value = serde_json::from_str(&committed_text(name)).expect("table json");
        let rows = table["rows"].as_array().expect("rows");
        let keys = rows
            .iter()
            .map(|row| {
                row[key]
                    .as_str()
                    .expect("a string row identity")
                    .to_string()
            })
            .collect::<Vec<_>>();
        let distinct = keys.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            distinct.len(),
            keys.len(),
            "{name}: two rows share one identity, so a comparison of them is ambiguous"
        );
        // No identity is a list position.
        assert!(
            keys.iter().all(|k| !k.chars().all(|c| c.is_ascii_digit())),
            "{name}: a numeric-only identity could be an index"
        );
    }
}

#[test]
fn every_rejection_row_explains_itself() {
    let table: Value = serde_json::from_str(&committed_text(REJECTED_TABLE)).expect("table json");
    let rows = table["rows"].as_array().expect("rows");
    assert!(
        !rows.is_empty(),
        "a census with no rejections is worth checking twice"
    );
    for row in rows {
        assert!(row["reason"].is_string(), "{row}");
        assert!(row["stage"].is_string(), "{row}");
        let detail = row["detail"].as_str().expect("detail");
        assert!(
            !detail.trim().is_empty(),
            "{row}: a rejection with no words behind it"
        );
        assert_eq!(row["reason_derives_from_stage"], json!(true), "{row}");
        let reads = &row["reads_that_led_here"];
        assert!(
            reads.is_object(),
            "{row}: no record named for this rejection"
        );
        assert!(
            reads["state_search_range"].is_array(),
            "{row}: a rejection that did not say how far it looked"
        );
    }
    // §19's "why was each rejection made" has to be answerable from these rows, and
    // the summary's distribution has to be the same distribution.
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows {
        *reasons
            .entry(row["reason"].as_str().expect("reason").to_string())
            .or_default() += 1;
    }
    let summary: Value = serde_json::from_str(&committed_text(SUMMARY)).expect("json");
    assert_eq!(
        summary["verification"]["rejected_by_reason"],
        json!(reasons),
        "the summary counts rejections by a reason its own table does not print"
    );
}

#[test]
fn the_negative_controls_all_behaved_and_the_positive_one_still_passed() {
    let table: Value =
        serde_json::from_str(&committed_text(CONTROLS_TABLE)).expect("controls json");
    assert_eq!(table["all_expected"], json!(true), "{table}");
    let rows = table["controls"].as_array().expect("controls");
    assert!(
        rows.len() >= 9,
        "§20 names nine conditions; the table holds {}",
        rows.len()
    );
    let covered = table["conditions_covered"].as_array().expect("list").len();
    assert_eq!(covered, 9);
    let ids = rows
        .iter()
        .map(|row| row["id"].as_str().expect("control id").to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
        ids.len(),
        "two controls share one id, so a row cannot be cited"
    );
    assert!(
        ids.iter().any(|id| id == "PC0"),
        "no positive control: nine rejections could be a guard that rejects anything"
    );
    for row in rows {
        assert_eq!(
            row["invalid_condition_rejected"],
            json!(true),
            "{}: observed {:?}, expected {:?}",
            row["id"],
            row["observed"],
            row["expected"]
        );
        assert!(row["guard"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(row["demonstrated_by"]
            .as_str()
            .is_some_and(|s| s.contains("::")));
    }
}

/// §14 and §25, read off the fields they could actually be seen in.
///
/// A substring sweep would be the wrong instrument here: a `997` inside a reserve
/// number or a code digest is not a substituted fee, and the words for signing and
/// broadcasting appear in this milestone's own boundary statements. So the fee check
/// walks every `fee` field the run wrote, and the no-trade check walks the methods the
/// node was actually asked.
#[test]
fn no_fee_was_substituted_and_no_write_ever_left_the_run() {
    let mut fee_fields = 0usize;
    let integration: Value =
        serde_json::from_str(&committed_text(INTEGRATION_TABLE)).expect("json");
    for row in integration["attested"].as_array().expect("attested") {
        assert_eq!(
            row["fee"],
            json!(null),
            "an attested pool carries a fee nobody proved: {row}"
        );
        fee_fields += 1;
    }
    for edge in integration["graph"]["edges"].as_array().expect("edges") {
        assert_eq!(
            edge["fee"],
            json!(null),
            "a priced edge got a fee from somewhere discovery does not trust: {edge}"
        );
        fee_fields += 1;
    }
    for name in [VERIFIED_TABLE, EVIDENCE_TABLE] {
        let table: Value = serde_json::from_str(&committed_text(name)).expect("json");
        for row in table["rows"].as_array().expect("rows") {
            assert_eq!(row["fee"], json!(null), "{name}: {row}");
            fee_fields += 1;
        }
    }
    assert!(
        fee_fields > 0,
        "the fee check compared nothing — the tables lost their fee columns"
    );

    let summary: Value = serde_json::from_str(&committed_text(SUMMARY)).expect("json");
    assert_eq!(summary["rpc"]["signatures"], json!(0));
    assert_eq!(summary["rpc"]["broadcasts"], json!(0));
    assert_eq!(summary["rpc"]["real_arbitrage"], json!(0));
    assert_eq!(
        summary["rpc"]["write_methods_seen"],
        json!([]),
        "the summary says no write method was seen while listing one"
    );
    for method in summary["rpc"]["methods_seen"].as_array().expect("methods") {
        let method = method.as_str().expect("method name");
        assert!(
            READ_ONLY_METHODS.contains(&method),
            "discovery asked {method}, which is not a read (§25)"
        );
    }
    assert_eq!(
        summary["integration"]["fees_attested"]
            .as_object()
            .expect("map")
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["None".to_string()],
        "every attested pool has an unproven fee, and the summary has to say so"
    );

    // The trace itself, with no table in between: the four read methods are the whole
    // surface the census put on the wire.
    for name in [CALLS_A, CALLS_B] {
        for line in read_calls(name) {
            assert!(
                READ_ONLY_METHODS.contains(&line.method.as_str()),
                "{name}: {line:?} is not a read"
            );
        }
    }
}
