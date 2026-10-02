//! §7/§8: the object that carries a risk decision into the transaction layer.
//!
//! An intent is a *transaction plus the reasons it may exist*. The eight buildable
//! fields (chain, sender, target, value, calldata, nonce, gas, fee) travel with the
//! three identities that produced them (opportunity, simulation, risk decision), the
//! block pin the simulation ran against, and the state fingerprint that ties the
//! simulation to a specific state update. That grouping is not decoration: §14's round
//! trip compares the decoded transaction against *this* object, and §30's idempotency
//! key is built from the identity half of it. If the two halves were in different
//! types, a transaction could be correct while its provenance was not.
//!
//! One intent is one transaction. The M4 plan is a sequence of steps from one sender, and
//! a milestone without an executor contract (§4) cannot turn a multi-step sequence into one
//! atomic transaction — so [`TransactionIntent::from_run`] refuses a multi-step run rather
//! than pretending: it names the step count, and the caller either executes a genuinely
//! single-step intent or stops. M7's answer is the other constructor,
//! [`TransactionIntent::from_simulated_step`]: one intent per step, each carrying the
//! [`SequencePosition`] a runner will honour, sent serially by [`crate::sequence`].

use alloy_primitives::{Address, Bytes, ChainId, B256, U256};
use evm_core::BlockNumber;
use evm_simulation::{BlockPin, NetProfit, SimulationResult};
use serde::{Deserialize, Serialize};

use crate::error::{ExecutionError, Result};
use crate::tx::{AccessTuple, TransactionType, UnsignedTransaction};

/// The three identities §7 requires an intent to carry, and §30's idempotency key.
///
/// `risk_decision_id` is derived rather than issued: the risk layer's [`evm_risk`]
/// decision is a value with no identity of its own (it is a `RiskDecision`, compared
/// field by field), so its identity is its own content hash over the simulation it
/// judged. Two identical decisions over the same run therefore have the same id, which
/// is the property §30 needs; inventing a counter would make the same judgement two
/// different executions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIds {
    pub opportunity_id: String,
    /// [`evm_simulation::SimulationResult::fingerprint`]: keccak over the serialized
    /// result. Any field change, down to one log byte, is a different simulation.
    pub simulation_id: B256,
    pub risk_decision_id: B256,
}

impl ExecutionIds {
    /// Derive the identity triple from a run and the decision taken about it.
    pub fn of(
        run: &SimulationResult,
        decision: &evm_risk::RiskDecision,
        opportunity_id: &str,
    ) -> Self {
        let simulation_id = run.fingerprint();
        let risk_decision_id = {
            let text = serde_json::to_string(decision)
                .unwrap_or_else(|_| String::from("a risk decision that does not serialize"));
            let mut bytes = Vec::with_capacity(32 + text.len());
            bytes.extend_from_slice(simulation_id.as_slice());
            bytes.extend_from_slice(text.as_bytes());
            alloy_primitives::keccak256(&bytes)
        };
        Self {
            opportunity_id: opportunity_id.to_string(),
            simulation_id,
            risk_decision_id,
        }
    }

    /// §30's key: the same opportunity, judged on the same simulation of the same
    /// state, is one execution and not two.
    pub fn idempotency_key(&self, state_fingerprint: &B256) -> String {
        format!(
            "{}|{:#x}|{:#x}",
            self.opportunity_id, self.simulation_id, state_fingerprint
        )
    }
}

/// Where one transaction sits in a serial sequence.
///
/// M7's trade cannot be one atomic transaction: §4 forbids the executor contract that
/// would make it one, so the route is carried by N transactions from one EOA, sent in
/// order by [`crate::sequence`]. This value is what lets the builder tell that shape
/// apart from the failure §4 was written against — an intent that claims to be step 1 of
/// a 7-step run *without* a runner that will send the other six, which is a half-trade
/// reaching a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequencePosition {
    /// 0-based index of this transaction within the sequence.
    pub index: usize,
    /// How many transactions the sequence has in total.
    pub count: usize,
}

