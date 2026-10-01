//! The two §32 legs that are answered by asking the chain what it currently holds.
//!
//! [`crate::fee::FeeSource`] covers the price and the balance, [`crate::nonce::NonceSource`]
//! covers the nonce, and [`crate::submitter::TransactionSubmitter`] covers the send and the
//! receipt. What was missing is the *canonical* question — "is the block this intent pins
//! still the block this chain names at that height, and which chain is this node on" — and
//! without a trait for it the answer could only come from one concrete adapter, which
//! would leave §32's `block_binding_valid` leg untestable against a scripted endpoint.
//!
//! Both methods are reads of the endpoint's present state, so both are fallible and both
//! name where the number came from: a reorg check that never ran must arrive at the gate as
//! [`gate::BlockBinding::Unverified`], not as a pass.

use alloy_primitives::B256;
use async_trait::async_trait;

use evm_core::BlockNumber;

use crate::error::Result;
use crate::gate::BlockBinding;

/// What the endpoint says the canonical chain is, right now.
#[async_trait]
pub trait ChainReader {
    /// The hash the endpoint holds at `number`, or `None` when it has no such block.
    async fn block_hash_at(&self, number: BlockNumber) -> Result<Option<B256>>;

    /// The third chain-id claim §6 compares: not the one configured, not the one the
    /// intent carries, the one this node answers for.
    async fn endpoint_chain_id(&self) -> Result<u64>;
}

/// §32's `block_binding_valid` leg, derived from one read rather than remembered.
///
/// A failed read is `Unverified` with the endpoint's own words, because "the node did not
/// answer" is a different fact from "the block is gone" and the gate has to be able to tell
/// the reader which one stopped the send.
pub async fn read_binding(
    reader: &dyn ChainReader,
    number: BlockNumber,
    pinned: B256,
) -> BlockBinding {
    match reader.block_hash_at(number).await {
        Ok(Some(hash)) if hash == pinned => BlockBinding::Confirmed {
            number: number.0,
            hash,
        },
        Ok(Some(hash)) => BlockBinding::Reorged {
            number: number.0,
            pinned,
            found: hash,
        },
        Ok(None) => {
            BlockBinding::Unverified(format!("the endpoint holds no block {} at all", number.0))
        }
        Err(error) => BlockBinding::Unverified(error.to_string()),
    }
}
