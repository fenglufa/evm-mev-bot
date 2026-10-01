//! `evm-mev-bot` — the entry point §47 asks for: one subcommand, `live`.
//!
//! This crate's whole job is to turn flags and environment into a
//! [`PipelineConfig`] and print what came back. It contains no market logic:
//! the chain's identity is read from the endpoint (§45), the state path is the
//! replay engine's (§9), and a run ends at a risk decision.
//!
//! The arguments and the printing live here as a library so §54's first test
//! layer can reach them: which flags describe which run, and which combinations
//! describe no run at all, are decisions with a right and a wrong answer, and
//! `src/main.rs` is only the three lines that call them.
//!
//! Two rules this code is written under:
//!
//! - **No endpoint is defaulted** (§44). `--rpc-url` (or `GIWA_RPC_URL`) is
//!   required; a WebSocket URL only ever comes from `--ws-url` / `GIWA_WS_URL`,
//!   a candidate endpoint only from `--flashblocks-url` /
//!   `GIWA_FLASHBLOCKS_URL`. Nothing here knows what GIWA's hosts are called, so
//!   pointing the same binary at another node is a flag, not a code change.
//!   Paths to *this project's own* committed attestations and evidence are
//!   defaulted, because those are files in the repository, not third-party
//!   endpoints.
//! - **Nothing here sends** (§26). There is no signer, no private key, no
//!   transaction-sending dependency, and no sequencer or relay endpoint. The
//!   furthest a run goes is a `RiskDecision` written to disk; an `Accept` is a
//!   record, not an order (§25).
//!
//! M6 moved that boundary, in one direction and by one flag. `--execution-mode`
//! hands an accepted finding to `evm-execution`, which may build and — only in
//! `sign-only` or `submit` — sign it. Three things did not change here:
//!
//! - the flag is *absent* by default, and the mode `evm-execution` defaults to is
//!   `build-only`, so a binary started with no new flag at all behaves exactly as
//!   it did in M5 (§20);
//! - this crate still never reads, prints, or passes a private key. The key comes
//!   from `GIWA_EXECUTION_PRIVATE_KEY`, in the signer's own process of reading it,
//!   and §19 forbids a default load even here (§17);
//! - the CLI cannot turn on something the execution layer would do anyway: §34
//!   keeps an intent whose sender was funded by a simulation override out of a
//!   node, and that answer is the gate's, not a flag's.
//!
//! M6 adds one subcommand, `validate`, for §35's controlled test transaction: one
//! value transfer — zero value to the sender's own account by default — driven through
//! the lane so that build, sign, submit and receipt are verified against the real chain
//! without any market data being invented (§43: M6 does not have to be profitable). It
//! is a separate command rather than a flag on `live` because a default market run must
//! never send (§26, §U), and the only way to guarantee that is for the sending path to
//! be a different path. `--execution-mode` is required there: `submit` is the only
//! spelling that puts bytes on the wire, and it has to be typed.
//!
//! Exit codes: `0` the session ended on its own terms, `1` it was stopped by a
//! fact that makes the session incomplete (§8's unrecovered gap, a closed queue,
//! a chain mismatch), `2` the flags themselves could not describe a run.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{json, Value};

use evm_chain::HttpChainAdapter;
use evm_execution::{ExecutionMode, ExecutionSetup};
use evm_live::{FlashblockConfig, SourceConfig};
use evm_metrics::{Clock, Metrics};
use evm_pipeline::config::{CanonicalSource, PipelineConfig, QueueConfig, RiskConfig};
use evm_pipeline::error::PipelineError;
use evm_pipeline::runner;
use evm_pipeline::EvidenceFile;
use evm_pipeline::EvidenceWriter;

/// Where the canonical blocks come from, in the words the CLI uses. The runner's
/// own [`CanonicalSource`] is not a `ValueEnum` because it carries a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum SourceArg {
    /// Canonical heads from a WebSocket connection.
    // Spelled like the other two, which are one token each in this CLI's
    // vocabulary: a caller should not have to guess `web-socket`.
    #[value(name = "websocket")]
    WebSocket,
    /// Canonical heads from polling `eth_blockNumber` over RPC.
    HttpPoll,
    /// Canonical heads from a directory of recorded blocks.
    Replay,
}

