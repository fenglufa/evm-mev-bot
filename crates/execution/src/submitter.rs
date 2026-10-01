//! §21/§25: the seam between "we hold bytes" and "a node has them".
//!
//! The trait is deliberately two methods wide — submit, and read a receipt — because
//! that is everything the milestone has verified a GIWA endpoint can do
//! (`data/evidence/m6/probe-method-whitelist.txt`). §21 also offers an optional
//! `get_transaction`; it is not implemented, because the probes never established that
//! the method is whitelisted, and a submission interface with one speculative method in
//! it is how a mock ends up carrying real meaning.
//!
//! The important part is what the trait does *not* do: there is no `submit_with_retry`,
//! no `submit_and_wait`, no timeout that turns into a second call. §25's rule —
//! "submission timeout != submission failure" — is only enforceable if the type that
//! submits cannot also re-send, so the three answers an implementation can give are kept
//! distinct and one of them (`Unknown`) is explicitly a request for a *read*, not for
//! another write.

use alloy_primitives::B256;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::receipt::Receipt;
use crate::tx::SignedTransaction;

/// Which kind of endpoint an implementation talks to. §53 requires this string in
/// submission evidence, and it belongs to the implementation rather than to a caller
/// because a caller can be wrong about what it pointed at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointKind {
    /// Plain `eth_sendRawTransaction` over HTTP.
    PublicHttpRpc,
    /// The same method over the flashblocks endpoint, if a run chooses that path.
    FlashblocksHttpRpc,
    /// A recorded directory. Nothing is ever sent; the variant exists so replay can use
    /// the same Execution API as live (§49) without pretending to broadcast.
    Recorded,
}

impl EndpointKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::PublicHttpRpc => "public_http_rpc",
            Self::FlashblocksHttpRpc => "flashblocks_http_rpc",
            Self::Recorded => "recorded_no_submission",
        }
    }

    /// Whether this endpoint can put a transaction on a network at all.
    pub fn may_broadcast(self) -> bool {
        !matches!(self, Self::Recorded)
    }
}

/// The three answers §25 distinguishes. `Included` is not here on purpose: inclusion is
/// a receipt question ([`crate::receipt::ReceiptStatus`]), and folding it into a
/// submission answer would make "we sent it" and "it landed" one value again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmissionOutcome {
    /// The node accepted the raw bytes and returned a hash.
    Accepted {
        /// The hash the node returned, if it returned one.
        transaction_hash: Option<B256>,
        /// Whether that hash equals the one we computed locally over these exact bytes.
        /// A node that accepts bytes and names a different hash is telling us it
        /// received something else, so the mismatch is recorded rather than smoothed
        /// over — the receipt tracker binds against the *local* hash either way.
        hash_matches_local: bool,
        endpoint: EndpointKind,
        /// The node's own answer or the method name, so a report can quote it.
        detail: String,
    },
    /// The node answered and said no. Nothing is in flight.
    Rejected {
        reason: String,
        endpoint: EndpointKind,
    },
    /// We asked and do not know: a transport error, a non-JSON answer, an HTTP status
    /// that is not a refusal. §25's prohibition lives on this variant.
    Unknown {
        reason: String,
        endpoint: EndpointKind,
    },
}

impl SubmissionOutcome {
    /// The hash to track, when the answer carried one — always the locally computed hash
    /// of the bytes we sent, never a node's paraphrase of it (§27).
    pub fn tracked_hash(&self, local: B256) -> B256 {
        match self {
            Self::Accepted {
                transaction_hash: Some(hash),
                ..
            } if *hash == local => *hash,
            _ => local,
        }
    }

    pub fn endpoint(&self) -> EndpointKind {
        match self {
            Self::Accepted { endpoint, .. }
            | Self::Rejected { endpoint, .. }
            | Self::Unknown { endpoint, .. } => *endpoint,
        }
    }

