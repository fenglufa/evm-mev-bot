//! §53: the classes of failure a live run can meet, kept apart because they end
//! a run differently.
//!
//! A provider that will not answer is retried by the source; a gap the provider
//! will not fill stops the run (§8 — continuing would mean claiming to be in
//! sync while not being); a closed queue means whatever is consuming events is
//! gone. Folding these into one "pipeline failed" line would hide exactly the
//! case where stopping is the correct answer.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("chain read failed: {0}")]
    Chain(#[from] evm_chain::ChainError),
    #[error("ingestion failed: {0}")]
    Live(#[from] evm_live::LiveError),
    #[error("state path failed: {0}")]
    Replay(#[from] evm_replay::ReplayError),
    #[error("graph build failed: {0}")]
    Graph(#[from] evm_graph::GraphError),
    #[error("opportunity scan failed: {0}")]
    Opportunity(#[from] evm_opportunity::OpportunityError),
    #[error("simulation worker died before producing an outcome: {job}")]
    WorkerPanic { job: String },
    /// §8: alignment failed. This one is terminal, and the run that hits it
    /// reports a gap rather than a shorter session.
    #[error("unrecovered gap {from}..={to}: {detail}")]
    GapUnrecovered { from: u64, to: u64, detail: String },
    #[error("event queue closed: nothing is consuming market events")]
    EventQueueClosed,
    #[error("outcome queue closed: nothing is consuming simulation results")]
    OutcomeQueueClosed,
    #[error("pool registry at {0} could not be loaded: {1}")]
    Registry(PathBuf, String),
    /// §63's second input: a run may read state from a recording an earlier run
    /// made off the same node. This is the failure of that path — the file is
    /// unreadable, or it does not describe the chain this run is on.
    #[error("recorded state at {path} cannot be used here: {detail}")]
    StateDump { path: PathBuf, detail: String },
    /// A selection that cannot describe a run: a source chosen without the
    /// endpoint it needs. This is caught before a connection is opened, because a
    /// run that starts and then discovers it has nothing to read is a wasted
    /// session, not a configuration error the operator was told about.
    #[error("configuration is not runnable: {0}")]
    Config(String),
    #[error("evidence file {path} could not be written: {detail}")]
    Evidence { path: PathBuf, detail: String },
    /// M6's lane, refused before a session starts. Terminal on purpose: a run that
    /// quietly carried on without the execution lane it was asked for would report
    /// the same numbers as a run that never asked, and the operator would find out
    /// from an evidence file after spending the session's wall clock.
    #[error("execution lane could not be started: {0}")]
    Execution(String),
    /// §46's boundary check, phrased as the failure it is: the registry attests
    /// pools for one chain and the endpoint answered `eth_chainId` for another.
    /// Nothing downstream can notice, so this stops the run before a block is
    /// read.
    #[error(
        "registry attests chain {registry} but the endpoint at {endpoint} answered chain {node}"
    )]
    ChainMismatch {
        registry: u64,
        node: u64,
        endpoint: String,
    },
    /// M12-B §3's readiness gate, and the run ends here. Separate from `Config`
    /// because it is not a bad selection — the node was asked and answered, and
    /// what it said was "not yet", or nothing at all.
    ///
    /// The two are named apart for the reason §53 states for every class in this
    /// file: continuing would mean reading an unsynced node's "this block is not
    /// here yet" as the market's "there is nothing here", which is §8's failure
    /// one milestone later, and §3 forbids reporting it as a session with no
    /// opportunities.
    #[error("the node at {endpoint} is not ready for this run: {detail}")]
    NodeNotReady { endpoint: String, detail: String },
}

pub type Result<T> = std::result::Result<T, PipelineError>;