#[derive(Parser)]
#[command(
    name = "evm-mev-bot",
    version,
    about = "GIWA live market pipeline: block → state → graph → opportunity → simulation → risk → execution lane.",
    long_about = "Reads a chain and decides whether an opportunity would have been worth taking. \
                  With no --execution-mode it never signs and never sends (§26): an Accept is a \
                  line in an evidence file. The flag hands accepted findings to the execution \
                  lane, which builds them and — only when named — signs them; §34 keeps an \
                  intent funded by a simulation override from reaching a node in any mode, and \
                  no mode has a relay, a bundle builder, or gas-bidding logic. The other \
                  subcommand, `validate`, runs one §35 execution validation transaction — a \
                  zero-value transfer to the signing account unless told otherwise — to verify \
                  build, sign, submit and receipt on the real chain; it trades nothing."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    // `live` carries every knob a session has, so it is a few hundred bytes wide
    // against `validate`'s handful — the box keeps the enum itself small.
    /// Run the pipeline against live market data for a while.
    Live(Box<LiveArgs>),
    /// Drive one §35 validation transaction through the execution lane.
    Validate(ValidateArgs),
}

/// §47's flags, plus the knobs §50's queues and §25's thresholds need in order to
/// be settable without recompiling.
#[derive(Parser)]
pub struct LiveArgs {
    /// JSON-RPC endpoint the state path reads. Required for every source except
    /// a replay, which contacts no endpoint at all.
    #[arg(long, env = "GIWA_RPC_URL")]
    rpc_url: Option<String>,

    /// WebSocket endpoint for canonical heads. Without it the run polls over RPC,
    /// and the session record says which one it did (§48's `source` field).
    #[arg(long, env = "GIWA_WS_URL")]
    ws_url: Option<String>,

    /// Sub-second candidate endpoint, if one is being probed at all.
    #[arg(long, env = "GIWA_FLASHBLOCKS_URL")]
    flashblocks_url: Option<String>,

    /// First block to process: the run reads the head when this is absent (§7).
    #[arg(long)]
    start_block: Option<u64>,

    /// How long to keep running, in seconds.
    #[arg(long, default_value_t = 60)]
    duration: u64,

    /// Stop after this many canonical blocks, whichever comes first.
    #[arg(long)]
    max_blocks: Option<u64>,

    /// Which producer supplies canonical blocks. Inferred from the other flags
    /// when absent: a `--ws-url` means websocket, a `--replay-dir` means replay.
    #[arg(long, value_enum)]
    source: Option<SourceArg>,

    /// Recorded block directory, used when the source is `replay` (§32's parity
    /// run uses this same binary rather than a second program).
    #[arg(long)]
    replay_dir: Option<PathBuf>,

    /// Attested pools. Repeatable; every directory is loaded and merged, and all
    /// of them must name the chain the endpoint answers for (§46).
    #[arg(long)]
    registry_dir: Vec<PathBuf>,

    /// Where this session's evidence files go. Created if missing.
    #[arg(long, default_value = "data/evidence/m5/live")]
    evidence_dir: PathBuf,

    /// The chain's wrapped-native token, as the input asset simulations are
    /// allowed to fund. Without it every finding is declined for a stated reason
    /// and counted (§57: no balance is ever manufactured).
    #[arg(long)]
    wrapped_native: Option<String>,

    /// Read simulation state from one recorded block instead of from the node: a
    /// `StateDump` written by an earlier run off this same chain. §63's acceptance
    /// over recorded live events, and the session record prints the file it used.
    #[arg(long)]
    state_dump: Option<PathBuf>,

    /// How far a run goes after the risk decision (M6 §20). Absent means there is
    /// no execution lane at all and the run ends exactly where M5's ended.
    ///
    /// `build-only` forms the intent and builds it; `sign-only` also signs, locally,
    /// and keeps the bytes in the evidence; `submit` also hands them to the endpoint
    /// and tracks the receipt. The latter two read the key from the environment
    /// variable named in §17 and from nowhere else — this flag takes no key, no
    /// address and no path, because a command line is not a place a private key
    /// belongs. And none of the three can send an intent whose sender was funded by
    /// a simulation override (§34), which is every simulated arbitrage this
    /// workspace can produce.
    #[arg(long)]
    execution_mode: Option<String>,

