//! The engine: one [`ExecutionPlan`][crate::plan::ExecutionPlan] in front of REVM,
//! at one pinned header, with nothing in between.
//!
//! This is the module the milestone is actually about. Everything else in the
//! crate is preparation for the six steps below:
//!
//! ```text
//! 1  check the pins          chain, height, header hash, overrides   (§15, §20, §57)
//! 2  ask the contracts       token0(), token1(), getReserves()        (§5, §42, §60)
//! 3  build the plan          the sequence, from the answers above     (§12, §21)
//! 4  execute it              one transaction per step, in order       (§3)
//! 5  diff the state          what the run changed                     (§36)
//! 6  price it                gas measured, costed only if it can be   (§28, §29, §31)
//! ```
//!
//! Step 2 is the one that decides whether the rest means anything. The pool's
//! side ordering and its reserves are not read out of a registry, a config file or
//! a storage layout this crate would have to guess: they are the return values of
//! calls executed by REVM against the pinned state, so a fixture and a live node
//! cannot disagree about what the contract says without the run reporting it. If a
//! reserve the route priced on is not the reserve the pool reports, that is
//! [`SimulationError::StateMismatch`][crate::error::SimulationError::StateMismatch]
//! and the run does not start (§42).
//!
//! This file is also the only place in the crate that names a REVM type (§7). The
//! one decision that belongs to it and to nothing else is turning the request's
//! declared [`EvmRules`][crate::request::EvmRules] into a `SpecId`; everything above
//! this module sees [`SimulationResult`][crate::result::SimulationResult] and errors
//! from [`crate::error`].
//!
//! Nothing here broadcasts. The only sender is the deterministic test account
//! (§58), REVM's state is dropped at the end of every call, and the crate has no
//! path to a node that writes — see the module documentation of
//! [`crate`][lib] for the two absolute boundaries.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use alloy_primitives::{Address, Bytes, B256, U256};
use async_trait::async_trait;
use revm::bytecode::Bytecode;
use revm::context::{BlockEnv, CfgEnv, TxEnv};
use revm::context_interface::block::BlobExcessGasAndPrice;
use revm::context_interface::result::{
    EVMError, ExecutionResult, HaltReason, InvalidTransaction, Output,
};
use revm::context_interface::{ContextTr, JournalTr};
use revm::handler::{MainBuilder, MainContext, MainnetContext};
use revm::primitives::hardfork::SpecId;
use revm::primitives::{Log, TxKind};
use revm::state::{AccountInfo, EvmState};
use revm::{AsyncDb, AsyncError, DatabaseAsync, ExecuteEvmAsync};

use evm_chain::{BlockContext, ChainAdapter};
use evm_core::BlockNumber;
use evm_protocol::{CallReturn, Reserves, V2Call};

use crate::error::SimulationError;
use crate::gas::{GasBudget, GasCharge, GasPricing};
use crate::plan::{ExecutionPlan, Funding, PairSides, PlanStep, ResolvedStep, Settle};
use crate::request::{EvmRules, SimulationRequest};
use crate::result::{
    AccountChange, Denomination, ExecutedLog, ExecutedStep, ExecutionStatus, GrossMovement,
    MeasuredValue, Movement, NetProfit, OutputComparison, PlanSummary, RevertData,
    SimulatedOutcome, StateChanges, StepStatus,
};
use crate::state::{
    code_hash_of, BlockPin, DumpStateProvider, ProviderError, RpcStateProvider, StateDump,
};
use crate::{Binding, Measurements, Result, SimulationResult, StateProvider};

// ---------------------------------------------------------------------------
// Which rules the EVM executes under
// ---------------------------------------------------------------------------

impl EvmRules {
    /// The one place a declared ruleset becomes REVM's own name for it. Keeping it
    /// here is what lets [`EvmRules`][crate::request::EvmRules] stay a fact about a
    /// request rather than a handle into the executor (§7).
    pub(crate) const fn spec_id(self) -> SpecId {
        match self {
            Self::Shanghai => SpecId::SHANGHAI,
            Self::Cancun => SpecId::CANCUN,
            Self::Prague => SpecId::PRAGUE,
            Self::Osaka => SpecId::OSAKA,
        }
    }
}

// ---------------------------------------------------------------------------
// The state bridge
// ---------------------------------------------------------------------------

/// A database failure, kept as two kinds because §34 asks the run to tell a pruned
/// block apart from a node that did not answer — and that distinction must survive
/// crossing into REVM and coming back out. [`ProviderError`] cannot be used
/// directly because REVM requires its own marker trait, and implementing a foreign
/// trait for a foreign type is not allowed here.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DbError {
    /// The source works and says this piece of state is not there.
    #[error("missing state: {0}")]
    Missing(String),
    /// The source itself failed; nothing was learned about the state.
    #[error("state provider failed: {0}")]
    Unavailable(String),
}

impl revm::database_interface::DBErrorMarker for DbError {}

fn to_db_error(error: ProviderError) -> DbError {
    match error {
        ProviderError::Missing { provider, what } => {
            DbError::Missing(format!("{provider}: {what}"))
        }
        ProviderError::Unavailable { provider, reason } => {
            DbError::Unavailable(format!("{provider}: {reason}"))
        }
    }
}

/// The same [`StateProvider`] seen as REVM's async database.
///
/// Thin on purpose. The caching §63 and §64 ask for — bytecode by chain and
/// address, storage by chain, block, address and slot — lives in the providers,
/// because a fixture has to be able to answer from what it recorded without an
/// engine in front of it. What this adds is only the shape REVM expects: an account
/// header that carries its own bytecode, so no code is ever fetched by hash alone,
/// which would be a guess about which contract the bytes belong to.
pub struct ProviderDb {
    provider: Arc<dyn StateProvider>,
}

impl ProviderDb {
    pub fn new(provider: Arc<dyn StateProvider>) -> Self {
        Self { provider }
    }
}

impl DatabaseAsync for ProviderDb {
    type Error = DbError;

