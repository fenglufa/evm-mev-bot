//! The RLP subset a transaction needs, written here rather than pulled in, so that
//! "the bytes we sign and the bytes the node decodes are the same bytes" is a property
//! of 200 lines in this repository and a test against real chain data
//! (`tests/real_transaction_codec.rs`) rather than a property of a dependency version.
//!
//! Scope is deliberate: two items exist in RLP — a byte string and a list of items —
//! and a transaction uses nothing else. Integers enter as *minimal big-endian byte
//! strings* (`encode_quantity`), which is the one rule where a plausible
//! implementation (left-padding to 32 bytes) produces bytes a real node rejects, and
//! therefore the rule the tests pin.

use alloy_primitives::U256;

use crate::error::{ExecutionError, Result};

/// A decoded RLP item: a byte string, or a list of items.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Bytes(Vec<u8>),
    List(Vec<Item>),
}

impl Item {
    /// The bytes of a string item, or a decode error naming what was expected.
    pub fn as_bytes(&self) -> Result<&[u8]> {
        match self {
            Self::Bytes(b) => Ok(b),
            Self::List(_) => Err(ExecutionError::InvalidIntent(
                "rlp item is a list".to_string(),
            )),
        }
    }

    /// The elements of a list item, or a decode error.
    pub fn as_list(&self) -> Result<&[Item]> {
        match self {
            Self::List(items) => Ok(items),
            Self::Bytes(_) => Err(ExecutionError::InvalidIntent(
                "rlp item is a string".to_string(),
            )),
        }
    }

    /// A quantity: big-endian, no leading zeros, empty meaning zero. A payload that
    /// is padded, or longer than 32 bytes, is refused rather than silently narrowed —
    /// a fee field that does not fit `U256` is a corrupt input, not a rounding
    /// opportunity.
    pub fn as_quantity(&self) -> Result<U256> {
        let bytes = self.as_bytes()?;
        if bytes.len() > 32 {
            return Err(ExecutionError::InvalidIntent(format!(
                "rlp quantity is {} bytes, more than 32",
                bytes.len()
            )));
        }
        if bytes.len() > 1 && bytes[0] == 0 {
            return Err(ExecutionError::InvalidIntent(
                "rlp quantity is not minimally encoded (leading zero)".to_string(),
            ));
        }
        Ok(U256::from_be_slice(bytes))
    }

    /// A quantity that has to fit a `u64` (nonce, gas limit) or a chain id.
    pub fn as_u64(&self) -> Result<u64> {
        let value = self.as_quantity()?;
        u64::try_from(value).map_err(|_| {
            ExecutionError::InvalidIntent(format!("rlp quantity {value} does not fit u64"))
        })
    }

    /// A 20- or 32-byte fixed item; the length is checked, not assumed.
    pub fn as_fixed<const N: usize>(&self) -> Result<[u8; N]> {
        let bytes = self.as_bytes()?;
        let slice: [u8; N] = bytes.try_into().map_err(|_| {
            ExecutionError::InvalidIntent(format!(
                "rlp item is {} bytes, expected exactly {N}",
                bytes.len()
            ))
        })?;
        Ok(slice)
    }
}

/// An RLP encoder. `list` is built by writing the payload first and prefixing the
/// header afterwards, which is what keeps nested encoding from needing a second pass.
#[derive(Default)]
pub struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// A byte string: single byte below 0x80 is its own encoding; 0..55 bytes get a
    /// `0x80+len` prefix; longer gets `0xb7+lenlen` followed by big-endian length.
    pub fn bytes(&mut self, payload: &[u8]) {
        if payload.len() == 1 && payload[0] < 0x80 {
            self.bytes.push(payload[0]);
            return;
        }
        self.header(payload.len(), 0x80, 0xb7);
        self.bytes.extend_from_slice(payload);
    }

    /// A quantity in minimal big-endian form, which is how every integer field of a
    /// transaction is encoded (`0` becomes an empty string, `0x80`).
    pub fn quantity(&mut self, value: U256) {
        self.bytes(&trim_be(value.to_be_bytes::<32>()));
    }

    pub fn u64(&mut self, value: u64) {
        self.quantity(U256::from(value));
    }

    /// Wrap an already-encoded payload as a list.
    pub fn list(&mut self, payload: &[u8]) {
        if payload.is_empty() {
            self.bytes.push(0xc0);
            return;
        }
        self.header(payload.len(), 0xc0, 0xf7);
        self.bytes.extend_from_slice(payload);
    }

    fn header(&mut self, len: usize, short_base: u8, long_base: u8) {
        if len < 56 {
            self.bytes.push(short_base + len as u8);
            return;
        }
        let be = trim_be((len as u64).to_be_bytes());
        self.bytes.push(long_base + be.len() as u8);
        self.bytes.extend_from_slice(&be);
    }

    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// The minimal big-endian form of a fixed-width value: leading zeros dropped, and an