    #[arg(long)]
    event_capacity: Option<usize>,

    #[arg(long)]
    simulation_capacity: Option<usize>,

    #[arg(long)]
    outcome_capacity: Option<usize>,

    #[arg(long)]
    simulation_workers: Option<usize>,

    /// How often the canonical source asks for the head.
    #[arg(long)]
    poll_interval_ms: Option<u64>,

    /// How often the candidate source reads the pending header.
    #[arg(long)]
    flashblock_poll_interval_ms: Option<u64>,

    /// §76: a threshold set low is stated as set low. `0` means "any strictly
    /// positive net figure", which is the untuned choice, not a tuned one.
    #[arg(long, default_value_t = 0)]
    minimum_net_profit_wei: u128,

    /// Gas ceiling. Absent means each block's own gas limit answers it.
    #[arg(long)]
    maximum_gas: Option<u64>,

    /// Do not print the per-block progress lines (§75).
    #[arg(long)]
    quiet: bool,

    /// Print the session record as JSON when the run ends.
    #[arg(long)]
    json: bool,
}

impl LiveArgs {
    /// The config these flags describe, or the reason they do not describe one.
    pub fn to_config(&self) -> std::result::Result<PipelineConfig, String> {
        let registry_dirs = if self.registry_dir.is_empty() {
            default_registry_dirs()
        } else {
            self.registry_dir.clone()
        };

        let canonical_source = match (self.source, &self.replay_dir, &self.ws_url) {
            (Some(SourceArg::Replay), Some(dir), _) => CanonicalSource::Replay {
                directory: dir.clone(),
            },
            (Some(SourceArg::Replay), None, _) => {
                return Err("--source replay needs --replay-dir".to_string());
            }
            (Some(SourceArg::WebSocket), _, Some(_)) => CanonicalSource::WebSocket,
            (Some(SourceArg::WebSocket), _, None) => {
                return Err("--source websocket needs --ws-url".to_string());
            }
            (Some(SourceArg::HttpPoll), _, _) => CanonicalSource::HttpPoll,
            (None, Some(dir), _) => CanonicalSource::Replay {
                directory: dir.clone(),
            },
            (None, None, Some(_)) => CanonicalSource::WebSocket,
            (None, None, None) => CanonicalSource::HttpPoll,
        };

        let wrapped_native = match &self.wrapped_native {
            Some(spec) => Some(
                alloy_primitives::FixedBytes::<20>::from_str(spec)
                    .map(|bytes| alloy_primitives::Address::from_slice(bytes.as_slice()))
                    .map_err(|error| format!("{spec} is not an address: {error}"))?,
            ),
            None => None,
        };

        if self.rpc_url.is_none() && !matches!(canonical_source, CanonicalSource::Replay { .. }) {
            return Err(
                "--rpc-url (or GIWA_RPC_URL) is required: the state path reads blocks, headers \
                 and account state from a node"
                    .to_string(),
            );
        }

        // The lane is asked for by name and parsed by the execution crate's own
        // parser, so the accepted spellings and the rejection of a typo are one
        // decision made in one place (§20).
        let execution = match &self.execution_mode {
            None => None,
            Some(text) => {
                let mode = ExecutionMode::parse(text).map_err(|error| error.to_string())?;
                if self.rpc_url.is_none() {
                    return Err(format!(
                        "--execution-mode {mode} needs --rpc-url (or GIWA_RPC_URL): the lane \
                         prices a fee, reads a nonce and re-reads the pinned block from an \
                         endpoint, and a replay's recorded blocks are not one"
                    ));
                }
                Some(ExecutionSetup {
                    mode,
                    ..ExecutionSetup::default()
                })
            }
        };

        let mut config = PipelineConfig::live(
            self.rpc_url.as_deref(),
            self.ws_url.as_deref(),
            registry_dirs,
            self.evidence_dir.clone(),
        );
        config.flashblocks_url = self.flashblocks_url.clone();
        config.canonical_source = canonical_source;
        config.start_block = self.start_block;
        config.max_blocks = self.max_blocks;
        config.duration = Duration::from_secs(self.duration);
        config.wrapped_native = wrapped_native;
        config.state_dump = self.state_dump.clone();
        config.execution = execution;
        config.progress = !self.quiet;
        let defaults = QueueConfig::default();
        config.queues = QueueConfig {
            event_capacity: self.event_capacity.unwrap_or(defaults.event_capacity),
            simulation_capacity: self
                .simulation_capacity
                .unwrap_or(defaults.simulation_capacity),
            outcome_capacity: self.outcome_capacity.unwrap_or(defaults.outcome_capacity),
            simulation_workers: self
                .simulation_workers
                .unwrap_or(defaults.simulation_workers),
        };
        config.source = SourceConfig {
            poll_interval_ms: self.poll_interval_ms.unwrap_or(900),
            ..SourceConfig::default()
        };
        config.flashblocks = FlashblockConfig {
            poll_interval_ms: self.flashblock_poll_interval_ms.unwrap_or(250),
            ..FlashblockConfig::default()
        };
        config.risk = RiskConfig {
            minimum_net_profit_wei: self.minimum_net_profit_wei,
            maximum_gas: self.maximum_gas,
        };
        Ok(config)
    }
}

