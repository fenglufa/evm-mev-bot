//! Configuration for a live run: endpoints, queue bounds, policies.
//!
//! §44 is the reason this file exists as a type rather than as constants: not
//! one endpoint URL appears anywhere in this crate's code, and the chain's
//! identity is never configured either — it is read from the endpoint
//! ([`evm_chain::HttpChainAdapter::connect`]), so a config that points at a
//! different chain runs against that chain instead of pretending to be the one
//! in the file (§45, §46).

use std::path::PathBuf;
use std::time::Duration;

use alloy_primitives::U256;
use serde::Serialize;

use evm_chain::BlockContext;
use evm_execution::ExecutionSetup;
use evm_live::{FlashblockConfig, SourceConfig};
use evm_opportunity::LedgerPolicy;
use evm_risk::RiskThresholds;

/// Which producer the canonical blocks come from.
///
/// `Replay` is here because §32's parity claim cannot be made by comparing two
/// programs: the same [`crate::runner::LiveRunner`] has to run over recorded
/// blocks, which is only possible if the recorded directory is one of the
/// selectable sources.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanonicalSource {
    /// One WebSocket connection: subscribe if the provider can, poll over the
    /// same socket if it cannot (§3.1A).
    WebSocket,
    /// HTTP polling of `eth_blockNumber` and the blocks below it.
    HttpPoll,
    /// A recorded directory, through the same source code.
    Replay { directory: PathBuf },
}

impl CanonicalSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::WebSocket => "websocket",
            Self::HttpPoll => "http-poll",
            Self::Replay { .. } => "replay",
        }
    }
}

/// §50: every queue in the pipeline, and what it does when it is full.
///
/// The answer is different per queue on purpose, and the difference is a design
/// statement rather than an accident of which channel type was at hand:
///
/// ```text
/// source      → event queue     bounded, blocking send  — a slow pipeline stops a
///                                                       source; no market event can
///                                                       be lost
/// pipeline    → sim queue       bounded, try_send       — a full queue declines the
///                                                       job and counts the decline;
///                                                       ingestion must not wait on
///                                                       REVM (§23)
/// workers     → outcome queue   bounded, blocking send  — backpressure lands on the
///                                                       worker, never on ingestion
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct QueueConfig {
    pub event_capacity: usize,
    pub simulation_capacity: usize,
    pub outcome_capacity: usize,
    pub simulation_workers: usize,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            event_capacity: 32,
            simulation_capacity: 16,
            outcome_capacity: 32,
            simulation_workers: 2,
        }
    }
}

impl QueueConfig {
    /// The §50 table, written out of the config that is actually in force rather
    /// than restated in prose.
    pub fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "event_queue": {
                "bounded": true,
                "capacity": self.event_capacity,
                "on_full": "block the source; market events are never dropped (§51)",
            },
            "simulation_queue": {
                "bounded": true,
                "capacity": self.simulation_capacity,
                "on_full": "decline the job, count it, keep the finding in the ledger; ingestion never waits on a simulation (§23)",
            },
            "outcome_queue": {
                "bounded": true,
                "capacity": self.outcome_capacity,
                "on_full": "block the worker, not the pipeline",
            },
            "simulation_workers": self.simulation_workers,
        })
    }
}

/// §25: the risk layer is M4's, unchanged. What M5 adds is only where its two
/// numbers come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RiskConfig {
    /// Wei. `0` means "any strictly positive net figure passes", which is the
    /// untuned choice: it is not a threshold set to make a run look selective
    /// (§76), and it is stated in the evidence rather than implied.
    pub minimum_net_profit_wei: u128,
    /// `None` takes the ceiling from the pinned block's own gas limit — a chain
    /// fact instead of a number this pipeline invented.
    pub maximum_gas: Option<u64>,
}

impl RiskConfig {
    pub fn thresholds(&self, header: &BlockContext) -> RiskThresholds {
        RiskThresholds {
            minimum_net_profit_wei: U256::from(self.minimum_net_profit_wei),
            maximum_gas: self.maximum_gas.unwrap_or(header.gas_limit),
        }
    }

    /// Where each number came from, for the evidence line that has to say it.
    pub fn provenance(&self, header: &BlockContext) -> String {
        match self.maximum_gas {
            Some(gas) => format!(
                "minimum net profit {} wei, gas ceiling {gas} (both from configuration)",
                self.minimum_net_profit_wei
            ),
            None => format!(
                "minimum net profit {} wei, gas ceiling {} taken from block {}'s own gas limit",
                self.minimum_net_profit_wei, header.gas_limit, header.number.0
            ),
        }
    }
}

