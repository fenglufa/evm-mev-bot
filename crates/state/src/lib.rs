//! State engine: `ProtocolEvent -> StateUpdate -> StateStore`.
//!
//! The store is the only place market state lives, and it only accepts updates
//! it can justify: registered pools, non-empty reserves, strictly increasing
//! chain positions.
//!
//! A snapshot keeps the position each `Sync` was published at. Reading that state
//! at a *later* block is a separate, explicit step — `StateSnapshot::at_target`,
//! which attaches scan coverage as its own evidence and never rewrites an
//! observation.

pub mod error;
pub mod snapshot;
pub mod store;
pub mod update;

pub use error::{Result, StateError};
pub use snapshot::{PoolRecord, StateSnapshot, SyncScanCoverage};
pub use store::{InMemoryStateStore, StateStore};
pub use update::{StateUpdate, UpdatePosition};
