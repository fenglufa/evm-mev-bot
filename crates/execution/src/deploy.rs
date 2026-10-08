//! M10 §46/§47: the two transactions that only an operator can send — put the executor on
//! a chain, and configure or call the contract that is now there.
//!
//! This module exists because §4's audit found the repository had no deployment path at
//! all: every M6–M9 transaction went to an address that was already on the chain. The
//! audit's §4.2 conclusion is what is implemented here — a creation is an
//! [`UnsignedTransaction`] with `to: None`, so the existing signer, the existing
//! submitter and the existing receipt tracker already accept it, and
//! [`crate::builder`] is deliberately untouched rather than widened to cover a case it was
//! never written for. Nothing here is a second signer, a second fee oracle, a second nonce
//! lane or a second receipt reader (§24/§33); the four endpoint surfaces arrive in the
//! same [`Abilities`] value [`crate::stage::ExecutionStage`] takes.
//!
//! Three facts make this different from the arbitrage path, and each is a restriction
//! rather than a feature:
//!
//! * **The head is handed in, not discovered.** [`crate::chain_read::ChainReader`] answers
//!   "what hash do you hold at height N" and "which chain are you on"; it has no head
//!   method, and adding one would change a module M9.4 already depends on. So the caller
//!   passes a [`ChainHead`] it read from the same endpoint, and this module re-reads that
//!   height before signing — an operator transaction priced against a block that is no
//!   longer canonical is §32's failure, just at a different altitude.
//! * **One step at a time.** Each method allocates a nonce from the single
//!   [`NonceAllocator`] lane and holds it until the receipt resolves, so a ladder cannot
//!   send step N+1 while step N is still unresolved (§11). A poll-budget `Timeout`
//!   therefore leaves the lane occupied and the next step fails loudly instead of
//!   duplicating a nonce.
//! * **A revert is an answer.** [`Step`] reports whatever status the chain gave, including
//!   `Reverted`, and returns `Ok`. §58's whole point is that a deliberately-too-high floor
//!   produces a *received* transaction with `status = 0`; a `send` that turned that into an
//!   `Err` would make the required evidence impossible to produce.

use std::sync::Arc;

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use serde_json::{json, Value};

use evm_core::BlockNumber;

use crate::chain_read::read_binding;
use crate::error::{ExecutionError, Result};
use crate::fee::FeePolicy;
use crate::gate::BlockBinding;
use crate::mode::ExecutionMode;
use crate::nonce::NonceAllocator;
use crate::receipt::{
    ExpectedTransaction, Receipt, ReceiptPolicy, ReceiptStatus, ReceiptTracker, TrackedReceipt,
};
use crate::rlp::Encoder;
use crate::signer::Signer;
use crate::stage::Abilities;
use crate::submitter::SubmissionOutcome;
use crate::tx::{TransactionType, UnsignedTransaction};

/// EIP-3860's bound on initcode size. This chain does not necessarily enforce it, so the
/// check is ours: a creation input that blows past it means the wrong artifact was loaded
/// (a whole `target/` tree, two contracts concatenated) far more often than it means a
/// genuinely large contract, and naming that here is much cheaper than naming it as a
/// rejected transaction.
pub const MAX_INITCODE_BYTES: usize = 49_152;

/// The block this transaction is priced against, as the endpoint named it.
///
/// A number alone would let a fee read survive a reorg, which is exactly what §8 forbids
/// for intents — and an operator transaction is an intent in every way except the route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainHead {
    pub number: u64,
    pub hash: B256,
}

/// The gas, envelope, fee and receipt policy for one operator session.
#[derive(Clone, Debug)]
pub struct DeployPolicy {
    /// Ceiling for the creation transaction.
    pub creation_gas_limit: u64,
    /// Ceiling for a call transaction.
    pub call_gas_limit: u64,
    pub tx_type: TransactionType,
    pub fee: FeePolicy,
    pub receipt: ReceiptPolicy,
}

impl DeployPolicy {
    /// Every number in the policy has to be one the chain can honour: a zero gas limit is
    /// not a cheap transaction, it is a transaction that cannot run.
    pub fn validate(&self) -> Result<()> {
        if self.creation_gas_limit == 0 || self.call_gas_limit == 0 {
            return Err(ExecutionError::InvalidIntent(
                "a deploy policy with a zero gas limit: the transaction would be refused \
                 before it ran, and the receipt would then say nothing about the contract"
                    .to_string(),
            ));
        }
        Ok(())
    }
}

