//! What a simulation produced: the execution, the numbers, and the reasons a
//! number is absent.
//!
//! §9's requirement is not "return a struct", it is *keep two situations apart*:
//!
//! ```text
//! the simulation failed              -> Err(SimulationError)
//! the simulation ran and the route
//!     did not make money             -> Ok(SimulationResult) with a status
//! ```
//!
//! A run that reverted is a successful simulation of a route that does not execute,
//! so [`ExecutionStatus::Reverted`] lives in this module while
//! [`SimulationError`] lives in `error`. Collapsing them would make a provider
//! outage look like an unprofitable trade, which is the one mistake a risk layer
//! cannot recover from.
//!
//! The profit vocabulary is §4's, kept strictly separate:
//!
//! ```text
//! analytical_output        M3's number, from reserves and fees
//! simulated_output         what the sender actually holds afterwards, measured
//! simulated_gross_profit   simulated_output - input, before gas (§27)
//! gas cost                 execution's own price, in wei (§30)
//! net_profit               only when the denomination is proved (§31/§32)
//! ```
//!
//! Nothing here adjusts M3's arithmetic to match execution or the other way round
//! (§69): [`OutputComparison`] holds both numbers and reports the gap, and the
//! explanation of the gap is the milestone's output, not a correction applied to
//! either side.

use alloy_primitives::{Address, Bytes, B256, U256};
use serde::Serialize;

use evm_core::{ChainId, TokenId};

use crate::gas::GasCharge;
use crate::plan::{Binding, ExecutionPlan, ResolvedStep};
use crate::route::SlippageRecord;
use crate::state::BlockPin;

/// The `Error(string)` selector, and the only revert shape this crate decodes.
///
/// A four-byte constant is a claim about keccak256, so it is pinned against the
/// function that computes it — see [`selector_is_the_hash_of_the_declaration`].
/// The value typed here was wrong (`0x086379a0`, one nibble short of the real
/// `0x08c379a0`) until the deployed pair's own revert bytes on block 37191169 said
/// so, which is the argument for pinning it rather than trusting the typing.
pub const ERROR_STRING_SELECTOR: [u8; 4] = [0x08, 0xc3, 0x79, 0xa0];

/// Revert data, kept raw (§35).
///
/// The raw bytes are the evidence; the text is a convenience that appears only
/// when the payload really is the standard `Error(string)` shape. Anything else —
/// a bare revert, a custom error, an empty payload — is reported as raw and left
/// undecoded rather than guessed at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RevertData {
    pub raw: Bytes,
    pub message: Option<String>,
}

impl RevertData {
    pub fn new(raw: Bytes) -> Self {
        let message = decode_error_string(&raw);
        Self { raw, message }
    }

    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// What to print in a report: the message when there is one, the raw hex when
    /// there is not.
    pub fn reason(&self) -> String {
        match &self.message {
            Some(message) => message.clone(),
            None if self.raw.is_empty() => "reverted with no return data".to_string(),
            None => format!("revert data 0x{}", hex::encode(&self.raw)),
        }
    }
}

/// Decode `Error(string)` if that is exactly what the payload is.
fn decode_error_string(raw: &[u8]) -> Option<String> {
    let body = raw.strip_prefix(&ERROR_STRING_SELECTOR)?;
    // ABI layout: offset word, length word, then the bytes. Anything shorter than
    // two words cannot be that shape, and a length that does not match the
    // remaining bytes is treated as not-a-message rather than as a decode failure.
    if body.len() < 64 {
        return None;
    }
    let offset = u256_from_word(body.get(..32)?)?;
    if offset != U256::from(32u8) {
        return None;
    }
    let length = u256_from_word(body.get(32..64)?)?;
    let length = usize::try_from(length).ok()?;
    let text = body.get(64..)?;
    // A conforming encoder pads the string to a whole word, so the trailing
    // region is at most 31 bytes of padding and never more.
    if text.len() < length || text.len() - length >= 32 {
        return None;
    }
    String::from_utf8(text[..length].to_vec()).ok()
}

fn u256_from_word(bytes: &[u8]) -> Option<U256> {
    let slice: &[u8; 32] = bytes.try_into().ok()?;
    Some(U256::from_be_bytes(*slice))
}

/// How one step ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum StepStatus {
    Success,
    Reverted(RevertData),
    OutOfGas,
    /// The EVM stopped the step for a reason that is neither: an invalid opcode, a
    /// state change inside a static call, a call-stack limit. It is its own variant
    /// because calling it `OutOfGas` would report a step that spent its whole
    /// allowance as if it had, and the two say different things about the contract.
    Halted(String),
}

impl StepStatus {
    pub const fn succeeded(&self) -> bool {
        matches!(self, Self::Success)
    }

    /// Did this step stop the sequence? Any variant but `Success` did.
    pub const fn stopped(&self) -> bool {
        !matches!(self, Self::Success)
    }
}

/// A log an executed step emitted, with the contract that emitted it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExecutedLog {
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Bytes,
}

/// One step as it actually ran.
///
/// This is where §8's `to` / `value` / `calldata` end up: they are facts about an
/// execution, so they are recorded by the execution rather than asserted by the
/// request that asked for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExecutedStep {
    pub index: usize,
    pub nonce: u64,
    pub from: Address,
    pub to: Address,
    pub value: U256,
    pub signature: String,
    pub selector: String,
    pub calldata: Bytes,
    pub gas_limit: u64,
    pub gas_used: u64,
    pub status: StepStatus,
    pub logs: Vec<ExecutedLog>,
    /// The binding this step filled, and the value the EVM reported for it. A
    /// measurement is stored next to the step that produced it so a reader can see
    /// which call answered which question.
    pub measured: Option<MeasuredValue>,
}

/// A balance the EVM was asked about and answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct MeasuredValue {
    pub binding: &'static str,
    pub value: U256,
}

impl ExecutedStep {
    /// Start the record of a step the engine is about to run.
    ///
    /// The gas limit is a parameter rather than a default: §28 makes the
    /// difference between `gas_limit` and `gas_used` the point of the field, so a
    /// constructed record begins with the limit it was given and no measurement,
    /// and the engine writes the measurement when the EVM reports one.
    pub fn begun(step: &ResolvedStep, gas_limit: u64) -> Self {
        Self {
            index: step.index,
            nonce: step.nonce,
            from: step.from,
            to: step.to,
            value: step.value,
            signature: step.call.signature().to_string(),
            selector: step.selector_hex(),
            calldata: step.calldata(),
            gas_limit,
            gas_used: 0,
            status: StepStatus::Success,
            logs: Vec::new(),
            measured: None,
        }
    }

