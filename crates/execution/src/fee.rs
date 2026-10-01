//! §12: where a fee comes from, stated as data.
//!
//! The task book's rule is not "use EIP-1559" or "use legacy" — GIWA's own blocks
//! contain both (19 type-`0x2` and 18 type-`0x0` user transactions in one measured
//! block). The rule is that a fee field must be traceable to a read or a declared
//! policy, and that no fee number is produced by floating-point arithmetic. This module
//! holds the read ([`FeeSource`]), the policy that turns a read into two integers
//! ([`FeePolicy`]), and the record of which one was used ([`FeeReading`]).

use alloy_primitives::{Address, Bytes, B256, U256};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{ExecutionError, Result};
use crate::tx::TransactionType;

/// Which endpoint field a fee value came from. Recorded rather than inferred, because
/// `maxFeePerGas` read from a block header and `maxFeePerGas` typed into a config file
/// are two different claims about the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeeSourceKind {
    /// The pinned block's `baseFeePerGas`.
    PinnedBlockBaseFee,
    /// The node's `eth_maxPriorityFeePerGas`.
    NodeSuggestedTip,
    /// A value from configuration, with its key named in the provenance string.
    Configured,
    /// A block with no `baseFeePerGas` field at all, which makes the transaction
    /// legacy-priced.
    NoBaseFeeField,
}

/// One fee read, and the arithmetic that turned it into the two EIP-1559 fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeeReading {
    pub chain_id: u64,
    /// The block the read is valid for. A fee read against `latest` is a fee read
    /// against no state, so the pin is part of the value (§31).
    pub block_number: u64,
    pub block_hash: B256,
    pub base_fee_per_gas: Option<U256>,
    pub suggested_tip_wei: Option<U256>,
    pub max_fee_per_gas: U256,
    pub max_priority_fee_per_gas: Option<U256>,
    pub kinds: Vec<FeeSourceKind>,
    /// The formula, spelled out with the numbers in it, so a report line answers
    /// "where did this fee come from" without reading code.
    pub provenance: String,
}

impl FeeReading {
    /// The fee fields for the transaction type this reading can honestly price.
    pub fn fields_for(&self, tx_type: TransactionType) -> Result<(Option<U256>, Option<U256>)> {
        match tx_type {
            TransactionType::DynamicFee => {
                let tip = self.max_priority_fee_per_gas.ok_or_else(|| {
                    ExecutionError::InvalidIntent(
                        "an eip1559 fee reading without a tip".to_string(),
                    )
                })?;
                Ok((Some(self.max_fee_per_gas), Some(tip)))
            }
            TransactionType::Legacy => Ok((Some(self.max_fee_per_gas), None)),
        }
    }

    /// `gas_limit * max_fee_per_gas`, the number §33 compares against a balance.
    pub fn maximum_gas_cost(&self, gas_limit: u64) -> Result<U256> {
        U256::from(gas_limit)
            .checked_mul(self.max_fee_per_gas)
            .ok_or_else(|| ExecutionError::InvalidIntent("gas * fee overflows".to_string()))
    }
}

/// The endpoint's fee surface, read at a pinned block.
#[async_trait]
pub trait FeeSource {
    /// Read the base fee of `block` and the node's suggested tip, and apply `policy` to
    /// produce the fields for `tx_type`.
    ///
    /// The transaction type is an argument and not an assumption (§12): GIWA's own blocks
    /// contain both legacy and EIP-1559 user transactions, so a source that could only
    /// price one of them would be a hardcoded fee type wearing a trait.
    async fn fee_reading(
        &self,
        block_number: u64,
        block_hash: B256,
        tx_type: TransactionType,
        policy: &FeePolicy,
    ) -> Result<FeeReading>;

    /// The node's own suggestion, unprocessed. A test uses this to prove the field
    /// exists on the endpoint rather than that the code invented a default.
    async fn suggested_tip(&self) -> Result<Option<U256>>;

    /// The address's native balance at a block — the other half of §33's check, kept
    /// next to the fee because the two are only meaningful as a pair.
    async fn balance(&self, address: Address, block_number: u64) -> Result<U256>;
}

/// How a base fee and a tip become a ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeePolicy {
    /// `maxFee = baseFee * blocks_of_headroom + tip`, the standard form: the ceiling
    /// survives this many blocks' worth of base-fee growth and still pays the tip.
    /// `headroom_blocks` is a count, not a multiplier chosen at the call site.
    BaseFeeHeadroom { headroom_blocks: u64 },
    /// A fixed ceiling, from configuration. Used when an operator wants a bound the
    /// node cannot move; `tip` is still required so the priority field is never
    /// defaulted to zero by omission.
    Fixed { max_fee_per_gas: U256, tip: U256 },
}

