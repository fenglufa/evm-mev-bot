//! M8.4.4 §4–§8: the first cross-stage state contract, stated as a type.
//!
//! M8.4.3 looked at eleven classes of chain state and found exactly one that answered
//! `yes` to all of *owner known, identity named, semantically equivalent, reusable in
//! principle* — the block header — and then recorded why it could not be reused anyway:
//! the only value that crosses the `Preflight → Build` boundary today is
//! [`crate::sequence::SnapshotPin`], which carries the **head** the gatherer read last and
//! no proof about it, while every header read that does carry a proof stays inside the
//! stage that paid for it. This module is that missing carrier, and the two checks that
//! give it meaning: the producer's, and the consumer's own.
//!
//! Three existing types were considered before writing a fourth, and each was refused for
//! a reason that is a fact about this repository rather than a taste:
//!
//! * [`crate::sequence::SnapshotPin`] is `{block_number, block_hash}` and already crosses
//!   the boundary, but it is produced from an `eth_getBlockByNumber` of the *moving head* —
//!   §8 calls that a live preflight block, whose identity is correct for a moment and not
//!   for a lifetime. It also carries no chain identity and no word about who verified it.
//! * `evm_chain::BlockContext` is a full header (base fee, gas limit, beneficiary). Handing
//!   it to `Build` would smuggle a *fee value* across a boundary whose §2 brief is identity
//!   only, and §25 names that exact failure mode.
//! * `evm_simulation::BlockPin` is `{number, hash}` with a real check attached
//!   ([`evm_simulation`] verifies it round-trip), but it belongs to the simulation's state
//!   provider, and the simulation is not the producer §5 asks for.
//!
//! The shape below therefore keeps only what the four questions need: which chain, which
//! block, which hash, and which read proved it. [`VerifiedBlockContext`] cannot be built
//! from a struct literal outside this module — its fields are private and its one
//! constructor *is* the verification — so a value of that type is evidence, not a claim,
//! and §26's "type system refuses the unverified construction" holds without a runtime
//! check being trusted to remember it.

use alloy_primitives::B256;
use serde::Serialize;

use evm_core::{BlockNumber, ChainId};

/// Which moment a context describes. §8 makes these two different things, and the split is
/// load-bearing: a fixed historical block's `number + hash` is a deterministic identity that
/// will never stop being true, while a live head's is only true until the next block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockContextScope {
    /// A block named by height, in the past: the pin the route was priced on. Freshness for
    /// this scope asks whether the endpoint is at or past that height.
    FixedHistorical,
    /// The head as one endpoint answered it at one moment. Freshness for this scope asks
    /// whether the head has moved at all.
    LiveHead,
}

/// A block named by chain, height and hash — the *claim* a verification is run against.
///
/// This type is deliberately cheap to construct, because a claim is not evidence: it is what
/// an intent, a plan or a node's answer says about a block. The proof that a claim is the
/// canonical block is [`VerifiedBlockContext`], and only [`VerifiedBlockContext::verify`]
/// can turn one into the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockIdentity {
    pub chain_id: ChainId,
    pub number: BlockNumber,
    pub hash: B256,
}

impl BlockIdentity {
    /// The three fields a reader needs side by side, in the words the evidence files use.
    pub fn describe(&self) -> String {
        format!(
            "chain {} block {} hash {:#x}",
            self.chain_id.0, self.number.0, self.hash
        )
    }
}

/// Why a producer refused to issue a context, or a consumer refused to accept one.
///
/// §29's rule: every refusal has a name. A refusal that only had a sentence would let the
/// evidence tables group by nothing, and M8.4.2's lesson was that an uncounted reason is an
/// unfalsifiable one.
///
/// Each variant carries the two values the failing comparison put side by side. `expected` is
/// the side that comparison started from — the block this stage means to act on, or, one check
/// later, the claim the propagated context makes — and `held` is the side it was compared
/// against. The words name no roles beyond that, because the two checks a consumer runs really
/// do start from different sides, and a sentence that pretended otherwise would be a guess
/// about which of its own two numbers is whose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRefusal {
    /// The context's chain, the consumer's expected chain, and the chain the consumer's own
    /// read came from are not one chain.
    ChainMismatch { expected: u64, held: u64 },
    /// The height the context names is not the height the consumer is asking about.
    BlockNumberMismatch { expected: u64, held: u64 },
    /// Same height, different block. §7's invalidation, and the case a bare height would
    /// have sailed through.
    BlockHashMismatch {
        number: u64,
        expected: String,
        held: String,
    },
    /// A live-head context whose head is no longer the head. §8: identity verified is not
    /// fresh forever.
    StaleBlockContext {
        context_number: u64,
        head_number: u64,
    },
    /// The consumer had nothing to compare against — no propagated context, or no read of
    /// its own. Refusing here is the §6 answer to "if the consumer cannot verify, reject".
    UnverifiableBlockContext(String),
}

