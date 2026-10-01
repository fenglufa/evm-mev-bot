//! §52/§53: the two evidence records this milestone is judged on.
//!
//! These structs exist because a report is a claim and an evidence line is a
//! measurement. §52's list is the *signed* transaction as the chain will see it — every
//! field the node will decode, plus the hash of the bytes and the sender the signature
//! proves — and §53's is the submission answer and, if one arrived, the receipt. Neither
//! can be assembled after the fact from prose, which is why they are built from the types
//! that already hold the values ([`Build`], [`SignedTransaction`], [`SubmissionOutcome`],
//! [`Receipt`]) rather than filled in by hand.
//!
//! The absence is the other half of the contract: **no private key, ever**. Nothing here
//! has a field that could hold one, and the test over this module serializes an evidence
//! line and looks for the key's bytes in it.

use alloy_primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};

use crate::builder::Build;
use crate::fee::calldata_hash;
use crate::receipt::Receipt;
use crate::submitter::{EndpointKind, SubmissionOutcome};
use crate::tx::SignedTransaction;

/// §52: one signed transaction, described without being reproduced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedTransactionEvidence {
    pub chain_id: u64,
    pub nonce: u64,
    /// `"eip1559"` / `"legacy"` — the envelope, named rather than numbered, because a
    /// reader of a report should not have to know the type byte table.
    pub tx_type: String,
    /// `null` for a contract creation. Kept as the transaction carries it rather than
    /// defaulted to the zero address, so an evidence file cannot describe a creation as
    /// a transfer to an address that never received anything.
    pub to: Option<Address>,
    pub value_wei: U256,
    pub gas_limit: u64,
    /// The fee fields as text with the numbers in them, mirroring
    /// [`crate::intent::TransactionIntent::fee_summary`]. A JSON pair would let a reader
    /// mistake `max_fee_per_gas` for the price actually paid.
    pub fee: String,
    /// keccak of the calldata, not the calldata itself: §52 asks for a `data_hash`, and a
    /// full input blob in an evidence file is how a route ends up reconstructed from logs.
    pub data_hash: B256,
    pub calldata_bytes: usize,
    /// keccak over the encoded signed envelope — the value a receipt is found under.
    pub signed_tx_hash: B256,
    /// The address the signature recovers to, i.e. who can actually send these bytes.
    pub recovered_sender: Address,
    /// The sender the intent claimed. Kept beside the recovered one because §14's eighth
    /// comparison is exactly this pair, and evidence should show both sides of it.
    pub expected_sender: Address,
    pub access_list_entries: usize,
    pub signing_hash: B256,
}

impl SignedTransactionEvidence {
    /// From a build and the signature it produced.
    ///
    /// Takes the *recovered* sender as an argument rather than computing it here, because
    /// recovery is the signer's job and a caller that has not verified the signature
    /// should not be able to produce this record at all.
    pub fn from_build(
        build: &Build,
        signed: &SignedTransaction,
        recovered_sender: Address,
    ) -> Result<Self, String> {
        let unsigned = &signed.unsigned;
        if unsigned != &build.unsigned {
            return Err(format!(
                "the signed transaction is not the one this build produced: gas {} vs {}",
                unsigned.gas_limit, build.gas_limit
            ));
        }
        let (fee_field, fee_value) = unsigned.fee_field().map_err(|e| e.to_string())?;
        let tip = unsigned
            .max_priority_fee_per_gas
            .map(|tip| format!(", maxPriorityFeePerGas={tip}"))
            .unwrap_or_default();
        Ok(Self {
            chain_id: unsigned.chain_id,
            nonce: unsigned.nonce,
            tx_type: unsigned.tx_type.name().to_string(),
            to: unsigned.to,
            value_wei: unsigned.value,
            gas_limit: unsigned.gas_limit,
            fee: format!("{fee_field}={fee_value}{tip}"),
            data_hash: calldata_hash(&unsigned.input),
            calldata_bytes: unsigned.input.len(),
            signed_tx_hash: signed.hash(),
            recovered_sender,
            expected_sender: build.sender_expected,
            access_list_entries: unsigned.access_list.len(),
            signing_hash: build.signing_hash,
        })
    }

