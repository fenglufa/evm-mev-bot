//! §10/§35/§37: the whole bill for a transaction that actually ran.
//!
//! M6 measured the gap this module closes. The receipt recorded in
//! `data/evidence/m6/probe-submission-surface.txt` charged `gasUsed = 0xb8a6` (47 270) at
//! `effectiveGasPrice = 0xea12c` (958 764) — an L2 bill of 45 320 774 280 wei — and
//! *also* an `l1Fee = 0x1b7cc1c00` (7 378 574 336 wei), another 16% taken from the same
//! wallet by the sequencer for publishing the transaction's bytes to L1. A model that
//! reads `gas_limit × max_fee_per_gas` as the total is therefore not merely conservative:
//! it is a different quantity from the one the wallet was charged, which is §10's point
//! and the reason M7 must not decide profitability with it.
//!
//! Three rules follow, and they are the whole content of this file:
//!
//! * the L1 charge is a **field**, never a footnote folded into the gas line
//!   ([`crate::receipt`] deliberately excludes it from `l2_cost_wei`);
//! * every L1 number carries the read that produced it ([`L1FeeSource`]), because §36
//!   forbids a report that says "L1 fee included" without naming the source;
//! * a missing L1 read is recorded as *missing* and its total is a lower bound, because
//!   §35 forbids the alternative — presenting zero as though it were a measured fee.
//!
//! Nothing here reads a node. A cost line is built from a [`Receipt`] the tracker has
//! already bound to our transaction (§27), so a cost can never be claimed for a transaction
//! the chain has not confirmed ours.

use alloy_primitives::{B256, U256};
use serde::{Deserialize, Serialize};

use crate::error::{ExecutionError, Result};
use crate::receipt::Receipt;

/// §36: where an L1 number came from, because the report has to say.
///
/// The three variants are not flavors of the same fact. `ReceiptField` is what the chain
/// charged; `OracleEstimate` is what the chain's own fee oracle says it *will* charge,
/// which is the number the preflight gate can legitimately use and the profit evidence
/// may not; `Unreadable` is the absence of both, and the type keeps that from becoming a
/// zero.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum L1FeeSource {
    /// The `l1Fee` field of `eth_getTransactionReceipt` for this transaction.
    ReceiptField {
        block_number: u64,
        /// The adapter's own provenance sentence for the read, copied from the receipt so
        /// the evidence names the endpoint and method rather than this crate's summary.
        read_by: String,
    },
    /// `GasPriceOracle(0x420000000000000000000000000000000000000f).getL1Fee(bytes)` called
    /// against a pinned block — measured by the chain, but before the charge exists.
    OracleEstimate {
        amount: U256,
        block_number: u64,
        read_by: String,
    },
    /// No read produced a number. Whatever total this module reports is a lower bound.
    Unreadable { reason: String },
}

impl L1FeeSource {
    /// The number this source carries, if it carries one.
    pub fn amount(&self) -> Option<U256> {
        match self {
            Self::ReceiptField { .. } | Self::Unreadable { .. } => None,
            Self::OracleEstimate { amount, .. } => Some(*amount),
        }
    }

    /// Whether this is a charge the chain applied, as opposed to a forecast or a gap.
    /// Profit evidence accepts only this; preflight evidence accepts an estimate.
    pub fn is_charged(&self) -> bool {
        matches!(self, Self::ReceiptField { .. })
    }

    /// §36's line in the report: the field or method, and the block it was read at.
    pub fn describe(&self) -> String {
        match self {
            Self::ReceiptField {
                block_number,
                read_by,
            } => format!("receipt field `l1Fee`, {read_by}, in block {block_number}"),
            Self::OracleEstimate {
                block_number,
                read_by,
                ..
            } => format!(
                "estimate only — `getL1Fee(bytes)` read from the GasPriceOracle predeploy \
                 at block {block_number} ({read_by}); the charge itself was never read"
            ),
            Self::Unreadable { reason } => {
                format!("not read: {reason}; the total below is a lower bound, not a bill")
            }
        }
    }
}

