//! M9.4 §5/§6: decode what the endpoint actually sends, and fail closed on what it
//! does not.
//!
//! The field set this parser reads is the field set measured in
//! `docs/v0.1/M9.4 Semantic Audit.md` §2.3 — the pending header's 26 keys, the
//! transaction object's keys, and the receipt/log keys from
//! `eth_getBlockReceipts("pending")`. Nothing else is read, and nothing is inferred
//! from a name: a quantity that is absent becomes [`PreconfError::Decode`] (the frame
//! is refused) or [`Field::Unknown`] (the frame is kept, the claim is withheld), and
//! which of those two it becomes is decided by whether the radar can still answer
//! its question without it.
//!
//! §6 forbids guessing a schema. The concrete consequence in this file: there is no
//! fallback like "treat a missing `transactionIndex` as its position in the array",
//! and there is no `Guessed` variant anywhere in `crates/live`.

use alloy_primitives::{Address, B256};
use serde_json::Value;

use evm_core::{BlockNumber, ChainId};

use crate::preconf::{
    Field, PreconfError, PreconfIdentity, PreconfLog, PreconfReceipt, PreconfTransaction,
    PreconfirmationFrame,
};

/// The all-zero `stateRoot` the preconfirmation host answers in every sample.
///
/// It is a *present* field carrying a *placeholder*, so it becomes
/// [`Field::Unknown`] with `key_present: true` — a different finding from a field
/// the payload never named, and the difference is what §6 asks the record to keep.
const ZERO_STATE_ROOT_DETAIL: &str =
    "stateRoot is the all-zero placeholder; this endpoint supplies no state credential at a pending view";
const NO_WIRE_INDEX_DETAIL: &str =
    "the pending payload has no frame index field of any kind (measured field set: the standard block object)";
/// A payload that *names* an index-like key is a different finding from one that names
/// nothing: the key is still not mapped to a frame index (§8), and repeating "no frame
/// index field of any kind" over it would state something false about the payload.
const UNMAPPED_WIRE_INDEX_DETAIL: &str =
    "the payload names an index-like key (`index` or `sequence`) that this decoder does not map, so the frame index stays local (§8)";

/// §8: the frame index is assigned in read order and never read off the wire. An
/// unknown index-like key is recorded as present — that is the finding — and still
/// yields no value.
fn wire_index_field(object: &serde_json::Map<String, Value>) -> Field<u64> {
    let named = object.contains_key("index") || object.contains_key("sequence");
    Field::Unknown {
        key_present: named,
        detail: if named {
            UNMAPPED_WIRE_INDEX_DETAIL
        } else {
            NO_WIRE_INDEX_DETAIL
        },
    }
}

fn hex_u64(value: Option<&Value>) -> Option<u64> {
    let text = value?.as_str()?;
    u64::from_str_radix(text.strip_prefix("0x").unwrap_or(text), 16).ok()
}

fn hex_b256(value: Option<&Value>) -> Option<B256> {
    let text = value?.as_str()?;
    text.parse::<B256>().ok()
}

fn hex_address(value: Option<&Value>) -> Option<Address> {
    let text = value?.as_str()?;
    text.parse::<Address>().ok()
}

fn quantity_or_unknown(value: Option<&Value>, detail: &'static str) -> Field<u64> {
    match hex_u64(value) {
        Some(parsed) => Field::Known(parsed),
        None => Field::Unknown {
            key_present: value.is_some(),
            detail,
        },
    }
}

/// The one place that decides whether a `stateRoot` is a root.
pub fn state_root_field(raw: Option<&Value>) -> Field<B256> {
    match hex_b256(raw) {
        Some(root) if root.is_zero() => Field::Unknown {
            key_present: true,
            detail: ZERO_STATE_ROOT_DETAIL,
        },
        Some(root) => Field::Known(root),
        None => Field::Unknown {
            key_present: raw.is_some(),
            detail: "stateRoot absent from the pending payload",
        },
    }
}

fn selector_of(input: Option<&Value>) -> Field<[u8; 4]> {
    let Some(text) = input.and_then(Value::as_str) else {
        return Field::Unknown {
            key_present: input.is_some(),
            detail: "transaction carries no `input` field",
        };
    };
    let body = text.strip_prefix("0x").unwrap_or(text);
    if body.len() < 8 {
        return Field::Unknown {
            key_present: true,
            detail: "`input` is shorter than a four-byte selector",
        };
    }
    let mut selector = [0u8; 4];
    for (slot, byte) in selector.iter_mut().zip(0..4) {
        match u8::from_str_radix(&body[byte * 2..byte * 2 + 2], 16) {
            Ok(parsed) => *slot = parsed,
            Err(_) => {
                return Field::Unknown {
                    key_present: true,
                    detail: "`input` is not hexadecimal",
                }
            }
        }
    }
    Field::Known(selector)
}