    fn basic_async(
        &mut self,
        address: Address,
    ) -> impl Future<Output = std::result::Result<Option<AccountInfo>, Self::Error>> + Send {
        let provider = Arc::clone(&self.provider);
        async move {
            // `Ok(None)` is REVM's own way of saying "this account does not exist",
            // which is a fact the EVM handles (a transfer to it creates it, a call to
            // it runs no code). It is not an error, and turning it into one would make
            // §58's never-seen test sender impossible to fund.
            let Some(state) = provider.account(address).await.map_err(to_db_error)? else {
                return Ok(None);
            };
            let code = provider.code(address).await.map_err(to_db_error)?;
            let hashed = code_hash_of(&code);
            if hashed != state.code_hash {
                return Err(DbError::Unavailable(format!(
                    "state source disagrees with itself at {address}: the account header reports \
                     code hash {} and the code it served hashes to {hashed}",
                    state.code_hash,
                )));
            }
            Ok(Some(AccountInfo {
                balance: state.balance,
                nonce: state.nonce,
                code_hash: state.code_hash,
                code: Some(Bytecode::new_raw(code)),
                ..Default::default()
            }))
        }
    }

    async fn code_by_hash_async(
        &mut self,
        code_hash: B256,
    ) -> std::result::Result<Bytecode, Self::Error> {
        Err(DbError::Unavailable(format!(
            "no bytecode is fetched by hash alone ({code_hash}): a hash is not an identity, \
             and the account that owns it is the only thing that makes these bytes the \
             contract this run is trading against"
        )))
    }

    fn storage_async(
        &mut self,
        address: Address,
        index: U256,
    ) -> impl Future<Output = std::result::Result<U256, Self::Error>> + Send {
        let provider = Arc::clone(&self.provider);
        async move { provider.storage(address, index).await.map_err(to_db_error) }
    }

    fn block_hash_async(
        &mut self,
        number: u64,
    ) -> impl Future<Output = std::result::Result<B256, Self::Error>> + Send {
        let provider = Arc::clone(&self.provider);
        async move {
            Ok(provider
                .block_hash(BlockNumber(number))
                .await
                .map_err(to_db_error)?
                .unwrap_or_default())
        }
    }
}

/// The concrete EVM this crate runs: mainnet handler stack, async database.
type SimEvm = revm::MainnetEvm<MainnetContext<AsyncDb<ProviderDb>>>;

/// What one `transact_one_async` call can fail with: the EVM's own error, wrapped
/// in whatever can go wrong with the fiber that ran it.
type FiberError = AsyncError<EVMError<AsyncError<DbError>, InvalidTransaction>>;

// ---------------------------------------------------------------------------
// Errors: from REVM's vocabulary back to this crate's (§34)
// ---------------------------------------------------------------------------

fn unsupported(reason: String) -> SimulationError {
    SimulationError::UnsupportedTransaction(reason)
}

/// Turn a provider's failure into the error §34 asks for.
///
/// `Missing` and `Unavailable` stay distinguishable all the way up: a pruned block
/// and a node that did not answer are different facts about the world, and a report
/// that merged them would tell the next reader nothing about which one to go fix.
fn state_error(error: ProviderError) -> SimulationError {
    match error {
        ProviderError::Missing { provider, what } => {
            SimulationError::MissingState(format!("{provider}: {what}"))
        }
        unavailable => SimulationError::ProviderError(unavailable.to_string()),
    }
}

/// A failure the EVM reported through its database, which is this crate's state
/// source seen from the other side. The two kinds are unpacked back apart.
fn db_failure(error: AsyncError<DbError>) -> SimulationError {
    match error {
        AsyncError::Inner(DbError::Missing(what)) => SimulationError::MissingState(what),
        AsyncError::Inner(unavailable) => SimulationError::ProviderError(unavailable.to_string()),
        fiber => SimulationError::ProviderError(format!(
            "a state read could not be completed on the EVM's execution fiber: {fiber}"
        )),
    }
}

fn evm_failure(error: EVMError<AsyncError<DbError>, InvalidTransaction>) -> SimulationError {
    match error {
        EVMError::Database(db) => db_failure(db),
        // The chain's own validity rules refused the transaction before it ran: a
        // nonce, a balance, a gas limit. Nothing about the market was learned, and
        // §34 keeps this its own kind rather than a revert.
        EVMError::Transaction(invalid) => SimulationError::InvalidTransaction(invalid.to_string()),
        EVMError::Header(header) => SimulationError::MissingState(format!(
            "the block environment this run was given is not one the EVM can execute at: {header}"
        )),
        other => SimulationError::ProviderError(format!(
            "the EVM refused to answer for reasons outside the state source: {other}"
        )),
    }
}

fn fiber_failure(error: FiberError) -> SimulationError {
    match error {
        AsyncError::Inner(evm) => evm_failure(evm),
        fiber => SimulationError::ProviderError(format!(
            "the EVM's execution fiber stopped before it finished: {fiber}"
        )),
    }
}

fn protocol_error(error: evm_protocol::ProtocolError) -> SimulationError {
    SimulationError::ProviderError(format!(
        "a call this crate encoded came back in a shape it does not define: {error}"
    ))
}

// ---------------------------------------------------------------------------
// The public surface
// ---------------------------------------------------------------------------

/// Run a request against a state source.
///
/// §7's reason for this trait to exist: a caller above the engine gets a
/// [`SimulationResult`] and never sees a REVM type. The two implementations are the
/// two state sources §62 allows — and only two, so a fixture cannot be mistaken for
/// a node and the other way round.
#[async_trait(?Send)]
pub trait Simulator: Send + Sync {
    /// One simulation, one answer. A revert is an answer; see [`crate::error`] for
    /// what is not.
    async fn simulate(&self, request: &SimulationRequest) -> Result<SimulationResult>;

    /// What this simulator reads state from, in the words §66 asks a report to
    /// carry.
    fn source(&self) -> String;
}

/// The real provider: a node, read at one pinned block and never at `latest`.
pub struct RpcSimulator {
    adapter: Arc<dyn ChainAdapter>,
}

impl RpcSimulator {
    pub fn new(adapter: Arc<dyn ChainAdapter>) -> Self {
        Self { adapter }
    }
}