/// §37's `ExecutionCostEvidence`, field for field.
///
/// `total_execution_cost` is the only place the two halves are added, and it is a
/// `U256` sum of `l2_fee` and `l1_fee` — not `gas_limit × max_fee`, which is
/// [`crate::tx::UnsignedTransaction::maximum_cost_wei`]'s ceiling and belongs before a
/// transaction, not after it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionCostEvidence {
    pub transaction_hash: B256,
    pub gas_used: u64,
    pub effective_gas_price: U256,
    /// `gas_used × effective_gas_price`: what the EVM charged.
    pub l2_fee: U256,
    /// What the sequencer charged for L1 data availability. Zero **only** together with
    /// [`L1FeeSource::Unreadable`], which is why the source is a field and not a comment.
    pub l1_fee: U256,
    pub l1_fee_source: L1FeeSource,
    /// `l2_fee + l1_fee`, or a lower bound of it when the L1 line is unmeasured.
    pub total_execution_cost: U256,
    /// §37's `source` for the whole line: how `gas_used` and the price were read.
    pub source: String,
}

impl ExecutionCostEvidence {
    /// One bound receipt, one line.
    pub fn from_receipt(receipt: &Receipt) -> Result<Self> {
        let l2_fee = receipt.l2_cost_wei().ok_or_else(|| {
            ExecutionError::ChainRead(format!(
                "gas_used {} × effective_gas_price {} does not fit in 256 bits; a receipt \
                 whose bill cannot be computed is not a cost line",
                receipt.gas_used, receipt.effective_gas_price
            ))
        })?;
        let (l1_fee, l1_fee_source) = match receipt.l1_fee {
            Some(amount) => (
                amount,
                L1FeeSource::ReceiptField {
                    block_number: receipt.block_number,
                    read_by: receipt.provenance.clone(),
                },
            ),
            None => (
                U256::ZERO,
                L1FeeSource::Unreadable {
                    reason: format!(
                        "the receipt for {:#x} carries no `l1Fee` field",
                        receipt.transaction_hash
                    ),
                },
            ),
        };
        let total_execution_cost = l2_fee.checked_add(l1_fee).ok_or_else(|| {
            ExecutionError::ChainRead(
                "the L2 bill and the L1 fee do not fit in one 256-bit total".to_string(),
            )
        })?;
        Ok(Self {
            transaction_hash: receipt.transaction_hash,
            gas_used: receipt.gas_used,
            effective_gas_price: receipt.effective_gas_price,
            l2_fee,
            l1_fee,
            l1_fee_source,
            total_execution_cost,
            source: format!(
                "gas_used and effectiveGasPrice from the bound receipt: {}",
                receipt.provenance
            ),
        })
    }

    /// Whether every wei in [`ExecutionCostEvidence::total_execution_cost`] was charged
    /// and read. [`crate::profit`] refuses to call a run profitable without this.
    pub fn is_fully_measured(&self) -> bool {
        self.l1_fee_source.is_charged()
    }

    pub fn describe(&self) -> String {
        format!(
            "tx {:#x}: l2 {} wei + l1 {} wei = {} wei; l1 source: {}",
            self.transaction_hash,
            self.l2_fee,
            self.l1_fee,
            self.total_execution_cost,
            self.l1_fee_source.describe()
        )
    }
}

/// The bill for a whole arbitrage.
///
/// M7's trade is carried by a sequence of transactions because §4 allows no executor
/// contract, so the profit model needs one total and the report needs the per-transaction
/// lines §37 asks for. Keeping both in one type is what stops a summary from being
/// computed from a different set of transactions than the one the evidence lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceCost {
    pub lines: Vec<ExecutionCostEvidence>,
    pub transaction_count: usize,
    pub l2_fee_total: U256,
    pub l1_fee_total: U256,
    pub total_execution_cost: U256,
}