    /// §25/§11: only a definite refusal proves the transaction is not in flight. An
    /// `Unknown` answer must keep the single nonce lane occupied until a read (receipt,
    /// or the pending nonce view) resolves it — which is the mechanism that makes a
    /// blind retry impossible rather than merely discouraged.
    pub fn proven_not_in_flight(&self) -> bool {
        matches!(self, Self::Rejected { .. })
    }

    /// The lifecycle status this answer earns. `Submitted` for an acknowledgement, and
    /// nothing higher — §2.3.
    pub fn status_word(&self) -> &'static str {
        match self {
            Self::Accepted { .. } => "submitted",
            Self::Rejected { .. } => "rejected",
            Self::Unknown { .. } => "unknown",
        }
    }
}

/// The submission surface (§21).
#[async_trait]
pub trait TransactionSubmitter {
    /// Which endpoint this is, as the implementation knows it.
    fn endpoint(&self) -> EndpointKind;

    /// §20: an implementation must refuse to send when the process was started in a mode
    /// that does not allow submission. The check belongs here because this is the last
    /// function before the network.
    fn may_submit(&self) -> bool;

    /// Hand the raw bytes to the endpoint. Never called twice for the same transaction
    /// by this crate — see the module doc.
    async fn submit(&self, transaction: &SignedTransaction) -> Result<SubmissionOutcome>;

    /// `eth_getTransactionReceipt` for a hash: `Ok(None)` is a legitimate "not yet".
    async fn receipt(&self, transaction_hash: B256) -> Result<Option<Receipt>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accepted(hash: B256, matches: bool) -> SubmissionOutcome {
        SubmissionOutcome::Accepted {
            transaction_hash: Some(hash),
            hash_matches_local: matches,
            endpoint: EndpointKind::PublicHttpRpc,
            detail: "eth_sendRawTransaction".to_string(),
        }
    }

    #[test]
    fn an_acknowledgement_is_never_inclusion() {
        let outcome = accepted(B256::left_padding_from(&[1]), true);
        assert_eq!(outcome.status_word(), "submitted");
        assert!(!outcome.proven_not_in_flight());
    }

    #[test]
    fn only_a_refusal_proves_nothing_is_in_flight() {
        for outcome in [
            accepted(B256::left_padding_from(&[1]), true),
            SubmissionOutcome::Unknown {
                reason: "connection reset".to_string(),
                endpoint: EndpointKind::PublicHttpRpc,
            },
        ] {
            assert!(
                !outcome.proven_not_in_flight(),
                "{} must keep the lane busy",
                outcome.status_word()
            );
        }
        let refused = SubmissionOutcome::Rejected {
            reason: "nonce too low".to_string(),
            endpoint: EndpointKind::PublicHttpRpc,
        };
        assert!(refused.proven_not_in_flight());
        assert_eq!(refused.status_word(), "rejected");
    }

    #[test]
    fn the_tracked_hash_is_ours_even_when_the_node_names_another_one() {
        let local = B256::left_padding_from(&[5]);
        let other = B256::left_padding_from(&[6]);
        assert_eq!(accepted(local, true).tracked_hash(local), local);
        // A node that returns a different hash does not get to redirect our tracking:
        // the receipt for the bytes we signed is the only receipt that answers for them.
        assert_eq!(accepted(other, false).tracked_hash(local), local);
        let refused = SubmissionOutcome::Rejected {
            reason: "invalid signature".to_string(),
            endpoint: EndpointKind::FlashblocksHttpRpc,
        };
        assert_eq!(refused.tracked_hash(local), local);
        assert_eq!(refused.endpoint(), EndpointKind::FlashblocksHttpRpc);
    }

    #[test]
    fn a_recorded_endpoint_is_honest_about_not_being_able_to_broadcast() {
        assert!(!EndpointKind::Recorded.may_broadcast());
        assert!(EndpointKind::PublicHttpRpc.may_broadcast());
        assert_eq!(EndpointKind::Recorded.name(), "recorded_no_submission");
    }
}
