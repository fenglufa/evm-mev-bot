//! §11: the nonce policy, which for M6 is deliberately small.
//!
//! Two views are read and kept separate, because they answer different questions:
//! the confirmed count is what the chain has executed, the pending count is what the
//! mempool has accepted, and "the next nonce I may use" is the second one. A policy
//! that used `latest` alone would hand the same number to two attempts, and a policy
//! that assumed `pending` had advanced after a timeout would re-use a nonce that is
//! already in flight. Both are §11's named failure.
//!
//! The allocator is a **single lane**: one sender, one allocator, one outstanding
//! transaction (§11's phase-one instruction). Parallel nonces are M8, and the reason
//! they are not here is that a lane which can hold two transactions also has to decide
//! what happens to the second when the first times out — which is a question M6 cannot
//! answer with evidence yet.

use alloy_primitives::Address;
use async_trait::async_trait;

use crate::error::{ExecutionError, Result};

/// Both nonce views, read together from one endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NonceReading {
    pub address: Address,
    /// `eth_getTransactionCount(addr, <block>)`: what the chain has executed at that block.
    pub confirmed: u64,
    /// `eth_getTransactionCount(addr, "pending")`: the count including transactions the
    /// pool has accepted.
    pub pending: u64,
    /// The block the confirmed view was read at, so a stale reading can be identified.
    pub at_block: u64,
    /// How the values were obtained, in words.
    pub source: String,
}

impl NonceReading {
    /// The nonce a new transaction would use: the pending view. `confirmed` is kept
    /// beside it because the difference between them is the number of transactions this
    /// account already has in flight — which is the fact §11 needs before allowing
    /// another.
    pub fn next(&self) -> u64 {
        self.pending
    }

    /// How many of this account's transactions are in flight, as the chain sees it. A
    /// negative-looking pair (pending below confirmed) is a contradictory read and is
    /// reported rather than normalized.
    pub fn in_flight(&self) -> Result<u64> {
        if self.pending < self.confirmed {
            return Err(ExecutionError::NonceUnavailable(format!(
                "the pending view ({}) is below the confirmed view ({}) at block {}: the two \
                 reads did not come from one moment",
                self.pending, self.confirmed, self.at_block
            )));
        }
        Ok(self.pending - self.confirmed)
    }
}

/// The endpoint's nonce surface.
#[async_trait]
pub trait NonceSource {
    /// Read both views. An implementation that reads them in two calls has to say so in
    /// `source`; a value that spans two blocks is not a nonce policy, it is a guess.
    async fn nonce(&self, address: Address) -> Result<NonceReading>;
}

/// One sender, one outstanding transaction (§11).
#[derive(Clone, Debug, Default)]
pub struct NonceAllocator {
    outstanding: Option<(Address, u64)>,
}

impl NonceAllocator {
    pub fn new() -> Self {
        Self::default()
    }

    /// The nonce this lane would use, if it is free.
    pub fn is_free(&self) -> bool {
        self.outstanding.is_none()
    }

    pub fn outstanding(&self) -> Option<(Address, u64)> {
        self.outstanding
    }

    /// Take the lane for `reading.next()`.
    ///
    /// Refused when the lane already holds a nonce — which is §11's "two execution
    /// attempts, same nonce" made unrepresentable — and when the account already has
    /// something in flight, because a lane that cannot tell whether that in-flight
    /// transaction is its own has to stop rather than allocate on top of it.
    pub fn allocate(&mut self, reading: &NonceReading) -> Result<u64> {
        if let Some((address, nonce)) = self.outstanding {
            return Err(ExecutionError::NonceUnavailable(format!(
                "the single execution lane is holding nonce {nonce} for {address}; a second \
                 transaction cannot be built until the first has a receipt or is known \
                 rejected"
            )));
        }
        let in_flight = reading.in_flight()?;
        if in_flight > 0 {
            return Err(ExecutionError::NonceUnavailable(format!(
                "{} already has {in_flight} transaction(s) in the pending view that this lane \
                 did not issue; allocating nonce {} anyway could duplicate a live transaction",
                reading.address, reading.pending
            )));
        }
        let nonce = reading.next();
        self.outstanding = Some((reading.address, nonce));
        Ok(nonce)
    }

    /// Release the lane after the transaction it issued is resolved (included,
    /// definitively rejected, or proven never accepted). A release of a nonce the lane
    /// does not hold is an error, because it means two callers think they own one
    /// transaction.
    pub fn release(&mut self, address: Address, nonce: u64) -> Result<()> {
        match self.outstanding {
            Some((held_address, held_nonce)) if held_address == address && held_nonce == nonce => {
                self.outstanding = None;
                Ok(())
            }
            Some((held_address, held_nonce)) => Err(ExecutionError::NonceUnavailable(format!(
                "the lane holds {held_nonce} for {held_address} and was asked to release \
                 {nonce} for {address}"
            ))),
            None => Err(ExecutionError::NonceUnavailable(
                "release was asked of a lane that holds nothing".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(confirmed: u64, pending: u64) -> NonceReading {
        NonceReading {
            address: Address::with_last_byte(7),
            confirmed,
            pending,
            at_block: 100,
            source: "test".to_string(),
        }
    }

    #[test]
    fn the_pending_view_is_the_next_nonce_and_an_idle_account_agrees_with_the_confirmed_view() {
        let lane = NonceAllocator::new();
        assert!(lane.is_free());
        assert_eq!(reading(0, 0).next(), 0);
        assert_eq!(reading(3, 3).in_flight().unwrap(), 0);
    }

    #[test]
    fn one_lane_holds_one_nonce_and_refuses_the_second_attempt() {
        let mut lane = NonceAllocator::new();
        let first = lane.allocate(&reading(0, 0)).unwrap();
        assert_eq!(first, 0);
        let error = lane.allocate(&reading(0, 1)).unwrap_err();
        assert!(
            matches!(error, ExecutionError::NonceUnavailable(_)),
            "{error}"
        );
        assert!(
            error.to_string().contains("single execution lane"),
            "{error}"
        );
        lane.release(Address::with_last_byte(7), first).unwrap();
        assert!(lane.is_free());
    }

    #[test]
    fn an_account_with_a_foreign_transaction_in_flight_is_not_allocated_on_top_of() {
        let mut lane = NonceAllocator::new();
        // confirmed 4, pending 6: two transactions are in the pool that this lane did
        // not issue. Allocating 6 anyway is exactly §11's duplicate.
        let error = lane.allocate(&reading(4, 6)).unwrap_err();
        assert!(error.to_string().contains("in the pending view"), "{error}");
        assert!(lane.is_free(), "a refusal must not take the lane");
    }

    #[test]
    fn a_pending_view_below_the_confirmed_view_is_a_contradiction_not_a_negative() {
        let error = reading(5, 4).in_flight().unwrap_err();
        assert!(
            error.to_string().contains("did not come from one moment"),
            "{error}"
        );
    }

    #[test]
    fn releasing_a_nonce_the_lane_does_not_hold_is_reported() {
        let mut lane = NonceAllocator::new();
        lane.allocate(&reading(0, 0)).unwrap();
        assert!(lane
            .release(Address::with_last_byte(9), 0)
            .unwrap_err()
            .to_string()
            .contains("was asked to release"));
        assert!(lane
            .release(Address::with_last_byte(7), 41)
            .unwrap_err()
            .to_string()
            .contains("was asked to release"));
    }
}
