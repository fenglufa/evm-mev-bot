//! Market graph: `StateSnapshot -> GraphSnapshot`.
//!
//! Tokens are nodes, pools are edges, and nothing here is allowed to invent a
//! fact: the graph reads the state layer and never the chain, so every edge
//! traces back to a pool, that pool's reserves, and the exact log position that
//! stated them. Finding out which pools are real stays the registry's job, and
//! so does refusing to run arbitrage searches over this (that is M3).

pub mod builder;
pub mod edge;
pub mod snapshot;

pub use builder::{GraphBuild, GraphError, MarketGraphBuilder, SkipReason, SkippedPool};
pub use edge::{EdgeId, EdgeRejection, GraphEdge};
pub use snapshot::GraphSnapshot;