/// The 32-byte ABI word for the constructor's single `address operator_` argument.
pub fn constructor_arguments(operator: Address) -> Bytes {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(operator.as_slice());
    Bytes::from(word.to_vec())
}

/// The transaction input for a creation: compiled creation code with the encoded
/// constructor arguments appended, which is what the EVM expects in `data`.
pub fn creation_input(creation_code: &[u8], operator: Address) -> Result<Bytes> {
    if creation_code.is_empty() {
        return Err(ExecutionError::InvalidIntent(
            "the creation code is empty: a transaction with no data and no `to` moves value \
             to nothing, it does not create anything"
                .to_string(),
        ));
    }
    if creation_code.len() > MAX_INITCODE_BYTES {
        return Err(ExecutionError::InvalidIntent(format!(
            "the creation code is {} bytes and the bound is {MAX_INITCODE_BYTES}: this is far \
             more likely to be the wrong artifact than a contract that is genuinely large, \
             and the honest next step is to check what was compiled",
            creation_code.len()
        )));
    }
    let arguments = constructor_arguments(operator);
    let mut input = Vec::with_capacity(creation_code.len() + arguments.len());
    input.extend_from_slice(creation_code);
    input.extend_from_slice(arguments.as_ref());
    Ok(Bytes::from(input))
}

/// The address a creation from `sender` at `nonce` produces:
/// `keccak256(rlp([sender, nonce]))[12..]`.
///
/// Computed before sending so the deployment record can hold two addresses — the one the
/// encoding predicts and the one the receipt reports — and say whether they agree. They
/// agree for every included creation, and the reason to keep both is the case where they
/// do not: a wrong nonce was assumed, or a node served a receipt that was not ours.
pub fn create_address(sender: Address, nonce: u64) -> Address {
    let mut body = Encoder::new();
    body.bytes(sender.as_slice());
    body.u64(nonce);
    let mut outer = Encoder::new();
    outer.list(&body.finish());
    let hash = keccak256(outer.finish());
    Address::from_slice(&hash.as_slice()[12..])
}

/// One operator transaction that reached the chain, with every read that priced it.
///
/// This is the same evidence an arbitrage attempt carries — the fee provenance, the balance
/// behind §33, the recovered sender, the submission word, the receipt — in one value,
/// because §31's rows and §32's reconciliation need them together.
#[derive(Clone, Debug)]
pub struct Step {
    pub label: String,
    pub chain_id: u64,
    pub nonce: u64,
    pub tx_type: String,
    /// `None` is the creation. Kept as an `Option` and serialized as `null` rather than as
    /// the zero address, for the reason documented on [`UnsignedTransaction::to`].
    pub to: Option<Address>,
    pub value_wei: U256,
    pub gas_limit: u64,
    pub input_bytes: usize,
    pub input_hash: B256,
    pub transaction_hash: B256,
    pub recovered_sender: Address,
    pub fee_provenance: String,
    pub max_fee_per_gas: U256,
    pub head_number: u64,
    pub head_binding: String,
    pub balance_wei: U256,
    pub maximum_spend_wei: U256,
    pub submission_word: String,
    pub status: ReceiptStatus,
    pub receipt: Option<Receipt>,
    pub detail: String,
}

impl Step {
    pub fn succeeded(&self) -> bool {
        self.status == ReceiptStatus::Included
    }

    pub fn reverted(&self) -> bool {
        self.status == ReceiptStatus::Reverted
    }

    pub fn gas_used(&self) -> Option<u64> {
        self.receipt.as_ref().map(|r| r.gas_used)
    }

    pub fn l1_fee(&self) -> Option<U256> {
        self.receipt.as_ref().and_then(|r| r.l1_fee)
    }

    pub fn block(&self) -> Option<(u64, B256)> {
        self.receipt
            .as_ref()
            .map(|r| (r.block_number, r.block_hash))
    }

    pub fn contract_address(&self) -> Option<Address> {
        self.receipt.as_ref().and_then(|r| r.contract_address)
    }

