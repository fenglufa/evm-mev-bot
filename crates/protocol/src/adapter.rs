use evm_chain::ChainLog;
use evm_core::{PoolId, ProtocolId};

use crate::error::Result;
use crate::event::ProtocolEvent;
use crate::registry::PoolAttestation;

/// Turns one raw chain log into a protocol-level statement, or says "not mine".
pub trait ProtocolAdapter: Send + Sync {
    fn protocol_id(&self) -> ProtocolId;

    /// `Ok(None)` means the log is not part of this protocol. A log that is part
    /// of this protocol but cannot be decoded must be `Err`, never `Ok(None)`.
    fn decode_log(&self, log: &ChainLog) -> Result<Option<ProtocolEvent>>;

    /// Pool identity is attested, never inferred from a single event shape.
    fn is_pool(&self, pool: PoolId) -> bool;

    fn attestation(&self, pool: PoolId) -> Option<PoolAttestation>;
}
