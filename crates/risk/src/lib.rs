//! The risk layer M4 is allowed to have: three checks, three answers, and no
//! way to send anything.
//!
//! M4's question was "does M3's theoretical profit survive real EVM execution
//! semantics?" — and a simulation can answer it and still produce a run nobody
//! should want: reverted, out of gas, larger than a gas ceiling, or profitable by
//! less than the fee it costs to be on chain. This crate turns a
//! [`evm_simulation::SimulationResult`] into one of §46's three answers and stops.
//!
//! ```text
//! SimulationResult
//!   -> RiskThresholds::evaluate     §45: did it run / was it cheap enough / is the
//!   -> RiskDecision                  net figure big enough
//!   -> ExecutionRequest              (plain data: who, what block, how much)
//!   -> NullExecutor | DryRunExecutor §48: the only two implementations M4 has
//!   -> ExecutionResult               §47: no transaction, in either case
//! ```
//!
//! ## What is deliberately absent
//!
//! §45 lists three checks and §46 says "不要实现复杂 token risk": there is no holder
//! analysis, no honeypot scoring, no liquidity ageing, no per-token trust here. A
//! rule that cannot be stated as a number and a comparison is not in this crate.
//!
//! §47 and §48 together decide the shape of [`executor`]. Both executors are total
//! functions over plain data: neither can reach a network, a key, or a transaction
//! type, because neither names one. An `Accept` here means *the simulation satisfies
//! these thresholds* — a sentence about a model, not permission to broadcast.
//!
//! ## Dependency direction
//!
//! This crate reads execution's record, so it depends on `evm-simulation` and on
//! nothing that quotes prices: §45's sketch passed an `&Opportunity` alongside the
//! simulation, and this does not, because [`evm_simulation::PricedRoute`] already
//! carries M3's numbers verbatim (`analytical_output`, `analytical_gross_profit`,
//! `priced_by`). Deciding with fewer inputs means fewer ways for the two sides of
//! the comparison to disagree about which run is being decided on.

pub mod decision;
pub mod executor;
pub mod policy;

pub use decision::{RiskDecision, RiskRule, NO_BROADCAST};
pub use executor::{DryRunExecutor, ExecutionRequest, ExecutionResult, Executor, NullExecutor};
pub use policy::{RiskPolicy, RiskThresholds};
