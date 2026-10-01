//! `evm-execution` — M6: the arrow out of a risk decision.
//!
//! M5 ends at a judgement. This crate starts from two values that M4 and M5 already
//! produce — a [`evm_simulation::SimulationResult`] and a [`evm_risk::RiskDecision`] —
//! and turns them into, in order: a [`intent::TransactionIntent`] bound to the block it
//! was decided against, an unsigned transaction
//! ([`tx::UnsignedTransaction`]), a signature over that transaction's own hash
//! ([`signer::Signer`]), raw bytes a node can decode ([`tx::SignedTransaction`]), a
//! submission ([`submitter::TransactionSubmitter`]) and a receipt
//! ([`receipt::ReceiptTracker`]), with one [`lifecycle::ExecutionRecord`] carrying the
//! whole chain of custody.
//!
//! Four boundaries give the milestone its meaning, and each is a type rather than a
//! paragraph:
//!
//! * **Accept is not send** ([`gate`]): a `RiskDecision::Accept` produces an intent;
//!   submission additionally needs the mode, the chain id, the block binding, the nonce,
//!   and the wallet's real balance.
//! * **Built, signed, submitted are three states** ([`lifecycle::ExecutionStatus`]):
//!   each has its own record and its own latency, so "we built it" can never be read as
//!   "we sent it".
//! * **Submitted is not included** ([`receipt`]): the RPC answer that returns a hash is
//!   an acknowledgement, and only a receipt whose hash matches decides `Included`.
//! * **The key is never a value in this repository** ([`signer::PRIVATE_KEY_ENV`]): it
//!   comes from the environment, is held by a type that redacts itself, and is read only
//!   by a mode that asked for it.
//!
//! [`stage::ExecutionStage`] is the one place that sequences those steps — §55's ladder,
//! from a risk decision to a receipt — and the only module in the crate that holds all four
//! endpoint surfaces (price, nonce, canonical chain, send) at once. Every other module can
//! be read on its own because none of them knows what comes next.
//!
//! GIWA-specific RPC knowledge lives in [`giwa`] and nowhere else; the builder, signer,
//! intent and lifecycle modules are chain-agnostic and would compile against any
//! EIP-1559 chain.

pub mod builder;
pub mod chain_read;
pub mod error;
pub mod evidence;
pub mod fee;
pub mod gate;
pub mod giwa;
pub mod intent;
pub mod lifecycle;
pub mod mode;
pub mod nonce;
pub mod receipt;
pub mod rlp;
pub mod signer;
pub mod stage;
pub mod submitter;
pub mod tx;

pub use builder::{Build, BuildPolicy, GasPolicy, RoundTrip, TransactionBuilder};
pub use chain_read::{read_binding, ChainReader};
pub use error::{ExecutionError, Result};
pub use evidence::{SignedTransactionEvidence, SubmissionEvidence};
pub use fee::{FeePolicy, FeeReading, FeeSource, FeeSourceKind};
pub use gate::{
    BalanceEvidence, BlockBinding, Freshness, GateAttempt, GateCheck, GateFacts, GateFailure,
    GateOutcome, NonceEvidence, PreSubmitGate,
};
pub use giwa::{parse_receipt, GiwaSequencerDirect};
pub use intent::{ExecutionIds, SenderFunding, TransactionIntent};
pub use lifecycle::{
    meter, Claim, ExecutionId, ExecutionLane, ExecutionRecord, ExecutionStatus, LaneRelease,
    Ledger, LANES,
};
pub use mode::ExecutionMode;
pub use nonce::{NonceAllocator, NonceReading, NonceSource};
pub use receipt::{
    bind, ExpectedTransaction, Receipt, ReceiptPolicy, ReceiptStatus, ReceiptTracker,
    TrackedReceipt,
};
pub use signer::{recover_sender, signing_hash_for_chain, ExecutionKey, Signer, PRIVATE_KEY_ENV};
pub use stage::{Abilities, AttemptProvenance, ExecutionSetup, ExecutionStage, StageReport};
pub use submitter::{EndpointKind, SubmissionOutcome, TransactionSubmitter};
pub use tx::{
    decode_raw, AccessTuple, DecodedTransaction, Signature, SignedTransaction, TransactionType,
    UnsignedTransaction,
};