impl SequenceCost {
    /// Sum the lines. An empty sequence is an error rather than a zero: "the arbitrage
    /// cost nothing" and "there was no arbitrage transaction" must not be the same
    /// sentence in the evidence.
    pub fn new(lines: Vec<ExecutionCostEvidence>) -> Result<Self> {
        if lines.is_empty() {
            return Err(ExecutionError::Evidence(
                "a cost sequence with no transactions proves nothing; a run that sent \
                 nothing has no execution cost to report and should say so instead of \
                 summing an empty list"
                    .to_string(),
            ));
        }
        let mut l2_fee_total = U256::ZERO;
        let mut l1_fee_total = U256::ZERO;
        for line in &lines {
            l2_fee_total = l2_fee_total.checked_add(line.l2_fee).ok_or_else(|| {
                ExecutionError::Evidence("the L2 bills of this sequence overflow".to_string())
            })?;
            l1_fee_total = l1_fee_total.checked_add(line.l1_fee).ok_or_else(|| {
                ExecutionError::Evidence("the L1 fees of this sequence overflow".to_string())
            })?;
        }
        let total_execution_cost = l2_fee_total.checked_add(l1_fee_total).ok_or_else(|| {
            ExecutionError::Evidence("this sequence's total execution cost overflows".to_string())
        })?;
        Ok(Self {
            transaction_count: lines.len(),
            l2_fee_total,
            l1_fee_total,
            total_execution_cost,
            lines,
        })
    }

    pub fn is_fully_measured(&self) -> bool {
        self.lines
            .iter()
            .all(ExecutionCostEvidence::is_fully_measured)
    }

    /// The receipts [`crate::profit`] is accounting for: one hash per cost line, in the
    /// order the lane sent them.
    pub fn transaction_hashes(&self) -> Vec<B256> {
        self.lines
            .iter()
            .map(|line| line.transaction_hash)
            .collect()
    }

    /// §36's requirement at sequence level: the report may not say the L1 fee was counted
    /// without saying how six times over.
    pub fn describe_l1_sources(&self) -> Vec<String> {
        self.lines
            .iter()
            .map(|line| line.l1_fee_source.describe())
            .collect()
    }
}

/// §26's pre-submission line: what the wallet has to be able to cover *before* anything
/// is signed.
///
/// This is the shape §10 says `gas_limit × max_fee_per_gas` alone is not — the L2 ceiling
/// plus an L1 estimate — and it is kept a distinct type from [`ExecutionCostEvidence`]
/// because a ceiling is not a bill: it is the number the balance check uses, it is always
/// greater than or equal to the real cost, and a run that never sent a transaction has one
/// of these and nothing of the other.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EstimatedCost {
    pub gas_limit: u64,
    pub max_fee_per_gas: U256,
    pub value_wei: U256,
    /// `gas_limit × max_fee_per_gas + value`.
    pub l2_ceiling: U256,
    pub l1_fee_estimate: U256,
    pub l1_fee_source: L1FeeSource,
    /// The ceiling the balance check runs against.
    pub total_ceiling_wei: U256,
    pub source: String,
}

impl EstimatedCost {
    /// `l1_fee_source` must be [`L1FeeSource::OracleEstimate`] or
    /// [`L1FeeSource::Unreadable`]: a receipt field cannot exist before the transaction
    /// has been sent, so an assembler handed one is being lied to about the stage.
    pub fn new(
        gas_limit: u64,
        max_fee_per_gas: U256,
        value_wei: U256,
        l1_fee_source: L1FeeSource,
    ) -> Result<Self> {
        if l1_fee_source.is_charged() {
            return Err(ExecutionError::Evidence(
                "a pre-submission estimate was handed a receipt's `l1Fee`: the charge it \
                 names cannot exist yet, so either the stage is wrong or the number is \
                 borrowed from another transaction"
                    .to_string(),
            ));
        }
        let gas_ceiling = U256::from(gas_limit)
            .checked_mul(max_fee_per_gas)
            .ok_or_else(|| {
                ExecutionError::Evidence("gas_limit × max_fee_per_gas overflows".to_string())
            })?;
        let l2_ceiling = gas_ceiling.checked_add(value_wei).ok_or_else(|| {
            ExecutionError::Evidence(
                "the L2 ceiling plus the transferred value overflows".to_string(),
            )
        })?;
        let l1_fee_estimate = l1_fee_source.amount().unwrap_or(U256::ZERO);
        let total_ceiling_wei = l2_ceiling.checked_add(l1_fee_estimate).ok_or_else(|| {
            ExecutionError::Evidence("this ceiling plus the L1 estimate overflows".to_string())
        })?;
        Ok(Self {
            gas_limit,
            max_fee_per_gas,
            value_wei,
            l2_ceiling,
            l1_fee_estimate,
            total_ceiling_wei,
            source: format!(
                "ceiling from the transaction's own fields; l1 line: {}",
                l1_fee_source.describe()
            ),
            l1_fee_source,
        })
    }

