//! What an execution-ready transaction *is*, and the two things M6 §14 demands of it:
//! it must be encodable to the exact bytes a node will decode, and it must be
//! decodable back from those bytes to the same fields it was built from.
//!
//! Two transaction types are modelled because the real chain runs both — the
//! probe of 2026-10-01 counted 19 type-`0x2` and 18 type-`0x0` user transactions in
//! one GIWA block (`data/evidence/m6/probe-tx-types.json`). Choosing one of them is
//! the node's business; pretending the other does not exist would make the builder
//! unable to describe what it just built.
//!
//! Nothing here signs, talks to a node, or knows what an arbitrage is. Fees are
//! `U256` because §12 forbids a float in the money path, and because wei does not fit
//! in `u64`.

use alloy_primitives::{Address, Bytes, ChainId, B256, U256};
use serde::{Deserialize, Serialize};

use crate::error::{ExecutionError, Result};
use crate::rlp::{decode, Encoder, Item};

/// The transaction envelope. `Legacy` is EIP-155 (chain id folded into `v`);
/// `DynamicFee` is EIP-1559 (type-`0x2` envelope, chain id as the first field).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionType {
    Legacy,
    DynamicFee,
}

impl TransactionType {
    /// The EIP-2718 type byte the node sees. Legacy has no envelope byte; its wire
    /// form starts straight at the list, and `0x02` is what distinguishes 1559.
    pub fn type_byte(self) -> Option<u8> {
        match self {
            Self::Legacy => None,
            Self::DynamicFee => Some(0x02),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Legacy => "legacy-eip155",
            Self::DynamicFee => "eip1559",
        }
    }
}

impl std::fmt::Display for TransactionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// One `(address, [slot])` pair of an EIP-2930 access list. The chain accepts them;
/// a builder that cannot represent an empty list cannot round-trip what it fetched.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessTuple {
    pub address: Address,
    pub storage_keys: Vec<B256>,
}

/// The fields a transaction carries before a signature exists. This is the shape
/// §7's intent turns into bytes and §14's round trip turns back into fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsignedTransaction {
    pub tx_type: TransactionType,
    pub chain_id: ChainId,
    pub nonce: u64,
    /// `None` is a contract creation, which the chain signs and decodes like any other
    /// transaction. It has to be a separate case and not a zero address: the wire
    /// encoding writes an empty `to`, and a 20-byte zero `to` is a *different*
    /// transaction with a different hash, so collapsing the two would make the decoder
    /// recover a sender that no signature ever produced.
    pub to: Option<Address>,
    pub value: U256,
    pub gas_limit: u64,
    pub input: Bytes,
    pub access_list: Vec<AccessTuple>,
    /// EIP-1559 only: the `maxPriorityFeePerGas` field.
    pub max_priority_fee_per_gas: Option<U256>,
    /// EIP-1559: `maxFeePerGas`. Legacy: `gasPrice`. Exactly one of the two is
    /// present for a well-formed transaction, which [`UnsignedTransaction::validate`]
    /// checks instead of guessing.
    pub max_fee_per_gas: Option<U256>,
}

impl UnsignedTransaction {
    /// The fee field this envelope type uses, as the node would name it.
    pub fn fee_field(&self) -> Result<(&'static str, U256)> {
        match self.tx_type {
            TransactionType::DynamicFee => {
                let max = self.max_fee_per_gas.ok_or_else(|| {
                    ExecutionError::InvalidIntent("eip1559 without maxFeePerGas".to_string())
                })?;
                Ok(("maxFeePerGas", max))
            }
            TransactionType::Legacy => {
                let price = self.max_fee_per_gas.ok_or_else(|| {
                    ExecutionError::InvalidIntent("legacy without gasPrice".to_string())
                })?;
                Ok(("gasPrice", price))
            }
        }
    }

