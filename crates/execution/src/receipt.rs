//! §26/§27: knowing what happened to a transaction, and only claiming what was read.
//!
//! The whole module exists for one distinction: an endpoint that returns a transaction
//! hash has *accepted* a transaction, which is not the same as it being in a block, and
//! a receipt that exists is not the same as a receipt that succeeded. The task book
//! spends §2.2 and §2.3 on this and then makes it a test (§O, §P), so the type carries
//! six statuses instead of a boolean and the tracker refuses to bind a receipt it cannot
//! match to the transaction it was asked about.
//!
//! The binding check (§27) has three parts, and all three are reads:
//! `receipt.transactionHash == the hash we computed locally over the bytes we sent`,
//! `receipt.blockHash == the hash the endpoint gives for that block number`, and
//! `receipt.from == the sender the signature recovers to`. The second is why a receipt is
//! never taken as final on its own — a provider can answer from a fork or a cache — and
//! the third is why a receipt for someone else's transaction cannot be mistaken for ours.

use std::time::Duration;

use alloy_primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};

use evm_chain::ChainLog;

/// §26's status list, verbatim, plus nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    /// The endpoint acknowledged the raw bytes; no block yet.
    Submitted,
    /// Asked, and there is no receipt yet. The transaction may still land.
    Pending,
    /// A receipt exists, is bound to our transaction, and `status` is success.
    Included,
    /// A receipt exists and is bound, and `status` is failure. Never recorded as
    /// success (§P).
    Reverted,
    /// The transaction is known absent: definitively not in any block we will look at
    /// again (the tracker reaches this only through an explicit answer, not through a
    /// timeout).
    NotFound,
    /// The polling budget ran out with no answer. §25: this is *not* a failure of the
    /// transaction, so it must not release the nonce lane as if nothing is in flight.
    Timeout,
}

impl ReceiptStatus {
    pub fn name(self) -> &'static str {
        match self {
            Self::Submitted => "submitted",
            Self::Pending => "pending",
            Self::Included => "included",
            Self::Reverted => "reverted",
            Self::NotFound => "not_found",
            Self::Timeout => "timeout",
        }
    }

    /// Whether further polling can change the answer.
    pub fn terminal(self) -> bool {
        matches!(self, Self::Included | Self::Reverted | Self::NotFound)
    }

    /// Whether the transaction may still be live on chain. A timeout keeps the lane
    /// busy (§11) precisely because this is true.
    pub fn may_be_in_flight(self) -> bool {
        matches!(self, Self::Submitted | Self::Pending | Self::Timeout)
    }
}

/// One transaction receipt, as read.
///
/// Field set is §26's list plus what GIWA's receipts actually carry. The `l1_*` fields
/// are kept because on this chain they are a real cost outside the EVM gas bill — the
/// measured receipt in `data/evidence/m6/probe-submission-surface.txt` has an `l1Fee` of
/// about 16% of its L2 bill — and M7 cannot do honest accounting from a record that
/// threw them away. They are stored, never summed into `gas_used * effective_gas_price`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub transaction_hash: B256,
    pub block_number: u64,
    pub block_hash: B256,
    pub transaction_index: u64,
    /// `status == 0x1`. Kept as a bool *and* mirrored by [`Receipt::outcome`] so a
    /// reader cannot mistake the flag for the lifecycle status.
    pub success: bool,
    pub gas_used: u64,
    pub effective_gas_price: U256,
    pub cumulative_gas_used: Option<U256>,
    pub from: Address,
    pub to: Option<Address>,
    /// Non-empty only for a contract creation.
    pub contract_address: Option<Address>,
    /// The provider's own `type` field, as a hex quantity, so a receipt can be compared
    /// against the transaction type we built.
    pub tx_type: Option<u64>,
    pub logs: Vec<ChainLog>,
    pub l1_fee: Option<U256>,
    pub l1_gas_price: Option<U256>,
    pub l1_gas_used: Option<U256>,
    pub l1_base_fee_scalar: Option<U256>,
    pub l1_blob_base_fee: Option<U256>,
    pub l1_blob_base_fee_scalar: Option<U256>,
    /// How this receipt was read, in words and with the method names — the same rule the
    /// fee reading follows (§52's provenance habit).
    pub provenance: String,
}

