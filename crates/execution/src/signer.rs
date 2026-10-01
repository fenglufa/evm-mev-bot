//! §16: the signer's whole job is `unsigned transaction → signature → signed raw
//! transaction`. It does not look at an opportunity, does not ask the chain for a
//! nonce or a fee, and does not own a network client — which is testable rather than
//! merely stated, because nothing in this module can reach a socket.
//!
//! The private key enters through exactly one door: [`ExecutionKey::from_secret_bytes`],
//! called by [`Signer::from_env`] with a value read from the environment when — and
//! only when — the mode allows it (§19). The type that holds it redacts itself in
//! `Debug` and `Display`, so the key cannot escape through a `{:?}` in a log line, an
//! `unwrap()` panic message, or a serialized record (§17, §37).

use std::fmt;

use alloy_primitives::{Address, ChainId, B256, U256};
use k256::ecdsa::signature::hazmat::PrehashVerifier;
use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, SigningKey, VerifyingKey};

use crate::error::{ExecutionError, Result};
use crate::mode::ExecutionMode;
use crate::tx::{SignedTransaction, TransactionType, UnsignedTransaction};

/// §17's single environment door. The name is the only place a secret is addressed,
/// and its value is never a literal in this repository.
pub const PRIVATE_KEY_ENV: &str = "GIWA_EXECUTION_PRIVATE_KEY";

/// A signing key, plus the address it proves — so the address is derived rather than
/// configured, and a mismatch between "the wallet we think we are" and "the wallet the
/// signature proves" cannot be written down by hand.
pub struct ExecutionKey {
    signing: SigningKey,
    address: Address,
}

impl ExecutionKey {
    /// From exactly 32 bytes. A scalar of zero, or one outside the curve order, is
    /// refused by the primitive and reported without echoing anything back.
    pub fn from_secret_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 32 {
            return Err(ExecutionError::SigningFailed(format!(
                "a secp256k1 secret is 32 bytes, this input is {}",
                bytes.len()
            )));
        }
        let signing = SigningKey::from_slice(bytes)
            .map_err(|e| ExecutionError::SigningFailed(format!("the key was rejected: {e}")))?;
        let address = address_of(&signing);
        Ok(Self { signing, address })
    }

    /// From the environment's own shape: `0x` + 64 hex digits, either case, surrounding
    /// whitespace ignored. The parsed bytes are checked for length before the hex is
    /// decoded, so a truncated value is a length error and not a partial key.
    pub fn from_hex(text: &str) -> Result<Self> {
        let trimmed = text.trim();
        let body = trimmed.strip_prefix("0x").unwrap_or(trimmed);
        if body.len() != 64 {
            return Err(ExecutionError::SigningFailed(format!(
                "expected 64 hex digits, got {}",
                body.len()
            )));
        }
        let bytes = hex::decode(body)
            .map_err(|_| ExecutionError::SigningFailed("the key is not hex".to_string()))?;
        Self::from_secret_bytes(&bytes)
    }

    /// The address this key signs as: the last 20 bytes of keccak256 over the
    /// uncompressed public point without its `0x04` prefix — the Ethereum convention,
    /// written out because it is the one step where an off-by-one silently produces a
    /// wallet that exists and is not ours.
    pub fn address(&self) -> Address {
        self.address
    }

    /// Sign a 32-byte prehash and return the signature with its recovery id normalized
    /// to a parity bit.
    pub fn sign(&self, prehash: &B256) -> Result<crate::tx::Signature> {
        use k256::ecdsa::signature::hazmat::PrehashSigner;
        let (signature, recovery): (EcdsaSignature, RecoveryId) = self
            .signing
            .sign_prehash(prehash.as_ref())
            .map_err(|e| ExecutionError::SigningFailed(e.to_string()))?;
        let y_parity = parity(recovery)?;
        // `to_bytes` is the 64-byte big-endian `r || s` form; splitting it is the same
        // read the network does, so a `r()`/`s()` accessor that changes shape between
        // k256 versions cannot silently change what we sign.
        let encoded = signature.to_bytes();
        Ok(crate::tx::Signature {
            r: U256::from_be_slice(&encoded[..32]),
            s: U256::from_be_slice(&encoded[32..]),
            y_parity,
        })
    }

    /// Self-check used at construction time in tests: this key's verification key
    /// accepts a signature it just made over a known prehash. Cheap, and it is the only
    /// place a "signing works" claim is made by doing it rather than by hoping.
    pub fn verify_prehash(&self, prehash: &B256, signature: &crate::tx::Signature) -> Result<()> {
        let encoded = encode_signature(signature)?;
        // A signature whose `s` is in the upper half of the curve order has a second,
        // equally valid recovery id, so it is not the form Ethereum accepts. `normalize_s`
        // answers `Some` only when it had to change something, which is exactly the
        // case this self-check refuses rather than silently repairing.
        if encoded.normalize_s().is_some() {
            return Err(ExecutionError::SigningFailed(
                "signature is not canonical: s is above half the curve order".to_string(),
            ));
        }
        self.signing
            .verifying_key()
            .verify_prehash(prehash.as_ref(), &encoded)
            .map_err(|e| ExecutionError::SigningFailed(format!("self-verification failed: {e}")))
    }
}