impl FeePolicy {
    /// Turn the two reads into a `FeeReading`. Every step is checked integer arithmetic;
    /// a missing base fee with a policy that needs one is an error, not a zero.
    pub fn apply(
        &self,
        chain_id: u64,
        block_number: u64,
        block_hash: B256,
        base_fee: Option<U256>,
        tip: Option<U256>,
        tx_type: TransactionType,
    ) -> Result<FeeReading> {
        let mut kinds = Vec::new();
        let (max_fee, max_tip, provenance) = match (self, base_fee, tip, tx_type) {
            (
                Self::BaseFeeHeadroom { headroom_blocks },
                Some(base),
                Some(tip),
                TransactionType::DynamicFee,
            ) => {
                kinds.push(FeeSourceKind::PinnedBlockBaseFee);
                kinds.push(FeeSourceKind::NodeSuggestedTip);
                let ceiling = base
                    .checked_mul(U256::from(*headroom_blocks))
                    .and_then(|scaled| scaled.checked_add(tip))
                    .ok_or_else(|| {
                        ExecutionError::InvalidIntent(format!(
                            "base fee {base} * {headroom_blocks} + tip {tip} overflows 256 bits"
                        ))
                    })?;
                (
                    ceiling,
                    Some(tip),
                    format!(
                        "eip1559 maxFeePerGas = baseFee({base}) * {headroom_blocks} + tip({tip}) = {ceiling}, \
                         read at block {block_number}"
                    ),
                )
            }
            (Self::BaseFeeHeadroom { .. }, _, _, TransactionType::Legacy) => {
                return Err(ExecutionError::InvalidIntent(
                    "a base-fee-headroom policy prices an eip1559 ceiling; a legacy transaction \
                     needs a single gasPrice, so choose FeePolicy::Fixed"
                        .to_string(),
                ))
            }
            (Self::BaseFeeHeadroom { .. }, None, _, _) => {
                return Err(ExecutionError::InvalidIntent(format!(
                    "block {block_number} carries no baseFeePerGas, so a headroom policy has \
                     nothing to scale; this chain section is legacy-priced"
                )));
            }
            (Self::BaseFeeHeadroom { .. }, _, None, _) => {
                return Err(ExecutionError::InvalidIntent(
                    "the endpoint gave no suggested priority fee; defaulting a tip to zero would \
                     be a decision about the transaction, not a read"
                        .to_string(),
                ));
            }
            (
                Self::Fixed {
                    max_fee_per_gas,
                    tip,
                },
                base,
                _,
                _,
            ) => {
                kinds.push(FeeSourceKind::Configured);
                if base.is_some() {
                    kinds.push(FeeSourceKind::PinnedBlockBaseFee);
                }
                (
                    *max_fee_per_gas,
                    Some(*tip),
                    format!("configured ceiling {max_fee_per_gas} with tip {tip}"),
                )
            }
        };
        if tx_type == TransactionType::DynamicFee {
            let max_tip = max_tip.ok_or_else(|| {
                ExecutionError::InvalidIntent("eip1559 reading without a tip".to_string())
            })?;
            if max_tip > max_fee {
                return Err(ExecutionError::InvalidIntent(format!(
                    "tip {max_tip} exceeds ceiling {max_fee}"
                )));
            }
        }
        Ok(FeeReading {
            chain_id,
            block_number,
            block_hash,
            base_fee_per_gas: base_fee,
            suggested_tip_wei: tip,
            max_fee_per_gas: max_fee,
            max_priority_fee_per_gas: if tx_type == TransactionType::Legacy {
                None
            } else {
                max_tip
            },
            kinds,
            provenance,
        })
    }
}

