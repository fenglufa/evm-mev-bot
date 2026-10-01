//! §9/§10/§13: the builder is a transformation, not a strategy.
//!
//! It takes a [`TransactionIntent`] and produces the bytes to be signed, or refuses
//! with a named reason. It cannot reach an opportunity, a pool, a reserve, a fee
//! formula, or the risk thresholds — there is no argument through which it could — and
//! the checks it runs are the §10 list plus two that §34 and §4 make necessary: a
//! transaction whose simulation was only possible under a state override is not a
//! transaction a real account can send, and a simulated sequence of more than one step
//! is not one transaction.
//!
//! Gas comes from [`GasPolicy`], i.e. from the gas the simulation measured plus a
//! declared margin. `30_000_000` appears nowhere in this file except as the example of
//! what must not be a live transaction's limit.

use alloy_primitives::{Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};

use crate::error::{ExecutionError, Result};
use crate::intent::{ExecutionIds, TransactionIntent};
use crate::tx::{SignedTransaction, TransactionType, UnsignedTransaction};

/// How a live transaction's gas limit is derived.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GasPolicy {
    /// §13's preferred source: what the simulation measured, plus a fixed headroom in
    /// gas units. The headroom is stated as a number and not folded into a percentage
    /// because a percentage of a measured figure is a second decision about the
    /// transaction, and the builder should make only one.
    SimulationGasPlus { margin: u64 },
    /// An explicitly configured limit. Allowed for fixtures and for transactions that
    /// were never simulated (§35's validation transaction); refused for arbitrage
    /// intents, whose limit has to be traceable to a measurement.
    Configured { gas_limit: u64 },
}

impl GasPolicy {
    pub fn resolve(&self, simulated_gas_used: Option<u64>) -> Result<u64> {
        match self {
            Self::SimulationGasPlus { margin } => {
                let measured = simulated_gas_used.ok_or_else(|| {
                    ExecutionError::BuildFailed(
                        "gas policy is simulation-gas-plus-margin and the intent carries no \
                         measured gas figure"
                            .to_string(),
                    )
                })?;
                measured.checked_add(*margin).ok_or_else(|| {
                    ExecutionError::BuildFailed(format!(
                        "measured gas {measured} + margin {margin} overflows"
                    ))
                })
            }
            Self::Configured { gas_limit } => Ok(*gas_limit),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::SimulationGasPlus { margin } => format!("simulation gas + {margin}"),
            Self::Configured { gas_limit } => format!("configured limit {gas_limit}"),
        }
    }
}

/// The bounds a build has to stay inside. Every field is compared against something the
/// intent already carries; none of them is a new decision about the trade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildPolicy {
    /// §6: the chain execution was configured for. A mismatch stops the build.
    pub expected_chain_id: u64,
    /// §13's ceiling. On GIWA a real block's gas limit was measured at 60 000 000
    /// (`data/evidence/m6/probe-read-surface-2.txt`), so a limit above the chain's own
    /// block gas limit describes a transaction no node can include.
    pub maximum_gas_limit: u64,
    /// A call shorter than a 4-byte selector cannot name a function; an input longer
    /// than this bound is not something this bot was asked to build.
    pub maximum_calldata_bytes: usize,
    /// §34: refuse to build a transaction whose run needed overridden state.
    pub require_unoverridden_state: bool,
    /// The gas policy, resolved against the intent's own limit.
    pub gas: GasPolicy,
    /// Measured gas for the policy, when the caller has it (a lifecycle pass carries it
    /// from the simulation; an intent alone does not).
    pub simulated_gas_used: Option<u64>,
}

impl Default for BuildPolicy {
    fn default() -> Self {
        Self {
            // No chain is assumed. `0` means "unset", and `TransactionBuilder` refuses
            // an unset expected chain id rather than treating zero as a wildcard.
            expected_chain_id: 0,
            maximum_gas_limit: 60_000_000,
            maximum_calldata_bytes: 32_768,
            require_unoverridden_state: true,
            gas: GasPolicy::SimulationGasPlus { margin: 20_000 },
            simulated_gas_used: None,
        }
    }
}

/// The §10 list, in the order it is checked, with the reason each check would give.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Build {
    pub policy_applied: GasPolicy,
    pub gas_limit: u64,
    pub unsigned: UnsignedTransaction,
    pub signing_payload: Bytes,
    pub signing_hash: B256,
    pub sender_expected: Address,
    pub ids: ExecutionIds,
}

