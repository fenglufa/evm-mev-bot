//! §14's acceptance, run against transactions the chain actually produced.
//!
//! The task book asks for more than "our encoder and our decoder agree with each
//! other": two implementations of the same wrong idea agree perfectly. So every case
//! here is anchored on values a node returned — the transaction hash in a hydrated
//! block and the `from` address the node itself reports — and the comparison runs in
//! both directions:
//!
//! * the bytes our builder produces must hash to the hash the provider names, which
//!   means the RLP, the envelope byte, the minimal quantity encoding and the EIP-155
//!   `v` are all right at once;
//! * the signature over those bytes must recover the `from` the provider names, which
//!   means the *signing domain* is right, not merely its length;
//! * re-decoding those bytes must return the fields the provider named, field by field
//!   and not only `to` and `data`;
//! * a wrong chain id must not recover that sender, and a one-field change must not
//!   reproduce that hash — the two controls that make the matches above evidence
//!   instead of a coincidence.
//!
//! The fixture carries no secrets: it is `eth_getBlockByNumber(number, true)` output —
//! public chain values, addresses that already appear in every block explorer. The
//! chain id is read from the fixture, never written as a literal here, so this test
//! stays a test of the codec and not of one network (§44, §45).

use alloy_primitives::{Address, Bytes, ChainId, B256, U256};
use evm_execution::{
    decode_raw, recover_sender, signing_hash_for_chain, Signature, SignedTransaction,
    TransactionType, UnsignedTransaction,
};
use serde_json::Value;

const FIXTURE: &str = include_str!("fixtures/real-transactions-91342.json");

/// The four cases the collection holds, in the order the fixture lists them.
const CASES: [&str; 4] = ["eip1559", "eip1559_value", "legacy_call", "legacy_create"];

/// One provider row, plus the transaction our codec builds from the same fields.
struct Case {
    name: &'static str,
    row: Value,
    unsigned: UnsignedTransaction,
    signature: Signature,
}

impl Case {
    /// The hash the provider put in the block for this transaction.
    fn provider_hash(&self) -> B256 {
        fixed(&self.row, "hash", 32)
    }

    /// The address the provider names as the sender, which is what the signature must
    /// recover to.
    fn provider_sender(&self) -> Address {
        address(&self.row, "from").expect("`from` is always present in a hydrated block")
    }

    fn provider_chain_id(&self) -> ChainId {
        number(&self.row, "chainId")
    }

    fn signed(&self) -> SignedTransaction {
        SignedTransaction::new(self.unsigned.clone(), self.signature)
    }

    /// The transaction's fields, as text, in one fixed order.
    ///
    /// The provider side of the pair: every value here comes from the fixture's JSON, so
    /// the comparison in [`redecoding_returns_every_field_the_chain_named`] is against
    /// what the chain answered and not against what this file fed itself.
    ///
    /// The access list is compared by entry count only: all four collected transactions
    /// carry an empty one, and a parser for the non-empty case would be code no fixture
    /// exercises. The crate's own [`crate::TransactionBuilder::round_trip`] compares the
    /// full list on the transactions it builds.
    fn provider_fields(&self) -> Vec<(&'static str, String)> {
        let fee = match self.unsigned.tx_type {
            TransactionType::Legacy => vec![
                ("gasPrice", quantity(&self.row, "gasPrice").to_string()),
                (
                    "maxPriorityFeePerGas",
                    optional_quantity(&self.row, "maxPriorityFeePerGas")
                        .map(|tip| tip.to_string())
                        .unwrap_or_else(|| "absent".to_string()),
                ),
            ],
            TransactionType::DynamicFee => vec![
                (
                    "maxFeePerGas",
                    quantity(&self.row, "maxFeePerGas").to_string(),
                ),
                (
                    "maxPriorityFeePerGas",
                    quantity(&self.row, "maxPriorityFeePerGas").to_string(),
                ),
            ],
        };
        let mut out = vec![
            ("chain_id", self.provider_chain_id().to_string()),
            ("nonce", number(&self.row, "nonce").to_string()),
            ("to", to_text(address(&self.row, "to"))),
            ("value", quantity(&self.row, "value").to_string()),
            (
                "input",
                format!("0x{}", hex::encode(bytes(&self.row, "input"))),
            ),
            ("gas_limit", number(&self.row, "gas").to_string()),
        ];
        out.extend(fee);
        out.push(("access_list", access_list_count(&self.row).to_string()));
        out.push((
            "type",
            envelope_name(&self.row).unwrap_or_else(|why| panic!("{}: {why}", self.name)),
        ));
        out
    }

