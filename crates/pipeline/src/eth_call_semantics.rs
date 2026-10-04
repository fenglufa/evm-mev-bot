//! M8.5.1: what one `eth_call` actually is, and who is allowed to believe its answer.
//!
//! M8.4.2 measured 78 duplicate read-pairs in three live runs and found 42 reuse candidates;
//! 18 of those candidates are `eth_call`, all of them `opportunity_detection → preflight`, all
//! refused on block identity. M8.4.3 then found that all 42 candidates — not just the
//! `eth_call` ones — stall on one axis, `lifecycle_ownership`: 「谁拥有这份答案」在记录里根本
//! 不是一个字段. M8.4.4 built a verified carrier for exactly one flow (the block header,
//! `Preflight → Build`) and proved no net RPC saving.
//!
//! So this module does not ask 「能不能缓存」. §68's instruction is the opposite question, and
//! these tables answer it: **what kind of artifact is an `eth_call` result, who owns it, what
//! makes it stale, and what would a consumer have to be able to check in order to be allowed to
//! trust one it did not pay for.**
//!
//! Nothing here is measured by asking the node. §32 forbids an instrumentation read, and this
//! module does not need one: every row is derived from
//!
//! * the production code that makes the call and the code that reads the answer
//!   ([`Anchor`] pointers, resolved to line numbers by the evidence gate rather than asserted
//!   here), and
//! * the records M8.4.2 already committed — its three live runs' `pipeline-calls.json` and the
//!   route-run rows those same runs published.
//!
//! The diagnosis is a verdict of `NOT_REUSABLE` for the whole `eth_call` class, and the reason
//! is not a missing proof. §16 of this file's task book asked for the artifact to be classified
//! before its reuse was judged; classified, it turns out to be a *freshness instrument* — the
//! second read is the check — so reusing the first answer does not save an RPC, it deletes a
//! gate. That conclusion is anchored, including by a test this repository already had
//! (`crates/execution/src/preflight.rs`: a reserve that was not re-read must block rather than
//! fall back to the priced numbers).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{json, Value};

use crate::canonicalization::RpcReadKey;
use crate::state_ownership::{code, record, test, Anchor, ProofStatus};

/// Where this milestone's tables are published: the directory §34 names. Its suggested
/// subdirectories (`calls/`, `ownership/`, …) are not used — a table here is read by the recompute
/// gate through `ETH_CALL_FILES`, so the seven names are the whole layout and the README maps each
/// one to the §34 slot it fills.
pub const EVIDENCE_DIR: &str = "data/evidence/m8/m8.5.1";

/// The seven tables this module owns, in publication order.
pub const ETH_CALL_FILES: [&str; 7] = [
    "call-surface.json",
    "normalized-identities.json",
    "ownership-matrix.json",
    "lifecycle-contracts.json",
    "dependency-matrix.json",
    "negative-controls.json",
    "reuse-verdicts.json",
];

/// §39's raw source: the records M8.4.2 committed. Every count this module publishes is
/// recomputed from these rows by the evidence gate; nothing here is a hand-typed tally.
pub const RAW_RUNS_GLOB: &str = "data/evidence/m8/cross-stage/runs/*/pipeline-calls.json";
/// The decoded answers of the same runs, at the artifact layer rather than the wire layer.
pub const RAW_ROUTE_RUNS_GLOB: &str = "data/evidence/m8/cross-stage/route-runs/*/";

// ---------------------------------------------------------------------------
// §36: a field that is absent is not a field that is unknown
// ---------------------------------------------------------------------------

/// The four-way distinction §36 demands for every identity field.
///
/// The failure mode this guards is the one that would make this whole milestone worthless: a
/// report that writes `"from": null` and is then read as 「查过了」. A `null` says nothing about
/// the ask; these states say what kind of nothing it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldEvidence {
    /// The field cannot be present, and a line of production code is why. The string names the
    /// rule, and [`FieldEvidence::anchors`] points at the code.
    ProvenAbsent(&'static str),
    /// The field is present and this build recorded its value.
    Recorded(String),
    /// The field exists in the JSON-RPC method but this class of ask has no use for it, with the
    /// reason. Distinct from [`FieldEvidence::ProvenAbsent`]: that is a measurement, this is a
    /// judgement about the method.
    NotApplicable(&'static str),
    /// The ask may or may not carry the field, and the record does not say. §42's answer for the
    /// response bytes, and the honest answer for the node's default sender.
    NotRecorded(&'static str),
    /// This module declines to guess. Reserved for fields a future build might add without
    /// this table noticing; an assembly that emits it is telling the reader it cannot judge.
    Unknown,
}

impl FieldEvidence {
    pub fn label(&self) -> &'static str {
        match self {
            FieldEvidence::ProvenAbsent(_) => "proven_absent",
            FieldEvidence::Recorded(_) => "recorded",
            FieldEvidence::NotApplicable(_) => "not_applicable",
            FieldEvidence::NotRecorded(_) => "not_recorded",
            FieldEvidence::Unknown => "unknown",
        }
    }

    /// The rule or the value, whichever this state carries.
    pub fn detail(&self) -> Option<&str> {
        match self {
            FieldEvidence::ProvenAbsent(rule) | FieldEvidence::NotApplicable(rule) => Some(rule),
            FieldEvidence::Recorded(value) => Some(value.as_str()),
            FieldEvidence::NotRecorded(rule) => Some(rule),
            FieldEvidence::Unknown => None,
        }
    }

    /// Whether this state may be read as 「已调查」. Only a proven absence is an answer; an
    /// unrecorded field is a gap, and the gate counts the gaps rather than smoothing them.
    pub const fn is_settled(&self) -> bool {
        matches!(
            self,
            FieldEvidence::ProvenAbsent(_) | FieldEvidence::Recorded(_)
        )
    }

    pub fn to_json(&self) -> Value {
        json!({
            "state": self.label(),
            "detail": self.detail(),
            "settled": self.is_settled(),
        })
    }
}

/// §11/§12's block term, kept with the form it arrived in.
///
/// `form` is M8.4.2's vocabulary reused rather than re-derived: a decimal height, a tag the node
/// resolves at its own discretion, or no block term at all. Which of the three a row carries is a
/// measured count, not an assumption, and §13's rule — a `latest` must never be read as a fixed
/// block — only bites if the two forms stay distinguishable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BlockTerm {
    pub term: String,
    pub form: &'static str,
    /// Whether the record also pins this height to a header hash. The read that fixed this
    /// run's pin is a number with no hash beside it; the read that fixed the gate's head is a
    /// number and a hash out of one header, which is why this is a per-ask field rather than a
    /// per-run one.
    pub hash_provenance: &'static str,
}

// ---------------------------------------------------------------------------
// §7/§51: request identity, decided by EVM semantics rather than by the parameter list
// ---------------------------------------------------------------------------

/// What one `eth_call` asked for.
///
/// §7 forbids stuffing every JSON-RPC parameter in for safety, so the field set here is
/// determined by what can change an execution result: chain, block, `to`, `data`, and then
/// `from`, `value` and the state override object — each of the last three carrying a
/// [`FieldEvidence`] state instead of a silent `None`.
///
/// The four terms that *are* identity come from M8.4.2's [`RpcReadKey`], not from a second
/// parser: §51 says reuse the canonical representation the production code already has, and a
/// parallel implementation is exactly how two tables end up disagreeing about one ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EthCallIdentity {
    pub chain_id: String,
    pub block: BlockTerm,
    pub to: String,
    pub calldata: String,
    pub from: FieldEvidence,
    pub value: FieldEvidence,
    pub state_override: FieldEvidence,
    pub gas: FieldEvidence,
    pub stage: String,
    pub caller: String,
    pub run: String,
}

/// Why each non-identity field is absent, in the words the gate prints beside it.
pub const FROM_ABSENT: &str = "CallRequest has two fields — to and data — so no caller in this \
    build can name a sender; the node answers with its own default, which the record does not \
    capture";
pub const VALUE_ABSENT: &str = "the same two-field request, and eth_call's params object is \
    written with those two keys and no third";
pub const OVERRIDE_ABSENT: &str = "the params array this build sends is exactly two elements — \
    the call object and the block term — so there is no third element to carry a balance, nonce, \
    code or storage override";
pub const GAS_ABSENT: &str = "no gas field in CallRequest and none in the params object: the \
    node answers with its own call gas ceiling";
pub const FROM_EFFECTIVE: &str = "node_default_for_absent_from_not_recorded";