impl ContextRefusal {
    /// The name the evidence tables group by.
    pub fn name(&self) -> &'static str {
        match self {
            Self::ChainMismatch { .. } => "chain_mismatch",
            Self::BlockNumberMismatch { .. } => "block_number_mismatch",
            Self::BlockHashMismatch { .. } => "block_hash_mismatch",
            Self::StaleBlockContext { .. } => "stale_block_context",
            Self::UnverifiableBlockContext(_) => "unverifiable_block_context",
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::ChainMismatch { expected, held } => {
                format!("the two answers name chains {expected} and {held}")
            }
            Self::BlockNumberMismatch { expected, held } => {
                format!("the two answers name blocks {expected} and {held}")
            }
            Self::BlockHashMismatch {
                number,
                expected,
                held,
            } => format!("at block {number} the two answers are {expected} and {held}"),
            Self::StaleBlockContext {
                context_number,
                head_number,
            } => format!(
                "the context describes head {context_number} and the chain is now at \
                 {head_number}, so the moment it vouched for is over"
            ),
            Self::UnverifiableBlockContext(why) => {
                format!("the consumer cannot verify this context: {why}")
            }
        }
    }
}

/// A block identity a producer obtained from the endpoint and checked, and that a consumer
/// has checked again with its own read.
///
/// The fields answer §27's four questions: `chain_id`, `number` and `hash` are the identity,
/// `source` says which read produced it, and `verified_by` names the leg that compared it.
/// `verified_at_ms` is deliberately absent from [`Self::to_row`]: it is a process fact of one
/// run, and §28 forbids a timestamp from doing a hash's job — including the job of making an
/// evidence row reproducible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedBlockContext {
    chain_id: ChainId,
    number: BlockNumber,
    hash: B256,
    scope: BlockContextScope,
    source: String,
    verified_by: String,
    verified_at_ms: u64,
}

impl VerifiedBlockContext {
    /// §5's producer leg, and the only way a value of this type exists.
    ///
    /// `expected` is what this stage means to act on — the block the intent pins. `observed`
    /// is what the endpoint answered *at that height, in the same read that produced the
    /// hash*: two calls can straddle a block, which is why the head gather in
    /// `giwa/preflight_facts.rs` reads number and hash out of one header and why this
    /// function takes them together. A producer that could pass a locally computed hash, or
    /// a height it filled in itself, would make the type's name a lie, so the check runs
    /// before the value can exist and every disagreement is a named refusal.
    pub fn verify(
        expected: &BlockIdentity,
        observed: &BlockIdentity,
        scope: BlockContextScope,
        source: impl Into<String>,
        verified_by: impl Into<String>,
        verified_at_ms: u64,
    ) -> std::result::Result<Self, ContextRefusal> {
        if expected.chain_id != observed.chain_id {
            return Err(ContextRefusal::ChainMismatch {
                expected: expected.chain_id.0,
                held: observed.chain_id.0,
            });
        }
        if expected.number != observed.number {
            return Err(ContextRefusal::BlockNumberMismatch {
                expected: expected.number.0,
                held: observed.number.0,
            });
        }
        if expected.hash != observed.hash {
            return Err(ContextRefusal::BlockHashMismatch {
                number: observed.number.0,
                expected: format!("{:#x}", expected.hash),
                held: format!("{:#x}", observed.hash),
            });
        }
        Ok(Self {
            chain_id: observed.chain_id,
            number: observed.number,
            hash: observed.hash,
            scope,
            source: source.into(),
            verified_by: verified_by.into(),
            verified_at_ms,
        })
    }

