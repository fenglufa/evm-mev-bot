//! The M10 execution primitive, run through this crate's REVM boundary.
//!
//! §25 asks the simulation to put `EOA → Executor → Pair A → Pair B → EOA` in front of
//! real bytecode, and to refuse to answer the question with a Rust formula. Everything
//! below therefore goes through the same `ProviderDb`, the same pinned header, the same
//! [`build_evm`][crate::engine::build_evm] and the same state-diff read-back the M4 plan
//! path uses; what differs is only *what* is executed — one call to an executor contract
//! instead of a sequence of calls to two pools. There is no second EVM here.
//!
//! The one thing the plan path cannot express is the call itself: [`PlanStep`][crate::plan::PlanStep]
//! speaks of transfers, swaps, deposits and withdrawals, and an executor call is none of
//! those — it is a single transaction whose internal sequence is the contract's, not this
//! crate's. So this module drives that one transaction directly rather than widening the
//! plan vocabulary, which keeps every M4 run byte-identical.
//!
//! ## What a run reports, and why each piece is read the way it is
//!
//! ```text
//! status, gas_used, logs     REVM's own answer about the one transaction
//! contract error name        decoded from the revert data by the protocol crate's ABI
//! reserves before / after    getReserves(), executed against the state, never read from a slot
//! balances before / after    balanceOf(), likewise
//! state_changes              §36's word-level diff, read back from the pin
//! ```
//!
//! The before-values come from a view EVM over the pinned state. The after-values are
//! *separate transactions from the same operator on the run's own EVM*, so what they read
//! is the state the executor call left in the journal — which on a revert is the state it
//! found. That is the shape §27 asks for: a forced second-leg failure has to show the
//! pools and the tokens holding the same numbers as before, not merely a reverted status.
//!
//! Nothing here broadcasts, signs, or holds a key. The operator is an address, the state
//! comes from a pinned source, and the result is dropped at the end of the call.

use std::sync::Arc;

use alloy_primitives::{Address, Bytes, U256};
use revm::context::TxEnv;
use revm::context_interface::result::{ExecutionResult, HaltReason, Output};
use revm::context_interface::{ContextTr, JournalTr};
use revm::primitives::TxKind;
use revm::ExecuteEvmAsync;
use serde::Serialize;

use evm_core::{BlockNumber, ChainId};
use evm_protocol::{decode_revert, CallReturn, ExecutorCall, ExecutorLeg, V2Call};

use crate::engine::{
    executed_logs, fee_ceiling, fiber_failure, interpreter_phase, protocol_error, state_changes,
    state_error, tx_env, unsupported, SimEvm, Touched, Views, EXECUTE_PHASE_PREFIX,
    VIEWS_PHASE_PREFIX,
};
use crate::error::SimulationError;
use crate::gas::{GasBudget, GasCharge, GasPricing};
use crate::request::EvmRules;
use crate::result::{ExecutedLog, Movement, RevertData, StateChanges, StepStatus};
use crate::state::{BlockPin, StateOverride};
use crate::{Result, StateProvider};

// ---------------------------------------------------------------------------
// The request
// ---------------------------------------------------------------------------

/// One executor call, asked of one pinned state source.
///
/// Every field is a claim the run checks rather than assumes: `chain_id`, `priced_at` and
/// `state_source` are compared against what the provider actually serves, `endowment` is
/// the only state this run is allowed to write (and only at `operator`, the account §58
/// describes — see [`ExecutorRun::setup`]), and `call` is encoded to the calldata bytes a
/// real transaction would carry.
#[derive(Clone, Debug)]
pub struct ExecutorRun {
    pub chain_id: ChainId,
    /// The block the plan was priced at, for the mismatch message §20 asks a refusal to
    /// carry. The run's own height comes from the provider, never from here.
    pub priced_at: BlockNumber,
    pub state_source: String,
    pub executor: Address,
    pub operator: Address,
    pub call: ExecutorCall,
    pub gas_limit: u64,
    pub rules: EvmRules,
    pub pricing: GasPricing,
    /// Native wei the operator is given for gas, or `None` to run at whatever the pinned
    /// block says it holds. A `Some` is scaffolding and says so in the override's reason.
    pub endowment: Option<U256>,
}

