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
//! Exit codes: `0` the session ended on its own terms, `1` it was stopped by a
//! fact that makes the session incomplete (§8's unrecovered gap, a closed queue,
//! a chain mismatch), `2` the flags themselves could not describe a run.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use serde_json::Value;

use evm_live::{FlashblockConfig, SourceConfig};
use evm_pipeline::config::{CanonicalSource, PipelineConfig, QueueConfig, RiskConfig};
use evm_pipeline::runner;

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
    about = "GIWA live market pipeline: block → state → graph → opportunity → simulation → risk.",
    long_about = "Reads a chain and decides whether an opportunity would have been worth taking. \
                  It never signs, broadcasts, submits, relays, bundles, or bids (§26): an Accept is \
                  a line in an evidence file."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the pipeline against live market data for a while.
    Live(LiveArgs),
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
        Command::Live(args) => Ok(args),
    }
}

/// Run the session one command line describes and report it, returning the
/// process's exit code.
///
/// No `exit` in here, so the codes stay comparable in a test: `0` the session
/// ended on its own terms, `1` a fact made it incomplete, `2` the flags could not
/// describe a run at all.
pub fn cli_main(cli: Cli) -> i32 {
    let Command::Live(args) = cli.command;
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
        "risk: accept={} reject={} unknown={}  (an Accept is a record; nothing was broadcast — {})",
        report.accepts,
        report.rejects,
        report.unknowns,
        evm_risk::NO_BROADCAST,
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