    pub const fn succeeded(&self) -> bool {
        matches!(self.status, StepStatus::Success)
    }

    /// The record of a step that read the EVM's own state rather than calling a
    /// contract.
    ///
    /// A native balance is not something a transaction asks for, so there is no
    /// target, no selector and no calldata to report, and the step consumes neither
    /// gas nor nonce — `nonce` is the one in effect when the read happened, which is
    /// the number the *next* transaction will use. The fields that would carry a
    /// call are empty rather than invented, so a reader cannot mistake this record
    /// for a step that executed bytecode.
    pub fn native_read(
        index: usize,
        account: Address,
        nonce: u64,
        binding: Binding,
        value: U256,
    ) -> Self {
        Self {
            index,
            nonce,
            from: account,
            to: Address::ZERO,
            value: U256::ZERO,
            signature: format!("native balance of {account} (no call)"),
            selector: String::new(),
            calldata: Bytes::new(),
            gas_limit: 0,
            gas_used: 0,
            status: StepStatus::Success,
            logs: Vec::new(),
            measured: Some(MeasuredValue {
                binding: binding.name(),
                value,
            }),
        }
    }

    pub fn revert(&self) -> Option<&RevertData> {
        match &self.status {
            StepStatus::Reverted(data) => Some(data),
            _ => None,
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "step {} nonce {} to {} {} value {} gas {}/{} {:?}",
            self.index,
            self.nonce,
            self.to,
            self.signature,
            self.value,
            self.gas_used,
            self.gas_limit,
            self.status
        )
    }
}

/// An account the run left different from how it found it (§36).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct AccountChange {
    pub address: Address,
    pub balance_before: U256,
    pub balance_after: U256,
    pub nonce_before: u64,
    pub nonce_after: u64,
}

impl AccountChange {
    /// The signed movement, as a direction and a magnitude rather than a wrapping
    /// subtraction — the same convention the opportunity layer uses because `U256`
    /// has no negative.
    pub fn balance_movement(&self) -> Movement {
        Movement::between(self.balance_before, self.balance_after)
    }
}

/// A storage word the run left different (§36).
///
/// The meaning of a slot is deliberately not decoded here. Which word holds a
/// pair's reserves is a claim about one contract's layout, and a simulation that
/// asserts it would be substituting a guess for the execution it just performed.
/// The pool's own answer to that question is a `getReserves()` call, and the
/// balances are measured by the tokens' own `balanceOf` — both appear in
/// [`SimulationResult::measurements`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SlotChange {
    pub address: Address,
    pub slot: U256,
    pub before: U256,
    pub after: U256,
}

/// A change that went up, down, or nowhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Movement {
    Unchanged,
    Increased { by: U256 },
    Decreased { by: U256 },
}

impl Movement {
    pub fn between(before: U256, after: U256) -> Self {
        if after > before {
            Self::Increased { by: after - before }
        } else if before > after {
            Self::Decreased { by: before - after }
        } else {
            Self::Unchanged
        }
    }

    /// The magnitude either way, for reporting.
    pub fn magnitude(&self) -> U256 {
        match self {
            Self::Unchanged => U256::ZERO,
            Self::Increased { by } | Self::Decreased { by } => *by,
        }
    }
}

/// Everything the run changed, in address order so two runs of the same plan
/// produce the same list (§37).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StateChanges {
    pub accounts: Vec<AccountChange>,
    pub slots: Vec<SlotChange>,
}

impl StateChanges {
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty() && self.slots.is_empty()
    }

    pub fn account(&self, address: Address) -> Option<&AccountChange> {
        self.accounts
            .iter()
            .find(|change| change.address == address)
    }

    /// Every storage word a contract changed. For a pool this is the reserve audit
    /// §36 asks for, stated as facts about words rather than as an interpretation
    /// of them.
    pub fn storage_of(&self, address: Address) -> Vec<&SlotChange> {
        self.slots
            .iter()
            .filter(|change| change.address == address)
            .collect()
    }

    pub fn touched(&self) -> Vec<Address> {
        let mut seen: Vec<Address> = self.accounts.iter().map(|a| a.address).collect();
        for change in &self.slots {
            if !seen.contains(&change.address) {
                seen.push(change.address);
            }
        }
        seen
    }
}

/// M3's number against execution's, with the gap reported rather than reconciled
/// (§26, §38, §69).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct OutputComparison {
    pub analytical: U256,
    pub simulated: Option<U256>,
}

impl OutputComparison {
    pub fn behind(&self) -> Option<U256> {
        let simulated = self.simulated?;
        (simulated < self.analytical).then_some(self.analytical - simulated)
    }

    pub fn ahead(&self) -> Option<U256> {
        let simulated = self.simulated?;
        (simulated > self.analytical).then_some(simulated - self.analytical)
    }

    pub fn is_exact(&self) -> bool {
        self.simulated == Some(self.analytical)
    }

    /// §26's three numbers in one line, including the case where the second does
    /// not exist because the run never produced an output.
    pub fn delta_text(&self) -> String {
        match self.simulated {
            None => format!("analytical {}, simulated none", self.analytical),
            Some(simulated) => match Movement::between(self.analytical, simulated) {
                Movement::Unchanged => format!(
                    "analytical {0}, simulated {0}, delta 0 (exact match)",
                    self.analytical
                ),
                Movement::Increased { by } => {
                    format!(
                        "analytical {}, simulated {simulated}, delta +{by}",
                        self.analytical
                    )
                }
                Movement::Decreased { by } => {
                    format!(
                        "analytical {}, simulated {simulated}, delta -{by}",
                        self.analytical
                    )
                }
            },
        }
    }
}

/// What the run ended with, in the input token's own unit.
///
/// `simulated_gross_profit` is §27's definition — `simulated_output - input`, before
/// gas — and it follows the opportunity layer's convention that a profit is only
/// reported when it is strictly positive. A route that comes back exactly even is
/// [`Movement::Unchanged`], not `Some(0)`, so a caller cannot mistake a break-even
/// scan for a finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum SimulatedOutcome {
    /// The sequence executed and the sender finished holding `output`.
    Executed { output: U256, movement: Movement },
    /// The sequence did not complete, so there is no output to report. The reason
    /// is in [`SimulationResult::status`].
    NotExecuted,
}