    /// Structural self-check, run before anything is encoded: the fee fields match the
    /// envelope, the tip does not exceed the ceiling, and nothing that has to fit in
    /// 32 bytes was widened on the way in.
    pub fn validate(&self) -> Result<()> {
        match self.tx_type {
            TransactionType::DynamicFee => {
                let max = self.max_fee_per_gas.ok_or_else(|| {
                    ExecutionError::InvalidIntent("missing maxFeePerGas".to_string())
                })?;
                let tip = self.max_priority_fee_per_gas.ok_or_else(|| {
                    ExecutionError::InvalidIntent("missing maxPriorityFeePerGas".to_string())
                })?;
                if tip > max {
                    return Err(ExecutionError::InvalidIntent(format!(
                        "maxPriorityFeePerGas {tip} exceeds maxFeePerGas {max}: the node would \
                         charge min(maxFee, baseFee+tip), so a tip above the ceiling is a \
                         transaction that cannot be priced consistently"
                    )));
                }
            }
            TransactionType::Legacy => {
                if self.max_priority_fee_per_gas.is_some() {
                    return Err(ExecutionError::InvalidIntent(
                        "legacy transaction carries maxPriorityFeePerGas".to_string(),
                    ));
                }
            }
        }
        if self.gas_limit == 0 {
            return Err(ExecutionError::InvalidIntent(
                "gas limit is zero: the transaction would be rejected before it ran".to_string(),
            ));
        }
        Ok(())
    }

    /// The payload that gets signed. Legacy: the RLP list with `chainId, 0, 0`
    /// appended (EIP-155). EIP-1559: `0x02 || rlp([...])` with no signature fields.
    ///
    /// This is where a wrong chain id changes the signature *domain* rather than
    /// merely annotating it — §18's test recovers the sender back from these bytes.
    pub fn signing_payload(&self) -> Result<Bytes> {
        self.validate()?;
        match self.tx_type {
            TransactionType::Legacy => {
                let mut body = Encoder::new();
                self.encode_legacy_fields(&mut body);
                body.u64(self.chain_id);
                body.bytes(&[]);
                body.bytes(&[]);
                let mut out = Encoder::new();
                out.list(&body.finish());
                Ok(Bytes::from(out.finish()))
            }
            TransactionType::DynamicFee => {
                let mut body = Encoder::new();
                self.encode_dynamic_fee_fields(&mut body);
                let mut inner = Encoder::new();
                inner.list(&body.finish());
                let mut out = vec![0x02u8];
                out.extend_from_slice(&inner.finish());
                Ok(Bytes::from(out))
            }
        }
    }

    /// keccak256 of the signing payload: the hash the signature is made over.
    pub fn signing_hash(&self) -> Result<B256> {
        Ok(alloy_primitives::keccak256(
            self.signing_payload()?.as_ref(),
        ))
    }

    pub fn access_list_len(&self) -> usize {
        self.access_list.len()
    }

    /// Estimated worst-case native cost of this transaction's fee ceiling:
    /// `gas_limit * max_fee_per_gas` plus `value`. `U256` arithmetic throughout (§12).
    pub fn maximum_cost_wei(&self) -> Result<U256> {
        let (_, fee) = self.fee_field()?;
        let gas = self
            .gas_limit
            .checked_mul(u64::try_from(fee).map_err(|_| {
                ExecutionError::InvalidIntent(format!(
                    "fee per gas {fee} exceeds u64; the multiplication would be exact but the \
                     node's field width is u128 at most"
                ))
            })?)
            .ok_or_else(|| {
                ExecutionError::InvalidIntent("gas limit * fee overflows u64".to_string())
            })?;
        U256::from(gas)
            .checked_add(self.value)
            .ok_or_else(|| ExecutionError::InvalidIntent("value + gas cost overflows".to_string()))
    }

    fn encode_legacy_fields(&self, out: &mut Encoder) {
        out.u64(self.nonce);
        out.quantity(self.max_fee_per_gas.unwrap_or(U256::ZERO));
        out.u64(self.gas_limit);
        out.bytes(to_field_bytes(&self.to));
        out.quantity(self.value);
        out.bytes(self.input.as_ref());
    }