impl EthCallIdentity {
    /// Build the identity from M8.4.2's key for one published row.
    ///
    /// `None` for a row that is not an `eth_call` or that has no comparable terms — a refusal,
    /// not an empty identity, so a caller cannot silently count an unparseable ask.
    pub fn of_row(key: &RpcReadKey) -> Option<Self> {
        if key.method != "eth_call" {
            return None;
        }
        let to = key.terms.get("to")?;
        let calldata = key.terms.get("data")?;
        let block = key.block.as_ref()?;
        Some(Self {
            chain_id: key
                .chain_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "chain-unknown".to_string()),
            block: BlockTerm {
                term: block.clone(),
                form: key.block_form,
                hash_provenance: "not_in_the_record",
            },
            to: to.clone(),
            calldata: calldata.clone(),
            from: FieldEvidence::ProvenAbsent(FROM_ABSENT),
            value: match key.terms.get("value") {
                Some(amount) => FieldEvidence::Recorded(amount.clone()),
                None => FieldEvidence::ProvenAbsent(VALUE_ABSENT),
            },
            state_override: FieldEvidence::ProvenAbsent(OVERRIDE_ABSENT),
            gas: FieldEvidence::ProvenAbsent(GAS_ABSENT),
            stage: key
                .stage
                .clone()
                .unwrap_or_else(|| "<unstamped>".to_string()),
            caller: key
                .caller
                .clone()
                .unwrap_or_else(|| "<unstamped>".to_string()),
            run: key.run.clone().unwrap_or_default(),
        })
    }

    /// The first four bytes of calldata: which function is being asked.
    pub fn selector(&self) -> &str {
        let hex = self.calldata.strip_prefix("0x").unwrap_or(&self.calldata);
        &hex[..hex.len().min(8)]
    }

    /// The identity §26's `identity = proven` axis is about: every term that can change the
    /// answer, block included.
    pub fn identity_with_block(&self) -> String {
        format!(
            "eth_call|chain={}|block={}|to={}|data={}",
            self.chain_id, self.block.term, self.to, self.calldata
        )
    }

    /// The same identity with the block term removed — the figure §41's key control measures.
    /// Two asks equal here and different there are the case M8.4.2 refused on `different_block`,
    /// and this milestone decides whether that refusal was about a technicality or about the
    /// answer.
    pub fn identity_without_block(&self) -> String {
        format!(
            "eth_call|chain={}|to={}|data={}",
            self.chain_id, self.to, self.calldata
        )
    }

    /// The eight fields, as one row. A field that is absent still appears: dropping it would be
    /// the §36 failure this type exists to prevent.
    pub fn to_json(&self) -> Value {
        json!({
            "chain_id": self.chain_id,
            "block_term": self.block.term,
            "block_form": self.block.form,
            "block_hash_provenance": self.block.hash_provenance,
            "to": self.to,
            "calldata": self.calldata,
            "selector": self.selector(),
            "from": self.from.to_json(),
            "from_effective": FROM_EFFECTIVE,
            "value": self.value.to_json(),
            "state_override": self.state_override.to_json(),
            "gas": self.gas.to_json(),
            "stage": self.stage,
            "caller": self.caller,
            "identity_with_block": self.identity_with_block(),
            "identity_without_block": self.identity_without_block(),
        })
    }
}

// ---------------------------------------------------------------------------
// §3/§22: what the answer IS
// ---------------------------------------------------------------------------

/// §3's three hypotheses, refined against what the code actually does with each answer.
///
/// The task book's warning was that `eth_call result = state` must not be assumed. It is not
/// assumed here and it is not true here either, in a way the enumeration records: the same RPC
/// method is used for a state reading, for a derived protocol quote, and for a cost estimate
/// that is a function of bytes the caller supplied rather than of the market.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactClass {
    /// A read of canonical EVM state at a named block: the answer is whatever a storage slot or
    /// an account field holds there.
    CanonicalStateRead,
    /// A protocol contract's own computation over state — a quote, a re-pricing — whose answer
    /// is determined by state but is not a state cell.
    ProtocolDerivedQuote,
    /// A number the chain charges for the specific bytes this caller put in the calldata: an
    /// estimate of a future cost, not a market reading and not a canonical state value.
    FeeOracleEstimate,
    /// Asked only to decide something immediately, with no surviving holder for the answer.
    EphemeralProbe,
    /// This module cannot classify the ask, published as such.
    Unknown,
}

impl ArtifactClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            ArtifactClass::CanonicalStateRead => "CANONICAL_STATE_READ",
            ArtifactClass::ProtocolDerivedQuote => "PROTOCOL_DERIVED_RESULT",
            ArtifactClass::FeeOracleEstimate => "FEE_ORACLE_ESTIMATE",
            ArtifactClass::EphemeralProbe => "EPHEMERAL_PROBE",
            ArtifactClass::Unknown => "UNKNOWN",
        }
    }
}

/// Whether the block term is load-bearing for this selector's answer, and on what evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockDependency {
    /// Measured: the same `to` and the same calldata answered differently at two recorded
    /// heights. Nothing further needs to be proved — a reuse that ignores the block has been
    /// shown to return a different value.
    ProvenLoadBearing,
    /// The contract's code contains block-context opcodes, but this build cannot attribute them
    /// to the dispatched selector without a path analysis it does not run (§18). Not a claim of
    /// independence.
    NotAttributable,
    /// The ask carries no block term at all, so the question does not arise.
    NotApplicable,
}

impl BlockDependency {
    pub const fn as_str(self) -> &'static str {
        match self {
            BlockDependency::ProvenLoadBearing => "proven_load_bearing",
            BlockDependency::NotAttributable => "not_attributable",
            BlockDependency::NotApplicable => "not_applicable",
        }
    }
}

/// One selector this build asks, what function it is, and what its answer is.
///
/// §44 prefers the repository's own decoders and fixtures over a network lookup, and this table
/// is drawn from `evm_protocol::V2Call`'s generated encoders and the ABI of the OP-Stack fee
/// predeploy as the execution crate names it. §45's `UNKNOWN_SELECTOR` path exists beside it:
/// [`find_selector`] returns `None` rather than a plausible guess.
#[derive(Clone, Copy, Debug)]
pub struct SelectorFact {
    pub selector: &'static str,
    pub signature: &'static str,
    /// §22: the question in one line, not 「用于检测机会」.
    pub question: &'static str,
    pub returns: &'static str,
    pub artifact: ArtifactClass,
    pub state_dependency: &'static str,
    pub block_dependency: BlockDependency,
    pub anchors: &'static [Anchor],
}

/// The five selectors in the recorded corpus, by 4-byte id.
pub const SELECTORS: [SelectorFact; 5] = [
    SelectorFact {
        selector: "0dfe1681",
        signature: "token0()",
        question: "which token this pair calls slot 0",
        returns: "one address, in the low 20 bytes of a word",
        artifact: ArtifactClass::CanonicalStateRead,
        state_dependency: "the pair's two token slots, written once by initialize()",
        block_dependency: BlockDependency::NotAttributable,
        anchors: &[
            code(
                "crates/protocol/src/calls.rs",
                "fn encode",
                "the selector this build sends",
            ),
            code(
                "crates/execution/src/giwa/reads.rs",
                "V2Call::Token0",
                "the read that sends it",
            ),
        ],
    },
    SelectorFact {
        selector: "d21220a7",
        signature: "token1()",
        question: "which token this pair calls slot 1",
        returns: "one address, in the low 20 bytes of a word",
        artifact: ArtifactClass::CanonicalStateRead,
        state_dependency: "the pair's two token slots, written once by initialize()",
        block_dependency: BlockDependency::NotAttributable,
        anchors: &[code(
            "crates/execution/src/giwa/reads.rs",
            "V2Call::Token1",
            "the read that sends it",
        )],
    },
    SelectorFact {
        selector: "0902f1ac",
        signature: "getReserves()",
        question: "how much of each token the pool holds right now, and when it last synced",
        returns: "reserve0, reserve1, blockTimestampLast",
        artifact: ArtifactClass::CanonicalStateRead,
        state_dependency: "the pair's reserve0/reserve1/price0CumulativeLast storage, rewritten \
            by every mint, burn and swap",
        block_dependency: BlockDependency::ProvenLoadBearing,
        anchors: &[
            code(
                "crates/execution/src/giwa/reads.rs",
                "data: V2Call::GetReserves.encode(),",
                "the read that sends it",
            ),
            record(
                "data/evidence/m7/candidate-fee-measurement.json",
                "getReserves_blockTimestampLast",
                "the same pool at an earlier height, in this repository's own records",
            ),
        ],
    },
    SelectorFact {
        selector: "49948e0e",
        signature: "getL1Fee(bytes)",
        question: "what the chain will charge for exactly these transaction bytes",
        returns: "one uint256: the L1 data fee in wei",
        artifact: ArtifactClass::FeeOracleEstimate,
        state_dependency: "the predeploy's oracle parameters, plus the bytes the caller supplied",
        block_dependency: BlockDependency::NotAttributable,
        anchors: &[code(
            "crates/execution/src/giwa/reads.rs",
            "fn get_l1_fee_calldata",
            "the calldata this build assembles",
        )],
    },
    SelectorFact {
        selector: "70a08231",
        signature: "balanceOf(address)",
        question: "how many tokens one account holds",
        returns: "one uint256 balance",
        artifact: ArtifactClass::CanonicalStateRead,
        state_dependency: "one account's ERC-20 balance slot",
        block_dependency: BlockDependency::ProvenLoadBearing,
        anchors: &[code(
            "crates/execution/src/giwa/reads.rs",
            "V2Call::BalanceOf",
            "the read that sends it",
        )],
    },
];