    /// What this step cost: the L2 bill the EVM charged. `l1_fee` is reported beside it
    /// and never added into it — the two are billed by different layers of the stack.
    pub fn l2_cost_wei(&self) -> Option<U256> {
        self.receipt.as_ref().and_then(Receipt::l2_cost_wei)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "label": self.label,
            "chain_id": self.chain_id,
            "nonce": self.nonce,
            "tx_type": self.tx_type,
            "to": self.to.map(|a| format!("{a:#x}")),
            "value_wei": self.value_wei.to_string(),
            "gas_limit": self.gas_limit,
            "input_bytes": self.input_bytes,
            "input_hash": format!("{:#x}", self.input_hash),
            "transaction_hash": format!("{:#x}", self.transaction_hash),
            "recovered_sender": format!("{:#x}", self.recovered_sender),
            "fee_provenance": self.fee_provenance,
            "max_fee_per_gas_wei": self.max_fee_per_gas.to_string(),
            "head_number": self.head_number,
            "head_binding": self.head_binding,
            "balance_wei": self.balance_wei.to_string(),
            "maximum_spend_wei": self.maximum_spend_wei.to_string(),
            "submission": self.submission_word,
            "receipt_status": self.status.name(),
            "receipt_block_number": self.receipt.as_ref().map(|r| r.block_number),
            "receipt_block_hash": self.receipt.as_ref().map(|r| format!("{:#x}", r.block_hash)),
            "receipt_gas_used": self.receipt.as_ref().map(|r| r.gas_used),
            "receipt_effective_gas_price_wei": self
                .receipt
                .as_ref()
                .map(|r| r.effective_gas_price.to_string()),
            "receipt_l2_cost_wei": self.l2_cost_wei().map(|c| c.to_string()),
            "receipt_l1_fee_wei": self.l1_fee().map(|f| f.to_string()),
            "receipt_contract_address": self.contract_address().map(|a| format!("{a:#x}")),
            "receipt_log_count": self.receipt.as_ref().map(|r| r.logs.len()),
            "detail": self.detail,
        })
    }
}

/// A deployment: the creation step, §46's six fields, and the address proof.
#[derive(Clone, Debug)]
pub struct Deployment {
    pub step: Step,
    pub creation_code_bytes: usize,
    pub creation_code_hash: B256,
    pub abi_version: String,
    pub operator: Address,
    pub predicted_address: Address,
    pub contract_address: Option<Address>,
}

impl Deployment {
    /// The address the chain reports — the only one a plan may bind to (§36).
    pub fn deployed(&self) -> Option<Address> {
        self.contract_address
    }

    /// Whether the receipt's address is the one the sender-and-nonce encoding predicts.
    pub fn address_proved(&self) -> bool {
        self.contract_address == Some(self.predicted_address)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "contract_address": self.contract_address.map(|a| format!("{a:#x}")),
            "predicted_address": format!("{:#x}", self.predicted_address),
            "address_matches_prediction": self.address_proved(),
            "deployment_tx": format!("{:#x}", self.step.transaction_hash),
            "deployment_block": self.step.block().map(|(n, _)| n),
            "chain_id": self.step.chain_id,
            "creation_code_bytes": self.creation_code_bytes,
            "creation_code_hash": format!("{:#x}", self.creation_code_hash),
            "abi_version": self.abi_version,
            "operator": format!("{:#x}", self.operator),
            "step": self.step.to_json(),
        })
    }
}

/// The three facts that must be true before an operator session may exist.
///
/// Split out of [`Deployer::new`] because all three are decidable from the mode, the chain
/// id and the policy alone — they need no endpoint — and the two refusals are worth having
/// as unit tests rather than only as integration tests against a scripted node.
fn session_gate(mode: ExecutionMode, chain_id: u64, policy: &DeployPolicy) -> Result<()> {
    policy.validate()?;
    if chain_id == 0 {
        return Err(ExecutionError::InvalidIntent(
            "a deploy session with chain id 0: the configured chain is the value §7 compares \
             three opinions against, and 0 authorizes nothing"
                .to_string(),
        ));
    }
    if mode != ExecutionMode::Submit {
        return Err(ExecutionError::ModeGate(format!(
            "the deployer was handed a signer in mode {}; a deployment is a broadcast, so \
             the session needs submit — the same gate §20 puts on an arbitrage send, \
             applied to the transaction that creates the contract the arbitrage calls",
            mode.name()
        )));
    }
    Ok(())
}

/// The operator side of the executor: creation, configuration, and one-call executes.
///
/// It owns the nonce lane, which is why the sending methods take `&mut self`: two sessions
/// that each believe they hold the sender's next nonce is §11's bug, and the borrow checker
/// is cheaper here than a lock that would let the second one proceed anyway.
pub struct Deployer {
    abilities: Abilities,
    signer: Signer,
    policy: DeployPolicy,
    chain_id: u64,
    lane: NonceAllocator,
    tracker: ReceiptTracker,
}