    /// §6's consumer leg, run on the consumer's own read.
    ///
    /// Three identities have to agree, and the middle one is the point: the propagated
    /// context, what this stage intends to act on, and what *this stage's own read* answered.
    /// Comparing only the first two would be the producer vouching for itself by mail; the
    /// third is what makes this an independent verification rather than a copy. A consumer
    /// with no read of its own passes `None` and gets
    /// [`ContextRefusal::UnverifiableBlockContext`] — §6 refuses the "it looked the same, so
    /// continue" fallback by name.
    pub fn verify_as_consumer(
        &self,
        expected: &BlockIdentity,
        observed: Option<&BlockIdentity>,
    ) -> std::result::Result<(), ContextRefusal> {
        if self.chain_id != expected.chain_id {
            return Err(ContextRefusal::ChainMismatch {
                expected: expected.chain_id.0,
                held: self.chain_id.0,
            });
        }
        if self.number != expected.number {
            return Err(ContextRefusal::BlockNumberMismatch {
                expected: expected.number.0,
                held: self.number.0,
            });
        }
        if self.hash != expected.hash {
            return Err(ContextRefusal::BlockHashMismatch {
                number: expected.number.0,
                expected: format!("{:#x}", expected.hash),
                held: format!("{:#x}", self.hash),
            });
        }
        let Some(observed) = observed else {
            return Err(ContextRefusal::UnverifiableBlockContext(
                "this stage made no read of the block it is being handed: there is nothing to \
                 compare the propagated context against"
                    .to_string(),
            ));
        };
        if observed.chain_id != expected.chain_id {
            return Err(ContextRefusal::ChainMismatch {
                expected: expected.chain_id.0,
                held: observed.chain_id.0,
            });
        }
        if observed.number != expected.number {
            return Err(ContextRefusal::BlockNumberMismatch {
                expected: expected.number.0,
                held: observed.number.0,
            });
        }
        if observed.hash != self.hash {
            return Err(ContextRefusal::BlockHashMismatch {
                number: expected.number.0,
                expected: format!("{:#x}", self.hash),
                held: format!("{:#x}", observed.hash),
            });
        }
        Ok(())
    }

    /// §8's second axis, kept separate from identity on purpose.
    ///
    /// Every check above can pass and this one still refuse, and that is the point: a hash
    /// that matches forever describes a block that is *canonical*, not one that is *current*.
    /// The two rules are this repository's existing semantics, not a rule written for the
    /// experiment:
    ///
    /// * a fixed historical block (the pin) is usable while the endpoint is at or past it —
    ///   the head-behind test `preflight.rs`'s `current_head` line already runs;
    /// * a live head is usable only while it *is* the head, because the §26 fee line and the
    ///   nonce legs compare against the moment the transaction will be sent into, and a head
    ///   that moved makes the answer describe a different block.
    pub fn fresh_at_head(
        &self,
        head_number: BlockNumber,
    ) -> std::result::Result<(), ContextRefusal> {
        match self.scope {
            BlockContextScope::FixedHistorical => {
                if head_number < self.number {
                    Err(ContextRefusal::StaleBlockContext {
                        context_number: self.number.0,
                        head_number: head_number.0,
                    })
                } else {
                    Ok(())
                }
            }
            BlockContextScope::LiveHead => {
                if head_number != self.number {
                    Err(ContextRefusal::StaleBlockContext {
                        context_number: self.number.0,
                        head_number: head_number.0,
                    })
                } else {
                    Ok(())
                }
            }
        }
    }

    pub fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    pub fn number(&self) -> BlockNumber {
        self.number
    }

    pub fn hash(&self) -> B256 {
        self.hash
    }

    pub fn scope(&self) -> BlockContextScope {
        self.scope
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn verified_by(&self) -> &str {
        &self.verified_by
    }

    /// Diagnostics only. §28: a timestamp cannot be an identity, cannot stand in for a hash,
    /// and cannot prove freshness — the freshness question is
    /// [`Self::fresh_at_head`], which answers it with a block number.
    pub fn verified_at_ms(&self) -> u64 {
        self.verified_at_ms
    }

    pub fn identity(&self) -> BlockIdentity {
        BlockIdentity {
            chain_id: self.chain_id,
            number: self.number,
            hash: self.hash,
        }
    }

    /// The evidence row this context can answer for itself: the identity, its scope, and the
    /// two provenance strings — with the timestamp left out, because an evidence file that
    /// cannot be regenerated byte for byte is not evidence.
    pub fn to_row(&self) -> serde_json::Value {
        serde_json::json!({
            "chain_id": self.chain_id.0,
            "block_number": self.number.0,
            "block_hash": format!("{:#x}", self.hash),
            "scope": self.scope,
            "source": self.source,
            "verified_by": self.verified_by,
        })
    }
}

/// The serialized form is the row form, so the report, the evidence file and the comparison
/// tables can never show two spellings of one context.
impl Serialize for VerifiedBlockContext {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.to_row().serialize(serializer)
    }
}