impl Receipt {
    /// The lifecycle status a bound receipt implies. `Included` / `Reverted` only — the
    /// other four statuses describe the *absence* of a receipt and are decided by the
    /// tracker.
    pub fn outcome(&self) -> ReceiptStatus {
        if self.success {
            ReceiptStatus::Included
        } else {
            ReceiptStatus::Reverted
        }
    }

    /// The L2 gas bill: what the EVM charged. Deliberately excludes `l1_fee`.
    pub fn l2_cost_wei(&self) -> Option<U256> {
        U256::from(self.gas_used).checked_mul(self.effective_gas_price)
    }
}

/// The transaction a receipt has to match (§27). Everything here came from the bytes we
/// signed, not from the provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpectedTransaction {
    pub transaction_hash: B256,
    pub sender: Address,
    pub target: Option<Address>,
    pub nonce: u64,
    pub chain_id: u64,
}

/// What the tracker decided, with the receipt when there is one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackedReceipt {
    Included(Receipt),
    Reverted(Receipt),
    /// No receipt after the whole budget. Carries how many polls were made and what the
    /// last answer was, so the report can say what was tried.
    Pending {
        attempts: usize,
        last_answer: String,
    },
    /// A receipt appeared and did not bind. This is an error condition, reported as its
    /// own status rather than being folded into `Reverted`.
    Unbound {
        receipt: Receipt,
        reason: String,
    },
}

impl TrackedReceipt {
    pub fn status(&self) -> ReceiptStatus {
        match self {
            Self::Included(_) => ReceiptStatus::Included,
            Self::Reverted(_) => ReceiptStatus::Reverted,
            Self::Pending { .. } => ReceiptStatus::Timeout,
            Self::Unbound { .. } => ReceiptStatus::NotFound,
        }
    }
}

/// §27's three-way binding check, as a function so a test can hand it a receipt that is
/// subtly wrong.
pub fn bind(receipt: &Receipt, expected: &ExpectedTransaction) -> std::result::Result<(), String> {
    if receipt.transaction_hash != expected.transaction_hash {
        return Err(format!(
            "the receipt is for transaction {:#x}, not the {:#x} we sent",
            receipt.transaction_hash, expected.transaction_hash
        ));
    }
    if receipt.from != expected.sender {
        return Err(format!(
            "the receipt names {} as its sender; the signature on our transaction \
             recovers to {}",
            receipt.from, expected.sender
        ));
    }
    if receipt.to != expected.target {
        return Err(format!(
            "the receipt's target is {:?} and our transaction's is {:?}",
            receipt.to, expected.target
        ));
    }
    Ok(())
}

/// The polling policy: a bounded number of reads with a fixed pause, and an explicit
/// statement that running out is not a failure (§25).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiptPolicy {
    pub attempts: usize,
    pub between_attempts: Duration,
}

impl Default for ReceiptPolicy {
    fn default() -> Self {
        Self {
            // GIWA's own blocks arrive about once a second
            // (`data/evidence/m6/probe-read-surface-2.txt`: 191 blocks in 191 seconds),
            // so twelve attempts spans roughly twelve blocks — far more than a
            // transaction that is going to land needs.
            attempts: 12,
            between_attempts: Duration::from_millis(1_000),
        }
    }
}

/// Reads receipts until one exists, the budget runs out, or the answer is definitive.
///
/// The tracker is generic over the read: it takes a closure rather than a concrete
/// adapter, so the same binding logic runs against a live endpoint in the pipeline and
/// against a canned answer in a test. It never sends anything — submission is
/// [`crate::submitter`]'s job, and keeping the two apart is what makes §25's "no blind
/// retry" enforceable rather than a habit.
pub struct ReceiptTracker {
    pub policy: ReceiptPolicy,
}