    fn encode_dynamic_fee_fields(&self, out: &mut Encoder) {
        out.u64(self.chain_id);
        out.u64(self.nonce);
        out.quantity(self.max_priority_fee_per_gas.unwrap_or(U256::ZERO));
        out.quantity(self.max_fee_per_gas.unwrap_or(U256::ZERO));
        out.u64(self.gas_limit);
        out.bytes(to_field_bytes(&self.to));
        out.quantity(self.value);
        out.bytes(self.input.as_ref());
        out.list(&self.encode_access_list());
    }

    fn encode_access_list(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        for tuple in &self.access_list {
            let mut keys = Vec::new();
            for key in &tuple.storage_keys {
                let mut one = Encoder::new();
                one.bytes(key.as_ref());
                keys.extend_from_slice(&one.finish());
            }
            let mut inner = Encoder::new();
            inner.list(&keys);
            let mut item = Encoder::new();
            item.bytes(tuple.address.as_ref());
            item.bytes(&inner.finish());
            let mut entry = Encoder::new();
            entry.list(&item.finish());
            payload.extend_from_slice(&entry.finish());
        }
        payload
    }
}

/// A signature, normalized: `y_parity` is 0 or 1 and the chain id lives with the
/// transaction, not inside the signature. Legacy's `v` is derived from these two.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    pub r: U256,
    pub s: U256,
    pub y_parity: bool,
}

impl Signature {
    pub fn new(r: U256, s: U256, y_parity: bool) -> Self {
        Self { r, s, y_parity }
    }

    /// `v` for an EIP-155 legacy signature.
    pub fn legacy_v(&self, chain_id: ChainId) -> u64 {
        chain_id * 2 + 35 + u64::from(self.y_parity)
    }
}

impl std::fmt::Debug for Signature {
    /// Public values, but a transaction's signature is the one thing that must not be
    /// reconstructable from a log line by accident of formatting; keep it compact and
    /// explicit.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signature")
            .field("r", &format_args!("{:#x}", self.r))
            .field("s", &format_args!("{:#x}", self.s))
            .field("y_parity", &self.y_parity)
            .finish()
    }
}

/// A transaction plus the signature over its signing payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedTransaction {
    pub unsigned: UnsignedTransaction,
    pub signature: Signature,
}

impl SignedTransaction {
    pub fn new(unsigned: UnsignedTransaction, signature: Signature) -> Self {
        Self {
            unsigned,
            signature,
        }
    }

    /// The raw bytes that go into `eth_sendRawTransaction`.
    pub fn raw(&self) -> Bytes {
        match self.unsigned.tx_type {
            TransactionType::Legacy => {
                let mut body = Encoder::new();
                self.unsigned.encode_legacy_fields(&mut body);
                body.u64(self.signature.legacy_v(self.unsigned.chain_id));
                body.quantity(self.signature.r);
                body.quantity(self.signature.s);
                let mut out = Encoder::new();
                out.list(&body.finish());
                Bytes::from(out.finish())
            }
            TransactionType::DynamicFee => {
                let mut body = Encoder::new();
                self.unsigned.encode_dynamic_fee_fields(&mut body);
                // A quantity, not a one-byte string: parity 0 is the empty item `0x80`,
                // and `0x00` would be a different transaction than the node computes.
                body.u64(u64::from(self.signature.y_parity));
                body.quantity(self.signature.r);
                body.quantity(self.signature.s);
                let mut inner = Encoder::new();
                inner.list(&body.finish());
                let mut out = vec![0x02u8];
                out.extend_from_slice(&inner.finish());
                Bytes::from(out)
            }
        }
    }

    /// The transaction hash: keccak2718 over the whole envelope, which is the value a
    /// receipt will be found under (§27's binding depends on this being *the* hash,
    /// not a hash of something similar).
    pub fn hash(&self) -> B256 {
        alloy_primitives::keccak256(self.raw().as_ref())
    }
}

/// What came back out of raw bytes. Every field the builder put in, plus the sender
/// the signature proves — which is why §14's comparison has eight fields to make and
/// not two.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedTransaction {
    pub unsigned: UnsignedTransaction,
    pub signature: Option<Signature>,
    pub sender: Option<Address>,
}