/// The whole run, in one value.
#[derive(Clone, Debug)]
pub struct PipelineConfig {
    /// The node the state path reads. Never defaulted (§44): a live source
    /// without one is refused before a connection is opened. `None` is only
    /// reachable for a replay run, which contacts no endpoint at all — the
    /// session record then says so rather than naming a node it never spoke to.
    pub rpc_url: Option<String>,
    pub ws_url: Option<String>,
    /// The candidate endpoint, if one is being probed at all. `None` means no
    /// flashblock source runs, and the report says so rather than showing an
    /// empty table.
    pub flashblocks_url: Option<String>,
    pub canonical_source: CanonicalSource,
    /// The attested pools, in the order they are merged. A run without a registry
    /// has no market: a log cannot promote an address to a pool, only the registry
    /// can. More than one directory because the attestations were earned across
    /// milestones; every one of them has to name the same chain (§46).
    pub registry_dirs: Vec<PathBuf>,
    /// Where this session's evidence files go. Created if missing.
    pub evidence_dir: PathBuf,
    /// §7: `None` is "the head at connect time, then forward from it".
    pub start_block: Option<u64>,
    /// Stop after this many canonical blocks, for a run with a fixed sample.
    pub max_blocks: Option<u64>,
    pub duration: Duration,
    pub queues: QueueConfig,
    pub source: SourceConfig,
    pub flashblocks: FlashblockConfig,
    pub ledger: LedgerPolicy,
    pub risk: RiskConfig,
    /// The chain's wrapped-native asset, if this run is allowed to fund routes.
    ///
    /// `None` is a real setting, not a placeholder: M4's §57 confines state
    /// overrides to the test sender, so a route spending any ERC-20 other than
    /// this address cannot be funded without inventing a balance, and every such
    /// finding is recorded as a decline. Configured here rather than derived,
    /// because nothing on the chain labels one.
    pub wrapped_native: Option<alloy_primitives::Address>,
    /// Where a finding's state is read from, when not from this run's own node.
    ///
    /// A file here means the run simulates against **one block's state as an
    /// earlier run recorded it off that node** — the same reads, written down.
    /// This is the §63 route for acceptance over recorded live events: the
    /// recording is labelled, it answers for exactly one height, and a finding
    /// priced at any other block is refused rather than served state that is not
    /// its own (§18, §19). `None` — the only setting a live run uses — means every
    /// state read goes to the endpoint named above.
    pub state_dump: Option<PathBuf>,
    /// M6's execution lane, and `None` is M5's run exactly: no intent is formed,
    /// nothing is built, signed or sent, and the session record says the lane was
    /// absent rather than leaving the reader to infer it from an empty file.
    ///
    /// `Some` is not a licence to broadcast. Two independent facts stand between a
    /// configured lane and a node, and both are the execution crate's, not this
    /// file's: §20's mode defaults to `BuildOnly` and only `Submit` may send, and
    /// §34 refuses to send any intent whose sender was funded by a simulation
    /// override — which, as long as M4's §58 scaffolding is what makes a simulated
    /// arbitrage payable, is every simulated arbitrage. Configuring `Submit`
    /// therefore buys a signed transaction and a recorded blocked submission, not
    /// a trade.
    pub execution: Option<ExecutionSetup>,
    /// §75: print one line per stage as it happens, on the way a reader of a live
    /// run wants it. Off by default so a test run stays quiet; the CLI turns it on
    /// and `--quiet` turns it back off.
    pub progress: bool,
}

impl PipelineConfig {
    /// A run over the live endpoints, WebSocket first.
    pub fn live(
        rpc_url: Option<&str>,
        ws_url: Option<&str>,
        registry_dirs: Vec<PathBuf>,
        evidence_dir: PathBuf,
    ) -> Self {
        Self {
            rpc_url: rpc_url.map(str::to_string),
            ws_url: ws_url.map(str::to_string),
            flashblocks_url: None,
            canonical_source: match ws_url {
                Some(_) => CanonicalSource::WebSocket,
                None => CanonicalSource::HttpPoll,
            },
            registry_dirs,
            evidence_dir,
            start_block: None,
            max_blocks: None,
            duration: Duration::from_secs(60),
            queues: QueueConfig::default(),
            source: SourceConfig::default(),
            flashblocks: FlashblockConfig::default(),
            ledger: LedgerPolicy::default(),
            risk: RiskConfig::default(),
            wrapped_native: None,
            state_dump: None,
            execution: None,
            progress: false,
        }
    }

