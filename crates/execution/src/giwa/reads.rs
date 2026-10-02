//! §20/§26/§36: the three chain reads M6's adapter was never asked to make.
//!
//! M6's endpoint type answers the price, the nonce, the canonical chain and the send, and
//! that is the whole of what a single validation transaction needs. A sequence needs three
//! more questions answered by the node rather than by a caller:
//!
//! * **`balanceOf` at a named block** (§20). The asset audit compares the wallet before and
//!   after the sequence, and half of that comparison is ERC-20 — WETH above all, since
//!   §16's route starts and ends in it. [`crate::fee::FeeSource::balance`] covers native
//!   only, so without this read the token side of §20 could only come from a scripted
//!   answer.
//! * **`getReserves()` and the pair's own tokens at a named block** (§26's reserve line).
//!   §27's verdict is a comparison between the reserves the route was priced against and the
//!   reserves the head holds now, so the live half has to be a read, and the mapping from
//!   `(reserve0, reserve1)` to the route's `(in, out)` has to come from `token0()`/`token1()`
//!   rather than from a guess about which side is which.
//! * **`getL1Fee(bytes)` from the fee predeploy** (§10/§36). The preflight gate prices the
//!   whole sequence before anything is signed, and §35 forbids an L2-only ceiling presented
//!   as the bill; the only L1 number that can exist before a receipt is the oracle's.
//!
//! Nothing here invents a number when a read fails. The two reads that feed a comparison
//! return [`crate::error::ExecutionError::ChainRead`] — a missing reserve read is not a
//! zero reserve — and the one that feeds a cost line returns [`L1FeeSource::Unreadable`],
//! because that type already exists to carry an absence without letting it become a bill.
//!
//! The block is always named by height. `HttpChainAdapter`'s own block parameter is the
//! only place a tag is chosen, and a height is what §44's "no `latest`" rule asks for here.

use alloy_primitives::{address, keccak256, Address, Bytes, U256};
use async_trait::async_trait;
use serde_json::json;

use evm_chain::{CallRequest, ChainAdapter, HttpChainAdapter};
use evm_core::BlockNumber;
use evm_protocol::{signatures::selector_of, CallReturn, V2Call};

use crate::cost::L1FeeSource;
use crate::error::{ExecutionError, Result};
use crate::sequence::{AssetReader, AssetReading};
use crate::tx::{Signature, SignedTransaction, UnsignedTransaction};

/// The OP-Stack fee predeploy this chain charges L1 data costs through.
///
/// Its address is a predeploy constant, not an endpoint: §44 forbids baking in a host or a
/// URL, and this is neither — it is part of what the chain is, the same way WETH's address
/// is carried in the fixtures.
pub const GAS_PRICE_ORACLE: Address = address!("0x420000000000000000000000000000000000000f");

/// The ERC-20 half of §20's snapshot, read from the node.
///
/// `Clone` shares [`HttpChainAdapter`]'s connection pool, so a run that needs this reader
/// and the submission adapter does not open two sockets to one endpoint (§62).
#[derive(Clone)]
pub struct GiwaAssetReader {
    http: HttpChainAdapter,
}

impl GiwaAssetReader {
    pub fn new(http: &HttpChainAdapter) -> Self {
        Self { http: http.clone() }
    }

    pub fn url(&self) -> &str {
        self.http.url()
    }

    /// `balanceOf(account)` called against `token` at one pinned height.
    pub async fn token_balance(
        &self,
        token: Address,
        account: Address,
        at: BlockNumber,
    ) -> Result<AssetReading> {
        let call = V2Call::BalanceOf { owner: account };
        let amount = read_amount(&self.http, token, &call, at).await?;
        Ok(AssetReading {
            amount,
            source: format!(
                "eth_call balanceOf({account}) on token {token} at block {}",
                at.0
            ),
        })
    }
}

#[async_trait]
impl AssetReader for GiwaAssetReader {
    async fn token_balance(
        &self,
        token: Address,
        account: Address,
        block_number: u64,
    ) -> Result<AssetReading> {
        Self::token_balance(self, token, account, BlockNumber(block_number)).await
    }
}