/// §42's exact words for what this transaction is. They go into the record and into
/// every row the run writes, because a validation transaction that is not labelled is
/// indistinguishable from an arbitrage that skipped the risk layer.
pub const VALIDATION_LABEL: &str = "M6 execution validation transaction";

/// The gas a validation transaction carries: a plain value transfer to an account,
/// which is 21 000 gas on this chain and every chain that follows the yellow paper.
const DEFAULT_VALIDATION_GAS: u64 = 21_000;

/// §35's one transaction, asked for by name.
///
/// Nothing about this subcommand is a market decision: it reads no pool, prices no
/// route, and claims no profit (§43). What it verifies is the four things §35 says must
/// be verified — build, sign, submit, receipt — on the real chain, with the cheapest
/// transaction the chain will accept. The mode still decides how far it goes, and the
/// mode has to be named: with no `--execution-mode` there is nothing to run, because a
/// validation transaction is not something a default should do.
#[derive(Parser)]
pub struct ValidateArgs {
    /// JSON-RPC endpoint the lane prices, nonces and sends through. Required.
    #[arg(long, env = "GIWA_RPC_URL")]
    rpc_url: Option<String>,

    /// How far this transaction may go: `build-only`, `sign-only` or `submit` (§20).
    /// The last one is the only spelling that puts bytes on the wire.
    #[arg(long)]
    execution_mode: Option<String>,

    /// The account that pays for the transaction. Absent means the account the
    /// lane's own signer proves — which is the only answer §17 allows a signer to
    /// sign for. Naming it in `build-only` is how a run with no key still gets an
    /// envelope; naming it in a signing mode and disagreeing with the key is refused.
    #[arg(long)]
    sender: Option<String>,

    /// Where the value goes. Absent means the sender, so the default transaction moves
    /// no value at all and spends only gas.
    #[arg(long)]
    to: Option<String>,

    /// Wei to transfer. Zero by default: the point is the pipeline, not the money.
    #[arg(long, default_value_t = 0)]
    value_wei: u128,

    /// Gas limit, taken as given (§13's configured policy — a transaction that was
    /// never simulated has no measurement to be traceable to).
    #[arg(long, default_value_t = DEFAULT_VALIDATION_GAS)]
    gas_limit: u64,

    /// Attested pools, used only for §46's chain-id agreement: this run trades nothing,
    /// but it must still not be on a chain the repository does not attest.
    #[arg(long)]
    registry_dir: Vec<PathBuf>,

    /// Where this attempt's §52/§53 rows go. Created if missing.
    #[arg(long, default_value = "data/evidence/m6/validation")]
    evidence_dir: PathBuf,

    /// Print the attempt record as JSON instead of the summary lines.
    #[arg(long)]
    json: bool,
}

/// What [`ValidateArgs`] describes once the strings have become types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationPlan {
    pub rpc_url: String,
    pub mode: ExecutionMode,
    pub sender: Option<alloy_primitives::Address>,
    pub to: Option<alloy_primitives::Address>,
    pub value_wei: u128,
    pub gas_limit: u64,
    pub registry_dirs: Vec<PathBuf>,
    pub evidence_dir: PathBuf,
}

