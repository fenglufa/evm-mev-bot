//! Simulation and profit: `Opportunity -> what the EVM would actually do`.
//!
//! M3 proved that two pools disagree by more than their fees on one block's
//! reserves. That is a statement about a formula. This crate asks the question
//! the formula cannot answer: put the route in front of the deployed bytecode,
//! with the real tokens and the real state of the block it was found on, and
//! what happens? [`crate::engine`] runs REVM. Nothing here re-derives a
//! simulation formula in Rust, because a formula that agrees with itself proves
//! nothing (§2 of the task).
//!
//! The vocabulary is the point of the milestone, and these are four different
//! words:
//!
//! ```text
//! gross profit       M3's number, from reserves and fees alone
//! simulated output   what the EVM handed back after real execution
//! gas cost           execution's own price, in wei
//! net profit         only when the denomination of the profit is provable
//! ```
//!
//! A simulation that reverts **succeeded** — it produced an answer, and the
//! answer is "this does not execute". That is
//! [`ExecutionStatus::Reverted`][crate::result::ExecutionStatus], not an
//! error. [`SimulationError`][crate::error::SimulationError] is reserved for
//! the cases where no answer was produced at all.
//!
//! Two boundaries are absolute here, and they are the whole of v0.1's safety
//! story:
//!
//! - **Nothing is ever broadcast.** There is no signing key, no
//!   `eth_sendRawTransaction`, no relay and no bundle submission anywhere in
//!   this crate; the only sender is a deterministic test account that exists to
//!   spend gas in a sandbox (§10, §11, §58).
//! - **State is never guessed.** Every read is pinned to one block header, the
//!   pin is verified before execution, and a source that cannot answer fails
//!   with [`MissingState`][crate::error::SimulationError::MissingState] rather
//!   than a default (§15, §42, §60).
//!
//! What this crate does **not** do, deliberately: no live pipeline, no
//! continuous subscription, no execution of a real transaction, no realized
//! profit. See §11, §44, §47 and §49 of the task, and the Limitations section of
//! the completion report, which repeats all of it in the same words.

pub mod engine;
pub mod error;
pub mod gas;
pub mod plan;
pub mod request;
pub mod result;
pub mod route;
pub mod state;

pub use engine::{dump_provider, rpc_provider, DumpSimulator, ProviderDb, RpcSimulator, Simulator};
pub use error::{Result, SimulationError};
pub use gas::{GasBudget, GasCharge, GasPricing};
pub use plan::{
    AmountSource, Binding, ExecutionPlan, Funding, Measurement, Measurements, PairSides, PlanStep,
    ResolvedStep, Settle,
};
pub use request::{
    Endowment, EvmRules, SimulationRequest, SimulationSender, StateSpec, TransactionSpec,
    DEFAULT_SENDER_LABEL,
};
pub use result::{
    AccountChange, Denomination, ExecutedLog, ExecutedStep, ExecutionStatus, GrossMovement,
    MeasuredValue, Movement, NetProfit, OutputComparison, PlanSummary, RevertData,
    SimulatedOutcome, SimulationResult, SlotChange, StateChanges, StepStatus,
    ERROR_STRING_SELECTOR,
};
pub use route::{PricedRoute, RouteLeg, SlippagePolicy, SlippageRecord};
pub use state::{
    AccountState, BlockPin, CodeKey, DumpStateProvider, ProviderError, ProviderResult,
    RpcStateProvider, StateDump, StateOverride, StateProvider, StorageCacheKey,
};