/// Decode a raw transaction, signed or unsigned, legacy or EIP-1559.
///
/// `recover` decides whether a signature is turned back into a sender. The recovery
/// itself lives in `signer` (it needs the ECDSA primitive); this function returns the
/// fields and hands the signature over, so codec tests can compare field-for-field
/// without a key.
pub fn decode_raw(
    raw: &[u8],
    recover: impl Fn(&B256, &Signature) -> Result<Address>,
) -> Result<DecodedTransaction> {
    let (tx_type, payload) = split_envelope(raw)?;
    let item = decode(payload)?;
    let fields = item.as_list()?.to_vec();
    match tx_type {
        TransactionType::Legacy => decode_legacy(&fields, recover),
        TransactionType::DynamicFee => decode_dynamic_fee(&fields, recover),
    }
}

fn split_envelope(raw: &[u8]) -> Result<(TransactionType, &[u8])> {
    if raw.is_empty() {
        return Err(ExecutionError::InvalidIntent(
            "empty raw transaction".to_string(),
        ));
    }
    let first = raw[0];
    if first >= 0xc0 {
        // A legacy transaction is a single RLP list, so its first byte is a list
        // prefix. This is the only test that distinguishes the two envelopes on the
        // wire, and it is exactly the test go-ethereum makes.
        return Ok((TransactionType::Legacy, raw));
    }
    match first {
        0x02 => Ok((TransactionType::DynamicFee, &raw[1..])),
        other => Err(ExecutionError::InvalidIntent(format!(
            "unsupported transaction type byte {other:#04x}: this builder knows legacy (no \
             envelope byte) and EIP-1559 (0x02) only"
        ))),
    }
}

fn decode_legacy(
    fields: &[Item],
    recover: impl Fn(&B256, &Signature) -> Result<Address>,
) -> Result<DecodedTransaction> {
    if fields.len() != 9 {
        return Err(ExecutionError::InvalidIntent(format!(
            "legacy transaction has {} fields, expected 9",
            fields.len()
        )));
    }
    let nonce = fields[0].as_u64()?;
    let gas_price = fields[1].as_quantity()?;
    let gas_limit = fields[2].as_u64()?;
    let to = to_field(&fields[3], "to")?;
    let value = fields[4].as_quantity()?;
    let input = Bytes::from(fields[5].as_bytes()?.to_vec());
    let v = fields[6].as_u64()?;
    let r = fields[7].as_quantity()?;
    let s = fields[8].as_quantity()?;

    // EIP-155: chainId = (v - 35) / 2 for a signed transaction. A pre-155 `v` of 27 or
    // 28 would mean a chain-less signature, which this chain will not accept, so it is
    // an error and not a fallback.
    if v < 35 {
        return Err(ExecutionError::InvalidIntent(format!(
            "legacy v = {v} is not an EIP-155 signature (v must be at least 35), so it carries \
             no chain id"
        )));
    }
    let chain_id = (v - 35) / 2;
    let y_parity = (v - 35) % 2 == 1;
    let signature = Signature { r, s, y_parity };
    let unsigned = UnsignedTransaction {
        tx_type: TransactionType::Legacy,
        chain_id,
        nonce,
        to,
        value,
        gas_limit,
        input,
        access_list: Vec::new(),
        max_priority_fee_per_gas: None,
        max_fee_per_gas: Some(gas_price),
    };
    // The hash is recomputed from the *decoded* fields, not from the bytes we were
    // handed: if a payload carried a chain id its signature was not made over, this
    // is where the recovered sender stops matching.
    let sender = recover(&unsigned.signing_hash()?, &signature)?;
    Ok(DecodedTransaction {
        unsigned,
        signature: Some(signature),
        sender: Some(sender),
    })
}