impl SequencePosition {
    pub fn describe(self) -> String {
        format!("transaction {} of {}", self.index + 1, self.count)
    }
}

/// §34's boundary, stated by whoever assembled the intent.
///
/// This cannot be derived from a [`SimulationResult`]: the result records what the EVM
/// did, and the run that made it possible was funded by the request's own setup — every
/// M4/M5 simulation request hands `preflight()` a balance override for its sender, whose
/// stated reason is that it "is not a fact about this chain". So the caller says which of
/// the two it has, and the build policy refuses anything that is not real state. Making
/// this an argument rather than a field the intent fills in itself is the point: a hard-coded
/// `false` would let an override-funded arbitrage run reach a signature.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SenderFunding {
    /// The pinned chain state already says this sender can pay for the transaction. The
    /// `source` names the reads that established it.
    RealState { source: String },
    /// A state override made this run possible. `detail` quotes the override. Buildable
    /// for evidence, never submittable (§34).
    Overridden { detail: String },
}

impl SenderFunding {
    /// Whether an override, and not the chain, is what would have made this transaction
    /// payable (§34's question).
    pub fn derived_from_overridden_state(&self) -> bool {
        matches!(self, Self::Overridden { .. })
    }

    /// The one-line statement §52's evidence carries.
    pub fn describe(&self) -> String {
        match self {
            Self::RealState { source } => format!("real chain state: {source}"),
            Self::Overridden { detail } => format!("state override: {detail}"),
        }
    }
}

/// Everything a builder needs, and nothing it is allowed to decide for itself (§9).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TransactionIntent {
    pub ids: ExecutionIds,

    pub chain_id: ChainId,
    pub block_number: BlockNumber,
    pub block_hash: B256,
    /// The state version the opportunity was priced on
    /// ([`evm_state::UpdatePosition`]'s `{block, log_index}` rendering), kept as a
    /// string so this crate does not depend on the state crate for one display field.
    pub state_fingerprint: String,

    pub sender: Address,
    pub target: Address,
    pub value: U256,
    pub calldata: Bytes,

    pub nonce: u64,
    pub gas_limit: u64,
    pub tx_type: TransactionType,

    /// EIP-1559 fields; both `None` for a legacy intent, whose ceiling is
    /// `max_fee_per_gas` alone.
    pub max_fee_per_gas: Option<U256>,
    pub max_priority_fee_per_gas: Option<U256>,

    pub access_list: Vec<AccessTuple>,

    /// What the simulation measured as profit, and the floor the risk layer applied.
    /// The intent states both so a reader can see the margin it was built with;
    /// neither is used to change anything (§9).
    pub simulation_profit_wei: Option<U256>,
    pub minimum_required_profit_wei: U256,

    /// §34's record: was this run only possible because state was overridden? An
    /// intent derived from an override-funded simulation is buildable and *not*
    /// submittable, and this is what makes that sayable — with the override quoted, so
    /// the evidence names the scaffolding rather than just denying it.
    pub funding: SenderFunding,
    /// How many steps the simulated sequence had; `1` is the only submittable value
    /// without an executor contract.
    pub simulated_steps: usize,
    /// [`Some`] exactly when a runner has committed to sending this transaction as part of
    /// a serial sequence — with its position. `None` on M6's two paths (a single-step
    /// arbitrage intent and §35's validation transaction), and the builder requires it to
    /// be `Some` for any intent whose `simulated_steps` is above one.
    pub sequence: Option<SequencePosition>,
}

