//! Protocol layer: turns raw chain logs into protocol statements.
//!
//! Nothing here touches state. A decoder says what a log means; the state layer
//! decides whether and how to apply it.

pub mod adapter;
pub mod error;
pub mod event;
pub mod registry;
pub mod signatures;
pub mod v2;

pub use adapter::ProtocolAdapter;
pub use error::{ProtocolError, Result};
pub use event::{LogPosition, PoolCreatedEvent, ProtocolEvent, SwapEvent, SyncEvent};
pub use registry::{AttestationEvidence, PoolAttestation, Registry, RegistryError};
pub use signatures::V2Topics;
pub use v2::V2Adapter;