/// The numbers a call carries, read out of the call rather than restated by its caller,
/// plus the route it describes.
///
/// `execute` is the route M10 exists for. `withdraw` — §54's item 12, the operator-only
/// move of a balance the contract genuinely holds — carries no route, so its watch list is
/// the one token the call names and the amount is both the ask and the floor. The two share
/// a type because the outcome reports the same columns either way, and the words describing
/// those columns must not change meaning between the two calls.
#[derive(Clone, Debug)]
pub struct Watch {
    pub legs: Vec<ExecutorLeg>,
    pub input_token: Address,
    pub amount_in: U256,
    pub min_final_amount: U256,
    pub recipient: Address,
    /// Whether the call is declared to answer with nothing. Kept as its own answer rather
    /// than inferred from empty return data, so an `execute` that returns no bytes and a
    /// `withdraw` that returns some are both refusals instead of a `None`.
    pub void_return: bool,
}

impl ExecutorRun {
    /// What this run will watch. A configuration call or a view is refused: it answers a
    /// question about the contract's setup, not about a run, and it has no balances for a
    /// run to compare either side of.
    pub fn watch(&self) -> Result<Watch> {
        match &self.call {
            ExecutorCall::Execute {
                legs,
                input_token,
                amount_in,
                min_final_amount,
                recipient,
            } => Ok(Watch {
                legs: legs.clone(),
                input_token: *input_token,
                amount_in: *amount_in,
                min_final_amount: *min_final_amount,
                recipient: *recipient,
                void_return: false,
            }),
            ExecutorCall::Withdraw { token, to, amount } => Ok(Watch {
                legs: Vec::new(),
                input_token: *token,
                amount_in: *amount,
                min_final_amount: *amount,
                recipient: *to,
                void_return: true,
            }),
            other => Err(unsupported(format!(
                "{} carries no amount to be moved, and this run reports balances either side \
                 of the call",
                other.signature()
            ))),
        }
    }

    /// The contracts whose bytecode must exist at the pin: the executor first, then each
    /// leg's pool and both of its tokens, in leg order, then the token the call names.
    /// Deduplicated but not sorted — this is the order the §60 batch asks for them, and the
    /// first missing one is the address the refusal names.
    pub fn contracts(&self) -> Result<Vec<Address>> {
        let watch = self.watch()?;
        let mut seen: Vec<Address> = Vec::new();
        let mut push = |address: Address| {
            if !seen.contains(&address) {
                seen.push(address);
            }
        };
        push(self.executor);
        for leg in watch.legs.iter() {
            push(leg.pool);
            push(leg.token_in);
            push(leg.token_out);
        }
        push(watch.input_token);
        Ok(seen)
    }

    /// The accounts whose token balances this run measures: the operator that funds the
    /// route, the executor that must end empty, and the recipient the call pays.
    pub fn holders(&self) -> Result<Vec<Address>> {
        let watch = self.watch()?;
        Ok(vec![self.operator, self.executor, watch.recipient])
    }