impl TransactionIntent {
    /// The intent for a single-step execution of a run the risk layer accepted.
    ///
    /// This is §44's consumption point: everything about the economics comes out of
    /// [`SimulationResult`]; no pool state is re-read and no profit is recomputed here.
    ///
    /// It refuses when the decision is not an Accept, when the run did not complete, and
    /// when the sequence has more than one step — a multi-step M4 plan needs an executor
    /// contract, which §4 does not allow in this milestone. An override-funded run is
    /// *not* refused here: the intent is the evidence of what the simulation decided, and
    /// [`crate::builder`] is where §34's rule stops it reaching a signature or a node.
    ///
    /// `funding` is not inferred from the run: a [`SimulationResult`] does not carry the
    /// setup that funded it, so the caller that built the request states it, and
    /// [`SenderFunding::Overridden`] makes the intent buildable and never submittable.
    pub fn from_run(
        run: &SimulationResult,
        decision: &evm_risk::RiskDecision,
        opportunity_id: &str,
        state_fingerprint: &str,
        funding: SenderFunding,
    ) -> Result<Self> {
        if !decision.accepted() {
            return Err(ExecutionError::InvalidIntent(format!(
                "an intent needs a RiskDecision::Accept and this is {}: {}",
                decision.name(),
                decision.reason()
            )));
        }
        if !run.success() {
            return Err(ExecutionError::InvalidIntent(format!(
                "the accepted run did not complete: {}",
                run.summary()
            )));
        }
        if run.steps.len() != 1 {
            return Err(ExecutionError::InvalidIntent(format!(
                "this run simulated {} steps and an intent carries one transaction; executing \
                 a multi-step sequence needs an executor contract, which §4 does not allow in \
                 this milestone",
                run.steps.len()
            )));
        }
        let step = run
            .steps
            .first()
            .ok_or_else(|| ExecutionError::InvalidIntent("the run has no step".to_string()))?;
        if !step.status.succeeded() {
            return Err(ExecutionError::InvalidIntent(format!(
                "step 0 of the accepted run did not succeed: {:?}",
                step.status
            )));
        }
        let net_profit = match &run.net_profit {
            NetProfit::Gain { amount, .. } => Some(*amount),
            _ => None,
        };
        let minimum_required_profit_wei = match decision {
            evm_risk::RiskDecision::Accept {
                minimum_net_profit_wei,
                ..
            } => *minimum_net_profit_wei,
            _ => U256::ZERO,
        };
        let ids = ExecutionIds::of(run, decision, opportunity_id);
        Ok(Self {
            ids,
            chain_id: run.chain_id.0,
            block_number: run.block.number,
            block_hash: run.block.hash,
            state_fingerprint: state_fingerprint.to_string(),
            sender: run.sender,
            target: step.to,
            value: step.value,
            calldata: step.calldata.clone(),
            // The plan's own nonce is the nonce *inside the simulation's account
            // sequence*; a real run re-reads it. It is carried here so a reader can see
            // what the simulation assumed.
            nonce: step.nonce,
            gas_limit: step.gas_limit,
            tx_type: TransactionType::DynamicFee,
            max_fee_per_gas: None,
            max_priority_fee_per_gas: None,
            access_list: Vec::new(),
            simulation_profit_wei: net_profit,
            minimum_required_profit_wei,
            funding,
            simulated_steps: run.steps.len(),
            sequence: None,
        })
    }