impl Build {
    /// The transaction this build would put on the wire once signed.
    pub fn signed(&self, signature: crate::tx::Signature) -> SignedTransaction {
        SignedTransaction::new(self.unsigned.clone(), signature)
    }
}

pub struct TransactionBuilder;

impl TransactionBuilder {
    /// §10's pre-build validation, then encoding. Every rejection names the field that
    /// failed rather than the transaction as a whole, because "BuildRejected" without a
    /// field is not actionable in a log.
    pub fn build(intent: &TransactionIntent, policy: &BuildPolicy) -> Result<Build> {
        if policy.expected_chain_id == 0 {
            return Err(ExecutionError::ChainMismatch(
                "the build policy has no expected chain id; refusing to treat an unset chain as \
                 a wildcard"
                    .to_string(),
            ));
        }
        if intent.chain_id != policy.expected_chain_id {
            return Err(ExecutionError::ChainMismatch(format!(
                "the intent is for chain {} and execution is configured for chain {}",
                intent.chain_id, policy.expected_chain_id
            )));
        }
        if intent.sender == Address::ZERO {
            return Err(ExecutionError::InvalidIntent(
                "the sender is the zero address".to_string(),
            ));
        }
        if intent.target == Address::ZERO {
            return Err(ExecutionError::InvalidIntent(format!(
                "the target is the zero address; a creation transaction is not what this bot \
                 builds (sender {})",
                intent.sender
            )));
        }
        if intent.simulated_steps != 1 {
            return Err(ExecutionError::InvalidIntent(format!(
                "this intent describes step 1 of a {}-step sequence and there is no executor \
                 contract to make it atomic (§4)",
                intent.simulated_steps
            )));
        }
        if policy.require_unoverridden_state && intent.funding.derived_from_overridden_state() {
            return Err(ExecutionError::OverrideDependent(format!(
                "the simulation this intent came from needed state overrides, so no real \
                 account could send this transaction without the same override (§33, §34); \
                 this intent says how it was funded: {}",
                intent.funding.describe()
            )));
        }
        if intent.calldata.len() > policy.maximum_calldata_bytes {
            return Err(ExecutionError::InvalidIntent(format!(
                "calldata is {} bytes, over the configured bound of {}",
                intent.calldata.len(),
                policy.maximum_calldata_bytes
            )));
        }
        if !intent.calldata.is_empty() && intent.calldata.len() < 4 {
            return Err(ExecutionError::InvalidIntent(format!(
                "calldata is {} bytes, too short to carry a 4-byte selector",
                intent.calldata.len()
            )));
        }
        if intent.nonce == u64::MAX {
            return Err(ExecutionError::NonceUnavailable(format!(
                "nonce {} is not allocatable",
                intent.nonce
            )));
        }

        let gas_limit = policy.gas.resolve(policy.simulated_gas_used)?;
        // §13 in both directions: the limit comes from the policy, and the one thing
        // that makes a policy wrong is a limit below the gas the same run measured,
        // because that transaction cannot finish for a reason the record could not
        // explain. The intent's own carried limit is evidence, not a floor to be maxed
        // against — M4's plan default (30 000 000 per step) is exactly the number §13
        // says must not become a live transaction's gas limit.
        if let Some(measured) = policy.simulated_gas_used {
            if gas_limit < measured {
                return Err(ExecutionError::BuildFailed(format!(
                    "resolved gas limit {gas_limit} is below the {} gas units this run measured",
                    measured
                )));
            }
        }
        if gas_limit == 0 {
            return Err(ExecutionError::InvalidIntent(
                "the resolved gas limit is zero".to_string(),
            ));
        }
        if gas_limit > policy.maximum_gas_limit {
            return Err(ExecutionError::InvalidIntent(format!(
                "resolved gas limit {gas_limit} exceeds the configured ceiling {}",
                policy.maximum_gas_limit
            )));
        }

        let mut unsigned = intent.unsigned();
        unsigned.gas_limit = gas_limit;
        unsigned.validate()?;
        let signing_payload = unsigned
            .signing_payload()
            .map_err(|e| ExecutionError::BuildFailed(e.to_string()))?;
        let signing_hash = alloy_primitives::keccak256(signing_payload.as_ref());
        Ok(Build {
            policy_applied: policy.gas,
            gas_limit,
            unsigned,
            signing_payload,
            signing_hash,
            sender_expected: intent.sender,
            ids: intent.ids.clone(),
        })
    }