/// The JSON kind of a payload value, in the words a diagnostic line prints.
fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// One entry of the pending block's `transactions` array.
///
/// On the endpoint measured in M9.4 the array holds **full transaction objects**
/// (`eth_getBlockByNumber(["pending", true])`: 281 full objects, 0 hash-only
/// entries). A hash-only list is therefore not silently accepted as "the same
/// thing with less detail" — it cannot name a target, so the frame is refused with
/// [`PreconfError::HashOnlyTransaction`] rather than half-decoded (§24's
/// fail-closed rule, and the reason a `false` affected-pool count means something).
///
/// The refusal is split by what the entry actually is. A bare 32-byte hash is the
/// `full: false` answer — a shape that exists, with a name that tells the reader which
/// parameter to change. Anything else (a number, an array, a truncated string) is not
/// that shape, and calling it "hash-only" would send whoever reads the line looking for
/// provider behaviour the provider never showed.
fn transaction_from_value(value: &Value, index: usize) -> Result<PreconfTransaction, PreconfError> {
    let object = value.as_object().ok_or_else(|| match value {
        Value::String(text) if text.parse::<B256>().is_ok() => PreconfError::HashOnlyTransaction,
        other => PreconfError::UnexpectedTransactionEntry {
            index,
            kind: json_kind(other),
        },
    })?;
    let hash = object
        .get("hash")
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<B256>().ok())
        .ok_or(PreconfError::Decode("transactions[].hash"))?;
    Ok(PreconfTransaction {
        hash,
        index: quantity_or_unknown(
            object.get("transactionIndex"),
            "transactionIndex absent from the transaction object",
        ),
        from: match hex_address(object.get("from")) {
            Some(address) => Field::Known(address),
            None => Field::Unknown {
                key_present: object.contains_key("from"),
                detail: "from absent or not an address",
            },
        },
        to: match hex_address(object.get("to")) {
            Some(address) => Field::Known(address),
            None => Field::Unknown {
                key_present: object.contains_key("to"),
                detail: "to absent, null (a creation), or not an address",
            },
        },
        selector: selector_of(object.get("input")),
    })
}

fn log_from_value(value: &Value) -> Result<Option<PreconfLog>, PreconfError> {
    let object = value
        .as_object()
        .ok_or(PreconfError::Decode("receipts[].logs[]"))?;
    let Some(emitter) = hex_address(object.get("address")) else {
        return Ok(None);
    };
    let topic0 = match object
        .get("topics")
        .and_then(Value::as_array)
        .and_then(|topics| topics.first())
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<B256>().ok())
    {
        Some(topic) => Field::Known(topic),
        None => Field::Unknown {
            key_present: object.contains_key("topics"),
            detail: "no first topic in the log's topic list",
        },
    };
    let transaction_hash = object
        .get("transactionHash")
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<B256>().ok())
        .ok_or(PreconfError::Decode("logs[].transactionHash"))?;
    Ok(Some(PreconfLog {
        emitter,
        topic0,
        log_index: quantity_or_unknown(
            object.get("logIndex"),
            "logIndex absent from the log object",
        ),
        transaction_index: quantity_or_unknown(
            object.get("transactionIndex"),
            "transactionIndex absent from the log object",
        ),
        removed: object
            .get("removed")
            .and_then(Value::as_bool)
            .unwrap_or_default(),
        transaction_hash,
    }))
}