    /// One transaction of the serial sequence [`crate::sequence`] runs: the same
    /// opportunity, simulation and risk decision as its siblings, carrying one step of the
    /// simulated plan.
    ///
    /// `position.count` must equal the number of this run's steps that are transactions and
    /// `position.index` must name `step` within that list, because those two equalities are
    /// what make this intent's *sequence* a fact about the simulation rather than a claim by
    /// the caller: an intent that says "transaction 2 of 6" is only buildable if the run
    /// really has six executable steps and this is its second. [`crate::builder`] reads that
    /// agreement as §4's permission — without it, a multi-step intent is a half-trade with no
    /// runner committed to the other half.
    ///
    /// Note the position counts *transactions*, not plan steps: a plan that measures state
    /// between calls has step indices ahead of sequence positions, and the check below is
    /// against the executable list for exactly that reason.
    pub fn from_simulated_step(
        run: &SimulationResult,
        decision: &evm_risk::RiskDecision,
        opportunity_id: &str,
        state_fingerprint: &str,
        funding: SenderFunding,
        position: SequencePosition,
        step: &evm_simulation::ExecutedStep,
    ) -> Result<Self> {
        if !decision.accepted() {
            return Err(ExecutionError::InvalidIntent(format!(
                "a sequence step needs a RiskDecision::Accept and this is {}: {}",
                decision.name(),
                decision.reason()
            )));
        }
        if !run.success() {
            return Err(ExecutionError::InvalidIntent(format!(
                "the accepted run did not complete: {}",
                run.summary()
            )));
        }
        if step.index >= run.steps.len() || run.steps[step.index] != *step {
            return Err(ExecutionError::InvalidIntent(format!(
                "the step offered as position {} is not one of this run's {} steps, so the \
                 sequence position could not have come from the simulation",
                step.index,
                run.steps.len()
            )));
        }
        let executable = crate::sequence::broadcastable(run);
        if position.count != executable.len() {
            return Err(ExecutionError::InvalidIntent(format!(
                "the intent claims a sequence of {} transactions and this run has {} steps \
                 that are transactions; one of the two is not this simulation",
                position.count,
                executable.len()
            )));
        }
        if executable.get(position.index) != Some(&step) {
            return Err(ExecutionError::InvalidIntent(format!(
                "the intent claims to be transaction {} of {} and the step offered is plan step \
                 {}, which is not that transaction of this run",
                position.index, position.count, step.index
            )));
        }
        if !step.status.succeeded() {
            return Err(ExecutionError::InvalidIntent(format!(
                "step {} of the accepted run did not succeed: {:?}",
                step.index, step.status
            )));
        }
        let net_profit = match &run.net_profit {
            NetProfit::Gain { amount, .. } => Some(*amount),
            _ => None,
        };
        let minimum_required_profit_wei = match decision {
            evm_risk::RiskDecision::Accept {
                minimum_net_profit_wei,
                ..
            } => *minimum_net_profit_wei,
            _ => U256::ZERO,
        };
        let ids = ExecutionIds::of(run, decision, opportunity_id);
        Ok(Self {
            ids,
            chain_id: run.chain_id.0,
            block_number: run.block.number,
            block_hash: run.block.hash,
            state_fingerprint: state_fingerprint.to_string(),
            sender: step.from,
            target: step.to,
            value: step.value,
            calldata: step.calldata.clone(),
            nonce: step.nonce,
            gas_limit: step.gas_limit,
            tx_type: TransactionType::DynamicFee,
            max_fee_per_gas: None,
            max_priority_fee_per_gas: None,
            access_list: Vec::new(),
            simulation_profit_wei: net_profit,
            minimum_required_profit_wei,
            funding,
            simulated_steps: run.steps.len(),
            sequence: Some(position),
        })
    }