/// One V2-shaped pool as the node described it at one height.
///
/// All three of `token0`, `token1` and the reserves travel together because the route's
/// `(in, out)` cannot be named without them: a pool's reserve words are ordered by the
/// pair's own construction, and reading the reserves in the wrong order is the one
/// mistake that would make a stale market look like a profitable one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolState {
    pub pool: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    /// `blockTimestampLast` — the time the pool says these reserves are from.
    pub last_synced: U256,
    pub read_at_block: u64,
    pub source: String,
}

impl PoolState {
    /// The reserves in the direction the route moves: what the pool holds of `input`, then
    /// of `output`.
    pub fn in_out(&self, input: Address, output: Address) -> Result<(U256, U256)> {
        if self.token0 == input && self.token1 == output {
            Ok((self.reserve0, self.reserve1))
        } else if self.token1 == input && self.token0 == output {
            Ok((self.reserve1, self.reserve0))
        } else {
            Err(ExecutionError::ChainRead(format!(
                "pool {} holds {} and {}, which is not the pair this route moves through \
                 ({input} then {output}); the reserves cannot be ordered, and ordering them \
                 by assumption is how a wrong side becomes a phantom profit",
                self.pool, self.token0, self.token1
            )))
        }
    }
}

/// `token0()`, `token1()` and `getReserves()` over one pool at one height.
pub async fn read_pool(
    http: &HttpChainAdapter,
    pool: Address,
    at: BlockNumber,
) -> Result<PoolState> {
    let token0 = read_address(http, pool, &V2Call::Token0, at).await?;
    let token1 = read_address(http, pool, &V2Call::Token1, at).await?;
    let raw = http
        .call(
            at,
            &CallRequest {
                to: pool,
                data: V2Call::GetReserves.encode(),
            },
        )
        .await
        .map_err(|error| {
            ExecutionError::ChainRead(format!("getReserves() at {pool} block {}: {error}", at.0))
        })?;
    let reserves = match V2Call::GetReserves
        .decode_return(&raw)
        .map_err(|error| ExecutionError::ChainRead(format!("getReserves() at {pool}: {error}")))?
    {
        CallReturn::Reserves(reserves) => reserves,
        other => {
            return Err(ExecutionError::ChainRead(format!(
                "getReserves() at {pool} decoded to {other:?}, which is not three reserve words"
            )))
        }
    };
    Ok(PoolState {
        pool,
        token0,
        token1,
        reserve0: reserves.reserve0,
        reserve1: reserves.reserve1,
        last_synced: reserves.block_timestamp_last,
        read_at_block: at.0,
        source: format!(
            "eth_call token0(), token1(), getReserves() at {pool} for block {}",
            at.0
        ),
    })
}

/// `getL1Fee(bytes)` against the fee predeploy, as an [`L1FeeSource`].
///
/// A failure is `Unreadable` rather than an error because the gate has to keep going and
/// report the gap: §35's rule is that a missing L1 number makes the total a lower bound,
/// not that the run stops pretending it read one.
pub async fn estimate_l1_fee(
    http: &HttpChainAdapter,
    payload: &[u8],
    at: BlockNumber,
) -> L1FeeSource {
    let request = CallRequest {
        to: GAS_PRICE_ORACLE,
        data: get_l1_fee_calldata(payload),
    };
    match http.call(at, &request).await {
        Ok(raw) => match evm_protocol::signatures::word(&raw, 0) {
            Ok(amount) => L1FeeSource::OracleEstimate {
                amount,
                block_number: at.0,
                read_by: format!(
                    "eth_call getL1Fee(bytes) on {} at block {} over a {}-byte envelope",
                    GAS_PRICE_ORACLE,
                    at.0,
                    payload.len()
                ),
            },
            Err(error) => L1FeeSource::Unreadable {
                reason: format!(
                    "getL1Fee(bytes) at block {} answered {raw:?}: {error}",
                    at.0
                ),
            },
        },
        Err(error) => L1FeeSource::Unreadable {
            reason: format!(
                "getL1Fee(bytes) on {GAS_PRICE_ORACLE} at block {}: {error}",
                at.0
            ),
        },
    }
}

