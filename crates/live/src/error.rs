//! The classes §53 asks for, kept separate because they end a run differently.
//!
//! A provider outage is retried; a gap the provider will not fill is terminal;
//! a closed queue means the pipeline itself is gone. Reporting all three as
//! "ingestion failed" would hide the one case (§8) where continuing would mean
//! pretending to be in sync.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LiveError {
    #[error("head read failed: {0}")]
    Head(String),
    #[error("block {number} read failed: {detail}")]
    BlockRead { number: u64, detail: String },
    #[error("header decode failed: {0}")]
    Decode(String),
    /// The `pending` read behind a candidate. Its failure is not an ingestion
    /// failure: candidates never advance state, so this class is reported and the
    /// canonical path continues (§27's fallback).
    #[error("pending candidate read failed: {0}")]
    Pending(String),
    /// §8: the alignment failure is stated, and the run stops. It is not
    /// converted into a shorter run that looks complete.
    #[error("unrecovered gap {from}..={to} after {attempts} attempts: {detail}")]
    GapUnrecovered {
        from: u64,
        to: u64,
        attempts: u32,
        detail: String,
    },
    #[error("event queue closed by the pipeline: nothing left to deliver to")]
    QueueClosed,
    #[error("source stopped by request after {events} events")]
    Stopped { events: u64 },
    #[error("connection failed: {0}")]
    Connection(String),
}

pub type LiveResult<T> = Result<T, LiveError>;