    /// §58's one permitted state edit: native gas money at the operator, and nothing
    /// else. Returns empty when the run declares no endowment, which is the honest shape
    /// for a run on a real account's real balance.
    pub fn setup(&self) -> Vec<StateOverride> {
        match self.endowment {
            Some(wei) => vec![StateOverride::balance(
                self.operator,
                wei,
                format!(
                    "M10 executor simulation setup: the operator funded with {wei} wei to pay \
                     for one transaction of at most {} gas — scaffolding, not a fact about this \
                     chain",
                    self.gas_limit
                ),
            )],
            None => Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// The answer
// ---------------------------------------------------------------------------

/// A pool's own answer to `getReserves()`, at one side of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ReserveSnapshot {
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    pub block_timestamp_last: U256,
}

/// One pool, read before and after the call.
///
/// The sides are recorded because `reserve0` and `reserve1` mean nothing without them,
/// and they come from the pool's own `token0()`/`token1()` rather than from an ordering
/// of two addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ReserveRow {
    pub pool: Address,
    pub before: ReserveSnapshot,
    pub after: ReserveSnapshot,
}

impl ReserveRow {
    pub fn changed(&self) -> bool {
        self.before != self.after
    }
}

/// One (token, holder) pair, read before and after the call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct BalanceRow {
    pub token: Address,
    pub holder: Address,
    pub before: U256,
    pub after: U256,
}

impl BalanceRow {
    pub fn changed(&self) -> bool {
        self.before != self.after
    }

    pub fn movement(&self) -> Movement {
        Movement::between(self.before, self.after)
    }
}

/// What one executor call did.
#[derive(Clone, Debug, Serialize)]
pub struct ExecutorOutcome {
    pub chain_id: ChainId,
    pub block: BlockPin,
    pub state_source: String,
    pub executor: Address,
    pub operator: Address,
    pub recipient: Address,
    /// The call's claim about its own input, taken from the decoded call rather than
    /// restated by the caller: the profit invariant compares `delivered` against these. On
    /// a withdrawal there is no route, so these are the token and amount the call names.
    pub input_token: Address,
    pub amount_in: U256,
    pub min_final_amount: U256,
    pub signature: &'static str,
    pub selector: String,
    pub calldata: Bytes,
    pub calldata_len: usize,
    pub gas_limit: u64,
    pub gas_used: u64,
    pub charge: GasCharge,
    pub status: StepStatus,
    /// The contract's own named error, when the revert data decodes as one of the
    /// executor's 22. `None` alongside a revert means the payload was not the
    /// contract's — a pool's `Error(string)`, an empty revert, or something else.
    pub contract_error: Option<String>,
    /// What kind of payload the revert carried, in the protocol crate's own words.
    pub revert_kind: Option<&'static str>,
    /// What the call answered with. `execute` returns the amount it delivered, so a
    /// successful one always carries a number here; `withdraw` is declared void, and a
    /// successful withdrawal is proved at the recipient's balance row instead, so it
    /// reports `None`. The two are never folded together.
    pub delivered: Option<U256>,
    pub logs: Vec<ExecutedLog>,
    pub reserves: Vec<ReserveRow>,
    pub balances: Vec<BalanceRow>,
    pub state_changes: StateChanges,
}

impl ExecutorOutcome {
    pub fn succeeded(&self) -> bool {
        self.status.succeeded()
    }

    pub fn revert(&self) -> Option<&RevertData> {
        match &self.status {
            StepStatus::Reverted(data) => Some(data),
            _ => None,
        }
    }

    /// Did the market move? False on a run that reverted before anything settled, and
    /// the §27 check is that it stays false even when legs did run inside the call.
    pub fn market_moved(&self) -> bool {
        self.reserves.iter().any(ReserveRow::changed)
            || self.balances.iter().any(BalanceRow::changed)
    }

    /// The words the run left different inside a contract this run is watching. A
    /// reverted call must produce none, which is the residue check stated at the level
    /// of storage rather than at the level of the two numbers a row compares.
    pub fn changed_slots_in(&self, address: Address) -> Vec<&crate::result::SlotChange> {
        self.state_changes.storage_of(address)
    }

    pub fn reserve_row(&self, pool: Address) -> Option<&ReserveRow> {
        self.reserves.iter().find(|row| row.pool == pool)
    }

    pub fn balance_row(&self, token: Address, holder: Address) -> Option<&BalanceRow> {
        self.balances
            .iter()
            .find(|row| row.token == token && row.holder == holder)
    }