/// A `pending` block object, as the radar holds it.
///
/// `chain_id` is the caller's, never the payload's: §45's rule that no branch of
/// this codebase keys on a chain id survives here, and a pending object carries no
/// chain identity anyway.
pub fn frame_from_pending_value(
    chain_id: ChainId,
    raw: &Value,
    local_frame_sequence: u64,
    observed_at_unix_ms: u64,
    endpoint_id: &str,
) -> Result<PreconfirmationFrame, PreconfError> {
    let object = raw
        .as_object()
        .ok_or(PreconfError::Decode("pending block object"))?;
    let number = BlockNumber(hex_u64(object.get("number")).ok_or(PreconfError::Decode("number"))?);
    let view_hash = match hex_b256(object.get("hash")) {
        Some(hash) => Field::Known(hash),
        None => Field::Unknown {
            key_present: object.contains_key("hash"),
            detail: "hash absent or not a 32-byte hex value",
        },
    };
    let parent_hash = match hex_b256(object.get("parentHash")) {
        Some(hash) => Field::Known(hash),
        None => Field::Unknown {
            key_present: object.contains_key("parentHash"),
            detail: "parentHash absent, so continuity cannot be checked",
        },
    };
    let transactions_value = object
        .get("transactions")
        .and_then(Value::as_array)
        .ok_or(PreconfError::Decode("transactions"))?;
    let mut transactions = Vec::with_capacity(transactions_value.len());
    for (index, entry) in transactions_value.iter().enumerate() {
        transactions.push(transaction_from_value(entry, index)?);
    }
    Ok(PreconfirmationFrame {
        identity: PreconfIdentity {
            chain_id,
            block_number: number,
            local_frame_sequence,
            wire_index: wire_index_field(object),
            parent_hash,
            view_hash,
        },
        chain_timestamp_secs: quantity_or_unknown(
            object.get("timestamp"),
            "timestamp absent from the pending header",
        ),
        gas_used: quantity_or_unknown(
            object.get("gasUsed"),
            "gasUsed absent from the pending header",
        ),
        transaction_count: transactions_value.len(),
        transactions,
        state_root: state_root_field(object.get("stateRoot")),
        observed_at_unix_ms,
        endpoint_id: endpoint_id.to_string(),
    })
}

/// One receipt from `eth_getBlockReceipts("pending")`.
///
/// `status` is read, not assumed: the capture answered `0x0` on 86 receipts and
/// `0x1` on 1997, so a missing status is not "success by default" and a `0x0` is a
/// reverted transaction whose logs must not reach the affected-pool set (NC10).
pub fn receipt_from_value(value: &Value) -> Result<PreconfReceipt, PreconfError> {
    let object = value
        .as_object()
        .ok_or(PreconfError::Decode("receipt object"))?;
    let transaction_hash = object
        .get("transactionHash")
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<B256>().ok())
        .ok_or(PreconfError::Decode("transactionHash"))?;
    let status = match hex_u64(object.get("status")) {
        Some(1) => Field::Known(true),
        Some(0) => Field::Known(false),
        Some(_) => Field::Unknown {
            key_present: true,
            detail: "status is neither 0x0 nor 0x1",
        },
        None => Field::Unknown {
            key_present: object.contains_key("status"),
            detail: "status absent from the receipt",
        },
    };
    let mut logs = Vec::new();
    if let Some(entries) = object.get("logs").and_then(Value::as_array) {
        for entry in entries {
            if let Some(log) = log_from_value(entry)? {
                logs.push(log);
            }
        }
    }
    Ok(PreconfReceipt {
        transaction_hash,
        index: quantity_or_unknown(
            object.get("transactionIndex"),
            "transactionIndex absent from the receipt",
        ),
        status,
        logs,
        claimed_block_number: match hex_u64(object.get("blockNumber")) {
            Some(parsed) => Field::Known(BlockNumber(parsed)),
            None => Field::Unknown {
                key_present: object.contains_key("blockNumber"),
                detail: "blockNumber absent or not a hex quantity",
            },
        },
        claimed_block_hash: match hex_b256(object.get("blockHash")) {
            Some(parsed) => Field::Known(parsed),
            None => Field::Unknown {
                key_present: object.contains_key("blockHash"),
                detail: "blockHash absent or not a 32-byte hex value",
            },
        },
    })
}