/// The serialized envelope the oracle is asked to price *before* a signature exists.
///
/// `getL1Fee(bytes)` charges for the payload's *compressibility*, not for its length or for
/// how many of its bytes are zero — measured on this endpoint (`data/evidence/m7/probe-live-reads.json`,
/// 2026-10-02): four 110-byte payloads were priced at one block and the three low-entropy
/// ones (all zero, all `0xff`, alternating `00 ff`) each cost 1600 L1 gas while the two
/// high-entropy ones cost 1754, and the high-entropy figures matched the signature bytes of
/// M6's real mined transaction exactly. So the placeholder signature has to look like a
/// signature — two hash-derived, run-free 32-byte words plus a parity byte — because the
/// obvious "fill it with `0xff`" choice is the *cheapest* shape and would price the coming
/// charge about 9.7% too low.
///
/// A real ECDSA `r` and `s` are the outputs of a hash and are therefore high-entropy, so
/// this shape is the one a real signature pays at; a real signature can still come out
/// cheaper than this (a leading zero byte makes `r` or `s` serialize as one byte shorter),
/// which is why §37's bill remains the receipt's own `l1Fee` field and the estimate is only
/// ever used to ask a question *before* a signature exists.
pub fn pre_signing_envelope(unsigned: &UnsignedTransaction) -> Bytes {
    SignedTransaction::new(unsigned.clone(), placeholder_signature()).raw()
}

/// The placeholder signature itself, so a test can weigh the two words rather than the whole
/// envelope, whose other fields are the transaction's own and may legitimately repeat.
fn placeholder_signature() -> Signature {
    let r = high_entropy_word(b"evm-mev-bot M7 pre-signing L1 estimate, placeholder r");
    let s = high_entropy_word(&r.to_be_bytes::<32>());
    Signature::new(r, s, true)
}

/// A 32-byte high-entropy word, with the top byte forced nonzero so it serializes at full
/// width rather than being stripped to 31 bytes.
fn high_entropy_word(seed: &[u8]) -> U256 {
    let mut bytes = keccak256(seed).0;
    bytes[0] = bytes[0].max(0x01);
    U256::from_be_bytes(bytes)
}

/// The calldata for `getL1Fee(bytes)`: selector, then the standard one-word offset, the
/// length, and the payload padded up to a word boundary.
fn get_l1_fee_calldata(payload: &[u8]) -> Bytes {
    let mut data = Vec::with_capacity(4 + 64 + payload.len() + 32);
    data.extend_from_slice(&selector_of("getL1Fee(bytes)"));
    data.extend_from_slice(&U256::from(32u64).to_be_bytes::<32>());
    data.extend_from_slice(&U256::from(payload.len()).to_be_bytes::<32>());
    data.extend_from_slice(payload);
    let padding = (32 - (payload.len() % 32)) % 32;
    data.resize(data.len() + padding, 0u8);
    Bytes::from(data)
}

/// One `eth_call` whose answer is a single `uint256` word.
async fn read_amount(
    http: &HttpChainAdapter,
    to: Address,
    call: &V2Call,
    at: BlockNumber,
) -> Result<U256> {
    let raw = http
        .call(
            at,
            &CallRequest {
                to,
                data: call.encode(),
            },
        )
        .await
        .map_err(|error| {
            ExecutionError::ChainRead(format!(
                "{} at {to} block {}: {error}",
                call.signature(),
                at.0
            ))
        })?;
    match call.decode_return(&raw).map_err(|error| {
        ExecutionError::ChainRead(format!("{} at {to}: {error}", call.signature()))
    })? {
        CallReturn::Amount(amount) => Ok(amount),
        other => Err(ExecutionError::ChainRead(format!(
            "{} at {to} decoded to {other:?}, not a uint256",
            call.signature()
        ))),
    }
}