pub fn find_selector(selector: &str) -> Option<&'static SelectorFact> {
    SELECTORS.iter().find(|fact| fact.selector == selector)
}

/// §45: an unrecognised selector is published, not guessed at.
pub const UNKNOWN_SELECTOR: &str = "UNKNOWN_SELECTOR";

// ---------------------------------------------------------------------------
// §5/§14/§23: the six call sites, with the code that makes each read
// ---------------------------------------------------------------------------

/// What holds the answer after the call returns, or says that nothing does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnershipForm {
    /// The response is decoded into a value that a named struct field keeps, and that field's
    /// lifetime is the answer's lifetime.
    HeldByStructField,
    /// The answer survives only as a derived projection — a couple of integers copied into a
    /// route leg — and the object it was read into is dropped.
    ProjectionOnly,
    /// Function-local: decoded, used in the same expression, dropped. No component in the build
    /// holds it across a stage boundary.
    EphemeralLocal,
}

impl OwnershipForm {
    pub const fn as_str(self) -> &'static str {
        match self {
            OwnershipForm::HeldByStructField => "held_by_struct_field",
            OwnershipForm::ProjectionOnly => "projection_only",
            OwnershipForm::EphemeralLocal => "ephemeral_local",
        }
    }
}

/// §23: whether an answer crosses the stage boundary at all, and in what shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CarrierForm {
    /// No field anywhere carries it.
    None,
    /// A field carries it, but only as a re-encoded projection of one part of it.
    DerivedProjection,
    /// A field carries the value itself.
    DirectValue,
}

impl CarrierForm {
    pub const fn as_str(self) -> &'static str {
        match self {
            CarrierForm::None => "NO_CARRIER",
            CarrierForm::DerivedProjection => "DERIVED_PROJECTION",
            CarrierForm::DirectValue => "DIRECT_VALUE",
        }
    }
}

/// One place in the build that makes an `eth_call`.
///
/// §5's requirement is that the chain be established from code rather than inferred from the
/// task book, so every field below names a function or a type a reader can open, and
/// [`CallSite::anchors`] carries the pointers the evidence gate resolves to line numbers.
#[derive(Clone, Copy, Debug)]
pub struct CallSite {
    pub id: &'static str,
    /// The stage the trace recorded, and the caller family M8.4.2's rules classify it by.
    pub stage: &'static str,
    pub caller_family: &'static str,
    pub selectors: &'static [&'static str],
    pub producer_fn: &'static str,
    pub producer_type: &'static str,
    /// The single place a call becomes bytes on the wire. §5 asks for it by name because it is
    /// where the identity's field set is decided — not by the caller's taste but by the type.
    pub rpc_boundary: &'static str,
    pub result_type: &'static str,
    pub consumer_fn: &'static str,
    pub consumer_type: &'static str,
    /// §22: what the answer is used for, in one line.
    pub purpose: &'static str,
    pub artifact: ArtifactClass,
    pub ownership: OwnershipForm,
    pub owner: &'static str,
    pub carrier: CarrierForm,
    pub carrier_fields: &'static str,
    /// §18's decisive question for a reuse decision: does this consumer's job consist of making
    /// the second read, so that a reused answer would leave the check with nothing to check?
    pub read_is_the_check: bool,
    pub block_dependency: BlockDependency,
    pub freshness: ProofStatus,
    pub invalidation: ProofStatus,
    pub authority: ProofStatus,
    pub consumer_verification: ProofStatus,
    pub determinism: ProofStatus,
    pub anchors: &'static [Anchor],
}