    /// One line for a report: what ran, where, and how it ended.
    pub fn describe(&self) -> String {
        let ended = match &self.status {
            StepStatus::Success => match self.delivered {
                Some(amount) => format!("returned {amount}"),
                None => "returned nothing".to_string(),
            },
            StepStatus::Reverted(data) => match &self.contract_error {
                Some(name) => format!("reverted with {name} ({})", data.reason()),
                None => format!("reverted: {}", data.reason()),
            },
            StepStatus::OutOfGas => "ran out of gas".to_string(),
            StepStatus::Halted(reason) => format!("halted: {reason}"),
        };
        format!(
            "{} at block {} on {}: {ended}, {} gas of {} allowed, market moved: {}",
            self.signature,
            self.block.number.0,
            self.state_source,
            self.gas_used,
            self.gas_limit,
            self.market_moved(),
        )
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// Execute [`ExecutorRun::call`] against `provider` at one pinned header.
///
/// The provider arrives bare; §58's setup is layered here, after the request has been
/// read, so no caller can hand an overridden source to a run that skipped
/// [`ExecutorRun::setup`].
pub async fn run(provider: Arc<dyn StateProvider>, run: &ExecutorRun) -> Result<ExecutorOutcome> {
    let provider = provider.with_setup(run.setup());
    let watch = run.watch()?;
    let legs = &watch.legs;
    let input_token = watch.input_token;
    let recipient = watch.recipient;
    let calldata = run.call.encode();
    let selector = hex::encode(run.call.selector());

    // ---- 1. pins and permissions --------------------------------------------
    provider.note_read_phase("header at pin");
    let header = provider.header().await.map_err(state_error)?;
    if header.chain_id != run.chain_id {
        return Err(SimulationError::ChainMismatch {
            expected: run.chain_id,
            found: header.chain_id,
        });
    }
    let loaded = BlockPin::new(header.number, header.hash);
    let state_source = provider.source();
    if run.state_source != state_source {
        return Err(SimulationError::StateMismatch {
            priced: run.priced_at,
            pinned: loaded,
            reason: format!(
                "the plan was built against state source {:?}, but the provider it is being run \
                 on reports {state_source:?}",
                run.state_source
            ),
        });
    }
    let abstained = matches!(run.pricing, GasPricing::Unresolved { .. });
    let price = fee_ceiling(&run.pricing, &header)?;
    if let Some(cap) = run.rules.gas_limit_cap() {
        if run.gas_limit > cap {
            return Err(unsupported(format!(
                "{} caps one transaction's gas limit at {cap}; this run allows {}",
                run.rules.model(),
                run.gas_limit
            )));
        }
    }

    // ---- 2. the contracts have to be there ----------------------------------
    // §60: an empty `eth_getCode` is a refusal, not a bytecode to be filled in. This
    // matters more here than in the plan path: an executor with no code would make the
    // whole call a plain value transfer that succeeds.
    let contracts = run.contracts()?;
    provider.note_read_phase("codes: m10_route");
    let codes = provider.codes(&contracts).await.map_err(state_error)?;
    for (address, code) in contracts.into_iter().zip(codes) {
        if code.is_empty() {
            return Err(SimulationError::MissingCode {
                address,
                block: header.number,
            });
        }
    }
    // The operator is an account that signs, not a contract — the same reason §58 gives
    // for the plan path's test sender.
    provider.note_read_phase("code: operator");
    let operator_code = provider.code(run.operator).await.map_err(state_error)?;
    if !operator_code.is_empty() {
        return Err(unsupported(format!(
            "the operator {} has bytecode at block {}; an executor call from a contract would \
             make the access control a question about that contract",
            run.operator, header.number.0
        )));
    }
    provider.note_read_phase("account: operator");
    let operator_nonce = provider
        .account(run.operator)
        .await
        .map_err(state_error)?
        .map_or(0, |account| account.nonce);

    // ---- 3. ask the contracts what they hold --------------------------------
    // Every before-value below is a call executed against the pinned state, so a fixture
    // and a node answer the same question the same way (§25), and §27's equality is
    // between two executed answers rather than between a number and a layout guess.
    let mut before_views = Views {
        evm: crate::engine::build_evm(
            &provider,
            &header,
            run.rules,
            run.operator,
            run.gas_limit,
            crate::engine::FeePosture::view(),
        )?,
        sender: run.operator,
    };
    let mut reserves = Vec::with_capacity(legs.len());
    for leg in legs.iter() {
        if !reserves.iter().any(|row: &ReserveRow| row.pool == leg.pool) {
            provider.note_read_phase(&interpreter_phase(
                VIEWS_PHASE_PREFIX,
                V2Call::Token0.signature(),
            ));
            let token0 = before_views.address(leg.pool, &V2Call::Token0).await?;
            provider.note_read_phase(&interpreter_phase(
                VIEWS_PHASE_PREFIX,
                V2Call::Token1.signature(),
            ));
            let token1 = before_views.address(leg.pool, &V2Call::Token1).await?;
            provider.note_read_phase(&interpreter_phase(
                VIEWS_PHASE_PREFIX,
                V2Call::GetReserves.signature(),
            ));
            let snapshot = snapshot(&mut before_views, leg.pool, token0, token1).await?;
            reserves.push(ReserveRow {
                pool: leg.pool,
                before: snapshot,
                // Filled in after the run; a row is only ever emitted with both sides.
                after: snapshot,
            });
        }
    }
    let mut tokens: Vec<Address> = Vec::new();
    for leg in legs.iter() {
        for token in [leg.token_in, leg.token_out] {
            if !tokens.contains(&token) {
                tokens.push(token);
            }
        }
    }
    if !tokens.contains(&input_token) {
        tokens.push(input_token);
    }
    let holders = run.holders()?;
    let mut balances: Vec<BalanceRow> = Vec::with_capacity(tokens.len() * holders.len());
    for token in tokens.iter() {
        for holder in holders.iter() {
            provider.note_read_phase(&interpreter_phase(
                VIEWS_PHASE_PREFIX,
                V2Call::BalanceOf { owner: *holder }.signature(),
            ));
            let value = balance_of(&mut before_views, *token, *holder).await?;
            balances.push(BalanceRow {
                token: *token,
                holder: *holder,
                before: value,
                after: value,
            });
        }
    }

    // ---- 4. one transaction, as a real one would be shaped -------------------
    let mut evm = crate::engine::build_evm(
        &provider,
        &header,
        run.rules,
        run.operator,
        run.gas_limit,
        crate::engine::FeePosture::transaction(price, abstained),
    )?;
    let mut tx = tx_env(run.operator, run.gas_limit, price)?;
    tx.nonce = operator_nonce;
    tx.kind = TxKind::Call(run.executor);
    tx.value = U256::ZERO;
    tx.data = calldata.clone();
    provider.note_read_phase(&interpreter_phase(
        EXECUTE_PHASE_PREFIX,
        &format!("executor {}", run.call.signature()),
    ));
    let result = evm.transact_one_async(tx).await.map_err(fiber_failure)?;
    let (gas_used, status, logs, output) = classify(&result);
    let mut touched = Touched::default();
    touched.absorb(evm.ctx.journal().evm_state());

    // ---- 5. diff the state, then read the market back -----------------------
    // In this order, deliberately: the word-level diff describes the transaction, and
    // the reads below are further transactions of their own. Asking them first would
    // put their gas and their nonce into the same diff and report a fee as a route.
    let state_changes = state_changes(&touched, &provider).await?;
    // Always one higher, whether the call settled or reverted: an included transaction
    // spends its nonce, and the journal keeps that spend through a revert — the same fact
    // a real chain records. The reads below borrow the *next* nonce, never the spent one.
    let mut nonce = operator_nonce.saturating_add(1);
    for row in reserves.iter_mut() {
        provider.note_read_phase(&interpreter_phase(
            VIEWS_PHASE_PREFIX,
            V2Call::GetReserves.signature(),
        ));
        row.after = read_snapshot(
            &mut evm,
            run.operator,
            &mut nonce,
            run.gas_limit,
            price,
            row.pool,
            row.before.token0,
            row.before.token1,
        )
        .await?;
    }
    for row in balances.iter_mut() {
        provider.note_read_phase(&interpreter_phase(
            VIEWS_PHASE_PREFIX,
            V2Call::BalanceOf { owner: row.holder }.signature(),
        ));
        row.after = read_amount(
            &mut evm,
            run.operator,
            &mut nonce,
            run.gas_limit,
            price,
            row.token,
            V2Call::BalanceOf { owner: row.holder },
        )
        .await?;
    }

    // ---- 6. price the one transaction ---------------------------------------
    let mut budget = GasBudget::new(run.gas_limit, 1);
    budget.record(gas_used);
    let charge = budget.charged(&run.pricing, &header)?;

    let delivered = match (&status, output.as_ref()) {
        (StepStatus::Success, Some(bytes)) if watch.void_return => {
            if !bytes.is_empty() {
                return Err(unsupported(format!(
                    "{} is declared to answer with nothing, but the call returned {} bytes",
                    run.call.signature(),
                    bytes.len()
                )));
            }
            None
        }
        (StepStatus::Success, Some(bytes)) => {
            match run.call.decode_return(bytes).map_err(protocol_error)? {
                CallReturn::Amount(amount) => Some(amount),
                other => {
                    return Err(unsupported(format!(
                        "{} answered {other:?}, which is not the amount this call returns",
                        run.call.signature()
                    )))
                }
            }
        }
        _ => None,
    };
    let (contract_error, revert_kind) = match &status {
        StepStatus::Reverted(data) => {
            let payload = decode_revert(&data.raw);
            (
                payload.executor().map(|error| error.name().to_string()),
                Some(payload.kind()),
            )
        }
        _ => (None, None),
    };

    Ok(ExecutorOutcome {
        chain_id: run.chain_id,
        block: loaded,
        state_source,
        executor: run.executor,
        operator: run.operator,
        recipient,
        input_token,
        amount_in: watch.amount_in,
        min_final_amount: watch.min_final_amount,
        signature: run.call.signature(),
        selector,
        calldata_len: calldata.len(),
        calldata,
        gas_limit: run.gas_limit,
        gas_used,
        charge,
        status,
        contract_error,
        revert_kind,
        delivered,
        logs,
        reserves,
        balances,
        state_changes,
    })
}

// ---------------------------------------------------------------------------
// The reads, before and after
// ---------------------------------------------------------------------------

/// REVM's answer about the one transaction, in this crate's vocabulary.
///
/// A revert and an out-of-gas are results, not errors — the same rule §9 of the M4 task
/// sets, and the reason [`StepStatus`] is reused rather than a bool.
fn classify(
    result: &ExecutionResult<HaltReason>,
) -> (u64, StepStatus, Vec<ExecutedLog>, Option<Bytes>) {
    let gas_used = result.tx_gas_used();
    let logs = executed_logs(result.logs());
    match result {
        ExecutionResult::Success { output, .. } => match output {
            Output::Call(bytes) => (gas_used, StepStatus::Success, logs, Some(bytes.clone())),
            Output::Create(_, _) => (
                gas_used,
                StepStatus::Halted("the call created a contract".to_string()),
                logs,
                None,
            ),
        },
        ExecutionResult::Revert { output, .. } => (
            gas_used,
            StepStatus::Reverted(RevertData::new(output.clone())),
            logs,
            None,
        ),
        ExecutionResult::Halt { reason, .. } => {
            (gas_used, StepStatus::Halted(reason.to_string()), logs, None)
        }
    }
}

/// One read-only call through a view EVM, decoded as the amount the call answers with.
async fn balance_of(views: &mut Views, token: Address, holder: Address) -> Result<U256> {
    let call = V2Call::BalanceOf { owner: holder };
    let raw = views.call(token, call.encode()).await?;
    amount(&call, &raw)
}

/// A pool's own answer, with the pool's own side ordering attached to it.
async fn snapshot(
    views: &mut Views,
    pool: Address,
    token0: Address,
    token1: Address,
) -> Result<ReserveSnapshot> {
    let reserves = views.reserves(pool).await?;
    Ok(ReserveSnapshot {
        token0,
        token1,
        reserve0: reserves.reserve0,
        reserve1: reserves.reserve1,
        block_timestamp_last: reserves.block_timestamp_last,
    })
}

/// The same read as [`balance_of`], run as a transaction on the EVM the call just
/// executed, so the answer comes from the state the executor left rather than from the
/// pin. Each of these is its own transaction with its own nonce because the journal has
/// already spent one; they cost the operator gas, which is why the diff above is taken
/// before they run.
async fn read_amount(
    evm: &mut SimEvm,
    caller: Address,
    nonce: &mut u64,
    gas_limit: u64,
    price: u128,
    to: Address,
    call: V2Call,
) -> Result<U256> {
    let raw = read_call(evm, caller, nonce, gas_limit, price, to, call.encode()).await?;
    amount(&call, &raw)
}

/// [`snapshot`] on the run's own EVM.
#[allow(clippy::too_many_arguments)]
async fn read_snapshot(
    evm: &mut SimEvm,
    caller: Address,
    nonce: &mut u64,
    gas_limit: u64,
    price: u128,
    pool: Address,
    token0: Address,
    token1: Address,
) -> Result<ReserveSnapshot> {
    let raw = read_call(
        evm,
        caller,
        nonce,
        gas_limit,
        price,
        pool,
        V2Call::GetReserves.encode(),
    )
    .await?;
    let reserves = match V2Call::GetReserves
        .decode_return(&raw)
        .map_err(protocol_error)?
    {
        CallReturn::Reserves(reserves) => reserves,
        other => {
            return Err(unsupported(format!(
                "getReserves() at {pool} answered {other:?}, which is not a reserve triple"
            )))
        }
    };
    Ok(ReserveSnapshot {
        token0,
        token1,
        reserve0: reserves.reserve0,
        reserve1: reserves.reserve1,
        block_timestamp_last: reserves.block_timestamp_last,
    })
}

async fn read_call(
    evm: &mut SimEvm,
    caller: Address,
    nonce: &mut u64,
    gas_limit: u64,
    price: u128,
    to: Address,
    data: Bytes,
) -> Result<Bytes> {
    let mut tx: TxEnv = evm.ctx.tx.clone();
    tx.caller = caller;
    tx.nonce = *nonce;
    *nonce = nonce.saturating_add(1);
    tx.gas_limit = gas_limit;
    tx.gas_price = price;
    tx.kind = TxKind::Call(to);
    tx.data = data;
    let result = evm.transact_one_async(tx).await.map_err(fiber_failure)?;
    match result {
        ExecutionResult::Success {
            output: Output::Call(bytes),
            ..
        } => Ok(bytes),
        other => Err(unsupported(format!(
            "a read-back call to {to} did not answer: {other:?}"
        ))),
    }
}

fn amount(call: &V2Call, raw: &[u8]) -> Result<U256> {
    match call.decode_return(raw).map_err(protocol_error)? {
        CallReturn::Amount(amount) => Ok(amount),
        other => Err(unsupported(format!(
            "{} answered {other:?}, which is not an amount",
            call.signature()
        ))),
    }
}