/// One `eth_call` whose answer is an address in the low 20 bytes of a word.
async fn read_address(
    http: &HttpChainAdapter,
    to: Address,
    call: &V2Call,
    at: BlockNumber,
) -> Result<Address> {
    let raw = http
        .call(
            at,
            &CallRequest {
                to,
                data: call.encode(),
            },
        )
        .await
        .map_err(|error| {
            ExecutionError::ChainRead(format!(
                "{} at {to} block {}: {error}",
                call.signature(),
                at.0
            ))
        })?;
    let word = evm_protocol::signatures::word(&raw, 0).map_err(|error| {
        ExecutionError::ChainRead(format!("{} at {to}: {error}", call.signature()))
    })?;
    let bytes: [u8; 32] = word.to_be_bytes();
    Ok(Address::from_slice(&bytes[12..]))
}

/// A short JSON shape for the evidence rows a live run writes about one pool.
pub fn pool_state_row(pool: &PoolState) -> serde_json::Value {
    json!({
        "pool": format!("{:?}", pool.pool),
        "token0": format!("{:?}", pool.token0),
        "token1": format!("{:?}", pool.token1),
        "reserve0": pool.reserve0.to_string(),
        "reserve1": pool.reserve1.to_string(),
        "block_timestamp_last": pool.last_synced.to_string(),
        "read_at_block": pool.read_at_block,
        "source": pool.source,
    })
}

#[cfg(test)]
mod tests {
    use alloy_primitives::Bytes as AlloyBytes;

    use super::*;
    use crate::tx::TransactionType;

    fn transfer_tx() -> UnsignedTransaction {
        UnsignedTransaction {
            tx_type: TransactionType::DynamicFee,
            chain_id: 91_342,
            nonce: 7,
            to: Some(Address::from_slice(&[0x6bu8; 20])),
            value: U256::ZERO,
            gas_limit: 150_000,
            input: AlloyBytes::from(vec![0x12u8, 0x34, 0x56, 0x78]),
            access_list: Vec::new(),
            max_priority_fee_per_gas: Some(U256::from(1_000_000u64)),
            max_fee_per_gas: Some(U256::from(1_000_370u64)),
        }
    }

    /// The selector is the signature's own keccak, and the layout is the one the ABI
    /// prescribes; both are asserted rather than assumed, because a `getL1Fee` call with a
    /// wrong offset reads a fee for a payload of a different length.
    #[test]
    fn the_oracle_calldata_is_selector_offset_length_and_padded_payload() {
        let payload = vec![0xaau8; 33];
        let calldata = get_l1_fee_calldata(&payload);
        assert_eq!(calldata.len(), 4 + 32 + 32 + 64);
        assert_eq!(&calldata[..4], &selector_of("getL1Fee(bytes)"));
        assert_eq!(
            U256::from_be_slice(&calldata[4..36]),
            U256::from(32u64),
            "the dynamic bytes argument is pointed at by a one-word offset"
        );
        assert_eq!(
            U256::from_be_slice(&calldata[36..68]),
            U256::from(33u64),
            "and its length is its own word"
        );
        assert_eq!(&calldata[68..101], payload.as_slice());
        assert!(
            calldata[101..].iter().all(|b| *b == 0),
            "33 bytes pad to 64, so the tail is padding and nothing else"
        );
    }

    #[test]
    fn an_empty_payload_still_carries_the_offset_and_the_zero_length() {
        let calldata = get_l1_fee_calldata(&[]);
        assert_eq!(calldata.len(), 4 + 32 + 32);
        assert!(U256::from_be_slice(&calldata[36..68]).is_zero());
    }