    /// The intent for a transaction that is not an arbitrage: §35's controlled
    /// validation transaction. It carries the same identity fields, with the
    /// opportunity id saying plainly that no opportunity stands behind it.
    ///
    /// The transaction's fields arrive as one [`UnsignedTransaction`] rather than as
    /// eleven positional arguments, because a validation transaction is described by
    /// exactly the same field set as any other and a call site that swapped two `u64`s or
    /// two `Address`es would type-check either way.
    ///
    /// A creation is refused: §10's intent always names a target, and §35's validation
    /// transaction is a call to an account, so an empty `to` here would mean the caller
    /// handed over the wrong transaction rather than that the chain forbids it.
    pub fn validation(
        block: BlockPin,
        sender: Address,
        unsigned: &UnsignedTransaction,
    ) -> Result<Self> {
        let target = unsigned.to.ok_or_else(|| {
            ExecutionError::InvalidIntent(
                "a validation transaction must name a target; this envelope has no `to`, i.e. \
                 it is a contract creation, which is not what §35 validates"
                    .to_string(),
            )
        })?;
        let mut scratch = Vec::new();
        scratch.extend_from_slice(
            format!(
                "validation|{}|{}|{}|{}|",
                unsigned.chain_id, block.number.0, unsigned.nonce, unsigned.gas_limit
            )
            .as_bytes(),
        );
        scratch.extend_from_slice(&unsigned.value.to_be_bytes::<32>());
        scratch.extend_from_slice(unsigned.input.as_ref());
        let fingerprint = alloy_primitives::keccak256(&scratch);
        Ok(Self {
            ids: ExecutionIds {
                opportunity_id: "m6-execution-validation-transaction".to_string(),
                simulation_id: fingerprint,
                risk_decision_id: fingerprint,
            },
            chain_id: unsigned.chain_id,
            block_number: block.number,
            block_hash: block.hash,
            state_fingerprint: format!("validation block {}", block.number.0),
            sender,
            target,
            value: unsigned.value,
            calldata: unsigned.input.clone(),
            nonce: unsigned.nonce,
            gas_limit: unsigned.gas_limit,
            tx_type: unsigned.tx_type,
            max_fee_per_gas: unsigned.max_fee_per_gas,
            max_priority_fee_per_gas: unsigned.max_priority_fee_per_gas,
            access_list: unsigned.access_list.clone(),
            simulation_profit_wei: None,
            minimum_required_profit_wei: U256::ZERO,
            // A validation transaction is built from the account's real state by
            // construction: nothing was overridden to make it possible. The gate still
            // reads the balance and the nonce, and records where it read them.
            funding: SenderFunding::RealState {
                source: format!(
                    "§35's validation transaction, built against the account's own state at \
                     block {} and priced from the endpoint's own fee reading",
                    block.number.0
                ),
            },
            simulated_steps: 1,
            sequence: None,
        })
    }

    /// The shape §35 asks a caller to describe: a value transfer to an account that
    /// already exists, with no calldata. Everything the lane reads from the chain —
    /// the nonce and both fee fields — is left for [`crate::stage::ExecutionStage`]'s
    /// own price step to fill, so a caller cannot pin the transaction to a nonce the
    /// endpoint never gave it, and the identity fields are hashed over that unfilled
    /// envelope. Two validation transactions in one stage are therefore the same
    /// execution (§30), which is the answer for a run whose whole purpose is to be
    /// sent once.
    ///
    /// A trade cannot be built here on purpose: the fields that would make one (target
    /// contract, calldata, the amount) are market data, and §36 forbids the execution
    /// layer inventing any.
    pub fn validation_call(
        block: BlockPin,
        chain_id: ChainId,
        sender: Address,
        target: Address,
        value: U256,
        gas_limit: u64,
    ) -> Result<Self> {
        let unsigned = UnsignedTransaction {
            tx_type: TransactionType::DynamicFee,
            chain_id,
            nonce: 0,
            to: Some(target),
            value,
            gas_limit,
            input: Bytes::default(),
            access_list: Vec::new(),
            max_priority_fee_per_gas: None,
            max_fee_per_gas: None,
        };
        Self::validation(block, sender, &unsigned)
    }

    /// The shape the builder turns into bytes. The intent's fee fields are the only
    /// fee source; the builder adds nothing.
    pub fn unsigned(&self) -> UnsignedTransaction {
        UnsignedTransaction {
            tx_type: self.tx_type,
            chain_id: self.chain_id,
            nonce: self.nonce,
            to: Some(self.target),
            value: self.value,
            gas_limit: self.gas_limit,
            input: self.calldata.clone(),
            access_list: self.access_list.clone(),
            max_priority_fee_per_gas: self.max_priority_fee_per_gas,
            max_fee_per_gas: self.max_fee_per_gas,
        }
    }