pub const ETH_CALL_SITES: [CallSite; 6] = [
    CallSite {
        id: "detection.tokens",
        stage: "opportunity_detection",
        caller_family: "reserves",
        selectors: &["0dfe1681", "d21220a7"],
        producer_fn: "read_address",
        producer_type: "free fn in evm_execution::giwa::reads",
        rpc_boundary: "HttpChainAdapter::call → eth_call with a hex height",
        result_type: "alloy Bytes, decoded to Address",
        consumer_fn: "price_legs → PricedRoute::new",
        consumer_type: "free fn in evm_pipeline::arbitrage",
        purpose: "decide which reserve belongs to which token, so a leg can be oriented",
        artifact: ArtifactClass::CanonicalStateRead,
        ownership: OwnershipForm::ProjectionOnly,
        owner: "no component holds the answer; Address values are copied into RouteLeg",
        carrier: CarrierForm::DerivedProjection,
        carrier_fields: "RouteLeg.token_in / token_out (TokenId, one address each)",
        read_is_the_check: true,
        block_dependency: BlockDependency::NotAttributable,
        freshness: ProofStatus::Unknown,
        invalidation: ProofStatus::Unknown,
        authority: ProofStatus::Proven,
        consumer_verification: ProofStatus::Proven,
        determinism: ProofStatus::PartiallyProven,
        anchors: &[
            code(
                "crates/execution/src/giwa/reads.rs",
                "fn read_address",
                "the producer",
            ),
            code(
                "crates/pipeline/src/arbitrage.rs",
                "async fn price_legs",
                "the consumer",
            ),
            code(
                "crates/execution/src/giwa/reads.rs",
                "pub fn in_out",
                "the guard that re-derives orientation from a fresh read and errors if the \
                 pair does not hold the route's tokens",
            ),
        ],
    },
    CallSite {
        id: "detection.reserves",
        stage: "opportunity_detection",
        caller_family: "reserves",
        selectors: &["0902f1ac"],
        producer_fn: "read_pool",
        producer_type: "free fn in evm_execution::giwa::reads",
        rpc_boundary: "HttpChainAdapter::call → eth_call with a hex height",
        result_type: "PoolState { token0, token1, reserve0, reserve1, last_synced, \
            read_at_block, source }",
        consumer_fn: "price_legs → Venue::quote",
        consumer_type: "free fn in evm_pipeline::arbitrage",
        purpose: "price both legs of the pair at the pinned height, and decide which venue is \
            the buy",
        artifact: ArtifactClass::ProtocolDerivedQuote,
        ownership: OwnershipForm::ProjectionOnly,
        owner: "PoolState is dropped at the end of price_legs; only the quoted integers survive",
        carrier: CarrierForm::DerivedProjection,
        carrier_fields: "RouteLeg.reserve_in / reserve_out (U256) + PricedRoute.block_number",
        read_is_the_check: true,
        block_dependency: BlockDependency::ProvenLoadBearing,
        freshness: ProofStatus::Unknown,
        invalidation: ProofStatus::Unknown,
        authority: ProofStatus::Proven,
        consumer_verification: ProofStatus::Proven,
        determinism: ProofStatus::PartiallyProven,
        anchors: &[
            code(
                "crates/execution/src/giwa/reads.rs",
                "pub async fn read_pool",
                "the producer",
            ),
            code(
                "crates/execution/src/giwa/reads.rs",
                "pub struct PoolState",
                "the answer type",
            ),
            code(
                "crates/simulation/src/route.rs",
                "pub reserve_in: U256",
                "the projection that survives into the route",
            ),
            code(
                "crates/pipeline/src/arbitrage.rs",
                "\"getReserves_blockTimestampLast\": self.last_synced.to_string(),",
                "the writer that publishes each leg's answer into that run's record: every run in \
                    the corpus goes through this line, so the anchor names the code rather than \
                    one run's directory",
            ),
        ],
    },
    CallSite {
        id: "preflight.tokens",
        stage: "preflight",
        caller_family: "reserves",
        selectors: &["0dfe1681", "d21220a7"],
        producer_fn: "read_leg → read_address",
        producer_type: "method on LivePreflightReads, evm_execution::giwa::preflight_facts",
        rpc_boundary: "HttpChainAdapter::call → eth_call with a hex height",
        result_type: "alloy Bytes, decoded to Address",
        consumer_fn: "PoolState::in_out",
        consumer_type: "impl on evm_execution::giwa::reads::PoolState",
        purpose: "order the head's reserves for the leg the route actually moves through",
        artifact: ArtifactClass::CanonicalStateRead,
        ownership: OwnershipForm::EphemeralLocal,
        owner: "the gathered facts' local PoolState, dropped when reserve_reading returns",
        carrier: CarrierForm::None,
        carrier_fields: "nothing: the addresses are used to pick which reserve is in and out",
        read_is_the_check: true,
        block_dependency: BlockDependency::NotAttributable,
        freshness: ProofStatus::NotApplicable,
        invalidation: ProofStatus::Unknown,
        authority: ProofStatus::Proven,
        consumer_verification: ProofStatus::Proven,
        determinism: ProofStatus::PartiallyProven,
        anchors: &[
            code(
                "crates/execution/src/giwa/preflight_facts.rs",
                "async fn read_leg",
                "the producer, at the head this gather read",
            ),
            code(
                "crates/execution/src/giwa/reads.rs",
                "which is not the pair this route moves through",
                "the guard that consumes them",
            ),
        ],
    },
    CallSite {
        id: "preflight.reserves",
        stage: "preflight",
        caller_family: "reserves",
        selectors: &["0902f1ac"],
        producer_fn: "read_leg → read_pool",
        producer_type: "method on LivePreflightReads, evm_execution::giwa::preflight_facts",
        rpc_boundary: "HttpChainAdapter::call → eth_call with a hex height",
        result_type: "PoolState, projected into ReserveReading",
        consumer_fn: "ExecutionPreflight::run",
        consumer_type: "evm_execution::preflight",
        purpose: "re-price the same route on the head's reserves and reject the attempt if the \
            opportunity no longer clears the floor",
        artifact: ArtifactClass::CanonicalStateRead,
        ownership: OwnershipForm::HeldByStructField,
        owner: "PreflightFacts.reserves: [ReserveReading; 2], for the length of one gate run",
        carrier: CarrierForm::DirectValue,
        carrier_fields: "ReserveReading.current_reserve_in / current_reserve_out (Option<U256>)",
        read_is_the_check: true,
        block_dependency: BlockDependency::ProvenLoadBearing,
        freshness: ProofStatus::Proven,
        invalidation: ProofStatus::NotApplicable,
        authority: ProofStatus::Proven,
        consumer_verification: ProofStatus::Proven,
        determinism: ProofStatus::Proven,
        anchors: &[
            code(
                "crates/execution/src/preflight.rs",
                "pub struct ReserveReading",
                "both halves of the comparison, kept side by side on purpose",
            ),
            code(
                "crates/execution/src/preflight.rs",
                "verdict cannot be formed from",
                "the refusal when the head's answer is missing",
            ),
            test(
                "crates/execution/src/preflight.rs",
                "an_unread_reserve_blocks_instead_of_falling_back_to_the_priced_numbers",
                "a regression that forbids the reuse this milestone was asked to assess",
            ),
            code(
                "crates/execution/src/giwa/preflight_facts.rs",
                "head at latest",
                "the head this read is pinned to",
            ),
        ],
    },
    CallSite {
        id: "preflight.l1_fee",
        stage: "preflight",
        caller_family: "cost ceiling",
        selectors: &["49948e0e"],
        producer_fn: "estimate_l1_fee",
        producer_type: "free fn in evm_execution::giwa::reads",
        rpc_boundary: "HttpChainAdapter::call → eth_call with a hex height",
        result_type: "L1FeeSource::OracleEstimate { amount, block_number, read_by }",
        consumer_fn: "price_steps → EstimatedCost::new",
        consumer_type: "evm_execution::cost",
        purpose: "put the L1 data cost of the exact pre-signing envelope into each step's ceiling",
        artifact: ArtifactClass::FeeOracleEstimate,
        ownership: OwnershipForm::HeldByStructField,
        owner: "StepPricing.cost, for the length of the gather and the report built from it",
        carrier: CarrierForm::DirectValue,
        carrier_fields: "L1FeeSource::OracleEstimate.amount + block_number, read_by provenance",
        read_is_the_check: false,
        block_dependency: BlockDependency::NotAttributable,
        freshness: ProofStatus::PartiallyProven,
        invalidation: ProofStatus::Unknown,
        authority: ProofStatus::Proven,
        consumer_verification: ProofStatus::PartiallyProven,
        determinism: ProofStatus::PartiallyProven,
        anchors: &[
            code(
                "crates/execution/src/giwa/reads.rs",
                "pub async fn estimate_l1_fee",
                "producer",
            ),
            code(
                "crates/execution/src/cost.rs",
                "Unreadable { reason: String }",
                "a failed read carries an absence rather than a zero",
            ),
        ],
    },
    CallSite {
        id: "build.asset_snapshot",
        stage: "build",
        caller_family: "before-snapshot",
        selectors: &["70a08231"],
        producer_fn: "GiwaAssetReader::token_balance",
        producer_type: "impl AssetReader for evm_execution::giwa::reads::GiwaAssetReader",
        rpc_boundary: "HttpChainAdapter::call → eth_call with a hex height",
        result_type: "AssetReading { amount, source }",
        consumer_fn: "snapshot_assets → AssetSnapshot.token_balances",
        consumer_type: "evm_execution::profit",
        purpose: "state what the wallet holds before step 0, so the six executions afterwards \
            can be audited against a number that was not remembered",
        artifact: ArtifactClass::CanonicalStateRead,
        ownership: OwnershipForm::HeldByStructField,
        owner: "AssetSnapshot, one per sequence, paired with its after-snapshot",
        carrier: CarrierForm::DirectValue,
        carrier_fields: "AssetSnapshot.token_balances: BTreeMap<Address, U256>",
        read_is_the_check: false,
        block_dependency: BlockDependency::ProvenLoadBearing,
        freshness: ProofStatus::Proven,
        invalidation: ProofStatus::NotApplicable,
        authority: ProofStatus::Proven,
        consumer_verification: ProofStatus::Proven,
        determinism: ProofStatus::Proven,
        anchors: &[
            code(
                "crates/execution/src/giwa/reads.rs",
                "pub async fn token_balance",
                "the producer",
            ),
            code(
                "crates/execution/src/sequence.rs",
                "let mut token_balances",
                "the collector",
            ),
            code(
                "crates/execution/src/profit.rs",
                "pub token_balances: BTreeMap",
                "the holder",
            ),
        ],
    },
];

pub fn call_sites() -> &'static [CallSite] {
    &ETH_CALL_SITES
}

/// The single RPC boundary every one of these asks passes through, named once because §5 asks
/// for the boundary as a fact rather than as six descriptions.
pub const RPC_BOUNDARY_FILE: &str = "crates/chain/src/rpc.rs";
pub const RPC_BOUNDARY_TOKEN: &str = "\"eth_call\",";
pub const REQUEST_TYPE_FILE: &str = "crates/chain/src/types.rs";
pub const REQUEST_TYPE_TOKEN: &str = "pub struct CallRequest";

// ---------------------------------------------------------------------------
// §15: the lifecycle, one cell at a time
// ---------------------------------------------------------------------------

/// M8.4.3's five steps, reused verbatim so the two milestones' tables can be read against each
/// other.
pub const LIFECYCLE_STEPS: [&str; 5] = [
    "acquired",
    "validated",
    "published",
    "consumed",
    "invalidated_or_expired",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LifecycleCell {
    pub site: &'static str,
    pub step: &'static str,
    pub status: ProofStatus,
    pub where_it_lives: &'static str,
    pub note: &'static str,
}

/// One row per site per step: where the answer exists at that moment, in the code's own words.
///
/// A step that does not exist is `not_implemented` in the note rather than quietly `unknown`:
/// §15 requires the distinction, and for these artifacts the last step genuinely has no
/// mechanism — nothing invalidates an answer, because nothing keeps one.
pub fn lifecycle_rows() -> Vec<LifecycleCell> {
    let mut rows = Vec::with_capacity(ETH_CALL_SITES.len() * LIFECYCLE_STEPS.len());
    for site in &ETH_CALL_SITES {
        for (index, step) in LIFECYCLE_STEPS.iter().enumerate() {
            let (status, lives, note) = lifecycle_cell(site, index);
            rows.push(LifecycleCell {
                site: site.id,
                step,
                status,
                where_it_lives: lives,
                note,
            });
        }
    }
    rows
}

fn lifecycle_cell(site: &CallSite, step: usize) -> (ProofStatus, &'static str, &'static str) {
    let acquired = (
        ProofStatus::Proven,
        site.rpc_boundary,
        "bytes off the wire, parsed and returned as the call's only product",
    );
    let validated = (
        ProofStatus::PartiallyProven,
        site.result_type,
        "the ABI word is decoded and a wrong shape is an error; nothing checks that the state \
         behind the answer is the state anyone expected",
    );
    let published = match site.carrier {
        CarrierForm::None => (
            ProofStatus::NotApplicable,
            "nowhere",
            "NOT_IMPLEMENTED: no field holds this answer, so there is no publication step to \
             locate",
        ),
        CarrierForm::DerivedProjection => (
            ProofStatus::PartiallyProven,
            site.carrier_fields,
            "only a projection of the answer is kept, and the projection does not carry the \
             block it was read at as part of its own value",
        ),
        CarrierForm::DirectValue => (
            ProofStatus::Proven,
            site.carrier_fields,
            "the value is a named field of a named struct",
        ),
    };
    let consumed = (ProofStatus::Proven, site.consumer_fn, site.purpose);
    let invalidated = match site.invalidation {
        ProofStatus::NotApplicable => (
            ProofStatus::NotApplicable,
            "no holder to invalidate",
            "NOT_IMPLEMENTED as a rule, by design: a one-use reading has no lifetime to end",
        ),
        ProofStatus::Unknown => (
            ProofStatus::Unknown,
            "not in this build",
            "nothing in the repository expires or invalidates an eth_call answer: a new block is \
             not observed by the artifact, because no component owns it",
        ),
        other => (
            other,
            "not in this build",
            "no invalidation rule is stated anywhere for this reading",
        ),
    };
    [acquired, validated, published, consumed, invalidated][step]
}

// ---------------------------------------------------------------------------
// §26/§27/§28: the reuse verdict
// ---------------------------------------------------------------------------

/// §26's eight axes, one per field so a `false` verdict always names which axis failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Ownership,
    Identity,
    Freshness,
    Invalidation,
    Authority,
    ConsumerVerification,
    Determinism,
    Carrier,
}

