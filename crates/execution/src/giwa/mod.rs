//! GIWA-specific execution knowledge, and the only place in this crate that has any.
//!
//! §22's requirement is a directory boundary: `execution/giwa/sequencer_direct`, with the
//! rule that no GIWA-shaped RPC may appear in the builder, the signer, the intent or the
//! lifecycle modules. That rule is what makes §45 and §49 possible — the same
//! [`crate::submitter::TransactionSubmitter`] trait is implemented over a recorded
//! directory for replay and over this endpoint for live, and neither the transaction nor
//! the record can tell which one it went through.
//!
//! The endpoint knowledge collected here comes from measurement, not from the task book:
//! `data/evidence/m6/probe-summary.md` records which methods this node whitelists, which
//! fee fields its blocks carry, and — decisively — that no sequencer-direct protocol
//! exists on it (`mev_sendBundle`, `engine_submitBlock`, `sequencer_submit`,
//! `giwa_sendRawTransaction` all answer `-32601 rpc method is not whitelisted`). §24's
//! instruction for that case is followed literally: the type keeps the name §22 asks for,
//! its submission path is the one method the endpoint *does* answer, and any claim about a
//! private relay is recorded as BLOCKED rather than implemented.

pub mod preflight_facts;
pub mod reads;
pub mod sequencer_direct;

pub use preflight_facts::{GatheredPreflight, LivePreflightReads};
pub use reads::{
    estimate_l1_fee, pool_state_row, pre_signing_envelope, read_pool, GiwaAssetReader, PoolState,
    GAS_PRICE_ORACLE,
};
pub use sequencer_direct::{
    parse_receipt, receipt_provenance, submission_provenance, GiwaSequencerDirect,
};