impl SimulatedOutcome {
    /// §27's `simulated_gross_profit`: strictly positive, before gas.
    pub fn gross_profit(&self, input: U256) -> Option<U256> {
        match self {
            Self::Executed { output, .. } => output.checked_sub(input).filter(|p| !p.is_zero()),
            Self::NotExecuted => None,
        }
    }

    /// The mirror image: strictly positive, before gas.
    pub fn gross_loss(&self, input: U256) -> Option<U256> {
        match self {
            Self::Executed { output, .. } => input.checked_sub(*output).filter(|l| !l.is_zero()),
            Self::NotExecuted => None,
        }
    }

    pub fn output(&self) -> Option<U256> {
        match self {
            Self::Executed { output, .. } => Some(*output),
            Self::NotExecuted => None,
        }
    }
}

/// What a completed run did to the sender's holding of the input token, before gas.
///
/// §9 insists that a sequence which executed and came back short is a different
/// answer from one that never executed, and §31 makes both of those different from a
/// run whose net figure has no unit. Reporting the first as "no gross profit to net
/// off" would erase the distinction — and it is the shape M4's real route turns out
/// to have, so this type exists to keep the three cases apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrossMovement {
    /// The sender finished holding more of the input token than it spent.
    Gained(U256),
    /// Exactly as much.
    Even,
    /// Less, by this much.
    Lost(U256),
    /// The sequence stopped before the last step, so there is no figure.
    NotExecuted,
}

impl GrossMovement {
    pub fn of(outcome: &SimulatedOutcome, input: U256) -> Self {
        let SimulatedOutcome::Executed { output, .. } = outcome else {
            return Self::NotExecuted;
        };
        match output.cmp(&input) {
            std::cmp::Ordering::Greater => Self::Gained(*output - input),
            std::cmp::Ordering::Equal => Self::Even,
            std::cmp::Ordering::Less => Self::Lost(input - *output),
        }
    }
}

/// The unit a profit is stated in, and the execution that proved it.
///
/// §32 allows the wrapped/native conversion to be used as a denomination only when
/// the chain's own semantics prove the two are 1:1 *and* the execution path
/// determines it. That is what an executed `withdraw` does: it takes `wad` wrapped
/// tokens off the sender and the same number of wei of native arrives — if the
/// contract is the canonical one, and no tax, fee or rebase got in the way. The
/// numbers below are that statement read across the whole run, and
/// [`Denomination::is_proved`] is the check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Denomination {
    /// The token the profit arrived in, which was converted.
    pub token: TokenId,
    /// The exact-out amount the wrapped contract was asked to release.
    pub converted: U256,
    /// What the sender held in native at the run's first measurement, which under
    /// §58 includes the native the run was funded with.
    pub native_start: U256,
    /// What it holds at the last one, after the gas bill and the conversion.
    pub native_end: U256,
    /// The native the run itself put *into* the wrapped contract — the `msg.value`
    /// of a `deposit()` funding step, and zero for a plan that started from a token
    /// balance. Part of the sender's native ledger, so part of the equality.
    pub native_spent: U256,
    /// Gas the whole sequence consumed, in wei. Native is the gas currency on this
    /// chain, so the run's native receipt is net of it and the equality has to say
    /// so to be checkable.
    pub gas_paid: U256,
    /// Which steps produced these numbers.
    pub proved_by: String,
}

impl Denomination {
    /// `converted + native_start == native_end + native_spent + gas_paid`, exactly.
    ///
    /// Stated as a two-ended ledger rather than as "the native the conversion
    /// produced" because the honest version of that number can be negative: a run
    /// whose gas bill outweighs what it unwrapped ends with *less* native than it
    /// started with, and a unsigned difference would either underflow or have to be
    /// reported as unknown when what the chain actually said was "this lost money".
    ///
    /// Checked additions on both sides: a sum that overflows is a sign these numbers
    /// did not come from one run, and a saturating add could turn that into a false
    /// equality.
    pub fn is_proved(&self) -> bool {
        let released = self.converted.checked_add(self.native_start);
        let retained = self
            .native_end
            .checked_add(self.native_spent)
            .and_then(|running| running.checked_add(self.gas_paid));
        released == retained
    }

    /// The rate this proves, stated only because the equality above holds.
    pub fn wei_of_profit_per_wei_of_token(&self) -> Option<U256> {
        self.is_proved().then_some(U256::from(1u8))
    }
}

/// §31's number, or the reason it does not exist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum NetProfit {
    /// Profit after the gas bill, in the unit gas is paid in.
    Gain {
        amount: U256,
        gross: U256,
        gas_cost: U256,
        denomination: Denomination,
    },
    /// Gross profit exactly equal to the gas bill.
    BreakEven {
        gas_cost: U256,
        denomination: Denomination,
    },
    /// The run gained tokens but the gas bill was larger.
    Loss {
        amount: U256,
        gross: U256,
        gas_cost: U256,
        denomination: Denomination,
    },
    /// The run came back with no more of the input token than it spent, so there
    /// was never a gross profit for gas to eat into — the whole bill is a loss on
    /// top of a route that was already short. This is the shape M4's taxed route
    /// turns out to have, and §9's "executed and unprofitable" answer is a number
    /// here rather than a report of a missing fact.
    Shortfall {
        /// How much the sender ended up under water before gas. Zero when the run
        /// came back exactly even.
        before_gas: U256,
        gas_cost: U256,
        /// `before_gas + gas_cost`.
        amount: U256,
        denomination: Denomination,
    },
    /// No net number exists, and the reason is the milestone's finding rather than
    /// a placeholder: an unpriced gas bill, an unproved denomination, or a route
    /// that never completed.
    NotComputable { reason: String },
}

/// The answer for a sequence that stopped before its last step: §9's "simulation
/// failed" branch, whose finding is in [`SimulationResult::status`], not here.
fn never_completed() -> NetProfit {
    NetProfit::NotComputable {
        reason: "the sequence never completed, so the sender holds no measured output to \
                 compare against what it spent: there is no pre-gas movement to net off the \
                 gas bill, and the reason the run stopped is reported as the status"
            .to_string(),
    }
}