impl Axis {
    pub const fn as_str(self) -> &'static str {
        match self {
            Axis::Ownership => "ownership",
            Axis::Identity => "identity",
            Axis::Freshness => "freshness",
            Axis::Invalidation => "invalidation",
            Axis::Authority => "authority",
            Axis::ConsumerVerification => "consumer_verification",
            Axis::Determinism => "determinism",
            Axis::Carrier => "carrier",
        }
    }
}

pub const ALL_AXES: [Axis; 8] = [
    Axis::Ownership,
    Axis::Identity,
    Axis::Freshness,
    Axis::Invalidation,
    Axis::Authority,
    Axis::ConsumerVerification,
    Axis::Determinism,
    Axis::Carrier,
];

/// §55's blocker vocabulary, plus the two this milestone measured rather than assumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReuseBlocker {
    NoCarrier,
    OwnerNotEstablished,
    FreshnessRuleNotProven,
    InvalidationNotEstablished,
    BlockDependencyUnproven,
    ResultNotRecorded,
    ConsumerCannotVerify,
    LatestTag,
    /// Measured, not assumed: the same `to` and the same calldata answered differently at two
    /// heights this repository has records for, so ignoring the block term changes the answer.
    BlockChangesTheAnswer,
    /// The consumer's check is the second read. Reusing the first answer leaves the gate
    /// comparing a value with a copy of itself.
    ReadIsTheCheck,
}

impl ReuseBlocker {
    pub const fn as_str(self) -> &'static str {
        match self {
            ReuseBlocker::NoCarrier => "NO_CARRIER",
            ReuseBlocker::OwnerNotEstablished => "OWNER_NOT_ESTABLISHED",
            ReuseBlocker::FreshnessRuleNotProven => "FRESHNESS_RULE_NOT_PROVEN",
            ReuseBlocker::InvalidationNotEstablished => "INVALIDATION_NOT_ESTABLISHED",
            ReuseBlocker::BlockDependencyUnproven => "BLOCK_DEPENDENCY_UNPROVEN",
            ReuseBlocker::ResultNotRecorded => "RESULT_NOT_RECORDED",
            ReuseBlocker::ConsumerCannotVerify => "CONSUMER_CANNOT_VERIFY",
            ReuseBlocker::LatestTag => "BLOCK_TAG_LATEST",
            ReuseBlocker::BlockChangesTheAnswer => "block_changes_the_answer",
            ReuseBlocker::ReadIsTheCheck => "read_is_the_check",
        }
    }
}

/// §28's candidate classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReuseClass {
    ReuseReady,
    PropagationPossible,
    ReuseBlockedByIdentity,
    ReuseBlockedByFreshness,
    ReuseBlockedByOwnership,
    ReuseBlockedByInvalidation,
    ReuseBlockedByVerification,
    ReuseBlockedByDeterminism,
    ReuseBlockedByCarrier,
    Unknown,
}

impl ReuseClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            ReuseClass::ReuseReady => "REUSE_READY",
            ReuseClass::PropagationPossible => "PROPAGATION_POSSIBLE",
            ReuseClass::ReuseBlockedByIdentity => "REUSE_BLOCKED_BY_IDENTITY",
            ReuseClass::ReuseBlockedByFreshness => "REUSE_BLOCKED_BY_FRESHNESS",
            ReuseClass::ReuseBlockedByOwnership => "REUSE_BLOCKED_BY_OWNERSHIP",
            ReuseClass::ReuseBlockedByInvalidation => "REUSE_BLOCKED_BY_INVALIDATION",
            ReuseClass::ReuseBlockedByVerification => "REUSE_BLOCKED_BY_VERIFICATION",
            ReuseClass::ReuseBlockedByDeterminism => "REUSE_BLOCKED_BY_DETERMINISM",
            ReuseClass::ReuseBlockedByCarrier => "REUSE_BLOCKED_BY_CARRIER",
            ReuseClass::Unknown => "UNKNOWN",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AxisRow {
    pub axis: Axis,
    pub status: ProofStatus,
    pub note: &'static str,
}

/// One site's verdict: the eight axes, two separate answers for 「能不能」 and 「现在能不能」,
/// and the blockers named in the order §26 requires them all to be cleared.
#[derive(Clone, Debug)]
pub struct SiteVerdict {
    pub site: &'static str,
    pub axes: Vec<AxisRow>,
    pub safe_to_reuse_now: bool,
    pub reusable_in_principle: bool,
    pub class: ReuseClass,
    pub blockers: Vec<ReuseBlocker>,
    pub reason: &'static str,
    pub what_would_be_required: &'static str,
}

impl SiteVerdict {
    fn axis(&self, axis: Axis) -> ProofStatus {
        self.axes
            .iter()
            .find(|row| row.axis == axis)
            .map(|row| row.status)
            .unwrap_or(ProofStatus::Unknown)
    }

    /// §26's rule, stated once as a function so no table writer can forget an axis: every axis
    /// must be `proven` and nothing else. A `partially_proven` carrier is not a carrier.
    pub fn safe(&self) -> bool {
        ALL_AXES
            .iter()
            .all(|axis| self.axis(*axis).supports_positive_claim())
    }

    pub fn to_json(&self) -> Value {
        json!({
            "site": self.site,
            "axes": self.axes.iter().map(|row| json!({
                "axis": row.axis.as_str(),
                "status": row.status.as_str(),
                "note": row.note,
            })).collect::<Vec<_>>(),
            "safe_to_reuse_now": self.safe(),
            "reusable_in_principle": self.reusable_in_principle,
            "class": self.class.as_str(),
            "blockers": self.blockers.iter().map(|b| b.as_str()).collect::<Vec<_>>(),
            "reason": self.reason,
            "what_would_be_required_to_unblock": self.what_would_be_required,
        })
    }
}

