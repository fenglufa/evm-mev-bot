#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("log does not match protocol shape: {0}")]
    MalformedLog(String),
    #[error("pool identity is not attested: {0}")]
    UnattestedPool(String),
    #[error("registry error: {0}")]
    Registry(String),
}

pub type Result<T> = std::result::Result<T, ProtocolError>;