impl fmt::Debug for ExecutionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Deliberately not `self.signing`: k256's `Debug` for a signing key is redacted
        // already, but the type that would carry it into a log line is ours, and the
        // redaction should not depend on a dependency's formatting choice.
        f.debug_struct("ExecutionKey")
            .field("address", &self.address)
            .field("secret", &"[redacted]")
            .finish()
    }
}

impl fmt::Display for ExecutionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "execution key {}", self.address)
    }
}

fn address_of(signing: &SigningKey) -> Address {
    let point = signing.verifying_key().to_encoded_point(false);
    // `point` is `0x04 || X || Y`; the address digests the 64 coordinate bytes only.
    let body = &point.as_bytes()[1..];
    let hash = alloy_primitives::keccak256(body);
    Address::from_slice(&hash[12..])
}

fn parity(recovery: RecoveryId) -> Result<bool> {
    let byte = recovery.to_byte();
    match byte {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(ExecutionError::SigningFailed(format!(
            "recovery id {other} is not an Ethereum yParity (0 or 1)"
        ))),
    }
}

fn encode_signature(signature: &crate::tx::Signature) -> Result<EcdsaSignature> {
    let r = signature.r.to_be_bytes::<32>();
    let s = signature.s.to_be_bytes::<32>();
    let mut bytes = [0u8; 64];
    bytes[..32].copy_from_slice(&r);
    bytes[32..].copy_from_slice(&s);
    EcdsaSignature::from_slice(&bytes)
        .map_err(|e| ExecutionError::SigningFailed(format!("signature bytes are malformed: {e}")))
}

/// Recover the address that made `signature` over `prehash`. §18's test and §14's
/// round trip both run through here: the sender of a transaction is something this
/// code proves from the payload, not something the caller asserts.
pub fn recover_sender(prehash: &B256, signature: &crate::tx::Signature) -> Result<Address> {
    let encoded = encode_signature(signature)?;
    let recovery = RecoveryId::try_from(u8::from(signature.y_parity))
        .map_err(|_| ExecutionError::SigningFailed("bad recovery id".to_string()))?;
    let verifying = VerifyingKey::recover_from_prehash(prehash.as_ref(), &encoded, recovery)
        .map_err(|e| ExecutionError::SigningFailed(format!("sender recovery failed: {e}")))?;
    let point = verifying.to_encoded_point(false);
    let body = &point.as_bytes()[1..];
    let hash = alloy_primitives::keccak256(body);
    Ok(Address::from_slice(&hash[12..]))
}