/// §27: `reusable_in_principle` is decided on the artifact's nature, `safe_to_reuse_now` on the
/// eight axes, and the two are never allowed to be the same field.
pub fn verdict_for(site: &CallSite) -> SiteVerdict {
    let ownership = match site.ownership {
        OwnershipForm::HeldByStructField => ProofStatus::Proven,
        OwnershipForm::ProjectionOnly => ProofStatus::PartiallyProven,
        OwnershipForm::EphemeralLocal => ProofStatus::Unknown,
    };
    // Identity is proven in the sense that the four terms exist and are recorded for every ask;
    // it is not proven that those four terms *are* the identity of the answer, which is a
    // separate axis (§17's determinism) and is why identity here is not simply `proven`.
    let identity = match site.block_dependency {
        BlockDependency::ProvenLoadBearing => ProofStatus::Proven,
        BlockDependency::NotAttributable => ProofStatus::PartiallyProven,
        BlockDependency::NotApplicable => ProofStatus::PartiallyProven,
    };
    let carrier = match site.carrier {
        CarrierForm::None => ProofStatus::Unknown,
        CarrierForm::DerivedProjection => ProofStatus::PartiallyProven,
        CarrierForm::DirectValue => ProofStatus::Proven,
    };
    let verification = if site.read_is_the_check {
        ProofStatus::PartiallyProven
    } else {
        site.consumer_verification
    };
    let axes = vec![
        AxisRow {
            axis: Axis::Ownership,
            status: ownership,
            note: site.owner,
        },
        AxisRow {
            axis: Axis::Identity,
            status: identity,
            note: "chain, block, to and calldata are all recorded; whether block belongs in the \
                identity is decided by determinism, not by this axis",
        },
        AxisRow {
            axis: Axis::Freshness,
            status: site.freshness,
            note: "a validity window stated somewhere in code, or an explicit no-window rule",
        },
        AxisRow {
            axis: Axis::Invalidation,
            status: site.invalidation,
            note: "what makes the answer stale, and where that is enforced",
        },
        AxisRow {
            axis: Axis::Authority,
            status: site.authority,
            note: "the endpoint's answer at a named height is canonical state for that height; \
                the endpoint fingerprint is on every record",
        },
        AxisRow {
            axis: Axis::ConsumerVerification,
            status: verification,
            note: if site.read_is_the_check {
                "the consumer could check the value, but only by making this reading again — \
                    which is the reading reuse was supposed to save"
            } else {
                "the consumer has an independent check it can run on a handed-over value"
            },
        },
        AxisRow {
            axis: Axis::Determinism,
            status: site.determinism,
            note: "same canonical block plus same identity ⇒ same answer, as far as this build \
                can show",
        },
        AxisRow {
            axis: Axis::Carrier,
            status: carrier,
            note: site.carrier_fields,
        },
    ];
    let mut verdict = SiteVerdict {
        site: site.id,
        axes,
        safe_to_reuse_now: false,
        reusable_in_principle: false,
        class: ReuseClass::Unknown,
        blockers: Vec::new(),
        reason: "",
        what_would_be_required: "",
    };
    let mut blockers: Vec<ReuseBlocker> = Vec::new();
    if site.read_is_the_check {
        blockers.push(ReuseBlocker::ReadIsTheCheck);
    }
    if site.block_dependency == BlockDependency::ProvenLoadBearing {
        blockers.push(ReuseBlocker::BlockChangesTheAnswer);
    }
    if site.block_dependency == BlockDependency::NotAttributable {
        blockers.push(ReuseBlocker::BlockDependencyUnproven);
    }
    match site.carrier {
        CarrierForm::None => blockers.push(ReuseBlocker::NoCarrier),
        CarrierForm::DerivedProjection => blockers.push(ReuseBlocker::ResultNotRecorded),
        CarrierForm::DirectValue => {}
    }
    if ownership != ProofStatus::Proven {
        blockers.push(ReuseBlocker::OwnerNotEstablished);
    }
    if site.freshness != ProofStatus::Proven {
        blockers.push(ReuseBlocker::FreshnessRuleNotProven);
    }
    if site.invalidation != ProofStatus::Proven && site.invalidation != ProofStatus::NotApplicable {
        blockers.push(ReuseBlocker::InvalidationNotEstablished);
    }
    if !blockers.contains(&ReuseBlocker::ReadIsTheCheck) && verification != ProofStatus::Proven {
        blockers.push(ReuseBlocker::ConsumerCannotVerify);
    }
    verdict.blockers = blockers;
    verdict
}

/// Every site, judged. §29 forbids a reuse experiment in this milestone; a table of verdicts is
/// not an experiment and changes no behavior.
pub fn verdicts() -> Vec<SiteVerdict> {
    ETH_CALL_SITES
        .iter()
        .map(|site| {
            let mut verdict = verdict_for(site);
            verdict.safe_to_reuse_now = verdict.safe();
            verdict.reusable_in_principle = reusable_in_principle(site);
            verdict.class = classify(site, &verdict.blockers);
            verdict.reason = reason_for(site);
            verdict.what_would_be_required = unblock_for(site);
            verdict
        })
        .collect()
}

/// §27's separate question: is the artifact of a kind that *could* be handed over, assuming the
/// plumbing existed. A reading whose purpose is to be fresh is not such a thing, at any price in
/// fields and carriers.
fn reusable_in_principle(site: &CallSite) -> bool {
    !site.read_is_the_check
        && site.block_dependency != BlockDependency::ProvenLoadBearing
        && site.ownership == OwnershipForm::HeldByStructField
}

fn classify(site: &CallSite, blockers: &[ReuseBlocker]) -> ReuseClass {
    if blockers.contains(&ReuseBlocker::ReadIsTheCheck) {
        return ReuseClass::ReuseBlockedByVerification;
    }
    if blockers.contains(&ReuseBlocker::BlockChangesTheAnswer) {
        return ReuseClass::ReuseBlockedByIdentity;
    }
    if site.ownership == OwnershipForm::EphemeralLocal {
        return ReuseClass::ReuseBlockedByOwnership;
    }
    if blockers.contains(&ReuseBlocker::NoCarrier) {
        return ReuseClass::ReuseBlockedByCarrier;
    }
    if blockers.contains(&ReuseBlocker::BlockDependencyUnproven) {
        return ReuseClass::ReuseBlockedByDeterminism;
    }
    if blockers.contains(&ReuseBlocker::FreshnessRuleNotProven) {
        return ReuseClass::ReuseBlockedByFreshness;
    }
    if blockers.contains(&ReuseBlocker::InvalidationNotEstablished) {
        return ReuseClass::ReuseBlockedByInvalidation;
    }
    if reusable_in_principle(site) {
        return ReuseClass::PropagationPossible;
    }
    ReuseClass::Unknown
}

fn reason_for(site: &CallSite) -> &'static str {
    match site.id {
        "preflight.reserves" => {
            "the head's reserves are what this stage is for: §27's verdict is the difference \
             between the reserves the finding priced and the reserves the chain holds now, and a \
             handed-over copy of the finding's own numbers cannot express a difference"
        }
        "preflight.tokens" => {
            "the addresses order the reserves the head holds; reused, the guard at in_out() \
             would compare detection's orientation against detection's orientation"
        }
        "detection.tokens" => {
            "the answers survive only as token ids inside the route, and the stage that consumes \
             them re-reads them at a different height by design"
        }
        "detection.reserves" => {
            "a state reading the run quotes immediately, kept as four integers with no field for \
             the block they were read at, so the surviving projection cannot be defended as an \
             answer about any particular state"
        }
        "preflight.l1_fee" => {
            "an estimate about the bytes this caller built, not a market reading; it already \
             carries its block number and its own read_by provenance, and it is the one artifact \
             here a consumer could check without re-asking"
        }
        _ => {
            "a balance read against one account at one height, already a named field of a named \
             snapshot, and never asked twice for the same account in the recorded corpus"
        }
    }
}

fn unblock_for(site: &CallSite) -> &'static str {
    if site.read_is_the_check {
        return "nothing short of a different gate: the consumer would have to verify staleness \
                by a means other than re-reading, which would be a new check rather than an \
                optimization of this one";
    }
    match site.block_dependency {
        BlockDependency::ProvenLoadBearing => {
            "an invalidation rule that observes the state moving — the pair's Sync and Swap logs \
             are the mechanism this chain offers, and no code in this build subscribes to them \
             for the purpose of expiring a reading"
        }
        BlockDependency::NotAttributable => {
            "a path analysis showing the dispatched selector reads no block-context opcode, or \
             a recorded equality at two heights that a swap was known to have happened between"
        }
        BlockDependency::NotApplicable => "a named owner and a stated validity window",
    }
}

// ---------------------------------------------------------------------------
// §40: the negative controls, as data
// ---------------------------------------------------------------------------

/// §40's NC1–NC6, expressed as the field a reader would mutate and the term that must move.
///
/// The gate runs these against a real parsed identity rather than against this table, so a
/// change to the identity builder that stops noticing a field is caught here rather than
/// asserted here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NegativeControl {
    pub id: &'static str,
    pub field: &'static str,
    pub participates_in_identity: bool,
    pub why: &'static str,
}

pub const NEGATIVE_CONTROLS: [NegativeControl; 6] = [
    NegativeControl {
        id: "NC1",
        field: "to",
        participates_in_identity: true,
        why: "a different contract answers a different question",
    },
    NegativeControl {
        id: "NC2",
        field: "calldata",
        participates_in_identity: true,
        why: "the selector and its arguments are the question",
    },
    NegativeControl {
        id: "NC3",
        field: "block",
        participates_in_identity: true,
        why: "measured load-bearing for getReserves and balanceOf in this corpus",
    },
    NegativeControl {
        id: "NC4",
        field: "from",
        participates_in_identity: false,
        why: "this build cannot send it, so no two asks here can differ by sender; §8 still \
              refuses to delete it from the theoretical identity, which is why the field is \
              recorded as an absence with a proof rather than left out",
    },
    NegativeControl {
        id: "NC5",
        field: "value",
        participates_in_identity: false,
        why: "the request type has no such field; the recorded key's optional value tail is \
              never emitted by this build",
    },
    NegativeControl {
        id: "NC6",
        field: "state_override",
        participates_in_identity: false,
        why: "the params array is two elements, so there is no override object; §10 asked for \
              code evidence rather than a sentence, and this is the sentence with the code \
              beside it",
    },
];

// ---------------------------------------------------------------------------
// §17/§18: block-context opcodes, as far as a whole-code scan can show
// ---------------------------------------------------------------------------

