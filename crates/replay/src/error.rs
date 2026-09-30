use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReplayError {
    #[error(transparent)]
    Chain(#[from] evm_chain::ChainError),
    #[error(transparent)]
    Protocol(#[from] evm_protocol::ProtocolError),
    #[error(transparent)]
    State(#[from] evm_state::StateError),
    #[error("range {from}..={to} runs backwards")]
    InvertedRange { from: u64, to: u64 },
}

pub type Result<T> = std::result::Result<T, ReplayError>;