    /// Whether the ceiling covers the L1 half. When it does not, a wallet that passes this
    /// check can still fail on chain for a reason the gate did not price — which is a
    /// preflight finding to record, not a number to silently absorb.
    pub fn includes_l1(&self) -> bool {
        !matches!(self.l1_fee_source, L1FeeSource::Unreadable { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Address;

    /// The numbers are the ones GIWA charged, read out of
    /// `data/evidence/m6/probe-submission-surface.txt`: `gasUsed = 0xb8a6`,
    /// `effectiveGasPrice = 0xea12c`, `l1Fee = 0x1b7cc1c00`. A test built on invented
    /// quantities would pin this module's arithmetic to itself instead of to the chain.
    fn receipt() -> Receipt {
        Receipt {
            transaction_hash: B256::left_padding_from(&[0xab]),
            block_number: 37_500_000,
            block_hash: B256::left_padding_from(&[0x23]),
            transaction_index: 1,
            success: true,
            gas_used: 0xb8a6,
            effective_gas_price: U256::from(0xea12cu64),
            cumulative_gas_used: Some(U256::from(0xb8a6u64)),
            from: Address::from_slice(&[1u8; 20]),
            to: Some(Address::from_slice(&[2u8; 20])),
            contract_address: None,
            tx_type: Some(2),
            logs: Vec::new(),
            l1_fee: Some(U256::from(0x1b7cc1c00u64)),
            l1_gas_price: Some(U256::from(0x40e2b895u64)),
            l1_gas_used: Some(U256::from(0x640u64)),
            l1_base_fee_scalar: Some(U256::from(1_368u64)),
            l1_blob_base_fee: Some(U256::from(62_294_004u64)),
            l1_blob_base_fee_scalar: Some(U256::from(801_949u64)),
            provenance: "eth_getTransactionReceipt over the configured GIWA RPC URL \
                         (public http rpc)"
                .to_string(),
        }
    }

    fn line(l1_fee: Option<U256>) -> ExecutionCostEvidence {
        let mut receipt = receipt();
        receipt.l1_fee = l1_fee;
        ExecutionCostEvidence::from_receipt(&receipt).unwrap()
    }

    #[test]
    fn the_real_receipt_bill_is_the_l2_charge_plus_the_l1_charge() {
        let cost = line(Some(U256::from(0x1b7cc1c00u64)));
        // Each of these products was computed from the hex quantities in the evidence file
        // rather than copied from it, and the three are deliberately different numbers: a
        // test that asserted only `total != 0` would pass with the L1 fee dropped.
        assert_eq!(cost.l2_fee, U256::from(45_320_774_280u64));
        assert_eq!(cost.l1_fee, U256::from(7_378_574_336u64));
        assert_eq!(cost.total_execution_cost, U256::from(52_699_348_616u64));
        assert!(cost.is_fully_measured());
        assert_eq!(cost.gas_used, 47_270);
        assert_eq!(cost.effective_gas_price, U256::from(958_764u64));
    }

    #[test]
    fn a_receipt_that_never_answered_the_l1_question_is_not_a_receipt_with_a_zero_fee() {
        // §35's ban on `l1 fee = 0` is only enforceable if the model can tell "charged
        // nothing" from "did not read". Here the total equals the L2 bill, and the one
        // thing that keeps that from being a false account is the flag the report reads.
        let cost = line(None);
        assert_eq!(cost.l1_fee, U256::ZERO);
        assert!(matches!(cost.l1_fee_source, L1FeeSource::Unreadable { .. }));
        assert!(!cost.is_fully_measured());
        assert_eq!(cost.total_execution_cost, cost.l2_fee);
        assert!(cost.l1_fee_source.describe().contains("lower bound"));
    }

    #[test]
    fn an_oracle_estimate_is_a_forecast_and_never_counts_as_charged() {
        // The preflight gate may use this; profit evidence may not. Collapsing the two is
        // how a run comes to report a cost it never paid, or to pay one it never priced.
        let estimate = L1FeeSource::OracleEstimate {
            amount: U256::from(7_000_000_000u64),
            block_number: 37_530_593,
            read_by: "eth_call getL1Fee(bytes) at 0x4200…000f".to_string(),
        };
        assert_eq!(estimate.amount(), Some(U256::from(7_000_000_000u64)));
        assert!(!estimate.is_charged());
        assert!(estimate.describe().contains("estimate only"));

        let charged = L1FeeSource::ReceiptField {
            block_number: 1,
            read_by: "eth_getTransactionReceipt".to_string(),
        };
        assert!(charged.is_charged());
        // A receipt field carries no amount of its own: the number lives on the cost line,
        // so the source cannot be mistaken for a second copy that might disagree.
        assert_eq!(charged.amount(), None);
    }

    #[test]
    fn six_transaction_lines_become_one_bill_without_losing_their_own_hashes() {
        let lines = (0..6).map(|i| {
            let mut receipt = receipt();
            receipt.transaction_hash = B256::left_padding_from(&[i + 1]);
            ExecutionCostEvidence::from_receipt(&receipt).unwrap()
        });
        let cost = SequenceCost::new(lines.collect()).unwrap();
        assert_eq!(cost.transaction_count, 6);
        assert_eq!(cost.l2_fee_total, U256::from(6 * 45_320_774_280u128));
        assert_eq!(cost.l1_fee_total, U256::from(6 * 7_378_574_336u128));
        assert_eq!(
            cost.total_execution_cost,
            U256::from(6 * 52_699_348_616u128)
        );
        assert!(cost.is_fully_measured());
        assert_eq!(
            cost.transaction_hashes(),
            (1..7u8)
                .map(|i| B256::left_padding_from(&[i]))
                .collect::<Vec<_>>()
        );
        assert_eq!(cost.describe_l1_sources().len(), 6);
    }

    #[test]
    fn one_unmeasured_line_makes_the_whole_sequence_unmeasured() {
        let cost =
            SequenceCost::new(vec![line(Some(U256::from(0x1b7cc1c00u64))), line(None)]).unwrap();
        assert!(!cost.is_fully_measured());
        // The sum still adds what was read — the missing line contributes zero, and the
        // flag above is what stops that from being read as a complete account.
        assert_eq!(
            cost.total_execution_cost,
            U256::from(45_320_774_280u64 + 52_699_348_616u64)
        );
    }

    #[test]
    fn an_empty_sequence_is_not_a_cost_of_zero() {
        let error = SequenceCost::new(Vec::new()).unwrap_err();
        assert!(matches!(error, ExecutionError::Evidence(_)), "{error}");
    }

    #[test]
    fn the_preflight_ceiling_prices_the_l1_half_too_and_says_when_it_could_not() {
        let estimate = L1FeeSource::OracleEstimate {
            amount: U256::from(7_000_000_000u64),
            block_number: 37_530_593,
            read_by: "eth_call".to_string(),
        };
        let ceiling = EstimatedCost::new(
            460_000,
            U256::from(1_000_401u64),
            U256::from(100_000_000_000_000u128),
            estimate,
        )
        .unwrap();
        assert_eq!(
            ceiling.l2_ceiling,
            U256::from(460_000u128 * 1_000_401u128 + 100_000_000_000_000u128)
        );
        assert_eq!(
            ceiling.total_ceiling_wei,
            ceiling.l2_ceiling + U256::from(7_000_000_000u64)
        );
        assert!(ceiling.includes_l1());

        let blind = EstimatedCost::new(
            460_000,
            U256::from(1_000_401u64),
            U256::from(100_000_000_000_000u128),
            L1FeeSource::Unreadable {
                reason: "oracle returned no data".to_string(),
            },
        )
        .unwrap();
        assert_eq!(blind.total_ceiling_wei, blind.l2_ceiling);
        assert!(!blind.includes_l1());
    }

    #[test]
    fn a_pre_submission_ceiling_refuses_a_receipt_field_it_cannot_have_yet() {
        let error = EstimatedCost::new(
            21_000,
            U256::from(1_000_000u64),
            U256::ZERO,
            L1FeeSource::ReceiptField {
                block_number: 1,
                read_by: "eth_getTransactionReceipt".to_string(),
            },
        )
        .unwrap_err();
        assert!(matches!(error, ExecutionError::Evidence(_)), "{error}");
    }
}