impl NetProfit {
    /// Combine the profit's unit, the pre-gas movement and the gas bill.
    ///
    /// The order of the checks is deliberate: a run that did not complete has no
    /// movement to denominate, and an unpriced gas bill is a different failure of
    /// knowledge than an unproved conversion rate. Each reason names the missing
    /// fact so the next milestone knows what to go get.
    pub fn compute(
        movement: GrossMovement,
        charge: &GasCharge,
        denomination: Option<&Denomination>,
    ) -> Self {
        if matches!(movement, GrossMovement::NotExecuted) {
            return never_completed();
        }
        let Some(gas_cost) = charge.wei() else {
            return Self::NotComputable {
                reason: format!(
                    "gas cost is not computable ({}), so no net number exists",
                    charge.reason()
                ),
            };
        };
        let Some(denomination) = denomination else {
            return Self::NotComputable {
                reason: "the profit is stated in a token that is not the unit gas is paid in, and \
                         nothing in this run proved a rate between them; §32 forbids assuming \
                         1 ETH = 1 WETH"
                    .to_string(),
            };
        };
        if !denomination.is_proved() {
            return Self::NotComputable {
                reason: format!(
                    "the executed conversion did not add up: {} wei of {} released against a \
                     native ledger of {} in and {} out plus {} wrapped in and {} of gas, which \
                     is not one for one",
                    denomination.converted,
                    denomination.token.address,
                    denomination.native_start,
                    denomination.native_end,
                    denomination.native_spent,
                    denomination.gas_paid
                ),
            };
        }
        match movement {
            GrossMovement::Gained(gross) => match gas_cost.cmp(&gross) {
                std::cmp::Ordering::Less => Self::Gain {
                    amount: gross - gas_cost,
                    gross,
                    gas_cost,
                    denomination: denomination.clone(),
                },
                std::cmp::Ordering::Equal => Self::BreakEven {
                    gas_cost,
                    denomination: denomination.clone(),
                },
                std::cmp::Ordering::Greater => Self::Loss {
                    amount: gas_cost - gross,
                    gross,
                    gas_cost,
                    denomination: denomination.clone(),
                },
            },
            GrossMovement::Even => Self::came_back_short(U256::ZERO, gas_cost, denomination),
            GrossMovement::Lost(before_gas) => {
                Self::came_back_short(before_gas, gas_cost, denomination)
            }
            GrossMovement::NotExecuted => never_completed(),
        }
    }

    /// A run that did not end up ahead before gas: the bill lands on top of a hole
    /// that was already there, and the two amounts stay separate so the reader can
    /// tell a bad route from an expensive one.
    fn came_back_short(before_gas: U256, gas_cost: U256, denomination: &Denomination) -> Self {
        let Some(amount) = before_gas.checked_add(gas_cost) else {
            return Self::NotComputable {
                reason: format!(
                    "the run was {before_gas} short before gas and the bill is {gas_cost}, which \
                     do not add up inside a uint256 — the numbers did not come from one execution"
                ),
            };
        };
        if amount.is_zero() {
            return Self::BreakEven {
                gas_cost,
                denomination: denomination.clone(),
            };
        }
        Self::Shortfall {
            before_gas,
            gas_cost,
            amount,
            denomination: denomination.clone(),
        }
    }

    pub fn is_computable(&self) -> bool {
        !matches!(self, Self::NotComputable { .. })
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::NotComputable { reason } => Some(reason),
            _ => None,
        }
    }
}

/// How the sequence as a whole ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum ExecutionStatus {
    /// Every step of the plan ran and succeeded.
    Completed,
    /// A step reverted; the sequence stopped there.
    Reverted {
        step: usize,
        call: String,
        revert: RevertData,
    },
    /// A step spent its whole gas limit without finishing.
    OutOfGas { step: usize, call: String },
    /// A step was stopped by the EVM for a reason that is neither a revert nor gas
    /// exhaustion; the reason is REVM's own words for the halt it reported.
    Halted {
        step: usize,
        call: String,
        reason: String,
    },
}

impl ExecutionStatus {
    pub const fn completed(&self) -> bool {
        matches!(self, Self::Completed)
    }

    pub fn failed_at(&self) -> Option<usize> {
        match self {
            Self::Reverted { step, .. }
            | Self::OutOfGas { step, .. }
            | Self::Halted { step, .. } => Some(*step),
            Self::Completed => None,
        }
    }
}

/// The whole answer to one simulation request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SimulationResult {
    pub chain_id: ChainId,
    /// The pin the state was actually loaded from, verified before execution.
    pub block: BlockPin,
    pub state_source: String,
    pub sender: Address,
    pub status: ExecutionStatus,
    pub steps: Vec<ExecutedStep>,
    pub gas_charge: GasCharge,
    pub measurements: Vec<MeasuredValue>,
    pub compared: OutputComparison,
    pub outcome: SimulatedOutcome,
    pub gross_profit: Option<U256>,
    pub gross_loss: Option<U256>,
    pub net_profit: NetProfit,
    pub state_changes: StateChanges,
    pub slippage: SlippageRecord,
    /// The plan this result came from, kept in the report so calldata evidence and
    /// the finding it was built from travel together (§55, §56).
    pub plan_summary: PlanSummary,
}

/// The parts of a plan a reader needs to interpret a result, without carrying the
/// EVM-facing step list twice.
///
/// [`From<&ExecutionPlan>`] cannot fill [`PlanSummary::evm_rules`] — a plan does not
/// know what ruleset it will be run under — so that field is set by the engine,
/// which is the only thing in the crate that decides it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlanSummary {
    pub steps: Vec<String>,
    /// The EVM ruleset the plan was executed under, named by [`EvmRules`]'s own
    /// vocabulary rather than by a REVM type (§7: the record a report cites must
    /// not be a handle into the executor's internals).
    pub evm_rules: String,
    pub pools: Vec<Address>,
    pub input_token: Address,
    pub input_amount: U256,
    pub analytical_mid_amount: U256,
    pub analytical_output: U256,
    pub priced_by: String,
}

impl From<&ExecutionPlan> for PlanSummary {
    fn from(plan: &ExecutionPlan) -> Self {
        Self {
            steps: plan.describe(),
            evm_rules: String::new(),
            pools: plan.route.pools().iter().map(|pool| pool.address).collect(),
            input_token: plan.route.input_token.address,
            input_amount: plan.route.input_amount,
            analytical_mid_amount: plan.route.analytical_mid_amount,
            analytical_output: plan.route.analytical_output,
            priced_by: plan.route.priced_by.clone(),
        }
    }
}