/// The opcodes whose answer depends on which block execution is happening in.
pub const BLOCK_CONTEXT_OPCODES: [(u8, &str); 11] = [
    (0x40, "BLOCKHASH"),
    (0x41, "COINBASE"),
    (0x42, "TIMESTAMP"),
    (0x43, "NUMBER"),
    (0x44, "PREVRANDAO"),
    (0x45, "GASLIMIT"),
    (0x46, "CHAINID"),
    (0x47, "SELFBALANCE"),
    (0x48, "BASEFEE"),
    (0x49, "BLOBHASH"),
    (0x4a, "BLOBBASEFEE"),
];

/// The state-reading opcodes, counted beside them so §19's dependency set is not described as
/// empty merely because it was not enumerated slot by slot.
pub const STATE_OPCODES: [(u8, &str); 7] = [
    (0x30, "ADDRESS"),
    (0x31, "BALANCE"),
    (0x3b, "EXTCODESIZE"),
    (0x3c, "EXTCODECOPY"),
    (0x3f, "EXTCODEHASH"),
    (0x54, "SLOAD"),
    (0x55, "SSTORE"),
];

/// A PUSH-aware linear scan of a contract's code.
///
/// The PUSH immediate skipping is what makes the counts mean anything: without it, a `PUSH1
/// 0x42` is read as a TIMESTAMP and every contract looks block-dependent. §43 sets the bound
/// this respects — enough to classify reuse safety, not a symbolic executor — and states the
/// remaining limit plainly: this walk enumerates what the contract *can* execute, not what the
/// dispatched selector does. So an opcode's absence here proves independence, and its presence
/// proves nothing yet.
pub fn opcode_inventory(code: &[u8]) -> BTreeMap<String, usize> {
    let mut tally: BTreeMap<String, usize> = BTreeMap::new();
    let named = |opcode: u8| -> Option<String> {
        BLOCK_CONTEXT_OPCODES
            .iter()
            .find(|(byte, _)| *byte == opcode)
            .map(|(_, name)| format!("block:{name}"))
            .or_else(|| {
                STATE_OPCODES
                    .iter()
                    .find(|(byte, _)| *byte == opcode)
                    .map(|(_, name)| format!("state:{name}"))
            })
    };
    let mut index = 0;
    while index < code.len() {
        let opcode = code[index];
        if (0x60..=0x7f).contains(&opcode) {
            // PUSH1..PUSH32: the immediate bytes follow and are not instructions.
            index += 1 + (opcode as usize - 0x60 + 1);
            continue;
        }
        if let Some(name) = named(opcode) {
            *tally.entry(name).or_insert(0) += 1;
        }
        index += 1;
    }
    tally
}