impl ValidateArgs {
    /// The plan these flags describe, or the reason they describe none. No network, no
    /// key, no file: every refusal here is about the command line, and costs nothing.
    pub fn to_plan(&self) -> std::result::Result<ValidationPlan, String> {
        let rpc_url = self.rpc_url.clone().ok_or_else(|| {
            "--rpc-url (or GIWA_RPC_URL) is required: the lane reads a fee, a nonce and a \
             canonical block from an endpoint and sends through that same endpoint"
                .to_string()
        })?;
        let text = self.execution_mode.as_deref().ok_or_else(|| {
            format!(
                "{VALIDATION_LABEL} needs --execution-mode: a transaction that is not a \
                 market decision has to say which rung it is allowed to reach"
            )
        })?;
        let mode = ExecutionMode::parse(text).map_err(|error| error.to_string())?;
        let sender = self
            .sender
            .as_ref()
            .map(|spec| parse_address(spec, "--sender"))
            .transpose()?;
        let to = self
            .to
            .as_ref()
            .map(|spec| parse_address(spec, "--to"))
            .transpose()?;
        if mode == ExecutionMode::BuildOnly && sender.is_none() {
            return Err(format!(
                "--execution-mode {mode} reads no key, so there is no account this run can \
                 derive — name one with --sender, or run it in a mode that signs"
            ));
        }
        if self.gas_limit == 0 {
            return Err("--gas-limit 0 is a transaction the chain cannot execute".to_string());
        }
        Ok(ValidationPlan {
            rpc_url,
            mode,
            sender,
            to,
            value_wei: self.value_wei,
            gas_limit: self.gas_limit,
            registry_dirs: if self.registry_dir.is_empty() {
                default_registry_dirs()
            } else {
                self.registry_dir.clone()
            },
            evidence_dir: self.evidence_dir.clone(),
        })
    }
}

fn parse_address(spec: &str, flag: &str) -> std::result::Result<alloy_primitives::Address, String> {
    alloy_primitives::FixedBytes::<20>::from_str(spec)
        .map(|bytes| alloy_primitives::Address::from_slice(bytes.as_slice()))
        .map_err(|error| format!("{flag}: {spec} is not an address: {error}"))
}

/// One validation attempt, as the run finished.
pub struct ValidationOutcome {
    pub session_id: String,
    pub evidence_dir: PathBuf,
    pub chain_id: u64,
    /// The canonical head the transaction was pinned at, and that block's hash.
    pub block_number: u64,
    pub block_hash: String,
    pub sender: alloy_primitives::Address,
    pub target: alloy_primitives::Address,
    pub value_wei: u128,
    pub gas_limit: u64,
    pub report: evm_execution::StageReport,
    pub metrics: Value,
}