    /// The same field list, read out of the bytes this crate built and re-decoded.
    fn decoded_fields(unsigned: &UnsignedTransaction) -> Vec<(&'static str, String)> {
        let fee = match unsigned.tx_type {
            TransactionType::Legacy => vec![
                (
                    "gasPrice",
                    unsigned
                        .max_fee_per_gas
                        .expect("a legacy transaction decodes a gasPrice")
                        .to_string(),
                ),
                (
                    "maxPriorityFeePerGas",
                    unsigned
                        .max_priority_fee_per_gas
                        .map(|tip| tip.to_string())
                        .unwrap_or_else(|| "absent".to_string()),
                ),
            ],
            TransactionType::DynamicFee => vec![
                (
                    "maxFeePerGas",
                    unsigned
                        .max_fee_per_gas
                        .expect("an eip1559 transaction decodes a maxFeePerGas")
                        .to_string(),
                ),
                (
                    "maxPriorityFeePerGas",
                    unsigned
                        .max_priority_fee_per_gas
                        .expect("an eip1559 transaction decodes a tip")
                        .to_string(),
                ),
            ],
        };
        let mut out = vec![
            ("chain_id", unsigned.chain_id.to_string()),
            ("nonce", unsigned.nonce.to_string()),
            ("to", to_text(unsigned.to)),
            ("value", unsigned.value.to_string()),
            (
                "input",
                format!("0x{}", hex::encode(unsigned.input.as_ref())),
            ),
            ("gas_limit", unsigned.gas_limit.to_string()),
        ];
        out.extend(fee);
        out.push(("access_list", unsigned.access_list.len().to_string()));
        out.push(("type", unsigned.tx_type.name().to_string()));
        out
    }
}

/// An absent target and the zero address are different transactions; the field text says
/// which one the bytes carried.
fn to_text(to: Option<Address>) -> String {
    match to {
        Some(target) => target.to_string(),
        None => "<creation>".to_string(),
    }
}

/// A legacy row carries no `accessList` field at all — the envelope has no such slot —
/// so absent is zero entries rather than a missing value to panic over.
fn access_list_count(row: &Value) -> usize {
    match row.get("accessList") {
        None | Some(Value::Null) => 0,
        Some(list) => list
            .as_array()
            .unwrap_or_else(|| panic!("`accessList` is not an array"))
            .len(),
    }
}

/// `0x2` and `0x0` are the provider's type numbers; the names are what this crate's
/// envelope enum calls them.
fn envelope_name(row: &Value) -> std::result::Result<String, String> {
    let ty = number(row, "type");
    match ty {
        0 => Ok("legacy-eip155".to_string()),
        2 => Ok("eip1559".to_string()),
        other => Err(format!(
            "the fixture holds type {other:#x}, which this codec does not model; the test needs \
             updating rather than the data replacing"
        )),
    }
}

fn cases() -> Vec<Case> {
    let document: Value = serde_json::from_str(FIXTURE).expect("the fixture is one JSON document");
    CASES
        .iter()
        .map(|name| {
            let row = document
                .get(name)
                .unwrap_or_else(|| panic!("the fixture has no `{name}` case"))
                .clone();
            let tx_type = match number(&row, "type") {
                0 => TransactionType::Legacy,
                2 => TransactionType::DynamicFee,
                other => panic!("{name}: unsupported type {other:#x}"),
            };
            let (fee, tip) = match tx_type {
                TransactionType::Legacy => (Some(quantity(&row, "gasPrice")), None),
                TransactionType::DynamicFee => (
                    Some(quantity(&row, "maxFeePerGas")),
                    Some(quantity(&row, "maxPriorityFeePerGas")),
                ),
            };
            let chain_id = number(&row, "chainId");
            let signature = Signature::new(
                quantity(&row, "r"),
                quantity(&row, "s"),
                y_parity(&row, tx_type, chain_id),
            );
            let unsigned = UnsignedTransaction {
                tx_type,
                chain_id,
                nonce: number(&row, "nonce"),
                to: address(&row, "to"),
                value: quantity(&row, "value"),
                gas_limit: number(&row, "gas"),
                input: Bytes::from(bytes(&row, "input")),
                access_list: Vec::new(),
                max_priority_fee_per_gas: tip,
                max_fee_per_gas: fee,
            };
            Case {
                name,
                row,
                unsigned,
                signature,
            }
        })
        .collect()
}