/// all-zero value encoded as the empty string rather than `0x00`.
fn trim_be<const N: usize>(be: [u8; N]) -> Vec<u8> {
    let mut first = 0usize;
    while first < N && be[first] == 0 {
        first += 1;
    }
    be[first..].to_vec()
}

/// Decode one complete RLP item. `bytes` must contain exactly one item; trailing
/// bytes are an error, because a transaction envelope that carries extra bytes is
/// either truncated or padded and both are worth naming.
pub fn decode(bytes: &[u8]) -> Result<Item> {
    let (item, consumed) = decode_at(bytes, 0)?;
    if consumed != bytes.len() {
        return Err(ExecutionError::InvalidIntent(format!(
            "rlp item ends at byte {consumed} of {} ({})",
            bytes.len(),
            if consumed < bytes.len() {
                "trailing bytes"
            } else {
                "payload truncated"
            }
        )));
    }
    Ok(item)
}

fn decode_at(bytes: &[u8], at: usize) -> Result<(Item, usize)> {
    let prefix = *bytes
        .get(at)
        .ok_or_else(|| ExecutionError::InvalidIntent("rlp ends at a header".to_string()))?;
    match prefix {
        0x00..=0x7f => Ok((Item::Bytes(vec![prefix]), at + 1)),
        0x80..=0xb7 => {
            let len = (prefix - 0x80) as usize;
            let start = at + 1;
            let end = start + len;
            let payload = bytes.get(start..end).ok_or_else(|| {
                ExecutionError::InvalidIntent("rlp string overruns input".to_string())
            })?;
            if len == 1 && payload[0] < 0x80 {
                return Err(ExecutionError::InvalidIntent(
                    "rlp single byte must not carry a length prefix".to_string(),
                ));
            }
            Ok((Item::Bytes(payload.to_vec()), end))
        }
        0xb8..=0xbf => {
            let lenlen = (prefix - 0xb7) as usize;
            let len = read_len(bytes, at + 1, lenlen, "string")?;
            let start = at + 1 + lenlen;
            let end = start + len;
            let payload = bytes.get(start..end).ok_or_else(|| {
                ExecutionError::InvalidIntent("rlp long string overruns input".to_string())
            })?;
            if len < 56 {
                return Err(ExecutionError::InvalidIntent(
                    "rlp long string header is not canonical".to_string(),
                ));
            }
            Ok((Item::Bytes(payload.to_vec()), end))
        }
        0xc0..=0xf7 => {
            let len = (prefix - 0xc0) as usize;
            let mut out = Vec::new();
            let mut cursor = at + 1;
            let end = cursor + len;
            if end > bytes.len() {
                return Err(ExecutionError::InvalidIntent(
                    "rlp short list overruns input".to_string(),
                ));
            }
            while cursor < end {
                let (item, next) = decode_at(bytes, cursor)?;
                out.push(item);
                cursor = next;
            }
            Ok((Item::List(out), end))
        }
        _ => {
            let lenlen = (prefix - 0xf7) as usize;
            let len = read_len(bytes, at + 1, lenlen, "list")?;
            let start = at + 1 + lenlen;
            let end = start + len;
            if len < 56 {
                return Err(ExecutionError::InvalidIntent(
                    "rlp long list header is not canonical".to_string(),
                ));
            }
            let mut out = Vec::new();
            let mut cursor = start;
            if end > bytes.len() {
                return Err(ExecutionError::InvalidIntent(
                    "rlp long list overruns input".to_string(),
                ));
            }
            while cursor < end {
                let (item, next) = decode_at(bytes, cursor)?;
                out.push(item);
                cursor = next;
            }
            Ok((Item::List(out), end))
        }
    }
}

fn read_len(bytes: &[u8], at: usize, lenlen: usize, what: &str) -> Result<usize> {
    if lenlen > 8 {
        // Any length this implementation can hold fits in 8 bytes; a wider header is a
        // refusal to allocate, not a number to parse.
        return Err(ExecutionError::InvalidIntent(format!(
            "rlp {what} length header is {lenlen} bytes, wider than any real payload"
        )));
    }
    let field = bytes.get(at..at + lenlen).ok_or_else(|| {
        ExecutionError::InvalidIntent(format!("rlp {what} length header overruns input"))
    })?;
    if field.first().is_some_and(|b| *b == 0) {
        return Err(ExecutionError::InvalidIntent(format!(
            "rlp {what} length is not minimally encoded"
        )));
    }
    let mut len = 0usize;
    for byte in field {
        len = len
            .checked_mul(256)
            .and_then(|l| l.checked_add(*byte as usize))
            .ok_or_else(|| {
                ExecutionError::InvalidIntent(format!("rlp {what} length overflows usize"))
            })?;
    }
    Ok(len)
}