/// Run one [`ValidationPlan`] and report it.
///
/// The endpoint is read twice on purpose: once here, for the block the transaction is
/// pinned to, and once by the lane, for the fee and the nonce at that pin. Both are
/// named in the evidence, so a reader can tell what was read when (§51).
pub async fn run_validation(plan: ValidationPlan) -> evm_pipeline::Result<ValidationOutcome> {
    use evm_chain::ChainAdapter as _;
    use evm_execution::{ExecutionStage, TransactionIntent};
    use evm_simulation::BlockPin;

    let expected = {
        let ids = evm_pipeline::attested_chain_ids(&plan.registry_dirs)?;
        match ids.as_slice() {
            [one] => *one,
            other => {
                return Err(PipelineError::Config(format!(
                    "the attested registries name {} chains {other:?}; a transaction is on one, \
                     so pass the --registry-dir that names the chain this endpoint is",
                    other.len()
                )))
            }
        }
    };

    let adapter = HttpChainAdapter::connect(&plan.rpc_url).await?;
    let chain_id = adapter.chain_id().0;
    if chain_id != expected {
        return Err(PipelineError::ChainMismatch {
            registry: expected,
            node: chain_id,
            endpoint: plan.rpc_url.clone(),
        });
    }
    let head = adapter.latest_block().await?;
    let context = adapter.get_block_context(head).await?;
    let pin = BlockPin::new(head, context.hash);

    let mut stage = ExecutionStage::connect(
        &plan.rpc_url,
        chain_id,
        ExecutionSetup {
            mode: plan.mode,
            ..ExecutionSetup::default()
        },
        Clock::new(),
    )
    .await
    .map_err(|error| PipelineError::Execution(error.to_string()))?;

    let sender = match plan.sender {
        Some(address) => address,
        None => stage
            .signer_address()
            .map_err(|error| PipelineError::Execution(error.to_string()))?,
    };
    let target = plan.to.unwrap_or(sender);
    let intent = TransactionIntent::validation_call(
        pin,
        chain_id,
        sender,
        target,
        alloy_primitives::U256::from(plan.value_wei),
        plan.gas_limit,
    )
    .map_err(|error| PipelineError::Execution(error.to_string()))?;

    let session_id = format!("validate-{chain_id}-{}", evm_metrics::unix_ms());
    let dir = plan.evidence_dir.join(&session_id);
    let mut evidence = EvidenceWriter::execution_only(&dir, &session_id)?;
    let mut metrics = Metrics::default();
    let report = stage
        .on_validation(intent, VALIDATION_LABEL, &mut metrics)
        .await;

    evidence.line(EvidenceFile::Executions, &report.to_json())?;
    // Two rows, two types: the signed envelope and the endpoint's answer are different
    // records in §52 and §53, and a loop over them would have to erase that.
    if let Some(signed) = &report.signed {
        let row = serde_json::to_value(signed).map_err(|error| PipelineError::Evidence {
            path: dir.join(EvidenceFile::SignedTransactions.file_name()),
            detail: format!("the signed envelope does not serialize: {error}"),
        })?;
        evidence.line(EvidenceFile::SignedTransactions, &row)?;
    }
    if let Some(submission) = &report.submission {
        let row = serde_json::to_value(submission).map_err(|error| PipelineError::Evidence {
            path: dir.join(EvidenceFile::Submissions.file_name()),
            detail: format!("the submission row does not serialize: {error}"),
        })?;
        evidence.line(EvidenceFile::Submissions, &row)?;
    }

    let outcome = ValidationOutcome {
        session_id,
        evidence_dir: dir,
        chain_id,
        block_number: head.0,
        block_hash: format!("{:?}", context.hash),
        sender,
        target,
        value_wei: plan.value_wei,
        gas_limit: plan.gas_limit,
        metrics: metrics.to_json(),
        report,
    };
    let record = outcome.record();
    write_record(
        &outcome.evidence_dir.join("validation-session.json"),
        &record,
    )?;
    Ok(outcome)
}

impl ValidationOutcome {
    /// §37's record of one attempt: what was asked, what was read, and how far the
    /// bytes got. Built rather than derived because the report inside it is a type the
    /// execution crate renders with `to_json()` — the same rendering the evidence row
    /// uses, so the record and the row cannot drift.
    pub fn record(&self) -> Value {
        json!({
            "session_id": self.session_id,
            "milestone": "M6",
            "label": VALIDATION_LABEL,
            "chain_id": self.chain_id,
            "pinned_block": self.block_number,
            "pinned_block_hash": self.block_hash,
            "sender": format!("{:#x}", self.sender),
            "target": format!("{:#x}", self.target),
            "value_wei": self.value_wei.to_string(),
            "gas_limit": self.gas_limit,
            "mode": self.report.mode.name(),
            "attempt": self.report.to_json(),
            "counters": self.metrics,
            // §42's rule, stated in the first file a reader opens: this transaction
            // verified the pipeline, it did not make and does not claim a trade.
            "not_an_arbitrage": true,
        })
    }

    /// The process's exit code for this attempt: `0` when it ended at a rung — which
    /// includes `Built` for a mode that may not sign, and `Reverted` for a transaction
    /// the chain ran and refused — and `1` when it stopped before any rung or failed.
    pub fn exit_code(&self) -> i32 {
        match self.report.reached {
            None => 1,
            Some(status) if status.name() == "failed" => 1,
            Some(_) => 0,
        }
    }
}

/// Write the attempt record atomically enough for a reader: a temporary name, then a
/// rename, the same rule [`EvidenceWriter::finish`] follows for a session's summaries.
fn write_record(path: &Path, record: &Value) -> evm_pipeline::Result<()> {
    let temp = path.with_extension("json.partial");
    let text = serde_json::to_string_pretty(record).map_err(|error| PipelineError::Evidence {
        path: path.to_path_buf(),
        detail: format!("the attempt record does not serialize: {error}"),
    })?;
    std::fs::write(&temp, format!("{text}\n")).map_err(|error| PipelineError::Evidence {
        path: temp.clone(),
        detail: format!("could not be written: {error}"),
    })?;
    std::fs::rename(&temp, path).map_err(|error| PipelineError::Evidence {
        path: path.to_path_buf(),
        detail: format!("the finished record could not be renamed into place: {error}"),
    })
}