/// A whole `eth_getBlockReceipts("pending")` answer.
pub fn receipts_from_value(value: &Value) -> Result<Vec<PreconfReceipt>, PreconfError> {
    let entries = value
        .as_array()
        .ok_or(PreconfError::Decode("receipt list"))?;
    let mut receipts = Vec::with_capacity(entries.len());
    for entry in entries {
        receipts.push(receipt_from_value(entry)?);
    }
    Ok(receipts)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn chain() -> ChainId {
        ChainId(1)
    }

    fn pending(status_root: &str, transactions: Vec<Value>) -> Value {
        json!({
            "number": "0x2",
            "hash": "0x1111111111111111111111111111111111111111111111111111111111111111",
            "parentHash": "0x2222222222222222222222222222222222222222222222222222222222222222",
            "timestamp": "0x6500d200",
            "gasUsed": "0x5208",
            "stateRoot": status_root,
            "transactions": transactions,
        })
    }

    const ZERO: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";
    const REAL: &str = "0x9999999999999999999999999999999999999999999999999999999999999999";

    fn tx(hash_tail: u8, to: Option<&str>, input: &str) -> Value {
        json!({
            "hash": format!("0x{:02x}{}", hash_tail, "00".repeat(31)),
            "transactionIndex": "0x0",
            "from": "0x0000000000000000000000000000000000000001",
            "to": to,
            "input": input,
        })
    }

    #[test]
    fn a_zero_state_root_is_unknown_not_a_root() {
        let frame = frame_from_pending_value(chain(), &pending(ZERO, vec![]), 1, 10, "rpc-x")
            .expect("a well-formed pending object");
        assert_eq!(frame.state_root.state_label(), "unknown");
        assert!(
            matches!(
                frame.state_root,
                Field::Unknown {
                    key_present: true,
                    detail
                } if detail.contains("placeholder")
            ),
            "a placeholder root must be Unknown with key_present true"
        );
    }

    #[test]
    fn a_real_state_root_is_known() {
        let frame = frame_from_pending_value(chain(), &pending(REAL, vec![]), 1, 10, "rpc-x")
            .expect("a well-formed pending object");
        assert!(frame.state_root.is_known());
    }

    #[test]
    fn the_wire_frame_index_is_recorded_as_absent_not_zero() {
        let frame = frame_from_pending_value(chain(), &pending(ZERO, vec![]), 3, 10, "rpc-x")
            .expect("a well-formed pending object");
        assert_eq!(frame.identity.local_frame_sequence, 3);
        assert!(!frame.identity.wire_index.is_known());
        assert!(
            matches!(
                frame.identity.wire_index,
                Field::Unknown {
                    key_present: false,
                    ..
                }
            ),
            "no field on this payload can be known here"
        );
    }

    #[test]
    fn a_hash_only_transaction_list_is_refused_not_half_decoded() {
        let raw = pending(
            REAL,
            vec![json!(
                "0x3333333333333333333333333333333333333333333333333333333333333333"
            )],
        );
        let error = frame_from_pending_value(chain(), &raw, 1, 10, "rpc-x")
            .expect_err("a hash-only list must not decode");
        assert!(
            matches!(error, PreconfError::HashOnlyTransaction),
            "{error}"
        );
    }

    #[test]
    fn a_missing_number_fails_closed() {
        let mut raw = pending(REAL, vec![]);
        let _ = raw
            .as_object_mut()
            .expect("the fixture is an object")
            .remove("number");
        let error = frame_from_pending_value(chain(), &raw, 1, 10, "rpc-x")
            .expect_err("no number, no frame");
        assert!(
            matches!(error, PreconfError::Decode(field) if field == "number"),
            "{error}"
        );
    }

    #[test]
    fn a_creation_transaction_keeps_a_null_target_as_unknown_not_zero_address() {
        let frame = frame_from_pending_value(
            chain(),
            &pending(REAL, vec![tx(0x11, None, "0x")]),
            1,
            10,
            "rpc-x",
        )
        .expect("a creation tx still decodes");
        let tx = frame.transactions.first().expect("one transaction");
        assert!(!tx.to.is_known());
        assert!(!tx.selector.is_known(), "empty calldata has no selector");
    }

    #[test]
    fn a_reverted_status_reads_as_false_and_a_missing_one_as_unknown() {
        let reverted = receipt_from_value(&json!({
            "transactionHash": format!("0x{}", "aa".repeat(32)),
            "status": "0x0",
            "logs": [],
        }))
        .expect("a receipt with a hash and a status");
        assert_eq!(reverted.status, Field::Known(false));
        let missing = receipt_from_value(&json!({
            "transactionHash": format!("0x{}", "bb".repeat(32)),
        }))
        .expect("a receipt with no status");
        assert!(!missing.status.is_known());
        assert_eq!(missing.claimed_block_number.state_label(), "unknown");
    }

    #[test]
    fn a_log_without_an_address_is_dropped_and_the_receipt_still_decodes() {
        let receipt = receipt_from_value(&json!({
            "transactionHash": format!("0x{}", "cc".repeat(32)),
            "status": "0x1",
            "logs": [{"address": "not-an-address", "transactionHash": format!("0x{}", "dd".repeat(32))}],
        }))
        .expect("an unusable log is not an unusable receipt");
        assert!(receipt.logs.is_empty());
    }

    #[test]
    fn a_selector_is_read_from_calldata_and_nothing_else() {
        let frame = frame_from_pending_value(
            chain(),
            &pending(
                REAL,
                vec![tx(
                    0x22,
                    Some("0x37cd68c3aa5cbb95918844f0cc341d1905f4fcbd"),
                    "0x1272dc25deadbeef",
                )],
            ),
            1,
            10,
            "rpc-x",
        )
        .expect("a full transaction decodes");
        let tx = frame.transactions.first().expect("one transaction");
        assert_eq!(tx.selector, Field::Known([0x12, 0x72, 0xdc, 0x25]));
        assert!(tx.to.is_known());
    }
}