    /// §14's re-decode: the built bytes are decoded and every field compared with what
    /// the intent asked for. `to`, `data` and nothing else would not satisfy this, so
    /// the list is the eight fields the task book names.
    pub fn round_trip(build: &Build, signed: &SignedTransaction) -> Result<RoundTrip> {
        let raw = signed.raw();
        let decoded = crate::tx::decode_raw(raw.as_ref(), crate::signer::recover_sender)?;
        let mut mismatches = Vec::new();
        let mut compare = |field: &str, expected: String, found: String| {
            if expected != found {
                mismatches.push(format!(
                    "{field}: intent says {expected}, bytes say {found}"
                ));
            }
        };
        compare(
            "chain_id",
            build.unsigned.chain_id.to_string(),
            decoded.unsigned.chain_id.to_string(),
        );
        compare(
            "nonce",
            build.unsigned.nonce.to_string(),
            decoded.unsigned.nonce.to_string(),
        );
        compare(
            "target",
            to_text(&build.unsigned.to),
            to_text(&decoded.unsigned.to),
        );
        compare(
            "value",
            build.unsigned.value.to_string(),
            decoded.unsigned.value.to_string(),
        );
        compare(
            "calldata",
            format!("0x{}", hex::encode(build.unsigned.input.as_ref())),
            format!("0x{}", hex::encode(decoded.unsigned.input.as_ref())),
        );
        compare(
            "gas_limit",
            build.unsigned.gas_limit.to_string(),
            decoded.unsigned.gas_limit.to_string(),
        );
        compare(
            "fee",
            fee_text(&build.unsigned),
            fee_text(&decoded.unsigned),
        );
        compare(
            "tx_type",
            build.unsigned.tx_type.name().to_string(),
            decoded.unsigned.tx_type.name().to_string(),
        );
        compare(
            "access_list",
            format!("{:?}", build.unsigned.access_list),
            format!("{:?}", decoded.unsigned.access_list),
        );
        let sender = decoded.sender.ok_or_else(|| {
            ExecutionError::BuildFailed("decoded transaction has no sender".to_string())
        })?;
        if sender != build.sender_expected {
            mismatches.push(format!(
                "sender: intent says {}, the signature proves {sender}",
                build.sender_expected
            ));
        }
        if !mismatches.is_empty() {
            return Err(ExecutionError::BuildFailed(format!(
                "the built bytes do not describe the intent:\n{}",
                mismatches.join("\n")
            )));
        }
        Ok(RoundTrip {
            decoded: decoded.unsigned,
            sender,
            raw_len: raw.len(),
            hash: signed.hash(),
            tx_type: signed.unsigned.tx_type,
        })
    }
}

fn fee_text(unsigned: &UnsignedTransaction) -> String {
    let fee = unsigned
        .max_fee_per_gas
        .map(|v| v.to_string())
        .unwrap_or_else(|| "absent".to_string());
    let tip = unsigned
        .max_priority_fee_per_gas
        .map(|v| v.to_string())
        .unwrap_or_else(|| "absent".to_string());
    format!("{fee}/{tip}")
}

/// How §14's target comparison names an absent `to`: as itself, never as the zero
/// address, so a creation cannot be misread as a transfer to 0x0.
fn to_text(to: &Option<Address>) -> String {
    match to {
        Some(address) => address.to_string(),
        None => "<creation>".to_string(),
    }
}

/// What a successful round trip proved. Recorded as evidence (§52).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoundTrip {
    pub decoded: UnsignedTransaction,
    pub sender: Address,
    pub raw_len: usize,
    pub hash: B256,
    pub tx_type: TransactionType,
}

/// §12's rule that no floating point enters the money path: a fee in wei is `U256`, and
/// the only arithmetic here is checked integer arithmetic.
pub fn maximum_spend(fee_per_gas: U256, gas_limit: u64, value: U256) -> Result<U256> {
    let fee = U256::from(gas_limit)
        .checked_mul(fee_per_gas)
        .ok_or_else(|| {
            ExecutionError::InvalidIntent(format!(
                "gas limit {gas_limit} * fee {fee_per_gas} overflows 256 bits"
            ))
        })?;
    fee.checked_add(value).ok_or_else(|| {
        ExecutionError::InvalidIntent(format!("fee {fee} + value {value} overflows 256 bits"))
    })
}