#[async_trait(?Send)]
impl Simulator for RpcSimulator {
    async fn simulate(&self, request: &SimulationRequest) -> Result<SimulationResult> {
        let pin = request.pin();
        let provider = RpcStateProvider::new(Arc::clone(&self.adapter), pin);
        run(Arc::new(provider), request).await
    }

    fn source(&self) -> String {
        format!("rpc:chain-{}", self.adapter.chain_id().0)
    }
}

/// The fixture provider: a recorded dump, replayed byte for byte.
pub struct DumpSimulator {
    dump: StateDump,
    source: String,
}

impl DumpSimulator {
    pub fn new(dump: StateDump, source: impl Into<String>) -> Self {
        Self {
            dump,
            source: source.into(),
        }
    }

    /// The dump this fixture replays, for a test that has to extend it.
    pub fn dump(&self) -> &StateDump {
        &self.dump
    }
}

#[async_trait(?Send)]
impl Simulator for DumpSimulator {
    async fn simulate(&self, request: &SimulationRequest) -> Result<SimulationResult> {
        run(
            dump_provider(self.dump.clone(), self.source.clone()),
            request,
        )
        .await
    }

    fn source(&self) -> String {
        self.source.clone()
    }
}

/// A fixture's state source. Kept next to [`rpc_provider`] so the two paths §62
/// allows are readable from one place, and so a fixture author can see that a dump
/// and a node go through the same run.
pub fn dump_provider(dump: StateDump, source: impl Into<String>) -> Arc<dyn StateProvider> {
    Arc::new(DumpStateProvider::new(dump, source))
}