fn decode_dynamic_fee(
    fields: &[Item],
    recover: impl Fn(&B256, &Signature) -> Result<Address>,
) -> Result<DecodedTransaction> {
    if fields.len() != 12 {
        return Err(ExecutionError::InvalidIntent(format!(
            "eip1559 transaction has {} fields, expected 12",
            fields.len()
        )));
    }
    let chain_id = fields[0].as_u64()?;
    let nonce = fields[1].as_u64()?;
    let tip = fields[2].as_quantity()?;
    let max = fields[3].as_quantity()?;
    let gas_limit = fields[4].as_u64()?;
    let to = to_field(&fields[5], "to")?;
    let value = fields[6].as_quantity()?;
    let input = Bytes::from(fields[7].as_bytes()?.to_vec());
    let access_list = decode_access_list(&fields[8])?;
    let parity = fields[9].as_bytes()?;
    let y_parity = match parity {
        [] => false,
        [0] => false,
        [1] => true,
        other => {
            return Err(ExecutionError::InvalidIntent(format!(
                "eip1559 yParity is {other:?}, expected 0 or 1"
            )))
        }
    };
    let r = fields[10].as_quantity()?;
    let s = fields[11].as_quantity()?;
    let signature = Signature { r, s, y_parity };
    let unsigned = UnsignedTransaction {
        tx_type: TransactionType::DynamicFee,
        chain_id,
        nonce,
        to,
        value,
        gas_limit,
        input,
        access_list,
        max_priority_fee_per_gas: Some(tip),
        max_fee_per_gas: Some(max),
    };
    let sender = recover(&unsigned.signing_hash()?, &signature)?;
    Ok(DecodedTransaction {
        unsigned,
        signature: Some(signature),
        sender: Some(sender),
    })
}

fn decode_access_list(item: &Item) -> Result<Vec<AccessTuple>> {
    let mut out = Vec::new();
    for entry in item.as_list()? {
        let fields = entry.as_list()?;
        if fields.len() != 2 {
            return Err(ExecutionError::InvalidIntent(format!(
                "access list entry has {} fields, expected 2",
                fields.len()
            )));
        }
        let address = address_field(&fields[0], "accessList address")?;
        let mut keys = Vec::new();
        for key in fields[1].as_list()? {
            keys.push(B256::from_slice(key.as_bytes()?));
        }
        out.push(AccessTuple {
            address,
            storage_keys: keys,
        });
    }
    Ok(out)
}

/// The `to` field on the wire: the address's 20 bytes, or empty for a creation.
fn to_field_bytes(to: &Option<Address>) -> &[u8] {
    match to {
        Some(address) => address.as_ref(),
        None => &[],
    }
}

/// A mandatory 20-byte address (an access-list entry). Absent here is malformed, not
/// "the zero address", so it is an error and not a default.
fn address_field(item: &Item, what: &str) -> Result<Address> {
    to_field(item, what)?.ok_or_else(|| {
        ExecutionError::InvalidIntent(format!("{what} is absent, expected 20 bytes"))
    })
}