impl Deployer {
    /// A session over the four endpoint surfaces, in a mode that may actually broadcast.
    ///
    /// The mode check is why this returns `Result`: a `BuildOnly` or `SignOnly` session
    /// reaching [`Deployer::deploy`] would sign locally, hand the bytes to a submitter that
    /// then refuses, and leave the operator holding a signed transaction that never went
    /// out. Refusing the pair at construction keeps §19/§20's gate in one place.
    pub fn new(
        abilities: Abilities,
        signer: Signer,
        policy: DeployPolicy,
        chain_id: u64,
    ) -> Result<Self> {
        session_gate(signer.mode(), chain_id, &policy)?;
        Ok(Self {
            abilities,
            signer,
            tracker: ReceiptTracker::new(policy.receipt),
            policy,
            chain_id,
            lane: NonceAllocator::new(),
        })
    }

    pub fn sender(&self) -> Result<Address> {
        self.signer.address()
    }

    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// §7's third opinion, asked once per session: which chain does this node say it is.
    ///
    /// The configured value is still what authorizes a send; this only reports a node that
    /// disagrees, because a deployment on the wrong chain cannot be corrected later.
    pub async fn verify_chain(&self) -> Result<u64> {
        let answer = self.abilities.chain.endpoint_chain_id().await?;
        if answer != self.chain_id {
            return Err(ExecutionError::ChainMismatch(format!(
                "this session is configured for chain {} and the endpoint answers for chain \
                 {answer}; §7's configured value authorizes a send, so the session stops here \
                 rather than deploying onto a chain no plan can name",
                self.chain_id
            )));
        }
        Ok(answer)
    }

    /// Put the contract on the chain: `to: None`, creation code plus the operator argument
    /// as input, `value = 0`.
    ///
    /// All six §46 fields come out of the returned [`Deployment`]: the address (twice —
    /// predicted and reported), the transaction hash, the block, the bytecode hash, the
    /// ABI version tag the caller supplied, and the chain id.
    pub async fn deploy(
        &mut self,
        head: ChainHead,
        creation_code: &[u8],
        operator: Address,
        abi_version: &str,
    ) -> Result<Deployment> {
        let input = creation_input(creation_code, operator)?;
        let step = self
            .send(
                head,
                None,
                input,
                U256::ZERO,
                self.policy.creation_gas_limit,
                "deploy".to_string(),
            )
            .await?;
        Ok(Deployment {
            creation_code_bytes: creation_code.len(),
            creation_code_hash: keccak256(creation_code),
            abi_version: abi_version.to_string(),
            operator,
            predicted_address: create_address(step.recovered_sender, step.nonce),
            contract_address: step.contract_address(),
            step,
        })
    }

    /// A zero-value call to a contract — every configuration step and every `execute` (§34:
    /// the first version is ERC20-only, so `msg.value` is not part of the route).
    pub async fn call(
        &mut self,
        head: ChainHead,
        to: Address,
        calldata: Bytes,
        label: String,
    ) -> Result<Step> {
        self.send(
            head,
            Some(to),
            calldata,
            U256::ZERO,
            self.policy.call_gas_limit,
            label,
        )
        .await
    }

    /// A call carrying native value: the WETH wrap that funds an ERC20-only plan.
    ///
    /// Split from [`Deployer::call`] on purpose. §34's claim is that the *arbitrage*
    /// transaction has no value, and one method with a value parameter would let an
    /// `execute` be built with native attached by accident; wrapping is a separate step
    /// with its own label, and a deposit of nothing is a step whose evidence would claim a
    /// funding action that moved nothing.
    pub async fn funding_call(
        &mut self,
        head: ChainHead,
        to: Address,
        calldata: Bytes,
        value: U256,
        label: String,
    ) -> Result<Step> {
        if value.is_zero() {
            return Err(ExecutionError::InvalidIntent(format!(
                "{label}: a funding step with zero attached — a zero-value call belongs to \
                 [`Deployer::call`]"
            )));
        }
        self.send(
            head,
            Some(to),
            calldata,
            value,
            self.policy.call_gas_limit,
            label,
        )
        .await
    }