/// What the producer's leg (§5) decided: a context it can vouch for, or the named reason it
/// cannot.
///
/// The refusal travels in the report rather than appearing as an absent field because §30
/// makes a rejection a fact: a reader of the evidence has to be able to tell "the producer
/// checked and refused" from "the producer never answered", and `null` cannot tell those
/// apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProducerOutcome {
    Verified(VerifiedBlockContext),
    Refused(ContextRefusal),
}

impl ProducerOutcome {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Verified(_) => "verified",
            Self::Refused(_) => "refused",
        }
    }

    /// The value that may cross the stage boundary, if this producer produced one.
    pub fn context(&self) -> Option<&VerifiedBlockContext> {
        match self {
            Self::Verified(context) => Some(context),
            Self::Refused(_) => None,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Verified(context) => format!(
                "{} proved by {} from {}",
                context.identity().describe(),
                context.verified_by(),
                context.source()
            ),
            Self::Refused(reason) => reason.describe(),
        }
    }

    pub fn to_row(&self) -> serde_json::Value {
        match self {
            Self::Verified(context) => serde_json::json!({
                "outcome": self.name(),
                "reason": serde_json::Value::Null,
                "verified_block": context.to_row(),
            }),
            Self::Refused(reason) => serde_json::json!({
                "outcome": self.name(),
                "reason": reason.name(),
                "reason_detail": reason.describe(),
                "verified_block": serde_json::Value::Null,
            }),
        }
    }
}

impl Serialize for ProducerOutcome {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.to_row().serialize(serializer)
    }
}

/// What one stage decided about a propagated context, in one row.
///
/// §30's demand is that a rejection and a fallback never read as the same thing, so the
/// outcome names the consumer's own read alongside the verdict: `accepted` here means the
/// consumer checked, not that it skipped anything.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextOutcome {
    /// Nothing was propagated, so there was nothing to check.
    NoContext,
    /// The consumer's own read agreed with the propagated context, on chain, height and hash.
    Accepted { consumer_read: bool },
    /// The consumer refused it, by name. Whether the stage then used its own value is a
    /// separate field, because "rejected" and "recovered" are different facts.
    Rejected {
        reason: ContextRefusal,
        consumer_read: bool,
    },
}

impl ContextOutcome {
    pub fn name(&self) -> &'static str {
        match self {
            Self::NoContext => "no_context",
            Self::Accepted { .. } => "accepted",
            Self::Rejected { .. } => "rejected",
        }
    }

    /// The one line this verdict can be told in, for a report that records it as text.
    pub fn describe(&self) -> String {
        match self {
            Self::NoContext => "nothing was propagated to this stage".to_string(),
            Self::Accepted { consumer_read } => format!(
                "the propagated context, this stage's own pin and this stage's own read name \
                 one block (consumer read made: {consumer_read})"
            ),
            Self::Rejected { reason, .. } => reason.describe(),
        }
    }

    /// Whether the consumer's check passed. §31: `reused = true` may only be stated once the
    /// producer verified, the consumer verified, and the two answers name one canonical block.
    pub fn accepted(&self) -> bool {
        matches!(self, Self::Accepted { .. })
    }

    pub fn to_row(&self) -> serde_json::Value {
        match self {
            Self::NoContext => serde_json::json!({
                "outcome": self.name(),
                "reason": serde_json::Value::Null,
                "consumer_read": serde_json::Value::Null,
            }),
            Self::Accepted { consumer_read } => serde_json::json!({
                "outcome": self.name(),
                "reason": serde_json::Value::Null,
                "consumer_read": consumer_read,
            }),
            Self::Rejected {
                reason,
                consumer_read,
            } => serde_json::json!({
                "outcome": self.name(),
                "reason": reason.name(),
                "reason_detail": reason.describe(),
                "consumer_read": consumer_read,
            }),
        }
    }
}