    /// The same runner over recorded blocks. No endpoint is contacted, so this
    /// is the shape §32's parity test and every pipeline unit test run against.
    pub fn replay(registry_dirs: Vec<PathBuf>, evidence_dir: PathBuf, directory: PathBuf) -> Self {
        let mut config = Self {
            canonical_source: CanonicalSource::Replay { directory },
            ..Self::live(None, None, registry_dirs, evidence_dir)
        };
        // The 900 ms pace exists because a live chain has not made the next block
        // yet. A recording has all of them already, so waiting between cycles
        // would only make a replay run slower than the chain it came from.
        config.source.poll_interval_ms = 0;
        config
    }

    /// What this run does after the risk decision, in the words §48's session
    /// record carries.
    ///
    /// `None` is written as an absence rather than omitted: a reader comparing two
    /// sessions has to be able to tell "this run had no execution lane" from "this
    /// run's record did not look that far". And when the lane is present, the two
    /// facts that decide whether anything can reach a node — the mode and §34's
    /// override rule — are stated in the same object that names the endpoint, so a
    /// `submit` run cannot be read as a promise it never made.
    pub fn execution_description(&self) -> serde_json::Value {
        match &self.execution {
            None => serde_json::json!({
                "lane": "absent",
                "detail": "this run ends at the risk decision; no execution intent is formed",
            }),
            Some(setup) => serde_json::json!({
                "lane": setup.mode.name(),
                "may_sign": setup.mode.may_read_key(),
                "may_submit": setup.mode.may_submit(),
                "endpoint": self.rpc_url,
                "fee_policy": setup.fee,
                "gas_policy": setup.build.gas.describe(),
                "maximum_gas_limit": setup.build.maximum_gas_limit,
                "require_unoverridden_state": setup.build.require_unoverridden_state,
                "receipt_attempts": setup.receipts.attempts,
                "receipt_pause_ms": setup.receipts.between_attempts.as_millis(),
                "detail": "an intent whose sender was funded by a simulation override is built \
                           and recorded, and never submitted",
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backpressure_table_describes_the_config_that_is_in_force() {
        let queues = QueueConfig {
            event_capacity: 7,
            simulation_capacity: 3,
            outcome_capacity: 11,
            simulation_workers: 1,
        };
        let table = queues.describe();
        assert_eq!(table["event_queue"]["capacity"], 7);
        assert_eq!(table["simulation_queue"]["capacity"], 3);
        assert_eq!(table["outcome_queue"]["capacity"], 11);
        assert_eq!(table["simulation_workers"], 1);
    }

    #[test]
    fn an_unset_gas_ceiling_is_the_blocks_own_limit_not_a_number_from_this_file() {
        let header = BlockContext {
            gas_limit: 30_000_000,
            ..test_header()
        };
        let config = RiskConfig::default();
        assert_eq!(
            config.thresholds(&header).maximum_gas,
            30_000_000,
            "the ceiling came from the header"
        );
        assert!(config.provenance(&header).contains("gas limit"));
        let configured = RiskConfig {
            maximum_gas: Some(1234),
            ..RiskConfig::default()
        };
        assert_eq!(configured.thresholds(&header).maximum_gas, 1234);
    }

    #[test]
    fn a_run_without_the_execution_lane_says_so_instead_of_leaving_the_field_out() {
        let config = PipelineConfig::live(
            Some("http://127.0.0.1:1"),
            None,
            Vec::new(),
            PathBuf::from("data/evidence/none"),
        );
        let described = config.execution_description();
        assert_eq!(described["lane"], "absent");
        assert!(config.execution.is_none(), "a live run does not opt in");
        // A configured lane still answers two separate questions: what the mode
        // allows, and what §34 forbids regardless of the mode.
        let armed = PipelineConfig {
            execution: Some(ExecutionSetup::default()),
            ..config
        };
        let build_only = armed.execution_description();
        assert_eq!(build_only["lane"], "build-only");
        assert_eq!(build_only["may_sign"], false);
        assert_eq!(build_only["may_submit"], false);
        let submit = PipelineConfig {
            execution: Some(ExecutionSetup {
                mode: evm_execution::ExecutionMode::Submit,
                ..ExecutionSetup::default()
            }),
            ..armed
        };
        let described = submit.execution_description();
        assert_eq!(described["may_submit"], true);
        assert_eq!(described["require_unoverridden_state"], true);
    }

    fn test_header() -> BlockContext {
        BlockContext {
            chain_id: evm_core::ChainId(1),
            number: evm_core::BlockNumber(10),
            hash: alloy_primitives::B256::ZERO,
            timestamp: 1,
            gas_limit: 1,
            base_fee_per_gas: Some(1),
            excess_blob_gas: None,
            beneficiary: alloy_primitives::Address::ZERO,
            prevrandao: None,
        }
    }
}