    /// §14's eighth field, checked rather than assumed.
    pub fn sender_matches_expectation(&self) -> bool {
        self.recovered_sender == self.expected_sender
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("evidence serializes")
    }

    /// The exact key set §52 lists, so an added field is a decision and not an accident.
    pub fn required_fields() -> [&'static str; 10] {
        [
            "chain_id",
            "nonce",
            "tx_type",
            "to",
            "value_wei",
            "gas_limit",
            "fee",
            "data_hash",
            "signed_tx_hash",
            "recovered_sender",
        ]
    }
}

/// §53: the submission and, if it was answered, the receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmissionEvidence {
    /// The hash computed locally over the bytes handed to the endpoint.
    pub transaction_hash: B256,
    pub submission_endpoint_type: String,
    pub submitted_at_ms: u64,
    /// `accepted` / `rejected` / `unknown` (§25), then `included` / `reverted` /
    /// `timeout` once a receipt answers (§26). Kept as one word so a reader sees which
    /// question the line answers.
    pub outcome: String,
    /// The node's own answer, or the transport error — never a paraphrase of one into
    /// the other.
    pub detail: String,
    pub receipt_block: Option<u64>,
    pub receipt_block_hash: Option<B256>,
    pub receipt_status: Option<bool>,
    pub gas_used: Option<u64>,
    pub effective_gas_price: Option<U256>,
    /// The L1 component, when the receipt carried one. Recorded, never folded into the
    /// L2 bill.
    pub l1_fee: Option<U256>,
    /// §24/§53: set when the endpoint cannot accept submissions at all. A blocked
    /// submission is evidence too; the field names what was tried.
    pub blocked: Option<String>,
}

impl SubmissionEvidence {
    /// Opened at the moment the bytes were handed over.
    pub fn from_outcome(local_hash: B256, outcome: &SubmissionOutcome, at_ms: u64) -> Self {
        let (detail, endpoint) = match outcome {
            SubmissionOutcome::Accepted {
                detail, endpoint, ..
            } => (detail.clone(), *endpoint),
            SubmissionOutcome::Rejected { reason, endpoint } => (reason.clone(), *endpoint),
            SubmissionOutcome::Unknown { reason, endpoint } => (reason.clone(), *endpoint),
        };
        Self {
            transaction_hash: outcome.tracked_hash(local_hash),
            submission_endpoint_type: endpoint.name().to_string(),
            submitted_at_ms: at_ms,
            outcome: outcome.status_word().to_string(),
            detail,
            receipt_block: None,
            receipt_block_hash: None,
            receipt_status: None,
            gas_used: None,
            effective_gas_price: None,
            l1_fee: None,
            blocked: None,
        }
    }

    /// §24/§35: no submission happened, and the reason is the endpoint's answer rather
    /// than an omission.
    pub fn blocked(
        local_hash: Option<B256>,
        endpoint: EndpointKind,
        reason: String,
        at_ms: u64,
    ) -> Self {
        Self {
            transaction_hash: local_hash.unwrap_or(B256::ZERO),
            submission_endpoint_type: endpoint.name().to_string(),
            submitted_at_ms: at_ms,
            outcome: "blocked".to_string(),
            detail: reason.clone(),
            receipt_block: None,
            receipt_block_hash: None,
            receipt_status: None,
            gas_used: None,
            effective_gas_price: None,
            l1_fee: None,
            blocked: Some(reason),
        }
    }

    /// Fold a bound receipt in (§26's fields, §53's list).
    pub fn with_receipt(mut self, receipt: &Receipt) -> Self {
        self.receipt_block = Some(receipt.block_number);
        self.receipt_block_hash = Some(receipt.block_hash);
        self.receipt_status = Some(receipt.success);
        self.gas_used = Some(receipt.gas_used);
        self.effective_gas_price = Some(receipt.effective_gas_price);
        self.l1_fee = receipt.l1_fee;
        self.outcome = receipt.outcome().name().to_string();
        self
    }

    /// Whether this line may be read as "the transaction was sent".
    pub fn was_sent(&self) -> bool {
        !matches!(self.outcome.as_str(), "blocked" | "rejected")
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("evidence serializes")
    }