/// Print one attempt: what it was, where the bytes got to, and what it cost the reader
/// to trust that.
pub fn print_validation(outcome: &ValidationOutcome) {
    let lane = outcome.report.mode.name();
    println!();
    println!(
        "session={} chain={} block={} hash={}",
        outcome.session_id, outcome.chain_id, outcome.block_number, outcome.block_hash,
    );
    println!(
        "label={:?} mode={lane} sender={:#x} target={:#x}",
        VALIDATION_LABEL, outcome.sender, outcome.target
    );
    println!("{}", outcome.report.line());
    println!("evidence={}", outcome.evidence_dir.display());
    println!(
        "counters={}",
        serde_json::to_string(&outcome.metrics).unwrap_or_default()
    );
}

/// The repository's own committed attestations. These are paths inside this
/// project, not endpoints, which is why they may be defaults where a URL may not.
pub fn default_registry_dirs() -> Vec<PathBuf> {
    vec![
        PathBuf::from("data/protocols"),
        PathBuf::from("data/protocols-m3"),
    ]
}

/// The `live` arguments a command line describes, or the reason it does not.
///
/// Takes the whole `argv`, program name included, the way [`Cli::try_parse_from`]
/// does, so a test writes the command line a person would.
pub fn parse_live(argv: &[&str]) -> std::result::Result<LiveArgs, String> {
    let cli = Cli::try_parse_from(argv).map_err(|error| error.to_string())?;
    match cli.command {
        Command::Live(args) => Ok(*args),
        Command::Validate(_) => {
            Err("this command line is a `validate` run, not a `live` one".to_string())
        }
    }
}

/// The `validate` arguments a command line describes, or the reason it does not.
pub fn parse_validate(argv: &[&str]) -> std::result::Result<ValidateArgs, String> {
    let cli = Cli::try_parse_from(argv).map_err(|error| error.to_string())?;
    match cli.command {
        Command::Validate(args) => Ok(args),
        Command::Live(_) => {
            Err("this command line is a `live` run, not a `validate` one".to_string())
        }
    }
}

/// Run the session one command line describes and report it, returning the
/// process's exit code.
///
/// No `exit` in here, so the codes stay comparable in a test: `0` the session
/// ended on its own terms, `1` a fact made it incomplete, `2` the flags could not
/// describe a run at all.
pub fn cli_main(cli: Cli) -> i32 {
    match cli.command {
        Command::Live(args) => run_live(*args),
        Command::Validate(args) => run_validate(args),
    }
}

/// §35's one attempt, from the flags to the exit code.
fn run_validate(args: ValidateArgs) -> i32 {
    let json = args.json;
    let plan = match args.to_plan() {
        Ok(plan) => plan,
        Err(detail) => {
            eprintln!("evm-mev-bot: {detail}");
            return 2;
        }
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("evm-mev-bot: cannot start a runtime: {error}");
            return 2;
        }
    };
    let evidence_dir = plan.evidence_dir.clone();
    runtime.block_on(async move {
        match run_validation(plan).await {
            Ok(outcome) => {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&outcome.record()).unwrap_or_default()
                    );
                } else {
                    print_validation(&outcome);
                }
                outcome.exit_code()
            }
            Err(error) => {
                // The same rule as a market run: the attempt is reported as the failure
                // it is, and whatever it wrote stays where it was written.
                eprintln!("evm-mev-bot: {error}");
                eprintln!(
                    "evm-mev-bot: the attempt stopped before the lane ran, so nothing was \
                     written under {}.",
                    evidence_dir.display()
                );
                1
            }
        }
    })
}