/// Unused in the trait but kept where a caller that has calldata in hand can name the
/// digest §52 records without importing the codec.
pub fn calldata_hash(calldata: &Bytes) -> B256 {
    alloy_primitives::keccak256(calldata.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(byte: u8) -> B256 {
        B256::left_padding_from(&[byte])
    }

    #[test]
    fn a_headroom_ceiling_is_the_sum_of_a_scaled_base_fee_and_the_tip() {
        let reading = FeePolicy::BaseFeeHeadroom { headroom_blocks: 2 }
            .apply(
                91_342,
                100,
                hash(1),
                Some(U256::from(371u64)),
                Some(U256::from(1_000_000u64)),
                TransactionType::DynamicFee,
            )
            .unwrap();
        assert_eq!(reading.max_fee_per_gas, U256::from(371u64 * 2 + 1_000_000));
        assert_eq!(
            reading.max_priority_fee_per_gas,
            Some(U256::from(1_000_000u64))
        );
        assert!(reading
            .provenance
            .contains("baseFee(371) * 2 + tip(1000000)"));
    }

    #[test]
    fn a_missing_base_fee_is_a_refusal_and_not_a_zero() {
        let error = FeePolicy::BaseFeeHeadroom { headroom_blocks: 2 }
            .apply(
                91_342,
                100,
                hash(1),
                None,
                Some(U256::from(1u64)),
                TransactionType::DynamicFee,
            )
            .unwrap_err();
        assert!(error.to_string().contains("no baseFeePerGas"), "{error}");
    }

    #[test]
    fn a_missing_tip_is_a_refusal_because_defaulting_it_would_be_a_decision() {
        let error = FeePolicy::BaseFeeHeadroom { headroom_blocks: 2 }
            .apply(
                91_342,
                100,
                hash(1),
                Some(U256::from(1u64)),
                None,
                TransactionType::DynamicFee,
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("no suggested priority fee"),
            "{error}"
        );
    }

    #[test]
    fn a_legacy_transaction_needs_one_price_and_gets_no_tip_field() {
        let reading = FeePolicy::Fixed {
            max_fee_per_gas: U256::from(1_245_502u64),
            tip: U256::from(0u64),
        }
        .apply(
            91_342,
            100,
            hash(1),
            Some(U256::from(371u64)),
            None,
            TransactionType::Legacy,
        )
        .unwrap();
        assert_eq!(reading.max_priority_fee_per_gas, None);
        assert_eq!(reading.max_fee_per_gas, U256::from(1_245_502u64));
        let (max, tip) = reading.fields_for(TransactionType::Legacy).unwrap();
        assert_eq!(tip, None);
        assert_eq!(max, Some(U256::from(1_245_502u64)));
    }

    #[test]
    fn the_headroom_policy_refuses_to_price_a_legacy_transaction() {
        let error = FeePolicy::BaseFeeHeadroom { headroom_blocks: 2 }
            .apply(
                1,
                1,
                hash(2),
                Some(U256::from(1u64)),
                Some(U256::from(1u64)),
                TransactionType::Legacy,
            )
            .unwrap_err();
        assert!(error.to_string().contains("single gasPrice"), "{error}");
    }

    #[test]
    fn the_maximum_gas_cost_is_the_product_a_balance_check_needs() {
        let reading = FeePolicy::Fixed {
            max_fee_per_gas: U256::from(2_000u64),
            tip: U256::from(1u64),
        }
        .apply(1, 1, hash(3), None, None, TransactionType::Legacy)
        .unwrap();
        assert_eq!(
            reading.maximum_gas_cost(21_000).unwrap(),
            U256::from(42_000_000u64)
        );
    }

    #[test]
    fn a_tip_larger_than_the_ceiling_is_refused_rather_than_clamped() {
        // A node charges min(maxFee, base+tip), so a reading whose tip exceeds its own
        // ceiling describes a transaction that will not price the way it reads.
        let error = FeePolicy::Fixed {
            max_fee_per_gas: U256::from(5u64),
            tip: U256::from(6u64),
        }
        .apply(
            1,
            1,
            hash(4),
            None,
            Some(U256::from(6u64)),
            TransactionType::DynamicFee,
        )
        .unwrap_err();
        assert!(error.to_string().contains("exceeds ceiling"), "{error}");
    }

    #[test]
    fn a_byte_input_that_is_not_a_fee_reading_stays_an_error() {
        // The reading's fields are U256 end to end: a value that would silently widen
        // (an address-shaped blob, here) is refused by the type, and the checked
        // multiply refuses a gas limit that cannot be represented.
        assert!(FeeReading {
            chain_id: 1,
            block_number: 1,
            block_hash: hash(5),
            base_fee_per_gas: None,
            suggested_tip_wei: None,
            max_fee_per_gas: U256::MAX,
            max_priority_fee_per_gas: None,
            kinds: vec![FeeSourceKind::Configured],
            provenance: "unrepresentable ceiling".to_string(),
        }
        .maximum_gas_cost(u64::MAX)
        .is_err());
    }
}