impl SimulationResult {
    pub fn success(&self) -> bool {
        self.status.completed()
    }

    pub fn gas_used(&self) -> u64 {
        self.gas_charge.gas_used()
    }

    pub fn logs(&self) -> Vec<(usize, &ExecutedLog)> {
        self.steps
            .iter()
            .flat_map(|step| step.logs.iter().map(move |log| (step.index, log)))
            .collect()
    }

    pub fn revert(&self) -> Option<&RevertData> {
        match &self.status {
            ExecutionStatus::Reverted { revert, .. } => Some(revert),
            _ => None,
        }
    }

    /// The value a binding ended with, from the step that produced it.
    pub fn measurement(&self, binding: Binding) -> Option<U256> {
        self.measurements
            .iter()
            .find(|m| m.binding == binding.name())
            .map(|m| m.value)
    }

    /// A stable handle on this exact result, for §37's determinism check. Two runs
    /// that differ in any field, including a single log byte, differ here — and when
    /// they do, the test compares the structs themselves, not this handle.
    pub fn fingerprint(&self) -> B256 {
        let text = serde_json::to_string(self).expect("a result serializes");
        alloy_primitives::keccak256(text.as_bytes())
    }

    pub fn summary(&self) -> String {
        format!(
            "{} | {} | gas {} | {}",
            match &self.status {
                ExecutionStatus::Completed => "completed".to_string(),
                ExecutionStatus::Reverted { step, call, revert } => {
                    format!("reverted at step {step} ({call}): {}", revert.reason())
                }
                ExecutionStatus::OutOfGas { step, call } => {
                    format!("out of gas at step {step} ({call})")
                }
                ExecutionStatus::Halted { step, call, reason } =>
                    format!("halted at step {step} ({call}): {reason}"),
            },
            self.compared.delta_text(),
            self.gas_used(),
            match &self.net_profit {
                NetProfit::Gain { amount, .. } => format!("net gain {amount}"),
                NetProfit::Loss { amount, .. } => format!("net loss {amount}"),
                NetProfit::BreakEven { .. } => "net break-even".to_string(),
                NetProfit::Shortfall {
                    before_gas,
                    gas_cost,
                    amount,
                    ..
                } => format!("net loss {amount} ({before_gas} short before gas + {gas_cost} gas)"),
                NetProfit::NotComputable { reason } => format!("net not computable: {reason}"),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, b256, keccak256};

    use super::*;

    const SENDER: Address = address!("0x00000000000000000000000000000000cafe0001");
    const TOKEN: Address = address!("0x4200000000000000000000000000000000000006");

    fn priced(gas_used: u64, wei: u128) -> GasCharge {
        GasCharge::Priced {
            gas_used,
            effective_gas_price: wei,
            base_fee_per_gas: Some(wei),
            wei: U256::from(wei) * U256::from(gas_used),
            pricing: crate::gas::GasPricing::Legacy {
                gas_price: wei,
                provenance: "declared by a test".to_string(),
            },
        }
    }

    fn unpriced(gas_used: u64) -> GasCharge {
        GasCharge::Unpriced {
            gas_used,
            reason: "no pricing model".to_string(),
        }
    }

    /// A balance-funded run: nothing was wrapped in and the purse started empty, so
    /// the ledger is `converted` released minus `gas` paid.
    fn denomination(converted: u128, native: u128, gas: u128) -> Denomination {
        ledger(converted, 0, native, 0, gas)
    }

    /// The whole native ledger across the run: `converted` released into an account
    /// that started with `native_start`, spent `native_spent` on a wrap, and ended
    /// with `native_end`, while `gas` wei of the bill came out of the same purse.
    fn ledger(
        converted: u128,
        native_start: u128,
        native_end: u128,
        native_spent: u128,
        gas: u128,
    ) -> Denomination {
        Denomination {
            token: TokenId::new(ChainId(91342), TOKEN),
            converted: U256::from(converted),
            native_start: U256::from(native_start),
            native_end: U256::from(native_end),
            native_spent: U256::from(native_spent),
            gas_paid: U256::from(gas),
            proved_by: "steps 0 and 10".to_string(),
        }
    }

    /// §32's proof under a native-funded plan: the sender wrapped 500 in before it
    /// traded, so the ending purse is only 99 heavier than the starting one after
    /// releasing 600 and paying 1 of gas — and an equality that ignored either term
    /// would call a true one-for-one round trip unproved.
    #[test]
    fn a_native_funded_round_trip_proves_its_own_rate() {
        let round_trip = ledger(600, 1_000, 1_099, 500, 1);
        assert!(round_trip.is_proved());
        assert_eq!(
            round_trip.wei_of_profit_per_wei_of_token(),
            Some(U256::from(1u8))
        );
        // Read as though nothing had been wrapped in, the same run is not a proof.
        assert!(!ledger(600, 1_000, 1_099, 0, 1).is_proved());
        assert!(NetProfit::compute(
            GrossMovement::Gained(U256::from(100u32)),
            &priced(1, 1),
            Some(&round_trip)
        )
        .is_computable());

        // A run that ends with *less* native than it started with — its gas bill
        // outweighing what it unwrapped — still adds up. This is why the statement is
        // a ledger and not a gain: the unsigned difference underflows right here, and
        // a loss would have been reported as a missing fact.
        let spent_more = ledger(600, 1_000, 100, 500, 1_000);
        assert!(spent_more.is_proved(), "600 + 1000 = 100 + 500 + 1000");

        // An overflowing side is refused rather than saturating into a false equality.
        let absurd = Denomination {
            converted: U256::MAX,
            native_start: U256::from(1u8),
            native_end: U256::ZERO,
            native_spent: U256::ZERO,
            gas_paid: U256::ZERO,
            ..ledger(0, 0, 0, 0, 0)
        };
        assert!(!absurd.is_proved());
    }

    /// §35: the raw payload is always kept, and only the standard `Error(string)`
    /// shape is turned into text.
    /// The selector is a hash claim, so the hash function is what says it. This test
    /// exists because the constant was typed from memory and was wrong by one nibble
    /// until the deployed pair's own revert bytes disagreed with it — and because a
    /// decoder test that reuses the same constant cannot notice either.
    ///
    /// The payload below is the shape `0x5bef6275…7440` answers with on block
    /// 37191169 when the ask is above what it pays: `Error(string)` of `"K"`.
    #[test]
    fn selector_is_what_keccak_says_the_declaration_is() {
        assert_eq!(
            ERROR_STRING_SELECTOR,
            evm_protocol::signatures::selector_of("Error(string)"),
            "keccak256(\"Error(string)\") starts with these four bytes"
        );

        let mut payload = ERROR_STRING_SELECTOR.to_vec();
        payload.extend_from_slice(&U256::from(32u8).to_be_bytes::<32>());
        payload.extend_from_slice(&U256::from(1u8).to_be_bytes::<32>());
        payload.extend_from_slice(b"K");
        payload.extend_from_slice(&[0u8; 31]);
        assert_eq!(payload.len(), 100, "selector + offset + length + one word");

        let revert = RevertData::new(Bytes::from(payload));
        assert_eq!(revert.message.as_deref(), Some("K"), "{revert:?}");
        assert_eq!(revert.reason(), "K");
    }

    #[test]
    fn revert_data_keeps_the_raw_bytes_and_decodes_only_the_known_shape() {
        let known = RevertData::new(Bytes::from({
            let mut v = Vec::new();
            v.extend_from_slice(&ERROR_STRING_SELECTOR);
            v.extend_from_slice(&U256::from(32u8).to_be_bytes::<32>());
            v.extend_from_slice(&U256::from(3u8).to_be_bytes::<32>());
            v.extend_from_slice(b"UJS");
            v
        }));
        assert_eq!(known.message.as_deref(), Some("UJS"));
        assert_eq!(known.reason(), "UJS");
        assert_eq!(known.raw.len(), 4 + 32 + 32 + 3);

        let custom = RevertData::new(Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]));
        assert_eq!(custom.message, None);
        assert_eq!(custom.reason(), "revert data 0xdeadbeef");
        assert_eq!(custom.raw, Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]));

        let bare = RevertData::new(Bytes::new());
        assert!(bare.is_empty());
        assert_eq!(bare.reason(), "reverted with no return data");
    }

    /// A `Sync`-style log's `data` field is not a revert message, so a payload that
    /// starts with the right selector but lies about its length must not decode.
    #[test]
    fn a_payload_that_lies_about_its_length_is_not_a_message() {
        let truncated = RevertData::new(Bytes::from({
            let mut v = Vec::new();
            v.extend_from_slice(&ERROR_STRING_SELECTOR);
            v.extend_from_slice(&U256::from(32u8).to_be_bytes::<32>());
            v.extend_from_slice(&U256::from(64u8).to_be_bytes::<32>());
            v.extend_from_slice(b"short");
            v
        }));
        assert_eq!(truncated.message, None);

        let offset_is_a_lie = RevertData::new(Bytes::from({
            let mut v = Vec::new();
            v.extend_from_slice(&ERROR_STRING_SELECTOR);
            v.extend_from_slice(&U256::from(64u8).to_be_bytes::<32>());
            v.extend_from_slice(&U256::from(3u8).to_be_bytes::<32>());
            v.extend_from_slice(b"UJS");
            v
        }));
        assert_eq!(offset_is_a_lie.message, None);

        // A non-UTF8 body is raw data, not a message.
        let binary = RevertData::new(Bytes::from({
            let mut v = Vec::new();
            v.extend_from_slice(&ERROR_STRING_SELECTOR);
            v.extend_from_slice(&U256::from(32u8).to_be_bytes::<32>());
            v.extend_from_slice(&U256::from(2u8).to_be_bytes::<32>());
            v.extend_from_slice(&[0xff, 0xfe]);
            v
        }));
        assert_eq!(binary.message, None);
    }

    #[test]
    fn movements_do_not_wrap_and_deltas_read_as_signs() {
        assert_eq!(
            Movement::between(U256::from(10u8), U256::from(7u8)),
            Movement::Decreased {
                by: U256::from(3u8)
            }
        );
        assert_eq!(
            Movement::between(U256::from(7u8), U256::from(10u8)),
            Movement::Increased {
                by: U256::from(3u8)
            }
        );
        assert_eq!(
            Movement::between(U256::from(7u8), U256::from(7u8)),
            Movement::Unchanged
        );
        assert_eq!(Movement::Unchanged.magnitude(), U256::ZERO);

        let compared = OutputComparison {
            analytical: U256::from(1_000u32),
            simulated: Some(U256::from(900u32)),
        };
        assert_eq!(compared.behind(), Some(U256::from(100u32)));
        assert_eq!(compared.ahead(), None);
        assert!(!compared.is_exact());
        assert_eq!(
            compared.delta_text(),
            "analytical 1000, simulated 900, delta -100"
        );

        let exact = OutputComparison {
            analytical: U256::from(1_000u32),
            simulated: Some(U256::from(1_000u32)),
        };
        assert!(exact.is_exact());
        assert!(
            exact.delta_text().contains("exact match"),
            "{}",
            exact.delta_text()
        );

        let absent = OutputComparison {
            analytical: U256::from(1_000u32),
            simulated: None,
        };
        assert_eq!(absent.behind(), None);
        assert_eq!(absent.ahead(), None);
        assert_eq!(absent.delta_text(), "analytical 1000, simulated none");
    }

    /// §27's gross profit keeps M3's convention exactly: strictly positive or
    /// `None`, with a break-even run distinguishable from a finding.
    #[test]
    fn gross_profit_is_strict_and_loss_is_its_mirror() {
        let input = U256::from(1_000u32);
        let gained = SimulatedOutcome::Executed {
            output: U256::from(1_100u32),
            movement: Movement::Increased {
                by: U256::from(1_100u32),
            },
        };
        assert_eq!(gained.gross_profit(input), Some(U256::from(100u32)));
        assert_eq!(gained.gross_loss(input), None);
        assert_eq!(gained.output(), Some(U256::from(1_100u32)));

        let lost = SimulatedOutcome::Executed {
            output: U256::from(900u32),
            movement: Movement::Decreased {
                by: U256::from(900u32),
            },
        };
        assert_eq!(lost.gross_profit(input), None);
        assert_eq!(lost.gross_loss(input), Some(U256::from(100u32)));

        let even = SimulatedOutcome::Executed {
            output: input,
            movement: Movement::Unchanged,
        };
        assert_eq!(even.gross_profit(input), None);
        assert_eq!(even.gross_loss(input), None);

        let nothing = SimulatedOutcome::NotExecuted;
        assert_eq!(nothing.gross_profit(input), None);
        assert_eq!(nothing.output(), None);
    }

    /// §31/§32: a net number exists only with a priced gas bill *and* a proved
    /// denomination, and each missing piece is named as its own reason.
    #[test]
    fn net_profit_requires_a_price_and_a_proved_unit() {
        let gained = GrossMovement::Gained(U256::from(500u32));
        let proved = denomination(500, 400, 100);
        assert!(proved.is_proved());
        assert_eq!(
            proved.wei_of_profit_per_wei_of_token(),
            Some(U256::from(1u8))
        );

        let gain = NetProfit::compute(gained, &priced(10, 20), Some(&proved));
        assert_eq!(
            gain,
            NetProfit::Gain {
                amount: U256::from(300u32),
                gross: U256::from(500u32),
                gas_cost: U256::from(200u32),
                denomination: proved.clone(),
            }
        );
        assert!(gain.is_computable());

        let even = NetProfit::compute(gained, &priced(10, 50), Some(&proved));
        assert!(matches!(even, NetProfit::BreakEven { .. }), "{even:?}");

        let loss = NetProfit::compute(gained, &priced(10, 100), Some(&proved));
        assert_eq!(
            loss,
            NetProfit::Loss {
                amount: U256::from(500u32),
                gross: U256::from(500u32),
                gas_cost: U256::from(1_000u32),
                denomination: proved.clone(),
            }
        );

        let no_price = NetProfit::compute(gained, &unpriced(90_000), Some(&proved));
        assert!(no_price.reason().expect("reason").contains("gas cost"));

        let no_unit = NetProfit::compute(gained, &priced(10, 20), None);
        assert!(no_unit.reason().expect("reason").contains("1 ETH = 1 WETH"));

        let not_executed =
            NetProfit::compute(GrossMovement::NotExecuted, &priced(10, 20), Some(&proved));
        assert!(not_executed
            .reason()
            .expect("reason")
            .contains("never completed"));
    }

    /// §9/§31: "ran and came back short" is a *number*, not a missing fact — and it
    /// is a different answer from "did not run". Pre-gas the sender was 100 under
    /// water, the bill is 100, so the run lost 200 and both parts stay visible.
    #[test]
    fn a_run_that_came_back_short_reports_its_loss_in_two_parts() {
        // The route spent 600 wrapped in and came back holding 500, on a purse that
        // started at 1_000 and ended at 800 after paying 100 of gas. The ledger adds
        // up (500 + 1_000 = 800 + 600 + 100), so the rate is proved even though the
        // route as a whole lost — and the purse's own drop is the number `Shortfall`
        // reports, which is the cross-check that both parts were measured on one run.
        let proved = ledger(500, 1_000, 800, 600, 100);
        assert!(proved.is_proved());
        assert_eq!(
            U256::from(1_000u32) - proved.native_end,
            U256::from(200u32),
            "the purse lost exactly what the net figure says"
        );

        let short = NetProfit::compute(
            GrossMovement::Lost(U256::from(100u32)),
            &priced(10, 10),
            Some(&proved),
        );
        assert_eq!(
            short,
            NetProfit::Shortfall {
                before_gas: U256::from(100u32),
                gas_cost: U256::from(100u32),
                amount: U256::from(200u32),
                denomination: proved.clone(),
            }
        );
        assert!(
            short.is_computable(),
            "a loss is a computed answer: {short:?}"
        );
        assert!(short.reason().is_none());

        // Exactly even before gas: the whole bill is the loss, and `before_gas` says
        // the route itself broke even.
        let free_gas = NetProfit::compute(GrossMovement::Even, &priced(10, 10), Some(&proved));
        assert_eq!(
            free_gas,
            NetProfit::Shortfall {
                before_gas: U256::ZERO,
                gas_cost: U256::from(100u32),
                amount: U256::from(100u32),
                denomination: proved.clone(),
            }
        );

        // No movement and no bill is the one case that is genuinely break-even.
        assert!(matches!(
            NetProfit::compute(GrossMovement::Even, &priced(0, 1), Some(&proved)),
            NetProfit::BreakEven { .. }
        ));

        // And the same run with the gas bill unknown is still `NotComputable` — the
        // shortcoming is a computed figure only when both inputs exist.
        let no_bill = NetProfit::compute(
            GrossMovement::Lost(U256::from(100u32)),
            &unpriced(90_000),
            Some(&proved),
        );
        assert!(no_bill.reason().expect("reason").contains("gas cost"));
    }

    /// `GrossMovement::of` is what keeps the three §9 answers apart, so it is the
    /// place the "ran and lost" / "did not run" split has to be pinned down.
    #[test]
    fn gross_movement_separates_a_short_run_from_an_unrun_one() {
        let input = U256::from(600u32);
        let executed = |output: u32| SimulatedOutcome::Executed {
            output: U256::from(output),
            movement: Movement::between(input, U256::from(output)),
        };

        assert_eq!(
            GrossMovement::of(&executed(700), input),
            GrossMovement::Gained(U256::from(100u32))
        );
        assert_eq!(
            GrossMovement::of(&executed(600), input),
            GrossMovement::Even
        );
        assert_eq!(
            GrossMovement::of(&executed(500), input),
            GrossMovement::Lost(U256::from(100u32))
        );
        assert_eq!(
            GrossMovement::of(&SimulatedOutcome::NotExecuted, input),
            GrossMovement::NotExecuted
        );
    }

    /// A conversion that does not add up is not a denomination, even when it was
    /// executed: the equality is the proof, not the fact that a call happened.
    #[test]
    fn an_unbalanced_conversion_is_not_a_denomination() {
        let one_token_short = denomination(500, 399, 100);
        assert!(!one_token_short.is_proved());
        assert_eq!(one_token_short.wei_of_profit_per_wei_of_token(), None);
        let net = NetProfit::compute(
            GrossMovement::Gained(U256::from(500u32)),
            &priced(10, 20),
            Some(&one_token_short),
        );
        let reason = net.reason().expect("reason").to_string();
        assert!(reason.contains("did not add up"), "{reason}");
        assert!(reason.contains("399"), "{reason}");
    }

    #[test]
    fn state_changes_audit_accounts_and_storage_without_decoding_slots() {
        let changes = StateChanges {
            accounts: vec![
                AccountChange {
                    address: SENDER,
                    balance_before: U256::from(1_000u32),
                    balance_after: U256::from(900u32),
                    nonce_before: 0,
                    nonce_after: 7,
                },
                AccountChange {
                    address: TOKEN,
                    balance_before: U256::from(5u8),
                    balance_after: U256::from(5u8),
                    nonce_before: 1,
                    nonce_after: 1,
                },
            ],
            slots: vec![
                SlotChange {
                    address: TOKEN,
                    slot: U256::from(9u8),
                    before: U256::from(1u8),
                    after: U256::from(2u8),
                },
                SlotChange {
                    address: SENDER,
                    slot: U256::ZERO,
                    before: U256::ZERO,
                    after: U256::ZERO,
                },
            ],
        };
        assert!(!changes.is_empty());
        assert_eq!(
            changes.account(SENDER).map(|a| a.balance_movement()),
            Some(Movement::Decreased {
                by: U256::from(100u32)
            })
        );
        assert_eq!(
            changes.account(TOKEN).map(|a| a.balance_movement()),
            Some(Movement::Unchanged)
        );
        assert_eq!(changes.storage_of(TOKEN).len(), 1);
        assert_eq!(changes.touched(), vec![SENDER, TOKEN]);
        assert!(StateChanges::default().is_empty());
    }

    #[test]
    fn a_status_names_the_step_that_stopped() {
        let reverted = ExecutionStatus::Reverted {
            step: 5,
            call: "swap(uint256,uint256,address,bytes) 0x022c0d9f".to_string(),
            revert: RevertData::new(Bytes::from(ERROR_STRING_SELECTOR.to_vec())),
        };
        assert!(!reverted.completed());
        assert_eq!(reverted.failed_at(), Some(5));
        assert_eq!(
            ExecutionStatus::OutOfGas {
                step: 2,
                call: "transfer(address,uint256) 0xa9059cbb".to_string(),
            }
            .failed_at(),
            Some(2)
        );
        assert!(ExecutionStatus::Completed.completed());
        assert_eq!(ExecutionStatus::Completed.failed_at(), None);
        // There is no "refused" status: a run that never reached the EVM returns
        // `Err(SimulationError)` from the engine, so it produces no result to
        // carry a status at all.
    }

    /// §37: the fingerprint is a handle over the whole serialized result, so a
    /// single changed field moves it and an identical result reproduces it.
    #[test]
    fn a_fingerprint_covers_every_field() {
        let first = result_with(U256::from(1_100u32));
        let again = result_with(U256::from(1_100u32));
        assert_eq!(first.fingerprint(), again.fingerprint());

        let different = result_with(U256::from(1_099u32));
        assert_ne!(first.fingerprint(), different.fingerprint());
        assert_eq!(
            first.fingerprint(),
            keccak256(serde_json::to_string(&first).expect("json").as_bytes())
        );
    }

    #[test]
    fn a_result_reads_measurements_by_binding_and_prints_a_summary() {
        let result = result_with(U256::from(1_100u32));
        assert_eq!(
            result.measurement(Binding::SenderInputEnd),
            Some(U256::from(1_100u32))
        );
        assert_eq!(result.measurement(Binding::SenderNativeEnd), None);
        assert!(result.success());
        assert_eq!(result.gas_used(), 21_000);
        assert_eq!(result.logs().len(), 1);
        assert_eq!(result.logs()[0].0, 2);
        assert!(
            result.summary().contains("completed"),
            "{}",
            result.summary()
        );
        assert!(
            result.summary().contains("delta +100"),
            "{}",
            result.summary()
        );
        assert_eq!(result.revert(), None, "a completed run has no revert data");
        let text = serde_json::to_string(&result).expect("serializes");
        assert!(text.contains("\"selector\":\"0x022c0d9f\""), "{text}");
    }

    fn result_with(output: U256) -> SimulationResult {
        let charge = priced(21_000, 1);
        let denomination = denomination(500, 400, 100);
        let input = U256::from(1_000u32);
        let gross = output.checked_sub(input);
        let outcome = SimulatedOutcome::Executed {
            output,
            movement: Movement::Increased { by: output },
        };
        let movement = GrossMovement::of(&outcome, input);
        let net_profit = NetProfit::compute(movement, &charge, Some(&denomination));
        SimulationResult {
            chain_id: ChainId(91342),
            block: BlockPin::new(
                evm_core::BlockNumber(37_191_169),
                b256!("0x2222222222222222222222222222222222222222222222222222222222222222"),
            ),
            state_source: "dump:fixtures/m4.json".to_string(),
            sender: SENDER,
            status: ExecutionStatus::Completed,
            steps: vec![ExecutedStep {
                index: 2,
                nonce: 1,
                from: SENDER,
                to: TOKEN,
                value: U256::ZERO,
                signature: "swap(uint256,uint256,address,bytes)".to_string(),
                selector: "0x022c0d9f".to_string(),
                calldata: Bytes::from(vec![0x02, 0x2c, 0x0d, 0x9f]),
                gas_limit: 30_000_000,
                gas_used: 21_000,
                status: StepStatus::Success,
                logs: vec![ExecutedLog {
                    address: TOKEN,
                    topics: vec![b256!(
                        "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d703529e78a09f"
                    )],
                    data: Bytes::new(),
                }],
                measured: Some(MeasuredValue {
                    binding: Binding::SenderInputEnd.name(),
                    value: output,
                }),
            }],
            gas_charge: charge,
            measurements: vec![MeasuredValue {
                binding: Binding::SenderInputEnd.name(),
                value: output,
            }],
            compared: OutputComparison {
                analytical: U256::from(1_000u32),
                simulated: Some(output),
            },
            outcome,
            gross_profit: gross,
            gross_loss: None,
            net_profit,
            state_changes: StateChanges::default(),
            slippage: SlippageRecord {
                expected_output: U256::from(1_000u32),
                policy: crate::route::SlippagePolicy::Exact,
                minimum_output: U256::from(1_000u32),
            },
            plan_summary: PlanSummary {
                evm_rules: "prague".to_string(),
                steps: vec!["0: balanceOf".to_string()],
                pools: vec![TOKEN],
                input_token: TOKEN,
                input_amount: U256::from(1_000u32),
                analytical_mid_amount: U256::from(1_050u32),
                analytical_output: U256::from(1_000u32),
                priced_by: "priced by a test".to_string(),
            },
        }
    }
}
