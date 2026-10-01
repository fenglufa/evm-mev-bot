//! The live pipeline: source → state → graph → opportunity → simulation → risk
//! → execution lane (only when a run configures one).
//!
//! M5's one question is whether a *real* market-state change can walk the whole
//! loop while every stage keeps the semantics it earned in M1–M4. So this crate
//! composes rather than reimplements (§1): the state path is
//! [`evm_replay::ReplayEngine`], the graph is [`evm_graph`], the findings are
//! [`evm_opportunity`], the runs are [`evm_simulation`], and the three answers are
//! [`evm_risk`]. Nothing in here redefines a reserve, a fee, a quote, or a
//! simulation step.
//!
//! ```text
//! crates/live     which canonical blocks exist, and what the endpoint can do
//! crates/pipeline ─┬─ engine   block → decode → state → graph → finding      (§9–16)
//!                  ├─ sim      bounded queue + Option C worker threads       (§21–23)
//!                  ├─ runner   bootstrap, one stream, graceful shutdown      (§7–8, §49)
//!                  └─ evidence metrics.json, live-session.json, one .jsonl per stage
//!                                                                     (§41, §48, §52–53)
//! ```
//!
//! ## The four rules that decide a design choice here
//!
//! - **One state engine** (§9/§10, §66). Live and replay both drive
//!   [`engine::MarketEngine`] over the same [`evm_chain::ChainAdapter`], so a
//!   parity claim is about inputs, not about two implementations agreeing by
//!   luck. There is no `LiveState` type to diverge from a `ReplayState` type.
//! - **Sealed blocks only** (§12/§27). A `Sync` on a canonical block is the
//!   reserve authority; a flashblock candidate is an observation on its own
//!   evidence stream and never an input to state.
//! - **A finding carries its own state version** (§16–20). The header a
//!   simulation is built against is read at the height the finding was priced at;
//!   `latest` is not reachable from this path.
//! - **Accept is not send** (§25/§26, and M6's §20/§34). Without a configured
//!   execution lane the run ends at a `RiskDecision`, exactly as it did in M5.
//!   With one, this crate hands the decision and the simulation result to
//!   [`evm_execution::ExecutionStage`] and records what comes back — it holds no
//!   signer itself, names no submission method, reads no key, and knows no
//!   sequencer or relay endpoint, and `crates/cli/tests/no_execution.rs` fails the
//!   build if any of those appear outside the one crate that is allowed them.
//!
//! ## Configuration
//!
//! Endpoints, capacities and thresholds are data in [`config::PipelineConfig`],
//! supplied by the CLI or the environment (§44). No URL and no chain id appears in
//! this crate's code: the chain's identity is read from the endpoint that answers
//! (`eth_chainId`) and checked against the registry before a block is fetched
//! (§45/§46).

pub mod config;
pub mod engine;
pub mod error;
pub mod evidence;
pub mod runner;
pub mod sim;

pub use config::{CanonicalSource, PipelineConfig, QueueConfig, RiskConfig};
pub use engine::{BlockOutcome, ChainPool, MarketEngine};
pub use error::{PipelineError, Result};
pub use evidence::{EvidenceFile, EvidenceWriter};
pub use runner::{attested_chain_ids, run, SessionReport, SourceCompletion};
pub use sim::{
    decline_line, plan_job, priced_route, Decline, JobPlan, SimOutcome, SimRun, SimulationJob,
    SimulationPool, StateSource, WorkerReport,
};