    /// The ladder's one transaction path: bind the head, price it, prove the wallet can pay
    /// for it, take the nonce, sign, send once, read the receipt.
    ///
    /// The reads are ordered so every refusal happens before the send: a step that stops on
    /// a reorged head, an unaffordable ceiling, or a busy lane costs the reads it named and
    /// no gas.
    async fn send(
        &mut self,
        head: ChainHead,
        to: Option<Address>,
        input: Bytes,
        value: U256,
        gas_limit: u64,
        label: String,
    ) -> Result<Step> {
        let sender = self.signer.address()?;
        let binding = read_binding(
            self.abilities.chain.as_ref(),
            BlockNumber(head.number),
            head.hash,
        )
        .await;
        let binding_word = match binding {
            BlockBinding::Confirmed { number, hash } => {
                format!("confirmed: block {number} is {hash:#x}")
            }
            BlockBinding::Reorged {
                number,
                pinned,
                found,
            } => {
                return Err(ExecutionError::ChainMismatch(format!(
                    "{label}: block {number} now reads {found:#x} and this step is priced \
                     against {pinned:#x}, so the fee and nonce reads below would describe a \
                     state the endpoint no longer holds"
                )));
            }
            BlockBinding::Unverified(reason) => {
                return Err(ExecutionError::ChainRead(format!(
                    "{label}: the head could not be verified — {reason}"
                )));
            }
        };

        let fees = Arc::clone(&self.abilities.fees);
        let reading = fees
            .fee_reading(
                head.number,
                head.hash,
                self.policy.tx_type,
                &self.policy.fee,
            )
            .await?;
        let (max_fee_per_gas, max_priority_fee_per_gas) =
            reading.fields_for(self.policy.tx_type)?;
        let maximum_spend = reading
            .maximum_gas_cost(gas_limit)?
            .checked_add(value)
            .ok_or_else(|| {
                ExecutionError::InvalidIntent(format!(
                    "{label}: gas_limit {gas_limit} times the fee ceiling plus value {value} \
                     does not fit in 256 bits"
                ))
            })?;
        let balance = fees.balance(sender, head.number).await?;
        if balance < maximum_spend {
            return Err(ExecutionError::InsufficientBalance(format!(
                "{label}: {sender} holds {balance} wei at block {} and this step's ceiling is \
                 {maximum_spend} wei (gas_limit {gas_limit} at the fee ceiling, plus value \
                 {value}); the balance is read and not assumed (§33)",
                head.number
            )));
        }

        let nonces = Arc::clone(&self.abilities.nonces);
        let nonce_reading = nonces.nonce(sender).await?;
        let nonce = self
            .lane
            .allocate(&nonce_reading)
            .map_err(|e| ExecutionError::NonceUnavailable(format!("{label}: {e}")))?;

        let unsigned = UnsignedTransaction {
            tx_type: self.policy.tx_type,
            chain_id: self.chain_id,
            nonce,
            to,
            value,
            gas_limit,
            input: input.clone(),
            access_list: Vec::new(),
            max_priority_fee_per_gas,
            max_fee_per_gas,
        };
        let (signed, recovered) = self.signer.sign_and_recover(&unsigned)?;
        let transaction_hash = signed.hash();

        let submitter = Arc::clone(&self.abilities.submitter);
        if !submitter.may_submit() {
            self.lane.release(sender, nonce)?;
            return Err(ExecutionError::ModeGate(format!(
                "{label}: the endpoint at {} may not broadcast, so these bytes were built and \
                 signed and never sent",
                submitter.endpoint().name()
            )));
        }
        let outcome = submitter.submit(&signed).await?;
        let submission_word = outcome.status_word().to_string();
        let detail = match &outcome {
            SubmissionOutcome::Accepted { detail, .. } => detail.clone(),
            SubmissionOutcome::Rejected { reason, .. }
            | SubmissionOutcome::Unknown { reason, .. } => reason.clone(),
        };
        if !matches!(outcome, SubmissionOutcome::Accepted { .. }) {
            // A definite refusal proves nothing is in flight, so the lane goes back. An
            // unknown answer keeps it held: the next step must fail on the lane rather than
            // send a second transaction over the same nonce (§25).
            if outcome.proven_not_in_flight() {
                self.lane.release(sender, nonce)?;
            }
            return Err(match &outcome {
                SubmissionOutcome::Rejected { .. } => ExecutionError::SubmissionRejected(format!(
                    "{label}: the endpoint answered {submission_word} — {detail}"
                )),
                _ => ExecutionError::SubmissionUnknown(format!(
                    "{label}: the endpoint answered {submission_word} — {detail}; nothing is \
                     known about whether these bytes were accepted, so the lane stays held"
                )),
            });
        }

        let expected = ExpectedTransaction {
            transaction_hash,
            sender,
            target: to,
            nonce,
            chain_id: self.chain_id,
        };
        let chain = Arc::clone(&self.abilities.chain);
        let reader = Arc::clone(&self.abilities.submitter);
        let tracked = self
            .tracker
            .track(
                &expected,
                move || {
                    let reader = Arc::clone(&reader);
                    async move {
                        reader
                            .receipt(transaction_hash)
                            .await
                            .map_err(|e| e.to_string())
                    }
                },
                move |number| {
                    let chain = Arc::clone(&chain);
                    async move {
                        chain
                            .block_hash_at(BlockNumber(number))
                            .await
                            .map_err(|e| e.to_string())
                    }
                },
            )
            .await;

        let (status, receipt) = match tracked {
            TrackedReceipt::Included(receipt) => (ReceiptStatus::Included, Some(receipt)),
            TrackedReceipt::Reverted(receipt) => (ReceiptStatus::Reverted, Some(receipt)),
            TrackedReceipt::Unbound { receipt, reason } => {
                // The receipt exists and is not ours, or its block does not match it. Either
                // way the transaction is resolved on the chain even though this step cannot
                // vouch for the answer, so the lane goes back and the finding is reported.
                self.lane.release(sender, nonce)?;
                let _ = receipt;
                return Err(ExecutionError::ReceiptBinding(format!(
                    "{label}: a receipt came back and did not bind — {reason}"
                )));
            }
            TrackedReceipt::Pending {
                attempts,
                last_answer,
            } => {
                return Err(ExecutionError::ReceiptTimeout(format!(
                    "{label}: {attempts} receipt polls and no answer. The transaction may \
                     still land, so the lane stays held ({last_answer})"
                )));
            }
        };
        self.lane.release(sender, nonce)?;

        Ok(Step {
            label,
            chain_id: self.chain_id,
            nonce,
            tx_type: self.policy.tx_type.name().to_string(),
            to,
            value_wei: value,
            gas_limit,
            input_bytes: input.len(),
            input_hash: keccak256(input.as_ref()),
            transaction_hash,
            recovered_sender: recovered,
            fee_provenance: reading.provenance,
            max_fee_per_gas: reading.max_fee_per_gas,
            head_number: head.number,
            head_binding: binding_word,
            balance_wei: balance,
            maximum_spend_wei: maximum_spend,
            submission_word,
            status,
            receipt,
            detail,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use alloy_primitives::address;

    /// The real operator EOA this milestone deploys from, as an address and not a key
    /// (§35: the address is evidence, the key is never in this file).
    const OPERATOR: Address = address!("d450630c1c55b1c7df1ebf7eeaee1fffb45e520c");

    /// The address a creation at nonce 7 from [`OPERATOR`] produces, computed outside this
    /// repository: `rlp([sender, nonce])` from a Python RLP encoder, hashed with
    /// `openssl dgst -keccak-256`. That keccak implementation was controlled first, against
    /// [`alloy_primitives::keccak256`] on the empty input and on `abc` — in `evm-protocol`'s
    /// `keccak_witness_matches_the_shell_tool`, which is where a published digest may be typed:
    /// §17's key scan reads this crate's files whole, and a 64-hex-digit digest is the shape of
    /// a private key. So the witness and this crate agree on the hash function and differ only
    /// on the encoder.
    ///
    /// The point of pinning a number this repository did not compute is that a
    /// self-consistent wrong encoder would otherwise pass its own test forever.
    const CREATE_AT_NONCE_SEVEN: Address = address!("6588b08fd108fdea5732f58269de0d925ae16487");

    fn policy() -> DeployPolicy {
        DeployPolicy {
            creation_gas_limit: 2_000_000,
            call_gas_limit: 600_000,
            tx_type: TransactionType::DynamicFee,
            fee: FeePolicy::Fixed {
                max_fee_per_gas: U256::from(1_000_770u64),
                tip: U256::from(1_000_000u64),
            },
            receipt: ReceiptPolicy {
                attempts: 2,
                between_attempts: Duration::from_millis(1),
            },
        }
    }

    #[test]
    fn the_create_address_matches_a_witness_computed_outside_this_repo() {
        assert_eq!(create_address(OPERATOR, 7), CREATE_AT_NONCE_SEVEN);
    }

    #[test]
    fn the_creation_pair_is_encoded_by_hand_the_same_way() {
        // The RLP of `[address, 7]` written out from the spec rather than through the crate's
        // encoder: 21-byte string item (`0x94` + 20) plus a one-byte quantity (`0x07`) is a
        // 22-byte payload, so the list header is `0xc0 + 22 = 0xd6`.
        let mut by_hand = vec![0xd6u8, 0x94];
        by_hand.extend_from_slice(OPERATOR.as_slice());
        by_hand.push(0x07);
        let hash = keccak256(&by_hand);
        assert_eq!(&hash.as_slice()[12..], CREATE_AT_NONCE_SEVEN.as_slice());

        // Nonce 0 is the empty quantity (`0x80`), and the payload is one byte shorter — a
        // single byte of difference that a `to_be_bytes` shortcut would get wrong.
        let mut zero = vec![0xd6u8, 0x94];
        zero.extend_from_slice(OPERATOR.as_slice());
        zero.push(0x80);
        assert_eq!(
            &keccak256(&zero).as_slice()[12..],
            create_address(OPERATOR, 0).as_slice()
        );
    }

    #[test]
    fn the_constructor_argument_is_one_left_padded_word() {
        let arg = constructor_arguments(OPERATOR);
        assert_eq!(arg.len(), 32);
        assert_eq!(&arg[..12], &[0u8; 12]);
        assert_eq!(&arg[12..], OPERATOR.as_slice());
    }

    #[test]
    fn the_creation_input_is_the_code_then_the_arguments() {
        let code = vec![0x60u8; 100];
        let input = creation_input(&code, OPERATOR).expect("100 bytes is under the bound");
        assert_eq!(input.len(), 132);
        assert_eq!(&input[..100], code.as_slice());
        assert_eq!(&input[100..], constructor_arguments(OPERATOR).as_ref());
    }

    #[test]
    fn an_empty_or_oversized_creation_code_is_refused() {
        let empty = creation_input(&[], OPERATOR).expect_err("empty is not a deployment");
        assert!(
            matches!(empty, ExecutionError::InvalidIntent(_)),
            "the refusal is {empty}"
        );

        let oversized = vec![0xfeu8; MAX_INITCODE_BYTES + 1];
        let error = creation_input(&oversized, OPERATOR).expect_err("the bound holds");
        match error {
            ExecutionError::InvalidIntent(reason) => {
                assert!(
                    reason.contains(&format!("{}", MAX_INITCODE_BYTES + 1)),
                    "the refusal quotes the size it measured: {reason}"
                );
            }
            other => panic!("the wrong error class: {other}"),
        }

        // The bound itself is inclusive: exactly `MAX_INITCODE_BYTES` is legal.
        assert!(creation_input(&vec![0xfeu8; MAX_INITCODE_BYTES], OPERATOR).is_ok());
    }

    #[test]
    fn a_session_needs_submit_mode_and_a_chain_that_is_not_zero() {
        session_gate(ExecutionMode::Submit, 91_342, &policy())
            .expect("submit on a named chain is what a deployment needs");

        for mode in [ExecutionMode::BuildOnly, ExecutionMode::SignOnly] {
            let error = session_gate(mode, 91_342, &policy())
                .expect_err("a session that cannot broadcast must not be constructible");
            match error {
                ExecutionError::ModeGate(reason) => {
                    assert!(
                        reason.contains(mode.name()),
                        "the gate names the mode: {reason}"
                    )
                }
                other => panic!("the wrong error class for {mode:?}: {other}"),
            }
        }

        assert!(matches!(
            session_gate(ExecutionMode::Submit, 0, &policy()),
            Err(ExecutionError::InvalidIntent(_))
        ));
    }

    #[test]
    fn a_zero_gas_limit_is_refused_before_anything_is_sent() {
        for limit in [0u64, 0] {
            let mut broken = policy();
            broken.creation_gas_limit = limit;
            assert!(matches!(
                broken.validate(),
                Err(ExecutionError::InvalidIntent(_))
            ));
            broken.creation_gas_limit = 1;
            broken.call_gas_limit = limit;
            assert!(matches!(
                broken.validate(),
                Err(ExecutionError::InvalidIntent(_))
            ));
        }
        assert!(policy().validate().is_ok());
    }
}