/// The consumer leg, stated once so the production call site and the experiment's negative
/// controls run the same code path (§19's requirement that a control be *the* check, not a
/// paraphrase of it).
///
/// `observed` is the consumer's *own* answer for the block its intent pins — `None` when the
/// stage read none, which §6 refuses outright. `head_number` is the head the consumer itself
/// has read, and `None` means it has read none: [`crate::sequence`]'s `Build` lane reads the
/// block at its pin by number and never asks for the head (§20 forbids `latest` on this path),
/// so a live-head context has no honest freshness answer there and is refused by name rather
/// than accepted on the producer's timing.
pub fn consumer_check(
    propagated: Option<&VerifiedBlockContext>,
    expected: &BlockIdentity,
    observed: Option<&BlockIdentity>,
    head_number: Option<BlockNumber>,
) -> ContextOutcome {
    let Some(context) = propagated else {
        return ContextOutcome::NoContext;
    };
    if let Err(reason) = context.verify_as_consumer(expected, observed) {
        return ContextOutcome::Rejected {
            reason,
            consumer_read: observed.is_some(),
        };
    }
    match head_number {
        Some(head) => {
            if let Err(reason) = context.fresh_at_head(head) {
                return ContextOutcome::Rejected {
                    reason,
                    consumer_read: observed.is_some(),
                };
            }
        }
        None if context.scope() == BlockContextScope::LiveHead => {
            return ContextOutcome::Rejected {
                reason: ContextRefusal::UnverifiableBlockContext(
                    "this stage read no head, and a live-head context is only true while the \
                     block it names still is the head — the freshness question has no answer \
                     from this side (§8)"
                        .to_string(),
                ),
                consumer_read: observed.is_some(),
            };
        }
        // A fixed historical block's freshness rule is this repository's existing one: the
        // endpoint holds this hash at this height (§32's `block_binding_valid`), and the read
        // that just agreed with the context is what proves it. Nothing is being permitted here
        // that the gate did not already require.
        None => {}
    }
    ContextOutcome::Accepted {
        consumer_read: observed.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(byte: u8) -> B256 {
        B256::repeat_byte(byte)
    }

    fn identity(byte: u8) -> BlockIdentity {
        BlockIdentity {
            chain_id: ChainId(1),
            number: BlockNumber(100),
            hash: hash(byte),
        }
    }

    /// A producer's verified context, from the two halves the gather would hand it.
    fn produced(byte: u8) -> VerifiedBlockContext {
        VerifiedBlockContext::verify(
            &identity(byte),
            &identity(byte),
            BlockContextScope::FixedHistorical,
            "eth_getBlockByNumber(100) in preflight",
            "§26's block binding leg at the pin",
            7,
        )
        .expect("a read that agrees with the pin produces a context")
    }

    #[test]
    fn verified_context_requires_number_and_hash() {
        // §35's unit list, first line. The constructor is the only door, and it takes both
        // halves of the identity as values a read answered — there is no `hash: None` to
        // reach for, which is NC4 settled by the type rather than by a check.
        let produced = produced(9);
        assert_eq!(produced.number(), BlockNumber(100));
        assert_eq!(produced.hash(), hash(9));
        let row = produced.to_row();
        let fields: Vec<&str> = row
            .as_object()
            .expect("a row")
            .keys()
            .map(String::as_str)
            .collect();
        assert!(fields.contains(&"block_number") && fields.contains(&"block_hash"));
        assert!(
            !fields.contains(&"verified_at_ms"),
            "the timestamp is a process fact of one run; the identity row cannot carry it and \
             stay reproducible"
        );
    }

    #[test]
    fn verified_context_preserves_chain_identity() {
        let mut observed = identity(9);
        observed.chain_id = ChainId(2);
        let refusal = VerifiedBlockContext::verify(
            &identity(9),
            &observed,
            BlockContextScope::FixedHistorical,
            "read",
            "leg",
            1,
        )
        .expect_err("a read from another chain is not this chain's block");
        assert_eq!(
            refusal,
            ContextRefusal::ChainMismatch {
                expected: 1,
                held: 2
            }
        );
        assert_eq!(refusal.name(), "chain_mismatch");
    }

    #[test]
    fn same_height_different_hash_rejected() {
        // NC1.
        let refusal = VerifiedBlockContext::verify(
            &identity(1),
            &identity(2),
            BlockContextScope::FixedHistorical,
            "read",
            "leg",
            1,
        )
        .expect_err("the same height holding a different block is a reorg");
        assert!(matches!(refusal, ContextRefusal::BlockHashMismatch { .. }));
        assert_eq!(refusal.name(), "block_hash_mismatch");
    }

    #[test]
    fn different_height_rejected() {
        // NC2.
        let mut observed = identity(9);
        observed.number = BlockNumber(101);
        let refusal = VerifiedBlockContext::verify(
            &identity(9),
            &observed,
            BlockContextScope::FixedHistorical,
            "read",
            "leg",
            1,
        )
        .expect_err("a context for a different height describes a different block");
        assert_eq!(
            refusal,
            ContextRefusal::BlockNumberMismatch {
                expected: 100,
                held: 101
            }
        );
    }

    #[test]
    fn different_chain_rejected_at_the_consumer() {
        // NC3, on the consumer's side: a context from the right chain but a consumer on
        // another one must refuse before it compares anything else.
        let context = produced(9);
        let mut expected = identity(9);
        expected.chain_id = ChainId(3);
        let outcome = consumer_check(
            Some(&context),
            &expected,
            Some(&expected),
            Some(BlockNumber(100)),
        );
        assert!(matches!(
            outcome,
            ContextOutcome::Rejected {
                reason: ContextRefusal::ChainMismatch { .. },
                ..
            }
        ));
    }

    #[test]
    fn missing_hash_rejected_by_construction() {
        // NC4. There is no `Option<B256>` in the verified type, so the state a test would
        // have to construct to prove the refusal cannot be built at all. What *can* be built
        // is a consumer with no read of its own, and that is refused by name.
        let context = produced(9);
        let outcome = consumer_check(Some(&context), &identity(9), None, Some(BlockNumber(100)));
        assert_eq!(
            outcome.name(),
            "rejected",
            "a consumer that cannot check must reject, not accept on the producer's word"
        );
        assert!(matches!(
            outcome,
            ContextOutcome::Rejected {
                reason: ContextRefusal::UnverifiableBlockContext(_),
                consumer_read: false
            }
        ));
    }

    #[test]
    fn tampered_context_rejected() {
        // NC5: the producer's answer is edited after it left the read, and the consumer's own
        // read disagrees. Both halves of the identity are caught, whichever field was touched.
        let context = produced(9);
        let mut tampered_height = context.clone();
        tampered_height.number = BlockNumber(101);
        let by_height = consumer_check(
            Some(&tampered_height),
            &identity(9),
            Some(&identity(9)),
            Some(BlockNumber(100)),
        );
        assert!(matches!(
            by_height,
            ContextOutcome::Rejected {
                reason: ContextRefusal::BlockNumberMismatch { .. },
                consumer_read: true
            }
        ));

        let mut tampered_hash = context;
        tampered_hash.hash = hash(10);
        let by_hash = consumer_check(
            Some(&tampered_hash),
            &identity(9),
            Some(&identity(9)),
            Some(BlockNumber(100)),
        );
        assert!(matches!(
            by_hash,
            ContextOutcome::Rejected {
                reason: ContextRefusal::BlockHashMismatch { .. },
                consumer_read: true
            }
        ));
    }

    #[test]
    fn stale_live_head_context_rejected_and_fixed_pin_context_survives() {
        // NC6, split by scope, because §8 says the two scopes answer the same question with
        // different rules. A head context is only current while it *is* the head; a pin
        // context stays usable while the chain is at or past it.
        let head = VerifiedBlockContext::verify(
            &identity(9),
            &identity(9),
            BlockContextScope::LiveHead,
            "read",
            "leg",
            1,
        )
        .expect("a head read that agrees with itself");
        assert_eq!(
            head.fresh_at_head(BlockNumber(100)),
            Ok(()),
            "the same head is still fresh"
        );
        assert_eq!(
            head.fresh_at_head(BlockNumber(101)),
            Err(ContextRefusal::StaleBlockContext {
                context_number: 100,
                head_number: 101
            }),
            "one block later, a head context is a description of a moment that is over"
        );
        assert_eq!(
            produced(9).fresh_at_head(BlockNumber(101)),
            Ok(()),
            "a fixed historical block does not stop being canonical because the chain moved"
        );
        assert!(produced(9).fresh_at_head(BlockNumber(99)).is_err());
    }

    #[test]
    fn a_stale_head_context_is_rejected_at_the_consumer_by_its_own_head_read() {
        // NC6 and §21's Case B, run through the boundary check rather than through
        // `fresh_at_head` alone: the producer vouched for head 100, the consumer has read the
        // head itself and it is 101. The refusal is named, and the consumer's own read is what
        // produces it. The same stage handed the *pin* context is not refused for the same
        // head moving — a fixed historical block does not stop being canonical (§8), which is
        // §21's Case B answered from the semantics this repository already runs.
        let head_context = VerifiedBlockContext::verify(
            &identity(9),
            &identity(9),
            BlockContextScope::LiveHead,
            "read",
            "leg",
            1,
        )
        .expect("a head read that agrees with itself");
        let stale = consumer_check(
            Some(&head_context),
            &identity(9),
            Some(&identity(9)),
            Some(BlockNumber(101)),
        );
        assert!(matches!(
            stale,
            ContextOutcome::Rejected {
                reason: ContextRefusal::StaleBlockContext {
                    context_number: 100,
                    head_number: 101,
                },
                consumer_read: true
            }
        ));

        assert_eq!(
            consumer_check(
                Some(&produced(9)),
                &identity(9),
                Some(&identity(9)),
                Some(BlockNumber(101)),
            ),
            ContextOutcome::Accepted {
                consumer_read: true
            },
            "the identical head number refuses one scope and passes the other, which is the \
             whole content of §8's distinction"
        );
    }

    #[test]
    fn a_stage_that_reads_no_head_cannot_rely_on_a_live_head_context() {
        // §20 bans `latest` on the execution path, so `Build` reads the block at its pin by
        // number and never asks what the head is. A live-head context therefore has no
        // freshness answer at this boundary, and the honest outcome is a named refusal rather
        // than the pass the producer's timing would have implied (§6: no verification, no
        // accept). A pin context is unaffected, because its freshness rule is the read the
        // consumer just made.
        let head_context = VerifiedBlockContext::verify(
            &identity(9),
            &identity(9),
            BlockContextScope::LiveHead,
            "read",
            "leg",
            1,
        )
        .expect("a head read that agrees with itself");
        let outcome = consumer_check(Some(&head_context), &identity(9), Some(&identity(9)), None);
        assert!(matches!(
            outcome,
            ContextOutcome::Rejected {
                reason: ContextRefusal::UnverifiableBlockContext(_),
                consumer_read: true
            }
        ));
        assert_eq!(
            consumer_check(Some(&produced(9)), &identity(9), Some(&identity(9)), None),
            ContextOutcome::Accepted {
                consumer_read: true
            }
        );
    }

    #[test]
    fn a_consumer_that_only_agrees_with_the_producer_is_not_verifying() {
        // §6's line between trusting and checking: pass the producer's own answer in as the
        // consumer's read and the check is vacuous, so the outcome has to say which happened.
        let context = produced(9);
        let with_own_read = consumer_check(
            Some(&context),
            &identity(9),
            Some(&identity(9)),
            Some(BlockNumber(100)),
        );
        assert_eq!(
            with_own_read,
            ContextOutcome::Accepted {
                consumer_read: true
            }
        );
        let without = consumer_check(Some(&context), &identity(9), None, Some(BlockNumber(100)));
        assert_ne!(
            without, with_own_read,
            "an accepted with a read in hand and an accepted without one must never print \
             the same row"
        );
    }

    #[test]
    fn no_propagated_context_is_its_own_outcome() {
        let outcome = consumer_check(
            None,
            &identity(9),
            Some(&identity(9)),
            Some(BlockNumber(100)),
        );
        assert_eq!(outcome, ContextOutcome::NoContext);
        assert_eq!(outcome.name(), "no_context");
        assert!(!outcome.accepted());
    }

    #[test]
    fn a_hash_that_matches_the_pin_but_not_the_head_is_rejected_by_the_consumer() {
        // The producer pins block 100 as 0x0a; the endpoint now answers 0x0b there. The
        // context was never issued — §5's producer leg refuses first — so no consumer check
        // can be reached with a bad hash inside a verified value.
        let refusal = VerifiedBlockContext::verify(
            &identity(10),
            &identity(11),
            BlockContextScope::FixedHistorical,
            "read",
            "leg",
            1,
        )
        .expect_err("the pin and the endpoint disagree");
        assert_eq!(
            refusal.name(),
            "block_hash_mismatch",
            "and the refusal names the field, so a report can count reorgs apart from stale \
             heads"
        );
    }
}
