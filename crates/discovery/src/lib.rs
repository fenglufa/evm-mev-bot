//! Pool discovery: how this project finds out that a pool exists.
//!
//! Everything before M9.1 answered that question with a file: `data/protocols`
//! listed pools a human had checked, and a pool not in it was invisible to the
//! state engine, the graph, and every stage downstream. That is a fine place to
//! *start*, and a terrible place to stop — it means the bot can only ever trade
//! the markets somebody already knew about.
//!
//! The one rule this crate is built around:
//!
//! ```text
//! Discovery != Trust
//! ```
//!
//! Concretely, a `PairCreated` log becomes a [`CandidatePool`], which asserts
//! only that an address was announced. It becomes a `PoolAttestation` — the same
//! type every other stage already consumes, and the only way into the existing
//! [`evm_protocol::Registry`] — after [`verify`] has read the contract itself:
//! its bytecode, `token0()`, `token1()`, `getReserves()`, and its own `Sync` log.
//! Nothing here knows about any particular factory, and no address is trusted
//! because a list said so (M9.1 §8, §15).
//!
//! Two consequences worth stating before the code, because they are the ways this
//! could silently go wrong:
//!
//! - **The graph still requires state.** Identity evidence alone does not make a
//!   pool tradable. A candidate whose reserves nobody has published stays out of
//!   the graph, and `getReserves()` is kept as *contract verification state*, a
//!   different field from the *authoritative market state* only `Sync` may write
//!   (§12).
//! - **A verified pool is not an opportunity.** Verification proves an address
//!   was, at one pinned block, a V2-shaped pair with published reserves. It says
//!   nothing about liquidity being deep enough, the fee being known, or a route
//!   being profitable. Discovery feeds the graph; the graph feeds the search; the
//!   search's answer is what gets to claim anything about money.
//!
//! The scan loop and the verification decision are deliberately separated —
//! [`scan`] asks the node for records and [`verify`] decides from records already
//! in hand — so the evidence this crate writes can be recomputed offline, without
//! a second run against a provider that may answer differently next hour.

pub mod attest;
pub mod candidate;
pub mod error;
pub mod integrate;
pub mod reads;
pub mod scan;
pub mod verify;

pub use attest::{attestation_of, fee_of, SYNC_SIGNATURE};
pub use candidate::{CandidatePool, DiscoverySource};
pub use error::{DiscoveryError, Result};
pub use integrate::{integrate, DiscoveredState, DuplicateClaim, GraphOutcome, StoreRejection};
pub use reads::{
    collect_candidate_reads, collect_contract_reads, collect_sync_record, return_hex, CallKind,
    CallRecord, CandidateReads, SyncRecord,
};
pub use scan::{
    hex_of, log_record, pair_created_topic0, HistoricalPairCreatedSource, LogRecord, MalformedLog,
    ScanReport, ScanWindow, CHUNK_BLOCKS, NODE_LOG_LIMIT,
};
pub use verify::{
    verify, AuthoritativeMarketState, ContractVerificationState, RejectedPool, RejectionReason,
    Verification, VerificationStage, VerifiedPool,
};
