//! Replay: read a block range, feed the pipeline, end up with pool state.
//!
//! The engine is generic over the chain source, so a recorded block and a live
//! block travel through identical code. Determinism is a requirement, not a
//! hope: the same input replayed twice must produce the same snapshot.

pub mod audit;
pub mod engine;
pub mod error;
pub mod pipeline;

pub use audit::{ChangeSource, StateChange, Written};
pub use engine::{ReplayEngine, ReplayReport};
pub use error::{ReplayError, Result};
pub use pipeline::EventPipeline;