/// The parity bit, taken from wherever the provider actually puts it: an EIP-155 legacy
/// signature carries it inside `v`, so deriving it is part of what this test proves.
fn y_parity(row: &Value, tx_type: TransactionType, chain_id: ChainId) -> bool {
    match tx_type {
        TransactionType::Legacy => {
            let v = number(row, "v");
            assert_eq!(
                (v - 35) / 2,
                chain_id,
                "legacy v = {v} does not carry chain id {chain_id}: the fixture is not EIP-155 \
                 consistent"
            );
            (v - 35) % 2 == 1
        }
        TransactionType::DynamicFee => number(row, "yParity") == 1,
    }
}

fn digits<'a>(row: &'a Value, field: &str) -> &'a str {
    row.get(field)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("`{field}` is absent or not a hex string"))
        .strip_prefix("0x")
        .unwrap_or_else(|| panic!("`{field}` is not 0x-prefixed"))
}

fn quantity(row: &Value, field: &str) -> U256 {
    let text = digits(row, field);
    let mut out = U256::ZERO;
    for c in text.chars() {
        let digit = c
            .to_digit(16)
            .unwrap_or_else(|| panic!("`{field}` = {text} is not hex"));
        out = out * U256::from(16u64) + U256::from(u64::from(digit));
    }
    out
}

/// A field the provider only includes for some envelopes: legacy rows carry no
/// `maxPriorityFeePerGas` at all, and "absent" has to stay distinguishable from zero.
fn optional_quantity(row: &Value, field: &str) -> Option<U256> {
    match row.get(field) {
        None | Some(Value::Null) => None,
        Some(_) => Some(quantity(row, field)),
    }
}

fn number(row: &Value, field: &str) -> u64 {
    let text = digits(row, field);
    u64::from_str_radix(text, 16).unwrap_or_else(|error| panic!("`{field}` = {text}: {error}"))
}

fn bytes(row: &Value, field: &str) -> Vec<u8> {
    let text = digits(row, field);
    hex::decode(text).unwrap_or_else(|error| panic!("`{field}` = {text}: {error}"))
}

fn fixed(row: &Value, field: &str, width: usize) -> B256 {
    let raw = bytes(row, field);
    assert_eq!(raw.len(), width, "`{field}` is {} bytes", raw.len());
    B256::from_slice(&raw)
}

/// An optional address: `to` is `null` for a creation, and every other address field the
/// fixture carries is present.
fn address(row: &Value, field: &str) -> Option<Address> {
    match row.get(field) {
        Some(Value::Null) | None => None,
        Some(_) => {
            let raw = bytes(row, field);
            assert_eq!(
                raw.len(),
                20,
                "`{field}` is {} bytes, expected an address",
                raw.len()
            );
            Some(Address::from_slice(&raw))
        }
    }
}

#[test]
fn the_built_bytes_hash_to_the_hash_the_chain_published() {
    // If this fails, one of: the RLP item framing, the minimal quantity encoding, the
    // envelope byte, or EIP-155's `v` is wrong. All four are invisible to a codec that
    // only ever compares itself against itself.
    for case in cases() {
        let signed = case.signed();
        assert_eq!(
            signed.hash(),
            case.provider_hash(),
            "{}: our {} bytes hash to {:#x}, the chain calls it {:#x}",
            case.name,
            signed.unsigned.tx_type.name(),
            signed.hash(),
            case.provider_hash()
        );
    }
}

#[test]
fn the_signature_recovers_the_sender_the_chain_names() {
    for case in cases() {
        let prehash = case
            .unsigned
            .signing_hash()
            .unwrap_or_else(|error| panic!("{}: {}", case.name, error));
        let sender = recover_sender(&prehash, &case.signature)
            .unwrap_or_else(|error| panic!("{}: {}", case.name, error));
        assert_eq!(
            sender,
            case.provider_sender(),
            "{}: the signature recovers {sender}, the chain says {}",
            case.name,
            case.provider_sender()
        );
    }
}

