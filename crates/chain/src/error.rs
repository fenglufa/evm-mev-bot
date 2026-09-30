#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    #[error("rpc request failed: {0}")]
    Rpc(String),
    #[error("rpc returned an error payload: {0}")]
    RpcRejected(String),
    #[error("provider response could not be normalized: {0}")]
    Decode(String),
    #[error("recorded data is missing: {0}")]
    MissingData(String),
    #[error("local io failure: {0}")]
    Io(String),
    #[error("data is inconsistent: {0}")]
    Inconsistent(String),
}

pub type Result<T> = std::result::Result<T, ChainError>;