    pub fn required_fields() -> [&'static str; 7] {
        [
            "transaction_hash",
            "submission_endpoint_type",
            "submitted_at_ms",
            "receipt_block",
            "receipt_status",
            "gas_used",
            "effective_gas_price",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::TransactionIntent;
    use crate::mode::ExecutionMode;
    use crate::signer::{ExecutionKey, Signer};
    use crate::tx::TransactionType;
    use alloy_primitives::Bytes;
    use evm_core::BlockNumber;
    use evm_simulation::BlockPin;

    /// A synthetic key, used only to prove the redaction property. This is not a wallet
    /// belonging to anyone: the scalar is `1`, the curve's generator, so the address it
    /// produces is public arithmetic.
    const TEST_SCALAR: [u8; 32] = {
        let mut bytes = [0u8; 32];
        bytes[31] = 1;
        bytes
    };

    fn intent() -> TransactionIntent {
        TransactionIntent::validation(
            BlockPin::new(BlockNumber(123), B256::left_padding_from(&[123])),
            Address::from_slice(&[0xaa; 20]),
            &crate::tx::UnsignedTransaction {
                tx_type: TransactionType::DynamicFee,
                chain_id: 91_342,
                nonce: 7,
                to: Some(Address::from_slice(&[0xbb; 20])),
                value: U256::from(1u64),
                gas_limit: 21_000,
                input: Bytes::from(vec![0x12, 0x34, 0x56, 0x78]),
                access_list: Vec::new(),
                max_priority_fee_per_gas: Some(U256::from(1_000_000u64)),
                max_fee_per_gas: Some(U256::from(2_000_000u64)),
            },
        )
        .expect("a validation intent over a call transaction")
    }

    #[test]
    fn evidence_carries_every_field_52_lists_and_nothing_else_that_could_be_a_key() {
        let key = ExecutionKey::from_secret_bytes(&TEST_SCALAR).expect("a synthetic key");
        let signer = Signer::from_key(ExecutionMode::SignOnly, key);
        let mut intent = intent();
        // §14's eighth field is a comparison, so the fixture has to be the case where it
        // can hold: the key that signs is the sender the intent claims.
        intent.sender = signer.address().expect("a supplied key has an address");
        let policy = crate::builder::BuildPolicy {
            expected_chain_id: 91_342,
            gas: crate::builder::GasPolicy::Configured { gas_limit: 21_000 },
            ..Default::default()
        };
        let build = crate::builder::TransactionBuilder::build(&intent, &policy).expect("builds");
        let (signed, recovered) = signer.sign_and_recover(&build.unsigned).expect("signs");
        let evidence =
            SignedTransactionEvidence::from_build(&build, &signed, recovered).expect("evidence");

        let json = evidence.to_json();
        for field in SignedTransactionEvidence::required_fields() {
            assert!(json.get(field).is_some(), "§52 requires {field}");
        }
        assert_eq!(json["chain_id"], serde_json::json!(91_342));
        assert_eq!(json["nonce"], serde_json::json!(7));
        assert_eq!(json["tx_type"], serde_json::json!("eip1559"));
        assert_eq!(evidence.tx_type, TransactionType::DynamicFee.name());
        assert_eq!(json["gas_limit"], serde_json::json!(21_000));
        assert!(json["fee"]
            .as_str()
            .unwrap()
            .contains("maxFeePerGas=2000000"));
        assert!(evidence.sender_matches_expectation());
        assert_eq!(evidence.data_hash, calldata_hash(&intent.calldata));
        assert_eq!(evidence.signed_tx_hash, signed.hash());

        // The redaction property, measured rather than asserted in prose: neither the
        // secret scalar nor any rendering of it appears in the serialized line.
        let text = serde_json::to_string(&evidence).unwrap();
        let secret_hex = hex::encode(TEST_SCALAR);
        assert!(!text.contains(&secret_hex), "a serialized key leaked");
        assert!(!text.contains("private"), "no field even names a key");
        assert!(!text.contains(&secret_hex[..16]));
    }

    #[test]
    fn evidence_refuses_a_signed_transaction_that_is_not_the_one_built() {
        let key = ExecutionKey::from_secret_bytes(&TEST_SCALAR).expect("a synthetic key");
        let signer = Signer::from_key(ExecutionMode::SignOnly, key);
        let intent = intent();
        let policy = crate::builder::BuildPolicy {
            expected_chain_id: 91_342,
            gas: crate::builder::GasPolicy::Configured { gas_limit: 21_000 },
            ..Default::default()
        };
        let build = crate::builder::TransactionBuilder::build(&intent, &policy).expect("builds");

        let mut other = intent.unsigned();
        other.nonce = 8;
        let signed = signer.sign(&other).expect("signs the other one");
        let error = SignedTransactionEvidence::from_build(
            &build,
            &signed,
            Address::from_slice(&[0xaa; 20]),
        )
        .unwrap_err();
        assert!(error.contains("not the one this build produced"), "{error}");
    }

    #[test]
    fn a_submission_line_answers_one_question_at_a_time() {
        let local = B256::left_padding_from(&[5]);
        let accepted = SubmissionOutcome::Accepted {
            transaction_hash: Some(local),
            hash_matches_local: true,
            endpoint: EndpointKind::PublicHttpRpc,
            detail: "eth_sendRawTransaction returned the hash".to_string(),
        };
        let line = SubmissionEvidence::from_outcome(local, &accepted, 1_000);
        assert_eq!(line.outcome, "submitted");
        assert!(line.was_sent());
        assert_eq!(line.submission_endpoint_type, "public_http_rpc");
        assert!(
            line.receipt_block.is_none(),
            "an acknowledgement is not a receipt"
        );

        let json = line.to_json();
        for field in SubmissionEvidence::required_fields() {
            assert!(json.get(field).is_some(), "§53 requires {field}");
        }

        // §53's other branch: the endpoint cannot submit, and that is recorded as a fact
        // with a reason instead of as a missing line.
        let blocked = SubmissionEvidence::blocked(
            None,
            EndpointKind::Recorded,
            "mev_sendBundle is not whitelisted on this endpoint".to_string(),
            2_000,
        );
        assert_eq!(blocked.outcome, "blocked");
        assert!(!blocked.was_sent());
        assert!(blocked.blocked.unwrap().contains("not whitelisted"));
        assert_eq!(blocked.submission_endpoint_type, "recorded_no_submission");
    }

    #[test]
    fn a_receipt_folds_in_and_the_l1_fee_stays_its_own_number() {
        let local = B256::left_padding_from(&[5]);
        let accepted = SubmissionOutcome::Accepted {
            transaction_hash: Some(local),
            hash_matches_local: true,
            endpoint: EndpointKind::PublicHttpRpc,
            detail: String::new(),
        };
        let mut receipt = crate::receipt::Receipt {
            transaction_hash: local,
            block_number: 101,
            block_hash: B256::left_padding_from(&[101]),
            transaction_index: 0,
            success: true,
            gas_used: 21_000,
            effective_gas_price: U256::from(1_000_370u64),
            cumulative_gas_used: None,
            from: Address::from_slice(&[0xaa; 20]),
            to: Some(Address::from_slice(&[0xbb; 20])),
            contract_address: None,
            tx_type: Some(2),
            logs: Vec::new(),
            l1_fee: Some(U256::from(7_400_000_000u64)),
            l1_gas_price: None,
            l1_gas_used: None,
            l1_base_fee_scalar: None,
            l1_blob_base_fee: None,
            l1_blob_base_fee_scalar: None,
            provenance: "eth_getTransactionReceipt".to_string(),
        };
        let line = SubmissionEvidence::from_outcome(local, &accepted, 1_000).with_receipt(&receipt);
        assert_eq!(line.outcome, "included");
        assert_eq!(line.receipt_block, Some(101));
        assert_eq!(line.gas_used, Some(21_000));
        assert_eq!(line.l1_fee, Some(U256::from(7_400_000_000u64)));

        receipt.success = false;
        let line = SubmissionEvidence::from_outcome(local, &accepted, 1_000).with_receipt(&receipt);
        assert_eq!(line.outcome, "reverted");
        assert_eq!(line.receipt_status, Some(false));
    }
}
