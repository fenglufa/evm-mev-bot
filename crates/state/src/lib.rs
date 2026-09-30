//! State engine: `ProtocolEvent -> StateUpdate -> StateStore`.
//!
//! The store is the only place market state lives, and it only accepts updates
//! it can justify: registered pools, non-empty reserves, strictly increasing
//! chain positions.

pub mod error;
pub mod snapshot;
pub mod store;
pub mod update;

pub use error::{Result, StateError};
pub use snapshot::{PoolRecord, StateSnapshot};
pub use store::{InMemoryStateStore, StateStore};
pub use update::{StateUpdate, UpdatePosition};