impl ReceiptTracker {
    pub fn new(policy: ReceiptPolicy) -> Self {
        Self { policy }
    }

    /// Poll `read`. `read` returns `Ok(None)` for "no receipt yet"; an `Err` from the
    /// read is recorded as the last answer and the poll continues, because a failed read
    /// says nothing about the transaction (§25 again).
    ///
    /// When a receipt does appear, [`bind`] runs first, then `verify_block(number)` is
    /// asked for the hash the endpoint holds at that height — the second half of §27. A
    /// tracker that only had a receipt reader cannot ask the chain whether that block is
    /// real, and pretending otherwise would be §27's exact failure, so the block
    /// verification is a required argument rather than an optional extra.
    ///
    /// The two negative answers are not alike. A block read that holds *no* block at that
    /// height is absence of information and keeps the budget running, because an endpoint
    /// can serve a receipt a beat before it serves the block the receipt names. A read
    /// that answers a **different hash** contradicts the receipt, and that is the finding
    /// §27 exists to make: the attempt ends there and then, as `Unbound`.
    pub async fn track<F, Fut, V, FutV>(
        &self,
        expected: &ExpectedTransaction,
        mut read: F,
        mut verify_block: V,
    ) -> TrackedReceipt
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = std::result::Result<Option<Receipt>, String>>,
        V: FnMut(u64) -> FutV,
        FutV: std::future::Future<Output = std::result::Result<Option<B256>, String>>,
    {
        let mut attempts = 0usize;
        let mut last_answer = String::from("no attempt made");
        while attempts < self.policy.attempts {
            attempts += 1;
            match read().await {
                Ok(None) => {
                    last_answer = format!("attempt {attempts}: eth_getTransactionReceipt was null")
                }
                Ok(Some(receipt)) => {
                    if let Err(reason) = bind(&receipt, expected) {
                        return TrackedReceipt::Unbound { receipt, reason };
                    }
                    match verify_block(receipt.block_number).await {
                        Ok(Some(hash)) if hash == receipt.block_hash => {
                            return match receipt.outcome() {
                                ReceiptStatus::Reverted => TrackedReceipt::Reverted(receipt),
                                _ => TrackedReceipt::Included(receipt),
                            }
                        }
                        Ok(Some(hash)) => {
                            // Named before the receipt moves, so the reason quotes the
                            // two hashes this receipt is being refused for.
                            let reason = format!(
                                "the endpoint's block {} is {hash:#x}; the receipt claims \
                                 {:#x}, so this receipt is not evidence about a canonical \
                                 block",
                                receipt.block_number, receipt.block_hash
                            );
                            return TrackedReceipt::Unbound { receipt, reason };
                        }
                        Ok(None) => {
                            // The block is not readable *yet*, which is the same kind of
                            // answer as a null receipt: it says nothing about the
                            // transaction. An endpoint can serve a receipt a beat before it
                            // serves the block that receipt names — that is what GIWA's node
                            // did with the first §35 transaction, whose two answers are
                            // frozen in `data/evidence/m6/validation/node-answers-
                            // 37503978.json` — and calling that `Unbound` reported a
                            // transaction the chain had already run as a broken one. The
                            // fixed read was then asked of the same node again, for the
                            // same transaction, and got `Included`:
                            // `data/evidence/m6/validation/live-retrack-37503978.txt`.
                            last_answer = format!(
                                "attempt {attempts}: the endpoint has no block {} yet, and the \
                                 receipt says this transaction is in it",
                                receipt.block_number
                            )
                        }
                        Err(error) => {
                            last_answer = format!("attempt {attempts}: block check failed: {error}")
                        }
                    }
                }
                Err(error) => last_answer = format!("attempt {attempts}: {error}"),
            }
            if attempts < self.policy.attempts {
                tokio::time::sleep(self.policy.between_attempts).await;
            }
        }
        TrackedReceipt::Pending {
            attempts,
            last_answer,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt() -> Receipt {
        Receipt {
            transaction_hash: B256::left_padding_from(&[9]),
            block_number: 101,
            block_hash: B256::left_padding_from(&[101]),
            transaction_index: 3,
            success: true,
            gas_used: 21_000,
            effective_gas_price: U256::from(1_000_370u64),
            cumulative_gas_used: Some(U256::from(84_000u64)),
            from: Address::from_slice(&[1u8; 20]),
            to: Some(Address::from_slice(&[2u8; 20])),
            contract_address: None,
            tx_type: Some(2),
            logs: Vec::new(),
            l1_fee: Some(U256::from(7_400_000_000u64)),
            l1_gas_price: Some(U256::from(1_088_519_061u64)),
            l1_gas_used: Some(U256::from(1_600u64)),
            l1_base_fee_scalar: Some(U256::from(1_368u64)),
            l1_blob_base_fee: Some(U256::from(62_294_004u64)),
            l1_blob_base_fee_scalar: Some(U256::from(801_949u64)),
            provenance: "eth_getTransactionReceipt".to_string(),
        }
    }

    fn expected() -> ExpectedTransaction {
        ExpectedTransaction {
            transaction_hash: B256::left_padding_from(&[9]),
            sender: Address::from_slice(&[1u8; 20]),
            target: Some(Address::from_slice(&[2u8; 20])),
            nonce: 0,
            chain_id: 91_342,
        }
    }

    #[test]
    fn a_bound_receipt_binds_and_each_of_the_three_mismatches_breaks_it() {
        assert!(bind(&receipt(), &expected()).is_ok());

        let mut hash_mismatch = receipt();
        hash_mismatch.transaction_hash = B256::left_padding_from(&[7]);
        assert!(bind(&hash_mismatch, &expected())
            .unwrap_err()
            .contains("not the"));

        let mut sender_mismatch = receipt();
        sender_mismatch.from = Address::from_slice(&[3u8; 20]);
        assert!(bind(&sender_mismatch, &expected())
            .unwrap_err()
            .contains("recovers to"));

        let mut target_mismatch = receipt();
        target_mismatch.to = None;
        assert!(bind(&target_mismatch, &expected())
            .unwrap_err()
            .contains("target"));
    }

    #[test]
    fn status_is_the_end_of_a_one_way_ladder_and_only_three_answers_are_terminal() {
        assert!(ReceiptStatus::Included.terminal());
        assert!(ReceiptStatus::Reverted.terminal());
        assert!(ReceiptStatus::NotFound.terminal());
        for pending in [
            ReceiptStatus::Submitted,
            ReceiptStatus::Pending,
            ReceiptStatus::Timeout,
        ] {
            assert!(!pending.terminal());
            assert!(
                pending.may_be_in_flight(),
                "{} must keep the lane busy",
                pending.name()
            );
        }
    }

    #[test]
    fn the_l2_bill_excludes_the_layer_one_fee() {
        let r = receipt();
        // 21 000 * 1 000 370, and *not* plus `l1_fee` — the two are different bills and
        // adding them would be the unit error §33's cost check cannot recover from.
        assert_eq!(r.l2_cost_wei(), Some(U256::from(21_007_770_000u64)));
        assert_ne!(r.l2_cost_wei(), Some(U256::from(28_407_770_000u64)));
    }

    #[tokio::test]
    async fn a_timeout_is_reported_as_timeout_not_as_failure() {
        let tracker = ReceiptTracker::new(ReceiptPolicy {
            attempts: 3,
            between_attempts: Duration::from_millis(1),
        });
        let mut calls = 0usize;
        let outcome = tracker
            .track(
                &expected(),
                || {
                    calls += 1;
                    async { Ok(None) }
                },
                |_| async { Ok(Some(B256::left_padding_from(&[101]))) },
            )
            .await;
        assert_eq!(outcome.status(), ReceiptStatus::Timeout);
        match outcome {
            TrackedReceipt::Pending {
                attempts,
                last_answer,
            } => {
                assert_eq!(attempts, 3);
                assert_eq!(calls, 3);
                assert!(last_answer.contains("null"), "{last_answer}");
            }
            other => panic!("expected a timeout-shaped answer, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_receipt_from_a_block_that_is_not_canonical_never_becomes_included() {
        let tracker = ReceiptTracker::new(ReceiptPolicy {
            attempts: 2,
            between_attempts: Duration::from_millis(1),
        });
        let outcome = tracker
            .track(
                &expected(),
                || async { Ok(Some(receipt())) },
                |_| async { Ok(Some(B256::left_padding_from(&[7]))) },
            )
            .await;
        match outcome {
            TrackedReceipt::Unbound { reason, .. } => {
                assert!(reason.contains("the receipt claims"), "{reason}")
            }
            other => panic!("a receipt in a block the chain denies must not bind: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_block_that_is_not_readable_yet_spends_an_attempt_instead_of_ending_the_run() {
        // The failure this rule exists for was measured, not imagined: the first §35
        // transaction was answered by `eth_getTransactionReceipt` with block 37 503 978 a
        // beat before the same endpoint would hand out that block by number, and the
        // attempt ended as `Unbound` — a transaction the chain had run, reported as a
        // broken one. See `data/evidence/m6/validation/node-answers-37503978.json`.
        let tracker = ReceiptTracker::new(ReceiptPolicy {
            attempts: 4,
            between_attempts: Duration::from_millis(1),
        });
        let mut views = 0usize;
        let outcome = tracker
            .track(
                &expected(),
                || async { Ok(Some(receipt())) },
                |_| {
                    views += 1;
                    let readable = views >= 3;
                    async move {
                        Ok(if readable {
                            Some(B256::left_padding_from(&[101]))
                        } else {
                            None
                        })
                    }
                },
            )
            .await;
        assert_eq!(
            outcome.status(),
            ReceiptStatus::Included,
            "the receipt is the same object on every poll, so only the block read changed: \
             {outcome:?}"
        );
        assert_eq!(views, 3, "the unreadable views were tried, not skipped");
    }

    #[tokio::test]
    async fn a_block_that_never_becomes_readable_times_out_without_blaming_the_receipt() {
        // `Timeout` and `Unbound` are different findings and the report has to say which
        // one it means: here the chain never confirmed or denied the block, so nothing has
        // been learned about the transaction — which is §25's "unknown", not a failure.
        let tracker = ReceiptTracker::new(ReceiptPolicy {
            attempts: 3,
            between_attempts: Duration::from_millis(1),
        });
        let outcome = tracker
            .track(
                &expected(),
                || async { Ok(Some(receipt())) },
                |_| async { Ok(None) },
            )
            .await;
        match outcome {
            TrackedReceipt::Pending {
                attempts,
                last_answer,
            } => {
                assert_eq!(attempts, 3);
                assert!(
                    last_answer.contains("no block 101 yet"),
                    "the answer says which read came up short: {last_answer}"
                );
            }
            other => panic!("an unreadable block is not a receipt that failed to bind: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_read_error_retries_within_the_budget_and_a_reverted_receipt_stays_reverted() {
        let tracker = ReceiptTracker::new(ReceiptPolicy {
            attempts: 4,
            between_attempts: Duration::from_millis(1),
        });
        let mut calls = 0usize;
        let outcome = tracker
            .track(
                &expected(),
                || {
                    calls += 1;
                    let failed = calls == 1;
                    async move {
                        if failed {
                            Err("connection reset".to_string())
                        } else {
                            let mut r = receipt();
                            r.success = false;
                            Ok(Some(r))
                        }
                    }
                },
                |_| async { Ok(Some(B256::left_padding_from(&[101]))) },
            )
            .await;
        assert_eq!(
            calls, 2,
            "a failed read must consume one attempt, not the budget"
        );
        assert!(matches!(outcome, TrackedReceipt::Reverted(_)));
        assert_eq!(outcome.status(), ReceiptStatus::Reverted);
    }
}