/// …and the same for a node.
pub fn rpc_provider(adapter: Arc<dyn ChainAdapter>, pin: BlockPin) -> Arc<dyn StateProvider> {
    Arc::new(RpcStateProvider::new(adapter, pin))
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// One step's execution, as the loop needs it.
struct Stepped {
    gas_used: u64,
    status: StepStatus,
    logs: Vec<ExecutedLog>,
    /// What a measurement step's call answered, decoded, for the binding it fills.
    answered: Option<U256>,
}

/// Execute `request` against `provider`.
///
/// The provider arrives bare: the §58 scaffolding is layered on here, once, after
/// the request has been checked, so no caller can hand an overridden source to a
/// run that skipped [`SimulationRequest::preflight`].
pub async fn run(
    provider: Arc<dyn StateProvider>,
    request: &SimulationRequest,
) -> Result<SimulationResult> {
    // ---- 1. pins and permissions --------------------------------------------
    let setup = request.preflight()?;
    let provider = provider.with_setup(setup);

    let header = provider.header().await.map_err(state_error)?;
    if header.chain_id != request.chain_id {
        return Err(SimulationError::ChainMismatch {
            expected: request.chain_id,
            found: header.chain_id,
        });
    }
    let loaded = BlockPin::new(header.number, header.hash);
    request.check_pin(loaded)?;
    let state_source = provider.source();
    // §62's two sources are only two if a run cannot mix them: a request that says it
    // was built against a recorded dump is not asking to read a node, and a result
    // from the wrong source would be reported with a provenance this run does not
    // have. The pin above proves *which block*; this proves *which kind of answer*
    // the request was priced expecting, and §66 puts the string in the report.
    if request.state.source != state_source {
        return Err(SimulationError::StateMismatch {
            priced: request.route.block_number,
            pinned: loaded,
            reason: format!(
                "the request was built against state source {:?}, but the provider it is being \
                 run on reports {state_source:?}",
                request.state.source
            ),
        });
    }

    // Every fee decision below is against the header the state was actually loaded
    // at, not the one the request claims — §20's check above makes them the same
    // block, and running on the loaded one is what makes that check matter.
    //
    // `Unresolved` speaks with `Ok(0)`: the run then declares no fee, executes
    // against a base fee of zero so that absence is not read as a refusal (§29), and
    // §31's `NotComputable` arrives through the charge rather than by hiding the
    // measurement.
    let abstained = matches!(request.pricing, GasPricing::Unresolved { .. });
    let price = fee_ceiling(&request.pricing, &header)?;
    if let Some(cap) = request.transaction.rules.gas_limit_cap() {
        if request.transaction.gas_limit_per_step > cap {
            return Err(unsupported(format!(
                "{} caps one transaction's gas limit at {cap}; this plan allows {} per step",
                request.transaction.rules.model(),
                request.transaction.gas_limit_per_step
            )));
        }
    }

    // ---- 2. ask the contracts ----------------------------------------------
    // §60: an empty `eth_getCode` is a refusal, not a bytecode to be filled in.
    for address in request.route.touched_contracts() {
        let code = provider.code(address).await.map_err(state_error)?;
        if code.is_empty() {
            return Err(SimulationError::MissingCode {
                address,
                block: header.number,
            });
        }
    }
    // §58's sender is an account that spends gas, not a contract. A sender with code
    // would make the sequence's reentrancy properties a question about that contract
    // rather than about the two pools.
    let sender = request.sender_address();
    let sender_code = provider.code(sender).await.map_err(state_error)?;
    if !sender_code.is_empty() {
        return Err(unsupported(format!(
            "the test sender {sender} has bytecode at block {}; §58's account is one that \
             signs nothing and runs nothing",
            header.number.0
        )));
    }
    let sender_nonce = provider
        .account(sender)
        .await
        .map_err(state_error)?
        .map_or(0, |account| account.nonce);

    // One view instance serves all six preflight reads: the state is pinned, so the
    // journal's cached answers are exactly as true for the sixth call as for the
    // first, and §61's cost of a redundant read buys nothing.
    let mut views = Views::new(&provider, &header, request, sender)?;
    let mut sides = Vec::with_capacity(request.route.legs.len());
    for leg in request.route.legs.iter() {
        let pool = leg.pool.address;
        sides.push(PairSides {
            pool,
            token0: views.address(pool, &V2Call::Token0).await?,
            token1: views.address(pool, &V2Call::Token1).await?,
        });
    }
    let sides = [sides[0], sides[1]];
    check_reserves(&mut views, request, loaded, &sides).await?;

    // ---- 3. build the plan --------------------------------------------------
    let plan = ExecutionPlan::two_pool_cycle(
        &request.route,
        sender,
        sides,
        request.transaction.funding,
        request.transaction.settle,
        request.transaction.asked_output,
    )?;
    if plan.len() != request.transaction.steps_planned {
        return Err(unsupported(format!(
            "the request planned for {} steps and the plan built {} of them",
            request.transaction.steps_planned,
            plan.len()
        )));
    }
    let mut plan_summary = PlanSummary::from(&plan);
    plan_summary.evm_rules = request.transaction.rules.model().to_string();

    // ---- 4. execute ---------------------------------------------------------
    let mut evm = build(&provider, &header, request, price, false, abstained)?;
    let template = tx_env(sender, request.transaction.gas_limit_per_step, price)?;
    let mut measurements = Measurements::default();
    let mut budget = GasBudget::new(request.transaction.gas_limit_per_step, plan.len());
    let mut steps = Vec::with_capacity(plan.len());
    let mut status = ExecutionStatus::Completed;
    let mut nonce = sender_nonce;
    let mut touched = Touched::default();

    for index in 0..plan.len() {
        // A native balance read is not a transaction: no call asks the EVM for an
        // account's balance, so the answer comes from the state the run is already
        // holding. It consumes neither nonce nor gas.
        if let PlanStep::MeasureNative { account, binding } = plan.steps[index] {
            let value = native_balance(&evm, &provider, account).await?;
            measurements.set(binding, value);
            steps.push(ExecutedStep::native_read(
                index, account, nonce, binding, value,
            ));
            continue;
        }

        let step = plan.resolve_step(index, &measurements, nonce)?;
        let gas_limit = request.transaction.gas_limit_per_step;
        let mut record = ExecutedStep::begun(&step, gas_limit);
        let outcome = execute(&mut evm, &template, &step, gas_limit).await?;
        touched.absorb(evm.ctx.journal().evm_state());
        budget.record(outcome.gas_used);
        record.gas_used = outcome.gas_used;
        record.status = outcome.status.clone();
        record.logs = outcome.logs;
        if let (Some(binding), Some(value)) = (step.binding, outcome.answered) {
            measurements.set(binding, value);
            record.measured = Some(MeasuredValue {
                binding: binding.name(),
                value,
            });
        }
        nonce = nonce.saturating_add(1);
        steps.push(record);

        match outcome.status {
            StepStatus::Success => {}
            StepStatus::Reverted(revert) => {
                budget.stop_at(index);
                status = ExecutionStatus::Reverted {
                    step: index,
                    call: step.describe(),
                    revert,
                };
                break;
            }
            StepStatus::OutOfGas => {
                budget.stop_at(index);
                status = ExecutionStatus::OutOfGas {
                    step: index,
                    call: step.describe(),
                };
                break;
            }
            StepStatus::Halted(reason) => {
                budget.stop_at(index);
                status = ExecutionStatus::Halted {
                    step: index,
                    call: step.describe(),
                    reason,
                };
                break;
            }
        }
    }

    // ---- 5. diff the state --------------------------------------------------
    let state_changes = state_changes(&touched, &provider).await?;

    // ---- 6. price it --------------------------------------------------------
    let charge = budget.charged(&request.pricing, &header)?;
    let input = request.route.input_amount;
    let outcome = match &status {
        ExecutionStatus::Completed => match (
            measurements.get(Binding::SenderInputEnd),
            measurements.get(Binding::SenderInputStart),
        ) {
            // What the route *credited* the sender, in the input token's own unit
            // (§27) — the difference between the two balances the plan measures, not
            // the closing balance. Reading the closing balance would count a token the
            // sender already held as if the pools had paid it. It is the same number on
            // a sender that starts empty, which §58's derived address does on the real
            // block, and a different one on any other.
            //
            // Before any unwrap: a full `Settle::UnwrapInputToken` leaves the token
            // balance at zero by construction, and reading that as the output would
            // report a profitable route as a total loss.
            (Some(end), Some(start)) => {
                let output = end.checked_sub(start).ok_or_else(|| {
                    SimulationError::MissingState(format!(
                        "the sender held {start} of the input token before the sequence and {end} \
                         after it: a completed plan transfers the input in and nothing else takes \
                         it back, so the two measurements do not describe one run"
                    ))
                })?;
                SimulatedOutcome::Executed {
                    output,
                    movement: Movement::between(input, output),
                }
            }
            // A plan that completed without measuring what it was asked to measure
            // has no output to claim.
            _ => SimulatedOutcome::NotExecuted,
        },
        _ => SimulatedOutcome::NotExecuted,
    };
    let gross_profit = outcome.gross_profit(input);
    let gross_loss = outcome.gross_loss(input);
    let compared = OutputComparison {
        analytical: request.route.analytical_output,
        simulated: outcome.output(),
    };
    let denomination = denomination(&plan, request, &measurements, &charge);
    let net_profit = NetProfit::compute(
        GrossMovement::of(&outcome, input),
        &charge,
        denomination.as_ref(),
    );

    Ok(SimulationResult {
        chain_id: request.chain_id,
        block: loaded,
        state_source,
        sender,
        status,
        steps,
        gas_charge: charge,
        measurements: measurements
            .recorded()
            .into_iter()
            .map(|(binding, value)| MeasuredValue { binding, value })
            .collect(),
        compared,
        outcome,
        gross_profit,
        gross_loss,
        net_profit,
        state_changes,
        slippage: plan.slippage,
        plan_summary,
    })
}

/// The fee ceiling the run declares, or zero when the pricing model abstains.
///
/// A price has to exist for the sender to be able to pay for the gas it is about to
/// be allowed to spend, and [`GasPricing::Unresolved`] says it does not — so this is
/// the one place that turns the abstention into the `0` the EVM's fee fields carry,
/// and it is a stated absence rather than a discovered price.
fn fee_ceiling(pricing: &GasPricing, header: &BlockContext) -> Result<u128> {
    match pricing {
        GasPricing::Unresolved { .. } => Ok(0),
        declared => declared
            .max_fee_per_gas(header)?
            .ok_or_else(|| unsupported("a priced run asked for no price".to_string())),
    }
}

// ---------------------------------------------------------------------------
// execution
// ---------------------------------------------------------------------------

fn block_env(header: &BlockContext, rules: EvmRules) -> Result<BlockEnv> {
    let spec = rules.spec_id();
    // The blob field is the header's own, and §60's rule applies to it exactly as it
    // applies to bytecode: a ruleset that wants it and a source that never reported
    // it is a refusal, not a zero. A pre-blob ruleset asks for nothing, so the same
    // absence is simply irrelevant there.
    let blob_excess_gas_and_price = match header.excess_blob_gas {
        Some(excess) => Some(BlobExcessGasAndPrice::new_with_spec(excess, spec)),
        None if spec.is_enabled_in(SpecId::CANCUN) => {
            return Err(SimulationError::MissingState(format!(
                "block {} as this source reports it carries no excessBlobGas, which the {} \
                 ruleset needs in order to build a block environment; either read the field from \
                 a source that has it or run the sequence under a pre-blob ruleset",
                header.number.0,
                rules.model()
            )));
        }
        None => None,
    };
    Ok(BlockEnv {
        number: U256::from(header.number.0),
        beneficiary: header.beneficiary,
        timestamp: U256::from(header.timestamp),
        gas_limit: header.gas_limit,
        basefee: header
            .base_fee_per_gas
            .map(|fee| {
                u64::try_from(fee).map_err(|_| {
                    unsupported(format!("base fee {fee} does not fit the EVM's block field"))
                })
            })
            .transpose()?
            .unwrap_or(0),
        difficulty: U256::ZERO,
        prevrandao: header.prevrandao,
        blob_excess_gas_and_price,
        // EIP-7843's beacon slot, which is an Amsterdam field and is not in the
        // header this crate reads. No rule at the rulesets offered here uses it, and
        // inventing a slot from the timestamp would be a guess about a chain.
        slot_num: 0,
    })
}

/// Build the EVM for one run.
///
/// `view` marks the preflight reads. A view is not a transaction anyone intends to
/// pay for, so it is given a fee of zero, and REVM's rule that a fee must reach the
/// block's base fee is satisfied by running the read against a base fee of zero —
/// what an `eth_call` does — rather than by switching the validator off. Its nonce
/// check is off because the real sequence has not spent the nonce these reads borrow
/// while it runs. Nothing else differs: same pinned header fields, same ruleset, same
/// provider, and a separate journal that is discarded, so a view cannot leak a change
/// into a result — no engine path writes to the [`StateProvider`].
///
/// `abstained` is the same device for the other half of §29: a run whose pricing is
/// [`GasPricing::Unresolved`] has no fee to declare, so it must not be refused for
/// lacking one. The refusal would describe this crate's missing knowledge rather than
/// the market, and the run's cost stays `Unpriced` and its net profit
/// `NotComputable` (§31) whatever the base-fee field says — while its balance and
/// nonce checks stay on, because those are facts about a sender §58 does endow.
fn build(
    provider: &Arc<dyn StateProvider>,
    header: &BlockContext,
    request: &SimulationRequest,
    price: u128,
    view: bool,
    abstained: bool,
) -> Result<SimEvm> {
    let mut block = block_env(header, request.transaction.rules)?;
    if view || abstained {
        block.basefee = 0;
    }
    let mut cfg =
        CfgEnv::new().with_spec_and_mainnet_gas_params(request.transaction.rules.spec_id());
    cfg.disable_nonce_check = view;
    let db = AsyncDb::new(ProviderDb::new(Arc::clone(provider)));
    let gas_limit = if view {
        header.gas_limit
    } else {
        request.transaction.gas_limit_per_step
    };
    let tx = tx_env(
        request.sender_address(),
        gas_limit,
        if view { 0 } else { price },
    )?;
    Ok(revm::Context::mainnet()
        .with_cfg(cfg)
        .with_block(block)
        .with_tx(tx)
        .with_db(db)
        .build_mainnet())
}

/// The transaction envelope every step starts from: same sender, same fee model,
/// same chain. Nonce, target, value and calldata are per step — see
/// [`apply_step`].
///
/// The fee is carried as one number, the ceiling [`fee_ceiling`] resolved from the
/// declared model against the pinned header, and no separate priority fee is declared
/// with it: REVM's effective price for a transaction shaped like this is exactly that
/// number, which is the number §30 multiplies by `gas_used`. Splitting it into a base
/// fee and a tip here would put a second, independent claim about the market into the
/// execution; the split an auditor needs is already in the header fields
/// [`GasCharge`] reports (§29).
fn tx_env(caller: Address, gas_limit: u64, price: u128) -> Result<TxEnv> {
    Ok(TxEnv::builder()
        .caller(caller)
        .gas_limit(gas_limit)
        .gas_price(price)
        .chain_id(None)
        .nonce(0)
        .kind(TxKind::Call(Address::ZERO))
        .value(U256::ZERO)
        .data(Bytes::new())
        .build_fill())
}

/// Point the envelope at this step. Everything else — caller, fee ceiling, chain —
/// is the same for every step of the sequence, which is what makes the sequence one
/// sender's trades rather than six unrelated calls.
fn apply_step(tx: &mut TxEnv, step: &ResolvedStep, gas_limit: u64) {
    tx.nonce = step.nonce;
    tx.kind = TxKind::Call(step.to);
    tx.value = step.value;
    tx.data = step.calldata();
    tx.gas_limit = gas_limit;
}

/// Run one step and read what the EVM answered.
async fn execute(
    evm: &mut SimEvm,
    template: &TxEnv,
    step: &ResolvedStep,
    gas_limit: u64,
) -> Result<Stepped> {
    let mut tx = template.clone();
    apply_step(&mut tx, step, gas_limit);
    let result = evm.transact_one_async(tx).await.map_err(fiber_failure)?;
    stepped(&result, step)
}

/// What a step's measurement binding is worth, from the bytes its call returned.
///
/// A measurement is decoded through the protocol crate's own ABI reading rather
/// than by taking the last 32 bytes, so a token contract that returns something
/// other than a `uint256` from `balanceOf` is reported instead of being quietly
/// reinterpreted.
fn decoded(step: &ResolvedStep, bytes: &[u8]) -> Result<Option<U256>> {
    if step.binding.is_none() {
        return Ok(None);
    }
    match step.call.decode_return(bytes).map_err(protocol_error)? {
        CallReturn::Amount(amount) => Ok(Some(amount)),
        other => Err(unsupported(format!(
            "{} at {} answered {other:?}, which is not the amount this step measures",
            step.call.signature(),
            step.to
        ))),
    }
}

/// Map REVM's answer onto this crate's step status.
///
/// A revert, an out-of-gas and any other halt are results, so they go into the
/// status; the only errors that come out of here are ones where the EVM refused to
/// answer at all.
fn stepped(result: &ExecutionResult<HaltReason>, step: &ResolvedStep) -> Result<Stepped> {
    let gas_used = result.tx_gas_used();
    let logs = executed_logs(result.logs());
    match result {
        ExecutionResult::Success { output, .. } => {
            let bytes = match output {
                Output::Call(bytes) => bytes.as_ref(),
                // A plan this crate builds never creates a contract, so a create
                // output means the sequence is not the one the plan described.
                Output::Create(_, _) => {
                    return Err(unsupported(format!(
                        "step {} created a contract, which no plan from this crate asks for",
                        step.index
                    )))
                }
            };
            Ok(Stepped {
                gas_used,
                status: StepStatus::Success,
                logs,
                answered: decoded(step, bytes)?,
            })
        }
        ExecutionResult::Revert { output, .. } => Ok(Stepped {
            gas_used,
            status: StepStatus::Reverted(RevertData::new(output.clone())),
            logs,
            answered: None,
        }),
        ExecutionResult::Halt { reason, .. } => Ok(Stepped {
            gas_used,
            status: if matches!(reason, HaltReason::OutOfGas(_)) {
                StepStatus::OutOfGas
            } else {
                // Every other halt — an invalid opcode, a state change inside a
                // static call, the call-stack limit — gets its own kind, because
                // reporting it as `OutOfGas` would tell the reader the step spent
                // its whole allowance when the contract stopped for a reason the
                // allowance has nothing to do with.
                StepStatus::Halted(reason.to_string())
            },
            logs,
            answered: None,
        }),
    }
}

fn executed_logs(logs: &[Log]) -> Vec<ExecutedLog> {
    logs.iter()
        .map(|log| ExecutedLog {
            address: log.address,
            topics: log.data.topics().to_vec(),
            data: log.data.data.clone(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The preflight reads, executed rather than assumed
// ---------------------------------------------------------------------------

/// One EVM instance that answers the questions §5 and §42 ask the contracts
/// themselves: `token0()`, `token1()`, `getReserves()`.
///
/// These are run through REVM against the pinned state instead of `eth_call`, so the
/// fixture path and the node path answer from the same bytecode and the same
/// storage, and a pool that lies about its sides lies identically on both (§62).
struct Views {
    evm: SimEvm,
    sender: Address,
}

impl Views {
    fn new(
        provider: &Arc<dyn StateProvider>,
        header: &BlockContext,
        request: &SimulationRequest,
        sender: Address,
    ) -> Result<Self> {
        Ok(Self {
            evm: build(provider, header, request, 0, true, true)?,
            sender,
        })
    }

    /// Run one read-only call and hand back what the contract returned.
    async fn call(&mut self, to: Address, calldata: Bytes) -> Result<Bytes> {
        let mut tx = self.evm.ctx.tx.clone();
        tx.caller = self.sender;
        tx.kind = TxKind::Call(to);
        tx.data = calldata;
        let result = self
            .evm
            .transact_one_async(tx)
            .await
            .map_err(fiber_failure)?;
        match result {
            ExecutionResult::Success {
                output: Output::Call(bytes),
                ..
            } => Ok(bytes),
            other => Err(unsupported(format!(
                "a read-only call to {to} did not answer: {}",
                status_word(&other)
            ))),
        }
    }

    async fn address(&mut self, pool: Address, call: &V2Call) -> Result<Address> {
        let signature = call.signature();
        let bytes = self.call(pool, call.encode()).await?;
        match call.decode_return(&bytes).map_err(protocol_error)? {
            CallReturn::Address(address) => Ok(address),
            other => Err(unsupported(format!(
                "{signature} at {pool} answered {other:?}, which is not an address"
            ))),
        }
    }

    async fn reserves(&mut self, pool: Address) -> Result<Reserves> {
        let bytes = self.call(pool, V2Call::GetReserves.encode()).await?;
        match V2Call::GetReserves
            .decode_return(&bytes)
            .map_err(protocol_error)?
        {
            CallReturn::Reserves(reserves) => Ok(reserves),
            other => Err(unsupported(format!(
                "getReserves() at {pool} answered {other:?}, which is not a reserve triple"
            ))),
        }
    }
}

fn status_word(result: &ExecutionResult<HaltReason>) -> &'static str {
    match result {
        ExecutionResult::Success { .. } => "success with no call output",
        ExecutionResult::Revert { .. } => "a revert",
        ExecutionResult::Halt { .. } => "a halt",
    }
}

/// §42's state-mismatch detector in practice.
///
/// Each leg's `reserve_in` and `reserve_out` are M3's numbers, taken from the block
/// this opportunity was found on. `getReserves()` is the pool's own answer about the
/// same two tokens. If they disagree, the state being executed against is not the
/// state the opportunity was priced on, and a simulation of the difference would
/// report a profit that is an artifact of reading the wrong block.
///
/// Which word of the answer is which token is decided by the pool's own `token0()`
/// and `token1()`, read just above it — never by sorting the two addresses here.
async fn check_reserves(
    views: &mut Views,
    request: &SimulationRequest,
    loaded: BlockPin,
    sides: &[PairSides; 2],
) -> Result<()> {
    for (leg, side) in request.route.legs.iter().zip(sides.iter()) {
        let reserves = views.reserves(side.pool).await?;
        let seen_in = word_for(side, leg.token_in.address, reserves)?;
        let seen_out = word_for(side, leg.token_out.address, reserves)?;
        for (what, claimed, found) in [
            ("reserve_in", leg.reserve_in, seen_in),
            ("reserve_out", leg.reserve_out, seen_out),
        ] {
            if claimed != found {
                return Err(SimulationError::StateMismatch {
                    priced: request.route.block_number,
                    pinned: loaded,
                    reason: format!(
                        "pool {} reports {found} for {what} of {}, but the route priced it at \
                         {claimed}; the state being executed is not the state the opportunity \
                         was found on",
                        side.pool, leg.token_in.address
                    ),
                });
            }
        }
    }
    Ok(())
}

/// The reserve the pool holds of `token`, from its own side ordering.
fn word_for(side: &PairSides, token: Address, reserves: Reserves) -> Result<U256> {
    if side.token0 == token {
        return Ok(reserves.reserve0);
    }
    if side.token1 == token {
        return Ok(reserves.reserve1);
    }
    Err(unsupported(format!(
        "pool {} reports token0 {} and token1 {}, so its reserves cannot describe {}",
        side.pool, side.token0, side.token1, token
    )))
}

// ---------------------------------------------------------------------------
// measurements and denomination
// ---------------------------------------------------------------------------

/// A native balance, from the state the run is holding.
///
/// Before the first transaction the journal is empty, so the answer is the
/// provider's; afterwards it is the journal's, because the gas already spent is part
/// of what the account holds. Reading the provider again would report the pre-run
/// balance and make §32's equality trivially false.
async fn native_balance(
    evm: &SimEvm,
    provider: &Arc<dyn StateProvider>,
    account: Address,
) -> Result<U256> {
    if let Some(found) = evm
        .ctx
        .journal()
        .evm_state()
        .get(&account)
        .map(|record| record.info.balance)
    {
        return Ok(found);
    }
    provider
        .account(account)
        .await
        .map_err(state_error)?
        .map(|header| header.balance)
        .ok_or_else(|| SimulationError::MissingState(format!("native balance of {account}")))
}

/// §32's denomination proof, or `None` when the run did not make one.
///
/// Only a completed [`Settle::UnwrapInputToken`] run can prove it, and it proves it
/// with an equality rather than an assertion: the wrapped tokens the withdraw was
/// asked to release have to account for exactly the native the sender's ledger moved
/// — see [`Denomination::is_proved`]. A run that never executed the conversion cannot
/// state a rate between the token and the native asset at all, which is why
/// [`NetProfit::NotComputable`] is the honest answer and `1 WETH = 1 ETH` is not.
///
/// The three measurements gate themselves: the native read that closes the sequence
/// is its last step, so a run that stopped early has no `SenderNativeEnd` and returns
/// `None` here instead of a proof assembled from a half-run.
fn denomination(
    plan: &ExecutionPlan,
    request: &SimulationRequest,
    measurements: &Measurements,
    charge: &GasCharge,
) -> Option<Denomination> {
    if !matches!(plan.settle, Settle::UnwrapInputToken) {
        return None;
    }
    let withdraw_step = plan
        .steps
        .iter()
        .position(|step| matches!(step, PlanStep::Withdraw { .. }))?;
    let native_start = measurements.get(Binding::SenderNativeStart)?;
    let native_end = measurements.get(Binding::SenderNativeEnd)?;
    let converted = measurements.get(Binding::SenderInputEnd)?;
    let gas_paid = charge.wei()?;
    // The native this run put *into* the wrapped contract. Every transaction step
    // lies between the two native reads, so the whole sequence's bill is the right
    // figure for `gas_paid` — no step's gas falls outside the window they bound.
    let native_spent = request.native_to_wrap();
    Some(Denomination {
        token: plan.output_token(),
        converted,
        native_start,
        native_end,
        native_spent,
        gas_paid,
        proved_by: format!(
            "withdraw({converted}) executed on {} at step {withdraw_step} of a {}-step plan, \
             between the native balance reads that open and close it; the sequence was funded \
             by {}",
            plan.route.input_token.address,
            plan.len(),
            match plan.funding {
                Funding::WrapNative => "an executed deposit() of the input amount",
                Funding::Erc20Balance => "a token balance the sender already held",
            },
        ),
    })
}

// ---------------------------------------------------------------------------
// state diff
// ---------------------------------------------------------------------------

/// Everything the run's steps left holding a value, keyed by account and by storage
/// word, with the closing value as the entry.
///
/// Absorbed after every step rather than read once at the end, because a plan is
/// executed one transaction per step and REVM's journal only keeps the window of the
/// transaction that is current — see [`state_changes`] for what that costs an audit
/// built from the last window alone. The last step that held a word contributes its
/// value, which is the value the sequence ended with.
#[derive(Default)]
struct Touched {
    accounts: HashMap<Address, (U256, u64)>,
    slots: HashMap<(Address, U256), U256>,
}

impl Touched {
    fn absorb(&mut self, state: &EvmState) {
        for (address, account) in state {
            self.accounts
                .insert(*address, (account.info.balance, account.info.nonce));
            for (slot, record) in account.storage.iter() {
                self.slots.insert((*address, *slot), record.present_value());
            }
        }
    }
}

/// The §36 audit trail, ordered.
///
/// The two sides of every change come from two different places, and that is the
/// whole point. `after` is what the execution ended holding. `before` is read back
/// from the state provider — the pinned block, with §58's setup layered on it — and
/// not from REVM's own `original_value`, for two reasons the real block settled:
///
/// * The journal is a *per-transaction* record and a plan here is one transaction
///   per step, so the state map `finalize()` hands back after the last step covers
///   only the window it still holds. On block 37191169 the 12-step plan left 6
///   accounts and 6 storage words there, while the sequence had written words in
///   four contracts; the pool that was credited the route's input did not appear at
///   all.
/// * Where a word *was* recorded, its `original` was the value at the moment that
///   window opened, not the value the block held: the test sender's TTAX word is
///   recorded against 881325264824006417, which is what the first pool had just
///   paid it, and the dump it was loaded from says 0.
///
/// An audit that reports the middle of a run as its starting point is worse than no
/// audit, because it looks like one. Reading the pin back is exact, and the same
/// cached reads (§63, §64) serve it, so it costs the run nothing it had not already
/// paid.
///
/// Neither list is emitted in the accumulated map's order: [`account_reads`] and
/// [`slot_reads`] sort the keys first, so the audit list and the order the provider is
/// asked in are both fixed. Slot meanings are not decoded, per
/// [`crate::result::SlotChange`]'s note: which word holds a reserve is a claim about
/// a layout, and the pool's own `getReserves()` answer is what this crate cites
/// instead.
async fn state_changes(
    touched: &Touched,
    provider: &Arc<dyn StateProvider>,
) -> Result<StateChanges> {
    let mut accounts: Vec<AccountChange> = Vec::new();
    for (address, balance, nonce) in account_reads(touched) {
        let recorded = provider.account(address).await.map_err(state_error)?;
        let (balance_before, nonce_before) =
            recorded.map_or((U256::ZERO, 0), |account| (account.balance, account.nonce));
        if balance != balance_before || nonce != nonce_before {
            accounts.push(AccountChange {
                address,
                balance_before,
                balance_after: balance,
                nonce_before,
                nonce_after: nonce,
            });
        }
    }

    let mut slots: Vec<crate::result::SlotChange> = Vec::new();
    for (address, slot, after) in slot_reads(touched) {
        let before = provider.storage(address, slot).await.map_err(state_error)?;
        if before != after {
            slots.push(crate::result::SlotChange {
                address,
                slot,
                before,
                after,
            });
        }
    }
    Ok(StateChanges { accounts, slots })
}

/// The accumulated map's account facts, in the order §36 asks the provider for them.
///
/// Sorted because a `HashMap` iterates in an order chosen per process, and that order
/// is not only the order of the audit list — it is also the order the reads reach the
/// provider, which the fixture's `reads` field records (§62). Unsorted, two recordings
/// of one pinned block agree in every value and still differ in bytes.
fn account_reads(touched: &Touched) -> Vec<(Address, U256, u64)> {
    let mut facts: Vec<(Address, U256, u64)> = touched
        .accounts
        .iter()
        .map(|(address, (balance, nonce))| (*address, *balance, *nonce))
        .collect();
    facts.sort_by_key(|(address, _, _)| address.into_word());
    facts
}

/// [`account_reads`] for storage words, ordered by contract then slot.
fn slot_reads(touched: &Touched) -> Vec<(Address, U256, U256)> {
    let mut facts: Vec<(Address, U256, U256)> = touched
        .slots
        .iter()
        .map(|((address, slot), after)| (*address, *slot, *after))
        .collect();
    facts.sort_by_key(|(address, slot, _)| (address.into_word(), *slot));
    facts
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, Address, U256};

    use super::{account_reads, slot_reads, Touched};

    /// Eight accounts, deliberately not ascending, each holding a balance and a
    /// storage word that are its own — the facts stay put while the order they are
    /// accumulated in moves, which is the only thing this test is about.
    const ACCOUNTS: [(Address, u64, u64); 8] = [
        (
            address!("0x000000000000000000000000000000000000000b"),
            11,
            110,
        ),
        (
            address!("0x0000000000000000000000000000000000000003"),
            3,
            30,
        ),
        (
            address!("0x000000000000000000000000000000000000000f"),
            15,
            150,
        ),
        (
            address!("0x0000000000000000000000000000000000000001"),
            1,
            10,
        ),
        (
            address!("0x000000000000000000000000000000000000000d"),
            13,
            130,
        ),
        (
            address!("0x0000000000000000000000000000000000000007"),
            7,
            70,
        ),
        (
            address!("0x0000000000000000000000000000000000000002"),
            2,
            20,
        ),
        (
            address!("0x000000000000000000000000000000000000000a"),
            10,
            100,
        ),
    ];

    /// The same eight facts, inserted in the order `positions` says.
    fn touched(positions: &[usize]) -> Touched {
        let mut out = Touched::default();
        for &index in positions {
            let (address, value, nonce) = ACCOUNTS[index];
            let amount = U256::from(value);
            out.accounts.insert(address, (amount, nonce));
            out.slots.insert((address, amount), amount);
        }
        out
    }

    /// §37's determinism has a second consumer than the result: a fixture's `reads`
    /// field records the order the provider was asked in, so a state diff read back in
    /// `HashMap` order writes bytes that move between processes while every value in
    /// them stays put. Ascending by key is therefore not a presentation detail — and
    /// it holds no matter which insertion order filled the map.
    #[test]
    fn the_state_diff_is_read_back_in_key_order_whatever_order_it_was_accumulated() {
        let forwards: Vec<usize> = (0..ACCOUNTS.len()).collect();
        let backwards: Vec<usize> = forwards.iter().rev().copied().collect();
        let first = account_reads(&touched(&forwards));
        let second = account_reads(&touched(&backwards));
        assert_eq!(
            first, second,
            "the same eight accounts, accumulated in two orders, came back in two              different read-back orders"
        );

        let mut ascending: Vec<Address> = ACCOUNTS.iter().map(|(address, _, _)| *address).collect();
        ascending.sort_by_key(|address| address.into_word());
        assert_eq!(
            first
                .iter()
                .map(|(address, _, _)| *address)
                .collect::<Vec<_>>(),
            ascending,
            "accounts are read back ascending by address"
        );

        let slots = slot_reads(&touched(&backwards));
        assert_eq!(slots.len(), ACCOUNTS.len(), "one word per account");
        assert!(
            slots
                .windows(2)
                .all(|pair| (pair[0].0.into_word(), pair[0].1) < (pair[1].0.into_word(), pair[1].1)),
            "storage words are read back by contract, then by slot"
        );
    }
}
