//! §39's error taxonomy, one variant per reason the task book names.
//!
//! The point of the split is legibility after the fact: "we did not send it" has to
//! say whether the chain id disagreed, the state moved, the wallet could not pay, the
//! key was absent, or the node never answered — because only the last of those leaves
//! a transaction possibly in flight. A single `ExecutionError` string would make §25's
//! "timeout is not failure" unenforceable.

use thiserror::Error;

/// Every fallible step of `crates/execution`.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ExecutionError {
    /// §6: the endpoint answers for a chain other than the one execution was
    /// configured for. Nothing downstream runs.
    #[error("chain mismatch: {0}")]
    ChainMismatch(String),

    /// §10: the intent is not buildable — a zero address, a fee field that does not
    /// match its envelope, a gas limit of zero, bytes that will not decode.
    #[error("invalid intent: {0}")]
    InvalidIntent(String),

    /// §31: the state the intent was built against is no longer the state we are
    /// allowed to act on.
    #[error("stale opportunity: {0}")]
    StaleOpportunity(String),

    /// §40's M10 class: the plan itself was refused before anything was built. Distinct from
    /// [`ExecutionError::InvalidIntent`] because the thing rejected is not an encoding
    /// question about an intent — it is a decided route that disagrees with the runtime it was
    /// handed (chain, executor address, route continuity, floors), or with the contract's own
    /// guards. Nothing was built, so nothing is in flight.
    #[error("plan rejected: {0}")]
    PlanRejected(String),

    /// §33: the real on-chain account cannot cover `gas_limit * max_fee + value` (or a
    /// token balance the step spends). Measured, never assumed.
    #[error("insufficient balance: {0}")]
    InsufficientBalance(String),

    /// §11: no nonce can be allocated — the pending and confirmed views disagree in a
    /// way we cannot resolve, or the single lane already has a transaction outstanding.
    #[error("nonce unavailable: {0}")]
    NonceUnavailable(String),

    /// §34: the run this intent came from was only possible because of a state
    /// override, so no real account could send it. A refusal with a reason, not a bug.
    #[error("override-dependent execution: {0}")]
    OverrideDependent(String),

    /// §9/§10: everything checked out and the encoding itself failed.
    #[error("build failed: {0}")]
    BuildFailed(String),

    /// §16: the key was absent, malformed, or the signing primitive refused. Never a
    /// rendering of the key.
    #[error("signing failed: {0}")]
    SigningFailed(String),

    /// §25: the node answered and said no. The transaction is not in flight.
    #[error("submission rejected: {0}")]
    SubmissionRejected(String),

    /// §25: we asked and do not know — a timeout, a connection reset, a non-JSON
    /// answer. Distinct from `SubmissionRejected` precisely because a retry here could
    /// duplicate a transaction that was already accepted.
    #[error("submission unknown: {0}")]
    SubmissionUnknown(String),

    /// §26: the transaction was accepted but no receipt appeared within the budget.
    /// Not a failure of the transaction; a failure to learn about it.
    #[error("receipt timeout: {0}")]
    ReceiptTimeout(String),

    /// §26/§27: the receipt is there and `status` is 0. Recorded as `Reverted`, never
    /// as success (§P).
    #[error("transaction reverted in block {block_number} with hash {transaction_hash}")]
    TransactionReverted {
        transaction_hash: String,
        block_number: u64,
    },

    /// §27: the receipt we got is not the receipt of the transaction we sent, or it
    /// sits in a block that does not match. A provider mix-up is an error, not a
    /// detail.
    #[error("receipt binding broken: {0}")]
    ReceiptBinding(String),

    /// §18/§47: the transaction ran, and what it produced is not what the simulation said
    /// it would produce, by more than the tolerance the run declared. Distinct from
    /// `TransactionReverted` (the chain refused the call) and from `ReceiptBinding` (we
    /// cannot tie the receipt to our transaction): this is the case where every read is
    /// fine and the *world* disagreed with the model.
    #[error("execution mismatch: {0}")]
    ExecutionMismatch(String),

    /// §19/§20: the capability was asked for in a mode that does not have it — signing
    /// in `BuildOnly`, broadcasting in `SignOnly`. The gate, reported.
    #[error("mode gate: {0}")]
    ModeGate(String),

    /// §30: this exact (opportunity, simulation, state) triple has already been
    /// executed. Returning the existing record rather than sending a second one.
    #[error("already executed: {0}")]
    AlreadyExecuted(String),

    /// Reading the pinned state or a chain field produced something inconsistent.
    #[error("chain read: {0}")]
    ChainRead(String),

    /// An evidence file could not be written, or a session call failed.
    #[error("evidence: {0}")]
    Evidence(String),
}

pub type Result<T> = std::result::Result<T, ExecutionError>;