    /// §8's state binding, as the single value §30's key is built from.
    pub fn state_binding(&self) -> B256 {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(self.block_hash.as_slice());
        bytes.extend_from_slice(self.state_fingerprint.as_bytes());
        bytes.extend_from_slice(self.ids.simulation_id.as_slice());
        alloy_primitives::keccak256(&bytes)
    }

    /// The identity of the fee fields, for evidence (§52).
    pub fn fee_summary(&self) -> String {
        match (self.max_fee_per_gas, self.max_priority_fee_per_gas) {
            (Some(max), Some(tip)) => format!("eip1559 maxFee={max} tip={tip}"),
            (Some(max), None) => format!("legacy gasPrice={max}"),
            (None, _) => "unpriced".to_string(),
        }
    }
}

/// Small local view over the risk decision's name, so this crate can say which answer
/// it was handed without depending on `evm_risk`'s display format.
trait DecisionName {
    fn name(&self) -> &'static str;
}

impl DecisionName for evm_risk::RiskDecision {
    fn name(&self) -> &'static str {
        match self {
            evm_risk::RiskDecision::Accept { .. } => "Accept",
            evm_risk::RiskDecision::Reject { .. } => "Reject",
            evm_risk::RiskDecision::Unknown { .. } => "Unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> ExecutionIds {
        ExecutionIds {
            opportunity_id: "op-1".to_string(),
            simulation_id: B256::left_padding_from(&[1]),
            risk_decision_id: B256::left_padding_from(&[2]),
        }
    }

    #[test]
    fn the_idempotency_key_changes_when_the_state_changes_and_not_when_the_wording_does() {
        let a = ids().idempotency_key(&B256::left_padding_from(&[7]));
        let b = ids().idempotency_key(&B256::left_padding_from(&[8]));
        let again = ids().idempotency_key(&B256::left_padding_from(&[7]));
        assert_ne!(a, b, "a different state must be a different key");
        assert_eq!(a, again, "the same triple must be the same key");
    }

    #[test]
    fn an_intent_with_no_fee_fields_reports_itself_as_unpriced() {
        let mut intent = TransactionIntent::validation(
            BlockPin::new(BlockNumber(10), B256::left_padding_from(&[3])),
            Address::from_slice(&[1u8; 20]),
            &UnsignedTransaction {
                tx_type: TransactionType::DynamicFee,
                chain_id: 1,
                nonce: 0,
                to: Some(Address::from_slice(&[2u8; 20])),
                value: U256::ZERO,
                gas_limit: 21_000,
                input: Bytes::default(),
                access_list: Vec::new(),
                max_priority_fee_per_gas: Some(U256::from(1u64)),
                max_fee_per_gas: Some(U256::from(1u64)),
            },
        )
        .expect("a validation intent over a call transaction");
        assert_eq!(intent.fee_summary(), "eip1559 maxFee=1 tip=1");
        intent.max_fee_per_gas = None;
        assert_eq!(intent.fee_summary(), "unpriced");
    }

    #[test]
    fn a_validation_intent_is_still_bound_to_a_block() {
        let intent = TransactionIntent::validation(
            BlockPin::new(BlockNumber(37_191_169), B256::left_padding_from(&[9])),
            Address::from_slice(&[1u8; 20]),
            &UnsignedTransaction {
                tx_type: TransactionType::Legacy,
                chain_id: 91_342,
                nonce: 3,
                to: Some(Address::from_slice(&[2u8; 20])),
                value: U256::from(7u64),
                gas_limit: 21_000,
                input: Bytes::from(vec![0u8; 4]),
                access_list: Vec::new(),
                max_priority_fee_per_gas: None,
                max_fee_per_gas: Some(U256::from(1_000u64)),
            },
        )
        .expect("a validation intent over a call transaction");
        assert_eq!(intent.block_number, BlockNumber(37_191_169));
        assert_ne!(intent.state_binding(), B256::ZERO);
        assert_eq!(intent.unsigned().chain_id, 91_342);
        assert_eq!(intent.fee_summary(), "legacy gasPrice=1000");
    }
}