/// The local, isolated signer.
pub struct Signer {
    mode: ExecutionMode,
    key: Option<ExecutionKey>,
    /// The environment variable it would read, recorded so a report can say where the
    /// key came from without saying what it is.
    source: String,
}

impl Signer {
    /// Read the key from the environment if — and only if — the mode may sign (§19).
    ///
    /// `BuildOnly` returns a signer with no key *even when the variable is set*: the
    /// presence of a secret is not authorization to use it, and a run that wanted
    /// BuildOnly must not become a run that holds a key in memory because someone
    /// exported it for another command.
    pub fn from_env(mode: ExecutionMode) -> Result<Self> {
        if !mode.may_read_key() {
            return Ok(Self {
                mode,
                key: None,
                source: format!("no key read: mode is {mode}"),
            });
        }
        let text = std::env::var(PRIVATE_KEY_ENV).map_err(|_| {
            ExecutionError::SigningFailed(format!(
                "mode {mode} needs a signing key and `{PRIVATE_KEY_ENV}` is not set"
            ))
        })?;
        let key = ExecutionKey::from_hex(&text)?;
        // Drop the string: it was the secret in hex form.
        drop(text);
        Ok(Self {
            mode,
            key: Some(key),
            source: format!("read from `{PRIVATE_KEY_ENV}` for mode {mode}"),
        })
    }

    /// A signer built from bytes already in hand, for tests.
    pub fn from_key(mode: ExecutionMode, key: ExecutionKey) -> Self {
        Self {
            mode,
            key: Some(key),
            source: format!("supplied key for mode {mode}"),
        }
    }

    /// A signer that cannot sign, for `BuildOnly` runs.
    pub fn without_key(mode: ExecutionMode) -> Self {
        Self {
            mode,
            key: None,
            source: format!("no key: mode {mode}"),
        }
    }

    pub fn mode(&self) -> ExecutionMode {
        self.mode
    }

    pub fn key_present(&self) -> bool {
        self.key.is_some()
    }

    /// Where the key came from, in words that name the mechanism and not the value.
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn address(&self) -> Result<Address> {
        self.key
            .as_ref()
            .map(ExecutionKey::address)
            .ok_or_else(|| ExecutionError::SigningFailed(self.source.clone()))
    }

    /// §16's one transformation. Also the check that the signature is *ours*: the
    /// recovered sender is computed here and returned inside the signed transaction's
    /// caller-facing view, so a transaction that claims one sender and proves another
    /// cannot leave this function.
    pub fn sign(&self, unsigned: &UnsignedTransaction) -> Result<SignedTransaction> {
        if !self.mode.may_read_key() {
            return Err(ExecutionError::ModeGate(format!(
                "signing was asked of mode {}; only sign-only and submit may sign",
                self.mode.name()
            )));
        }
        let key = self
            .key
            .as_ref()
            .ok_or_else(|| ExecutionError::SigningFailed(self.source.clone()))?;
        let prehash = unsigned.signing_hash()?;
        let signature = key.sign(&prehash)?;
        key.verify_prehash(&prehash, &signature)?;
        let signed = SignedTransaction::new(unsigned.clone(), signature);
        // The hash of a signed transaction is only meaningful if the signature inside
        // it proves the sender that the intent claims. Checked here, once, so no
        // caller has to remember it.
        let recovered = recover_sender(&prehash, &signature)?;
        if recovered != key.address() {
            return Err(ExecutionError::SigningFailed(format!(
                "the signature over this payload recovers {recovered}, not the configured \
                 sender {}",
                key.address()
            )));
        }
        Ok(signed)
    }

    /// Sign and also report the sender the signature proves. The point of the extra
    /// return value is that `sign` alone cannot be used to produce a transaction whose
    /// sender was never checked.
    pub fn sign_and_recover(
        &self,
        unsigned: &UnsignedTransaction,
    ) -> Result<(SignedTransaction, Address)> {
        let signed = self.sign(unsigned)?;
        let prehash = unsigned.signing_hash()?;
        let sender = recover_sender(&prehash, &signed.signature)?;
        Ok((signed, sender))
    }
}