    /// The placeholder is the shape a real signature pays at, and the two properties that
    /// make it that shape are asserted rather than assumed: full width (no real `r` or `s`
    /// can serialize longer) and high entropy (the estimator's cheap shapes — repeated bytes,
    /// runs, zeros — are the ones a hash output does not have).
    #[test]
    fn the_placeholder_is_as_wide_as_a_real_signature_and_no_easier_to_compress() {
        let envelope = pre_signing_envelope(&transfer_tx());
        assert_eq!(
            envelope.as_ref(),
            pre_signing_envelope(&transfer_tx()).as_ref(),
            "the placeholder is derived from constants, so two preflight runs price the same \
             payload and their estimates are comparable"
        );
        let widest =
            SignedTransaction::new(transfer_tx(), Signature::new(U256::MAX, U256::MAX, true));
        assert_eq!(
            envelope.len(),
            widest.raw().len(),
            "both placeholder words serialize at 32 bytes, and the parity byte is present — \
             no real signature can make this payload wider"
        );
        assert!(envelope.len() > 100, "the envelope is a real payload");

        // The all-`0xff` filler this function used to carry repeated one value across all
        // sixty-four signature bytes and was priced 9.7% below a mined signature
        // (`data/evidence/m7/probe-live-reads.json`, §byte_cost_model_at_the_same_block). A
        // hash output does not repeat like that, and the two words are weighed on their own
        // because the rest of the envelope is the transaction's own fields, where an address
        // legitimately repeats twenty times.
        let signature = placeholder_signature();
        let mut worst = (0usize, 0usize, 0usize);
        for (which, word) in [("r", signature.r), ("s", signature.s)] {
            let bytes = word.to_be_bytes::<32>();
            assert_ne!(bytes[0], 0, "{which} serializes at full width");
            let mut counts = [0usize; 256];
            for byte in bytes.iter() {
                counts[*byte as usize] += 1;
            }
            let (value, count) = counts
                .iter()
                .enumerate()
                .max_by_key(|(_, count)| **count)
                .unwrap_or((0, &0));
            let runs = bytes.windows(2).filter(|pair| pair[0] == pair[1]).count();
            assert!(
                count <= &4 && runs <= 1,
                "{which} repeats the byte value {value} {count} times over {runs} adjacent \
                 equal pairs, which is a compressible shape; the estimate would price below \
                 what a real signature pays"
            );
            if count > &worst.1 {
                worst = (*count, value, runs);
            }
        }
        assert!(
            worst.0 <= 4,
            "the widest repeat in the placeholder is {worst:?}"
        );
    }

    #[test]
    fn a_pool_that_does_not_hold_the_pair_refuses_to_be_ordered() {
        let state = PoolState {
            pool: Address::from_slice(&[0x11u8; 20]),
            token0: Address::from_slice(&[0x22u8; 20]),
            token1: Address::from_slice(&[0x33u8; 20]),
            reserve0: U256::from(10u64),
            reserve1: U256::from(20u64),
            last_synced: U256::from(1u64),
            read_at_block: 1,
            source: "test".to_string(),
        };
        assert_eq!(
            state
                .in_out(
                    Address::from_slice(&[0x22u8; 20]),
                    Address::from_slice(&[0x33u8; 20])
                )
                .expect("the pool holds this pair in this order"),
            (U256::from(10u64), U256::from(20u64))
        );
        assert_eq!(
            state
                .in_out(
                    Address::from_slice(&[0x33u8; 20]),
                    Address::from_slice(&[0x22u8; 20])
                )
                .expect("and in the reverse order too"),
            (U256::from(20u64), U256::from(10u64))
        );
        let wrong = state.in_out(
            Address::from_slice(&[0x44u8; 20]),
            Address::from_slice(&[0x22u8; 20]),
        );
        let detail = wrong
            .expect_err("a pool that does not hold the pair says so")
            .to_string();
        assert!(detail.contains("phantom profit"), "{detail}");
    }

    /// `describe` is what the evidence file shows for one pool, so the two things a reader
    /// checks it against — the amount and the height — must be in it.
    #[test]
    fn the_pool_row_names_the_pool_and_the_height_it_was_read_at() {
        let state = PoolState {
            pool: Address::from_slice(&[0x11u8; 20]),
            token0: Address::ZERO,
            token1: Address::ZERO,
            reserve0: U256::from(1u64),
            reserve1: U256::from(2u64),
            last_synced: U256::from(3u64),
            read_at_block: 37_530_593,
            source: "eth_call".to_string(),
        };
        let row = pool_state_row(&state);
        assert_eq!(row["read_at_block"], json!(37_530_593u64));
        assert_eq!(row["reserve0"], json!("1"));
        assert_eq!(row["source"], json!("eth_call"));
    }
}