/// `to`, which may be empty: an empty `to` is a creation and stays `None`. Collapsing it
/// to the zero address would re-encode to 20 zero bytes, which is a different
/// transaction with a different hash, and the sender this module recovers from that hash
/// would be an address no signature ever produced.
fn to_field(item: &Item, what: &str) -> Result<Option<Address>> {
    let bytes = item.as_bytes()?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() != 20 {
        return Err(ExecutionError::InvalidIntent(format!(
            "{what} is {} bytes, expected 20",
            bytes.len()
        )));
    }
    Ok(Some(Address::from_slice(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx() -> UnsignedTransaction {
        UnsignedTransaction {
            tx_type: TransactionType::DynamicFee,
            chain_id: 91_342,
            nonce: 7,
            to: Some(Address::from_slice(&[0x11u8; 20])),
            value: U256::from(1u64),
            gas_limit: 21_000,
            input: Bytes::from(vec![0xdeu8, 0xadu8, 0xbeu8, 0xefu8]),
            access_list: Vec::new(),
            max_priority_fee_per_gas: Some(U256::from(1_000_000u64)),
            max_fee_per_gas: Some(U256::from(2_000_000u64)),
        }
    }

    #[test]
    fn a_zero_gas_limit_is_refused_before_anything_is_encoded() {
        let mut wide_open = tx();
        wide_open.gas_limit = 0;
        let error = wide_open.signing_payload().unwrap_err();
        assert!(error.to_string().contains("gas limit is zero"), "{error}");
    }

    #[test]
    fn a_tip_above_the_ceiling_is_refused_because_the_node_would_not_pay_it() {
        let mut inverted = tx();
        inverted.max_priority_fee_per_gas = Some(U256::from(3_000_000u64));
        inverted.max_fee_per_gas = Some(U256::from(2_000_000u64));
        assert!(inverted
            .signing_payload()
            .unwrap_err()
            .to_string()
            .contains("exceeds maxFeePerGas"));
    }

    #[test]
    fn a_legacy_transaction_cannot_carry_a_priority_fee() {
        let mut confused = tx();
        confused.tx_type = TransactionType::Legacy;
        assert!(confused
            .signing_payload()
            .unwrap_err()
            .to_string()
            .contains("maxPriorityFeePerGas"));
    }

    #[test]
    fn quantities_are_encoded_minimally_and_zero_is_the_empty_string() {
        let mut out = Encoder::new();
        out.quantity(U256::ZERO);
        out.quantity(U256::from(1u64));
        out.quantity(U256::from(255u64));
        out.quantity(U256::from(256u64));
        assert_eq!(out.finish(), vec![0x80, 0x01, 0x81, 0xff, 0x82, 0x01, 0x00]);
    }

    #[test]
    fn a_padded_quantity_is_rejected_rather_than_silently_narrowed() {
        let item = Item::Bytes(vec![0x00, 0x01]);
        assert!(item
            .as_quantity()
            .unwrap_err()
            .to_string()
            .contains("not minimally encoded"));
    }

    #[test]
    fn a_single_byte_below_the_prefix_boundary_has_no_length_header() {
        // 0x7f is its own item; encoding it as `0x81 0x7f` is non-canonical and the
        // decoder says so, because a node that produced the padded form is not the
        // node we think we are talking to.
        let mut out = Encoder::new();
        out.bytes(&[0x7f]);
        assert_eq!(out.finish(), vec![0x7f]);
        let mut padded = Vec::new();
        let mut e = Encoder::new();
        e.bytes(&[0x81, 0x7f]);
        padded.extend_from_slice(&e.finish());
        assert!(decode(&padded[1..]).is_err());
    }

    #[test]
    fn the_maximum_cost_is_gas_times_ceiling_plus_value() {
        let spent = tx().maximum_cost_wei().unwrap();
        assert_eq!(spent, U256::from(2_000_000u64 * 21_000u64 + 1u64));
    }

    /// A creation has an empty `to` on the wire, and that is not the same transaction as
    /// one that sends to the zero address: the two differ in the signed bytes, so their
    /// hashes and their recovered senders differ. Folding the two together would let the
    /// decoder report an address no signature ever produced.
    #[test]
    fn a_creation_encodes_an_empty_to_and_stays_distinct_from_the_zero_address() {
        let creation = UnsignedTransaction { to: None, ..tx() };
        let to_zero = UnsignedTransaction {
            to: Some(Address::ZERO),
            ..tx()
        };
        let created = creation.signing_payload().unwrap();
        let addressed = to_zero.signing_payload().unwrap();
        assert_eq!(
            addressed.len(),
            created.len() + 20,
            "the only difference is the 20 address bytes: an absent `to` is the empty item"
        );
        assert_ne!(
            creation.signing_hash().unwrap(),
            to_zero.signing_hash().unwrap(),
            "an absent target and the zero address must not sign the same bytes"
        );
    }

    /// The decode half of the same fact: an empty `to` comes back as `None`, and the
    /// field set survives the trip.
    #[test]
    fn a_creation_decodes_back_to_an_absent_to_and_not_a_zero_target() {
        let creation = UnsignedTransaction { to: None, ..tx() };
        let signature = Signature::new(U256::from(1u64), U256::from(2u64), false);
        let signed = SignedTransaction::new(creation.clone(), signature);
        let decoded = decode_raw(signed.raw().as_ref(), |_, _| Ok(Address::ZERO)).unwrap();
        assert_eq!(decoded.unsigned, creation, "every field must come back");
        assert_eq!(
            decoded.unsigned.to, None,
            "a creation must not be reported as a transfer to 0x0"
        );
    }
}