#[test]
fn redecoding_returns_every_field_the_chain_named() {
    for case in cases() {
        let raw = case.signed().raw();
        let decoded = decode_raw(raw.as_ref(), recover_sender)
            .unwrap_or_else(|error| panic!("{}: {}", case.name, error));
        let expected = case.provider_fields();
        let found = Case::decoded_fields(&decoded.unsigned);
        assert_eq!(
            expected.len(),
            found.len(),
            "{}: the two sides stopped naming the same number of fields",
            case.name
        );
        for (index, (field, want)) in expected.into_iter().enumerate() {
            let (found_field, got) = &found[index];
            assert_eq!(field, *found_field, "the field lists drifted apart");
            assert_eq!(
                &want, got,
                "{}: the chain's {field} is {want}, re-decoding our bytes says {got}",
                case.name
            );
        }
        // The signature the decoder read back is the one the chain published.
        let signature = decoded
            .signature
            .expect("a signed transaction decodes a signature");
        assert_eq!(
            signature, case.signature,
            "{}: the signature changed",
            case.name
        );
        assert_eq!(
            decoded.sender.expect("recovery ran"),
            case.provider_sender(),
            "{}: the decoder's sender",
            case.name
        );
    }
}

#[test]
fn a_creation_comes_back_as_an_absent_target_and_not_as_the_zero_address() {
    // The chain runs creations. A decoder that folded an empty `to` into 0x0 would
    // re-encode to a different transaction and then recover a sender that no signature
    // ever produced, so this case is checked separately from the field table.
    let creation = cases()
        .into_iter()
        .find(|case| case.name == "legacy_create")
        .expect("the fixture carries a creation");
    assert_eq!(creation.unsigned.to, None, "the fixture's `to` moved");
    let decoded = decode_raw(creation.signed().raw().as_ref(), recover_sender).unwrap();
    assert_eq!(
        decoded.unsigned.to, None,
        "the empty `to` did not survive the trip"
    );
    assert_ne!(
        decoded.unsigned.to,
        Some(Address::ZERO),
        "a creation was reported as a transfer to 0x0"
    );
    assert_eq!(decoded.sender.unwrap(), creation.provider_sender());
}

#[test]
fn the_same_bytes_on_another_chain_id_do_not_recover_this_sender() {
    // EIP-155's point: the chain id is part of what is signed, so a signature made for
    // one network is not a signature for another. A codec that treated the chain id as
    // an annotation would pass every other test in this file and still be wrong.
    for case in cases() {
        let foreign = case.provider_chain_id() + 1;
        let prehash = signing_hash_for_chain(&case.unsigned, foreign).unwrap();
        let sender = recover_sender(&prehash, &case.signature).unwrap();
        assert_ne!(
            sender,
            case.provider_sender(),
            "{}: reading the same signature against chain id {foreign} recovered the same \
             sender, so the chain id is not in the signed payload",
            case.name
        );
    }
}

#[test]
fn a_one_field_change_does_not_reproduce_the_published_hash() {
    // The control for the first test: if a field were missing from the encoding, the
    // hash would stop matching, and this proves the match is sensitive to the fields
    // rather than to the fixture being self-consistent.
    for case in cases() {
        let published = case.provider_hash();
        let mut moved = case.unsigned.clone();
        moved.nonce += 1;
        let signed = SignedTransaction::new(moved, case.signature);
        assert_ne!(
            signed.hash(),
            published,
            "{}: incrementing the nonce left the hash alone",
            case.name
        );
    }
}

#[test]
fn the_fixture_describes_one_chain_and_the_cases_this_test_names() {
    // A fixture is evidence about a specific network. If the provenance block and the
    // transactions stop agreeing, the file was edited rather than collected.
    let document: Value = serde_json::from_str(FIXTURE).unwrap();
    let provenance_chain = document
        .get("provenance")
        .and_then(|p| p.get("chain_id"))
        .and_then(Value::as_u64)
        .expect("the provenance names the chain id it collected from");
    let all = cases();
    assert_eq!(all.len(), CASES.len());
    for case in &all {
        assert_eq!(
            case.provider_chain_id(),
            provenance_chain,
            "{} was collected from a different chain than the provenance claims",
            case.name
        );
    }
    // Both envelopes the chain runs, and one creation: the mix is what §14's "not only
    // to and data" is about.
    let types: Vec<TransactionType> = all.iter().map(|c| c.unsigned.tx_type).collect();
    assert!(types.contains(&TransactionType::Legacy));
    assert!(types.contains(&TransactionType::DynamicFee));
    assert!(all.iter().any(|c| c.unsigned.to.is_none()));
    assert!(all.iter().any(|c| !c.unsigned.value.is_zero()));
    assert!(all
        .iter()
        .any(|c| c.unsigned.input.len() > 4 && c.name != "legacy_create"));
}
