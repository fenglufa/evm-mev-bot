//! Failure and refusal reasons for a simulation.
//!
//! The split matters more than the list. A simulation that ran the EVM and came
//! back with a revert **succeeded as a simulation** — it produced an answer, and
//! that answer is "this opportunity does not execute". That is
//! [`ExecutionOutcome::Reverted`][crate::result::ExecutionOutcome], not an error
//! here. Everything below is a case where the simulation itself could not
//! produce an answer, and each one has to stay distinguishable: a provider that
//! is down, a chain that cannot serve the block, a contract with no code, and a
//! state that is not the state the opportunity was found on are four different
//! risks, and flattening them into one `SimulationFailed` would make every one
//! of them look like the others.

use alloy_primitives::Address;
use thiserror::Error;

use evm_core::{BlockNumber, ChainId};

use crate::state::BlockPin;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SimulationError {
    /// The opportunity was priced on one block and the request pins another.
    /// Refused before any execution — this is the correctness gate, not a
    /// warning.
    #[error(
        "state mismatch: opportunity priced on block {}, request pins block {} ({})",
        .priced.0, .pinned.number.0, .reason
    )]
    StateMismatch {
        priced: BlockNumber,
        pinned: BlockPin,
        reason: String,
    },

    /// The chain the adapter reports is not the chain the opportunity is on.
    #[error(
        "state mismatch: opportunity is on chain {}, provider serves chain {}",
        .expected.0,
        .found.0
    )]
    ChainMismatch { expected: ChainId, found: ChainId },

    /// The provider could not answer, or answered with something that could not
    /// be read. Execution never started, so nothing about profitability was
    /// learned.
    #[error("state provider failed: {0}")]
    ProviderError(String),

    /// A contract the transaction touches has no code at the pinned block.
    /// Simulating against an empty account would silently produce a no-op
    /// instead of the arbitrage, so the run is refused.
    #[error("missing code at {address} in block {}", .block.0)]
    MissingCode {
        address: Address,
        block: BlockNumber,
    },

    /// The pinned block itself, or an account header, is not available from
    /// this state source.
    #[error("missing state: {0}")]
    MissingState(String),

    /// The transaction was rejected before execution by the chain's own
    /// validity rules (nonce, signature-less sender funds for gas, gas limit
    /// above the block limit, …). Distinct from a revert: the EVM never ran.
    #[error("invalid transaction: {0}")]
    InvalidTransaction(String),

    /// Something about this transaction this engine will not attempt.
    #[error("unsupported transaction: {0}")]
    UnsupportedTransaction(String),
}

pub type Result<T> = std::result::Result<T, SimulationError>;