fn run_live(args: LiveArgs) -> i32 {
    let json = args.json;
    let config = match args.to_config() {
        Ok(config) => config,
        Err(detail) => {
            eprintln!("evm-mev-bot: {detail}");
            return 2;
        }
    };

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("evm-mev-bot: cannot start a runtime: {error}");
            return 2;
        }
    };

    let evidence_dir = config.evidence_dir.clone();
    runtime.block_on(async move {
        match runner::run(&config).await {
            Ok(report) => {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&report.session).unwrap_or_default()
                    );
                } else {
                    print_summary(&report);
                }
                0
            }
            Err(error) => {
                // §8: a run that could not align is reported as that, and the
                // evidence it did produce stays on disk for the reader to weigh.
                eprintln!("evm-mev-bot: {error}");
                eprintln!(
                    "evm-mev-bot: what the run did produce is in {}; the session record there \
                     names what never completed.",
                    evidence_dir.display()
                );
                1
            }
        }
    })
}

/// §75's closing block: the counts, then the endpoint capability table, then the
/// latencies as distributions (§40 says a mean would describe none of it).
pub fn print_summary(report: &evm_pipeline::SessionReport) {
    println!();
    println!(
        "session={} chain={} source={} ended_by={}",
        report.session_id, report.session["chain_id"], report.session["source"], report.ended_by,
    );
    println!(
        "blocks={} events={} state_changes={} opportunities={} simulations={}",
        report.blocks,
        report.events,
        report.session["state_changes"],
        report.session["opportunities"],
        report.simulations,
    );
    println!(
        "risk: accept={} reject={} unknown={}  (a decision is a record, not an order)",
        report.accepts, report.rejects, report.unknowns,
    );
    // M6's boundary, printed as two numbers rather than as one sentence: §2.2 is
    // that an attempt and a submission are different facts, and a summary that
    // collapsed them would hide exactly the run that built everything and sent
    // nothing.
    let lane = report.session["execution"]["configuration"]["lane"]
        .as_str()
        .unwrap_or("absent");
    let signer = report.session["execution"]["signer"]
        .as_str()
        .unwrap_or("none: no lane");
    let caveat = if lane == "absent" {
        format!(
            "no lane was configured, so an Accept stayed a record — {}",
            evm_risk::NO_BROADCAST
        )
    } else {
        "an intent whose sender was funded by a simulation override is recorded and never sent \
         (§34)"
            .to_string()
    };
    println!(
        "execution: lane={lane} attempts={} bytes_to_a_node={} signer={signer}  ({caveat})",
        report.executions, report.sent,
    );
    println!("evidence={}", report.evidence_dir.display());

    print_capability_table(report);

    println!("latency (ms, nearest-rank percentiles):");
    let rows = report.metrics.get("latency").and_then(Value::as_array);
    let mut lines: Vec<String> = Vec::new();
    if let Some(rows) = rows {
        for row in rows {
            if row["measured"].as_bool() != Some(true) {
                continue;
            }
            let stats = &row["stats"];
            lines.push(format!(
                "  {:<34} count={:<4} p50={:<6} p95={:<6} p99={:<6} max={}",
                row["name"].as_str().unwrap_or_default(),
                stats["count"],
                stats["p50_ms"],
                stats["p95_ms"],
                stats["p99_ms"],
                stats["max_ms"],
            ));
        }
    }
    lines.sort();
    if lines.is_empty() {
        println!("  (nothing reached a second stage, so no hop was measured)");
    }
    for line in lines {
        println!("{line}");
    }
}

/// §41/§73: one row per endpoint per capability, and a capability the provider
/// refused is printed as refused rather than left out.
pub fn print_capability_table(report: &evm_pipeline::SessionReport) {
    if report.sources.is_empty() {
        return;
    }
    println!("endpoint capabilities (measured, not assumed):");
    for source in &report.sources {
        println!("  source={}", source.source);
        let capability = source.capability.as_object();
        let Some(entries) = capability else {
            println!("    (no capability record)");
            continue;
        };
        let mut keys: Vec<&String> = entries.keys().collect();
        keys.sort();
        for key in keys {
            let value = &entries[key];
            let shown = match value {
                Value::Object(_) | Value::Array(_) => value.to_string(),
                other => other.to_string(),
            };
            let shown = if shown.len() > 120 {
                format!("{}…", &shown[..117])
            } else {
                shown
            };
            println!("    {key}={shown}");
        }
        if let Some(error) = &source.error {
            println!("    error={error}");
        }
    }
}