impl fmt::Debug for Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Signer")
            .field("mode", &self.mode)
            .field("key_present", &self.key.is_some())
            .field("source", &self.source)
            .finish()
    }
}

/// §18's chain-id-in-the-domain check, spelled out for a test: the same payload signed
/// against two chain ids must recover two different senders. If it did not, a signature
/// would be replayable across chains.
pub fn signing_hash_for_chain(unsigned: &UnsignedTransaction, chain_id: ChainId) -> Result<B256> {
    let mut rebound = unsigned.clone();
    rebound.chain_id = chain_id;
    rebound.signing_hash()
}

/// Whether the payload's fee fields match its envelope, exposed for the builder's
/// §10 list without re-importing `tx`.
pub fn matches_envelope(unsigned: &UnsignedTransaction) -> bool {
    match unsigned.tx_type {
        TransactionType::DynamicFee => {
            unsigned.max_fee_per_gas.is_some() && unsigned.max_priority_fee_per_gas.is_some()
        }
        TransactionType::Legacy => {
            unsigned.max_fee_per_gas.is_some() && unsigned.max_priority_fee_per_gas.is_none()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unsigned(tx_type: TransactionType) -> UnsignedTransaction {
        UnsignedTransaction {
            tx_type,
            chain_id: 1,
            nonce: 0,
            to: Some(Address::from_slice(&[0x0au8; 20])),
            value: U256::ZERO,
            gas_limit: 21_000,
            input: Default::default(),
            access_list: Vec::new(),
            max_priority_fee_per_gas: if tx_type == TransactionType::DynamicFee {
                Some(U256::from(1u64))
            } else {
                None
            },
            max_fee_per_gas: Some(U256::from(2u64)),
        }
    }

    #[test]
    fn a_key_that_is_not_thirty_two_bytes_is_a_length_error_and_not_a_partial_key() {
        assert!(matches!(
            ExecutionKey::from_secret_bytes(&[7u8; 31]),
            Err(ExecutionError::SigningFailed(_))
        ));
        assert!(ExecutionKey::from_hex("0xdead").is_err());
    }

    #[test]
    fn the_debug_line_of_a_key_never_contains_the_bytes_it_was_made_from() {
        let secret = [0x11u8; 32];
        let key = ExecutionKey::from_secret_bytes(&secret).expect("a valid test key");
        let rendered = format!("{key:?}");
        assert!(rendered.contains("[redacted]"), "{rendered}");
        assert!(
            !rendered.to_lowercase().contains("1111"),
            "the Debug line carries key material: {rendered}"
        );
        let signer = Signer::from_key(ExecutionMode::SignOnly, key);
        let rendered = format!("{signer:?}");
        assert!(rendered.contains("key_present: true"), "{rendered}");
        assert!(
            !rendered.to_lowercase().contains("1111"),
            "the Signer Debug line carries key material: {rendered}"
        );
    }

    #[test]
    fn build_only_never_reads_the_environment_even_when_the_variable_is_present() {
        // §19, in code: the mode decides before the environment is touched.
        let before = std::env::var(PRIVATE_KEY_ENV).ok();
        std::env::set_var(PRIVATE_KEY_ENV, format!("0x{}", hex::encode([0x22u8; 32])));
        let signer = Signer::from_env(ExecutionMode::BuildOnly).expect("build-only needs no key");
        assert!(!signer.key_present());
        assert!(signer.sign(&unsigned(TransactionType::DynamicFee)).is_err());
        match before {
            Some(previous) => std::env::set_var(PRIVATE_KEY_ENV, previous),
            None => std::env::remove_var(PRIVATE_KEY_ENV),
        }
    }
}