/// Whether a contract's code can read block context at all, as a first cut at §17's question.
/// `false` is a proof of independence; `true` is the `NotAttributable` state, not a refutation.
pub fn block_context_reachable_in_code(code: &[u8]) -> bool {
    opcode_inventory(code)
        .keys()
        .any(|name| name.starts_with("block:"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonicalization::RpcReadKey;
    use crate::state_ownership::SourceKind;
    use serde_json::json;

    fn key(row: &Value) -> RpcReadKey {
        RpcReadKey::of_row(row, Some(91_342))
    }

    fn row(selector_data: &str, to: &str, block: &str, stage: &str) -> Value {
        json!({
            "method": "eth_call",
            "dedup_key": format!("call|91342|{block}|{to}|{selector_data}"),
            "block_tag": block,
            "target": to,
            "stage": stage,
            "caller": "reserves of both venues, at the pin",
            "run": "run-under-test",
            "rpc_id": 1,
            "logical_request_id": "run-under-test:lifecycle:rpc1",
            "endpoint_id": "rpc-faa716cada04a9ef",
            "sink": "lifecycle",
        })
    }

    const POOL: &str = "0x2a3ceafba30f6626170cbb0cd67392efb94bd9a4";
    const OTHER_POOL: &str = "0x5b3c1e3fb6a97c0130ae015ff10f53a1a30c353e";

    fn identity(data: &str, block: &str, to: &str) -> EthCallIdentity {
        EthCallIdentity::of_row(&key(&row(data, to, block, "opportunity_detection")))
            .expect("a keyed eth_call row has an identity")
    }

    #[test]
    fn nc1_to_and_nc2_calldata_and_nc3_block_each_move_the_identity() {
        // §40's three controls, each against the same base and each changing exactly one term.
        let base = identity("0902f1ac", "37700740", POOL);
        let other_to = identity("0902f1ac", "37700740", OTHER_POOL);
        let other_data = identity("0dfe1681", "37700740", POOL);
        for (label, other) in [("NC1 to", &other_to), ("NC2 calldata", &other_data)] {
            assert_ne!(
                base.identity_with_block(),
                other.identity_with_block(),
                "changing {label} left the identity alone, so the identity is blind to it"
            );
            assert_ne!(
                base.identity_without_block(),
                other.identity_without_block(),
                "changing {label} moved only the block-bearing identity, so {label} is not in \
                 the key at all and a pair differing solely by {label} would be called a \
                 duplicate"
            );
        }
        // NC3 is the opposite shape: the block term is the one the block-free identity does not
        // see, which is what lets a cross-block pair be reported as a duplicate at all. §41's
        // control then asks what the node answers for that pair, and the record answers it.
        let other_block = identity("0902f1ac", "37700753", POOL);
        assert_ne!(
            base.identity_with_block(),
            other_block.identity_with_block(),
            "NC3: the block is part of the semantic identity, so two heights are two asks"
        );
        assert_eq!(
            base.identity_without_block(),
            other_block.identity_without_block(),
            "NC3: the block-free identity still names the same ask, so a cross-block pair is \
             comparable — which is the shape of the 18 pairs M8.4.2 counted"
        );
    }

    #[test]
    fn nc4_from_nc5_value_and_nc6_override_are_proven_absences_not_gaps() {
        let base = identity("0902f1ac", "37700740", POOL);
        for (field, evidence) in [
            ("from", &base.from),
            ("value", &base.value),
            ("state_override", &base.state_override),
            ("gas", &base.gas),
        ] {
            assert!(
                matches!(evidence, FieldEvidence::ProvenAbsent(_)),
                "{field} is {evidence:?}; §10 and §36 require a code-backed absence, not a null"
            );
            assert!(evidence.is_settled(), "{field} reads as uninvestigated");
            assert!(evidence.detail().is_some(), "{field} names no rule");
        }
        // A control that would have caught the lazy version: an unset state must not be
        // reportable as settled, so writing `Unknown` or `NotRecorded` here fails the test.
        assert!(!FieldEvidence::Unknown.is_settled());
        assert!(!FieldEvidence::NotRecorded("no params array in the record").is_settled());
        assert!(!FieldEvidence::NotApplicable("the method has no such term").is_settled());
    }

    #[test]
    fn a_recorded_value_would_be_carried_rather_than_erased() {
        // The key shape allows a value tail; this build never sends one, and the identity must
        // show it the moment a record does.
        let row = json!({
            "method": "eth_call",
            "dedup_key": format!("call|91342|37700740|{POOL}|0902f1ac|value|7"),
            "block_tag": "37700740",
            "target": POOL,
            "stage": "opportunity_detection",
            "caller": "reserves of both venues, at the pin",
            "run": "run-under-test",
            "rpc_id": 1,
            "logical_request_id": "run-under-test:lifecycle:rpc1",
            "sink": "lifecycle",
        });
        let identity = EthCallIdentity::of_row(&key(&row)).expect("the value tail parses");
        assert_eq!(identity.value, FieldEvidence::Recorded("7".to_string()));
        assert!(identity.identity_with_block().contains("37700740"),);
    }

    #[test]
    fn a_non_eth_call_row_is_refused_rather_than_emptied() {
        let row = json!({
            "method": "eth_getBalance",
            "dedup_key": format!("balance|91342|37700740|{POOL}"),
            "block_tag": "37700740",
            "target": POOL,
            "stage": "build",
            "caller": "before-snapshot: native and input-token balances",
            "sink": "lifecycle",
        });
        assert!(EthCallIdentity::of_row(&key(&row)).is_none());
    }

    #[test]
    fn every_recorded_selector_is_known_or_reported_as_unknown() {
        assert_eq!(
            find_selector("0902f1ac").map(|f| f.signature),
            Some("getReserves()")
        );
        assert_eq!(
            find_selector("49948e0e").map(|f| f.artifact),
            Some(ArtifactClass::FeeOracleEstimate)
        );
        assert!(find_selector("deadbeef").is_none());
        // §45: a caller that cannot name the selector publishes the flag rather than guessing.
        let odd = identity("deadbeef00", "37700740", POOL);
        assert_eq!(odd.selector(), "deadbeef");
        assert!(find_selector(odd.selector()).is_none());
    }

    #[test]
    fn get_reserves_is_the_one_flow_measured_to_answer_differently_at_two_heights() {
        // §41's key control, as a claim about the table rather than about a record: the class
        // that the control was run on is marked load-bearing, and the marking is not derived
        // from the pair being different blocks.
        let reserves = find_selector("0902f1ac").expect("known selector");
        assert_eq!(
            reserves.block_dependency,
            BlockDependency::ProvenLoadBearing
        );
        assert_eq!(reserves.artifact, ArtifactClass::CanonicalStateRead);
        let tokens = find_selector("0dfe1681").expect("known selector");
        assert_eq!(tokens.block_dependency, BlockDependency::NotAttributable);
        assert_ne!(
            reserves.block_dependency.as_str(),
            tokens.block_dependency.as_str(),
            "the table says both are the same kind of read of the same contract, which the \
             control measured to be false for one of them"
        );
    }

    #[test]
    fn no_site_is_safe_to_reuse_now_and_every_false_names_a_blocker() {
        let verdicts = verdicts();
        assert_eq!(verdicts.len(), ETH_CALL_SITES.len());
        for verdict in &verdicts {
            assert!(
                !verdict.safe_to_reuse_now,
                "{} came out safe, which §58 does not require and the axes do not support",
                verdict.site
            );
            assert!(
                !verdict.blockers.is_empty(),
                "{} is false with no blocker named, so the verdict is unauditable",
                verdict.site
            );
            assert_ne!(verdict.class, ReuseClass::ReuseReady);
            assert_ne!(verdict.class, ReuseClass::Unknown);
            // §26's rule restated: an axis that is not proven must have a blocker behind it.
            for axis in ALL_AXES {
                let status = verdict.axis(axis);
                if !status.supports_positive_claim() {
                    assert!(
                        !verdict.safe_to_reuse_now,
                        "{} axis {} is {status:?} yet the verdict is safe",
                        verdict.site,
                        axis.as_str()
                    );
                }
            }
        }
    }

    #[test]
    fn reusable_in_principle_is_a_separate_answer_from_safe_now() {
        // §27 asks for two fields, and a table where one implies the other has only one field.
        let verdicts = verdicts();
        let any_principle = verdicts.iter().any(|v| v.reusable_in_principle);
        for verdict in &verdicts {
            if verdict.reusable_in_principle {
                assert!(
                    !verdict.safe_to_reuse_now,
                    "{} collapses principle and now, which is the distinction M8.4.3 was built \
                     to keep",
                    verdict.site
                );
            }
        }
        // And the two gate reads differ in this corpus: the fee estimate is a handover candidate
        // in principle while the reserve reads are not.
        let fee = verdicts
            .iter()
            .find(|v| v.site == "preflight.l1_fee")
            .expect("the fee site is in the table");
        let reserves = verdicts
            .iter()
            .find(|v| v.site == "preflight.reserves")
            .expect("the reserve site is in the table");
        assert!(
            fee.reusable_in_principle != reserves.reusable_in_principle,
            "both answers are {fee:?} vs {reserves:?}; a reading whose purpose is to be fresh \
             must not score the same as an estimate a consumer can re-derive"
        );
        assert!(
            any_principle && !verdicts.iter().all(|v| v.reusable_in_principle),
            "the principle flag came out uniform over {} sites ({} reusable), so it reads as a \
             class claim rather than a per-site answer",
            verdicts.len(),
            verdicts.iter().filter(|v| v.reusable_in_principle).count()
        );
    }

    #[test]
    fn the_check_that_would_be_deleted_is_named_in_code_not_in_prose() {
        let reserves = ETH_CALL_SITES
            .iter()
            .find(|site| site.id == "preflight.reserves")
            .expect("the gate's reserve read is a site");
        assert!(reserves.read_is_the_check);
        assert!(
            reserves.anchors.iter().any(|a| a.source == SourceKind::Test
                && a.token
                    == "an_unread_reserve_blocks_instead_of_falling_back_to_the_priced_numbers"),
            "the verdict leans on an existing regression; if the anchor is dropped the claim \
             becomes prose"
        );
    }

    #[test]
    fn a_lifecycle_row_exists_for_every_site_and_step_and_says_where_the_answer_is() {
        let rows = lifecycle_rows();
        assert_eq!(rows.len(), ETH_CALL_SITES.len() * LIFECYCLE_STEPS.len());
        for site in &ETH_CALL_SITES {
            for step in LIFECYCLE_STEPS {
                let row = rows
                    .iter()
                    .find(|row| row.site == site.id && row.step == step)
                    .unwrap_or_else(|| panic!("{} has no cell for {}", site.id, step));
                assert!(!row.where_it_lives.is_empty());
                assert!(!row.note.is_empty());
                if step == "acquired" {
                    assert_eq!(row.status, ProofStatus::Proven);
                }
                if step == "invalidated_or_expired" {
                    // §15: a step that does not exist must be said so. Nothing here is `proven`
                    // staleness, because nothing here invalidates a reading.
                    assert_ne!(row.status, ProofStatus::Proven);
                }
            }
        }
    }

    #[test]
    fn push_data_is_not_counted_as_a_block_opcode() {
        // A contract whose only 0x43 byte sits inside a PUSH immediate must not look
        // block-dependent: this is the difference between a scan and a guess.
        let pushed_only = [0x61u8, 0x43, 0x42, 0x00];
        assert!(opcode_inventory(&pushed_only).is_empty());
        assert!(!block_context_reachable_in_code(&pushed_only));
        // One step off the same bytes, with NUMBER executed rather than pushed, and the answer
        // flips — the scan is sensitive to the distinction rather than to the byte value.
        let executed = [0x43u8, 0x00];
        let tally = opcode_inventory(&executed);
        assert_eq!(tally.get("block:NUMBER"), Some(&1));
        assert!(block_context_reachable_in_code(&executed));
    }

    #[test]
    fn an_absence_in_the_scan_is_a_proof_and_a_presence_is_not_a_refutation() {
        // §18's asymmetry, stated where the reader will hit it: a contract with no
        // block-context opcode anywhere cannot read one on the dispatched path, while a
        // contract that has TIMESTAMP somewhere is not shown to use it.
        let clean = [0x54u8, 0x55];
        assert!(!block_context_reachable_in_code(&clean));
        let dirty = [0x42u8];
        assert!(block_context_reachable_in_code(&dirty));
        // …and the table's consequence: a dirty contract yields `NotAttributable`, never a
        // claim of independence, so the site cannot be marked safe on the scan alone.
        for site in &ETH_CALL_SITES {
            if site.block_dependency == BlockDependency::NotAttributable {
                assert!(!verdict_for(site).safe());
            }
        }
    }

    #[test]
    fn every_axis_of_every_site_is_populated_and_no_axis_defaults_to_absent() {
        for site in &ETH_CALL_SITES {
            let verdict = verdict_for(site);
            assert_eq!(verdict.axes.len(), ALL_AXES.len(), "{}", site.id);
            for axis in ALL_AXES {
                assert!(!verdict.axis(axis).as_str().is_empty());
            }
        }
    }

    #[test]
    fn the_field_evidence_states_are_four_ways_of_not_being_null() {
        let states = [
            FieldEvidence::ProvenAbsent("the type has two fields").label(),
            FieldEvidence::Recorded("7".into()).label(),
            FieldEvidence::NotApplicable("no such term for this method").label(),
            FieldEvidence::NotRecorded("the record holds no params").label(),
            FieldEvidence::Unknown.label(),
        ];
        let distinct: std::collections::BTreeSet<_> = states.iter().collect();
        assert_eq!(
            distinct.len(),
            states.len(),
            "two different states print the same word, so a table could not tell them apart"
        );
    }

    #[test]
    fn the_identity_terms_come_from_the_existing_canonical_key() {
        // §51: this module must not run a second parser. The identity's four terms are the
        // terms M8.4.2's RpcReadKey already extracts, verbatim.
        let row = row("0902f1ac", POOL, "37700740", "preflight");
        let key = key(&row);
        let identity = EthCallIdentity::of_row(&key).expect("keyed row");
        assert_eq!(identity.to, key.terms["to"]);
        assert_eq!(identity.calldata, key.terms["data"]);
        assert_eq!(
            identity.block.term,
            key.block.clone().expect("a block term")
        );
        assert_eq!(identity.block.form, key.block_form);
        assert_eq!(identity.stage, "preflight");
    }

    #[test]
    fn the_eth_call_surface_is_stated_in_terms_the_trace_can_recount() {
        // The call sites are named so an assembly can prove it saw every one of them: a new
        // producer in the build would otherwise flow through the tables unnoticed.
        let families: std::collections::BTreeSet<_> =
            ETH_CALL_SITES.iter().map(|s| s.caller_family).collect();
        assert_eq!(
            families,
            ["before-snapshot", "cost ceiling", "reserves"]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
        );
        let stages: Vec<_> = ETH_CALL_SITES
            .iter()
            .map(|s| s.stage)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(stages, vec!["build", "opportunity_detection", "preflight"]);
    }
}
