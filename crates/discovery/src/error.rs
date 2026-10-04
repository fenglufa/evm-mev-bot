use evm_chain::ChainError;
use evm_protocol::ProtocolError;

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// The node could not answer, or answered with something that is not a chain
    /// record. A scan that cannot ask stops rather than reporting a shorter
    /// history than it saw: "0 candidates" has to mean "nobody asked me to look
    /// at anything", never "the request failed".
    #[error(transparent)]
    Chain(#[from] ChainError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    /// A scan configured itself impossibly (an empty range, a chunk of zero
    /// blocks). Reported rather than quietly clamped.
    #[error("invalid scan configuration: {0}")]
    Configuration(String),
    /// The store, the graph, or the registry refused a verified pool. Every
    /// field says which layer spoke, so a rejection is attributable to the rule
    /// that produced it instead of to "integration".
    #[error("verified pool {pool} was refused by {layer}: {message}")]
    Integration {
        layer: &'static str,
        pool: String,
        message: String,
    },
}

pub type Result<T> = std::result::Result<T, DiscoveryError>;
