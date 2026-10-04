// §4/§5/§10: a diagnostic model of who owns a piece of chain state, what scope it belongs
// to, what makes it invalid, and what a later stage may therefore consume.
//
// # What this module is
//
// M8.4.2 measured 78 cross-stage duplicate pairs out of 246 asks, kept 42 of them as
// exact-duplicate candidates, and refused all 42 with one reason: `lifecycle_ownership` is
// `not_checkable_from_a_record` — no field of a recorded RPC call says who holds an answer
// after it arrives, or when that answer stops being true. That refusal was correct and it
// was also the boundary of what a call record can show.
//
// This module moves that boundary by describing the *code*, not the record: for each of
// task-book §4's eleven state categories it declares the producer, the owner, the scope, the
// version identity, the freshness rule, the invalidation rule, the authority when sources
// disagree, and the consumers — each one carrying an anchor that a gate can resolve back to
// a real line in a real file. `ownership_status` grades the declaration; `reuse` states the
// four tiers §10 names, and the tiers never substitute for each other.
//
// # What this module is not
//
// It is not a runtime abstraction. Nothing in the pipeline reads a `StateContract` to decide
// behaviour, no stage obtains an owner through it, and it adds no field to any type that
// decides anything: §3 forbids a cache, a reuse path, a scheduler change, a batch, a
// multicall, a prefetch, and any change to RPC count, order or concurrency. The whole point
// is that a reader can check these claims against the source while the source keeps doing
// what it did before M8.4.3 existed.
//
// # Anchors
//
// A claim that reads `proven` is a claim about code, so it names the code: [`Anchor`] holds a
// file and a substring, and `crates/pipeline/tests/state_ownership_evidence.rs` requires the
// substring to occur on exactly one line of that file's production region (or, for
// [`SourceKind::Test`], anywhere in a test file) and writes the resolved `file.rs:LINE` into
// the evidence. A token found twice is a failure, not a coin toss: ambiguity means the claim
// does not name one place. `RunRecord` anchors point at committed evidence instead, and are
// resolved by a file that must exist and contain the token.
//
// # The tier rules, stated where the reader can disagree with them
//
// §10's three inequalities are implemented as rules, not as prose:
//
// - `same value != same state identity`: two asks whose identity matches are `duplicate`;
//   they are `semantically_equivalent` only when the state their answer describes is one
//   thing, which an ask with no block term (`eth_maxPriorityFeePerGas`) or a tag (`latest`)
//   cannot establish from the record.
// - `same state identity != same lifecycle ownership`: `reusable_in_principle` additionally
//   requires a named owner that could serve both consumers and a freshness rule the code
//   enforces.
// - `reusable in principle != safe to reuse now`: `safe_to_reuse_now` is true only when the
//   ownership status is `Proven`, no proof is missing, and no consumer's check would stop
//   existing. Where that fails the tier is `false` — never `unknown` — with the missing
//   conditions listed, because a refusal is a decision this module makes, not a measurement
//   it failed to take.

use serde::Serialize;

// ---------------------------------------------------------------------------
// §4 vocabulary: finite, stable, machine-checkable
// ---------------------------------------------------------------------------

/// The eleven state categories task-book §4 requires, named in the table's own order.
///
/// These are not eleven methods: §4 forbids calling all of this "链上状态", and the
/// distinction that matters is that `pool_reserves` and `block_header` have different owners,
/// different scopes and different invalidation events even when one RPC method answers both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StateKind {
    PoolReserves,
    ContractCode,
    StorageSlot,
    NativeBalance,
    Nonce,
    FeeParameters,
    BlockHeader,
    ChainIdentity,
    EthCallResult,
    SimulationResult,
    TransactionIntent,
}

/// Every kind, in declaration order: the tables iterate this list rather than whatever a
/// measurement happened to produce, so a category with no measured duplicate is still a row.
pub const ALL_KINDS: [StateKind; 11] = [
    StateKind::PoolReserves,
    StateKind::ContractCode,
    StateKind::StorageSlot,
    StateKind::NativeBalance,
    StateKind::Nonce,
    StateKind::FeeParameters,
    StateKind::BlockHeader,
    StateKind::ChainIdentity,
    StateKind::EthCallResult,
    StateKind::SimulationResult,
    StateKind::TransactionIntent,
];

impl StateKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            StateKind::PoolReserves => "pool_reserves",
            StateKind::ContractCode => "contract_code",
            StateKind::StorageSlot => "storage_slot",
            StateKind::NativeBalance => "native_balance",
            StateKind::Nonce => "nonce",
            StateKind::FeeParameters => "fee_parameters",
            StateKind::BlockHeader => "block_header",
            StateKind::ChainIdentity => "chain_identity",
            StateKind::EthCallResult => "eth_call_result",
            StateKind::SimulationResult => "simulation_result",
            StateKind::TransactionIntent => "transaction_intent",
        }
    }
}

/// How far a declaration is backed by code the reader can check.
///
/// §10's four recommended statuses, kept exactly. `NotApplicable` is not a weaker `Unknown`:
/// it says the question has no referent in this build — e.g. a published state that nothing
/// holds — while `Unknown` says the build does not answer it yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofStatus {
    Proven,
    PartiallyProven,
    Unknown,
    NotApplicable,
}

impl ProofStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            ProofStatus::Proven => "proven",
            ProofStatus::PartiallyProven => "partially_proven",
            ProofStatus::Unknown => "unknown",
            ProofStatus::NotApplicable => "not_applicable",
        }
    }

    /// Whether a claim at this status may carry a positive reuse tier. Only `Proven` may:
    /// §12.14's 「`unknown` 不得自动降级为 `safe_to_reuse_now`」 is enforced here rather than
    /// in each table's writer, so a missing declaration cannot slip through as a `true`.
    pub const fn supports_positive_claim(self) -> bool {
        matches!(self, ProofStatus::Proven)
    }
}

/// Who holds a state value. A finite list of the components this build actually has, taken
/// from §2's review list rather than invented: a name here that no `Owner` type occupies
/// would be a claim about an architecture this repository does not contain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Owner {
    /// The node behind the endpoint: the answer to a chain-scoped question is the node's, and
    /// nothing in this build can overrule it.
    Node,
    /// `evm_chain::rpc::RpcChainAdapter`, per call.
    ChainAdapter,
    /// The adapter's own learned identity, held from connect to drop.
    ChainAdapterConnection,
    /// `evm_simulation::state`'s read cache, which lives and dies with one
    /// `RpcStateProvider` and therefore with one simulation.
    SimulationStateProvider,
    /// `evm_state::store::InMemoryStateStore` — a projection of pool state, of EVM state.
    StateStore,
    /// An immutable `evm_graph::snapshot::GraphSnapshot`, one per applied block.
    GraphSnapshot,
    /// `evm_opportunity::lifecycle`'s ledger entry, which carries the finding's own state
    /// version and pinned block hash.
    OpportunityLedger,
    /// `evm_execution::giwa::preflight_facts`'s `LivePreflightReads`, for one attempt.
    LivePreflightReads,
    /// `evm_execution::sequence::SequenceStage`, per step, at the moment of sending.
    SequenceStage,
    /// `evm_execution::lifecycle`'s lane allocator, which is the only component that advances
    /// a nonce inside this process.
    ExecutionLane,
    /// Configuration and startup: a value the run was handed rather than read.
    ConfigStartup,
    /// No component in this build holds it. A row here is a finding, not a placeholder.
    NoOwnerInCode,
}

impl Owner {
    pub const fn as_str(self) -> &'static str {
        match self {
            Owner::Node => "node",
            Owner::ChainAdapter => "chain_adapter",
            Owner::ChainAdapterConnection => "chain_adapter_connection",
            Owner::SimulationStateProvider => "simulation_state_provider",
            Owner::StateStore => "state_store",
            Owner::GraphSnapshot => "graph_snapshot",
            Owner::OpportunityLedger => "opportunity_ledger",
            Owner::LivePreflightReads => "live_preflight_reads",
            Owner::SequenceStage => "sequence_stage",
            Owner::ExecutionLane => "execution_lane",
            Owner::ConfigStartup => "config_startup",
            Owner::NoOwnerInCode => "no_owner_in_code",
        }
    }
}

/// §4's Scope: what a value is *about* in the versioned sense.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Constant for the chain; nothing in a run changes it.
    Chain,
    /// Constant for one endpoint connection.
    EndpointConnection,
    /// One exact block — and only that block.
    Block,
    /// One account at one moment, where the moment is not a block but the node's head.
    HeadMoment,
    /// One sender's account across the pending view, which includes transactions the node has
    /// accepted but not mined.
    PendingAccount,
    /// One simulation: the value exists inside one `RpcStateProvider` and cannot outlive it.
    Simulation,
    /// One attempt: gathered for a single opportunity id.
    Attempt,
    /// One step of a sequence: re-read per send.
    ExecutionStep,
    /// One applied position in the store's own ordering.
    UpdatePosition,
    /// Not determinable from the code. The word the tables print is `unknown`, so a row that
    /// ends up here is a gap the reader can find by grepping the tables.
    Unknown,
}

impl Scope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Scope::Chain => "chain",
            Scope::EndpointConnection => "endpoint_connection",
            Scope::Block => "block",
            Scope::HeadMoment => "head_moment",
            Scope::PendingAccount => "pending_account",
            Scope::Simulation => "simulation",
            Scope::Attempt => "attempt",
            Scope::ExecutionStep => "execution_step",
            Scope::UpdatePosition => "update_position",
            Scope::Unknown => "unknown",
        }
    }
}

/// §4's Authority: which source wins when two disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    /// The node's answer at the named height is authoritative, and the build has no second
    /// source to weigh against it.
    NodeAtHeight,
    /// The node's canonical log stream is authoritative, and the store is a projection of it.
    NodeCanonicalLogs,
    /// The value the run was configured with is authoritative; the node's answer is a check
    /// on it, not a replacement.
    Configuration,
    /// The most recent applied position wins; older restatements cannot resurrect a value.
    LatestAppliedPosition,
    /// The first answer held by the adapter wins for the rest of the connection.
    FirstAnswerAtConnect,
    /// The code refuses to say: two sources disagree somewhere the build does not resolve.
    DisputedUnresolved,
    /// No authority question arises: the value is computed, not fetched.
    NotApplicable,
}

impl Authority {
    pub const fn as_str(self) -> &'static str {
        match self {
            Authority::NodeAtHeight => "node_at_height",
            Authority::NodeCanonicalLogs => "node_canonical_logs",
            Authority::Configuration => "configuration",
            Authority::LatestAppliedPosition => "latest_applied_position",
            Authority::FirstAnswerAtConnect => "first_answer_at_connect",
            Authority::DisputedUnresolved => "disputed_unresolved",
            Authority::NotApplicable => "not_applicable",
        }
    }
}

/// §4's Version/Identity, and §5's tag question, in one field per kind.
///
/// The distinction this encodes is the one M8.4.2's `block_form` already drew and §23 then
/// priced: a number names one block, a tag names "whichever block the node calls the head
/// when it answers", and an absent term names nothing at all. Two asks of identical identity
/// are only the same *state* under the first of those.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityForm {
    /// The value is keyed by an exact height, and the code that reads it sends a number.
    HeightNumber,
    /// The value is keyed by a height whose *hash* is also carried and verified.
    HeightHash,
    /// The ask names a tag: the answer is whichever block matched at the time.
    Tag,
    /// The ask carries no block term; the answer depends on the endpoint's own state.
    NoBlockTerm,
    /// The value is not block-scoped at all (chain identity, configuration).
    NotBlockScoped,
}

impl IdentityForm {
    pub const fn as_str(self) -> &'static str {
        match self {
            IdentityForm::HeightNumber => "height_number",
            IdentityForm::HeightHash => "height_hash",
            IdentityForm::Tag => "tag",
            IdentityForm::NoBlockTerm => "no_block_term",
            IdentityForm::NotBlockScoped => "not_block_scoped",
        }
    }

    /// §10's four identity fields, spelled out per category.
    ///
    /// These are the same fact `identity` already states, spread across the four names the task
    /// book asks for, so a table row shows each field on its own line instead of asking a reader
    /// to decode one word. The values are from a fixed list of eleven:
    /// `pinned_height`, `not_pinned`, `not_applicable`, `carried_and_verified`, `not_carried`,
    /// `is_the_value`, `pinned_chain`, `number`, `tag`, `absent`, `none`.
    pub const fn identity_fields(self) -> IdentityFields {
        match self {
            IdentityForm::HeightNumber => IdentityFields {
                chain_id: "pinned_chain",
                block_number: "pinned_height",
                block_hash: "not_carried",
                block_tag: "number",
            },
            IdentityForm::HeightHash => IdentityFields {
                chain_id: "pinned_chain",
                block_number: "pinned_height",
                block_hash: "carried_and_verified",
                block_tag: "number",
            },
            IdentityForm::Tag => IdentityFields {
                chain_id: "pinned_chain",
                block_number: "not_pinned",
                block_hash: "not_carried",
                block_tag: "tag",
            },
            IdentityForm::NoBlockTerm => IdentityFields {
                chain_id: "pinned_chain",
                block_number: "not_pinned",
                block_hash: "not_carried",
                block_tag: "absent",
            },
            IdentityForm::NotBlockScoped => IdentityFields {
                chain_id: "is_the_value",
                block_number: "not_applicable",
                block_hash: "not_applicable",
                block_tag: "none",
            },
        }
    }
}

/// §10's identity fields for one category, as the tables print them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct IdentityFields {
    pub chain_id: &'static str,
    pub block_number: &'static str,
    pub block_hash: &'static str,
    pub block_tag: &'static str,
}

/// §5's five lifecycle steps, in the order the task book writes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStep {
    Acquired,
    Validated,
    Published,
    Consumed,
    InvalidatedOrExpired,
}

impl LifecycleStep {
    pub const fn as_str(self) -> &'static str {
        match self {
            LifecycleStep::Acquired => "acquired",
            LifecycleStep::Validated => "validated",
            LifecycleStep::Published => "published",
            LifecycleStep::Consumed => "consumed",
            LifecycleStep::InvalidatedOrExpired => "invalidated_or_expired",
        }
    }

    pub const fn next(self) -> Option<LifecycleStep> {
        match self {
            LifecycleStep::Acquired => Some(LifecycleStep::Validated),
            LifecycleStep::Validated => Some(LifecycleStep::Published),
            LifecycleStep::Published => Some(LifecycleStep::Consumed),
            LifecycleStep::Consumed => Some(LifecycleStep::InvalidatedOrExpired),
            LifecycleStep::InvalidatedOrExpired => None,
        }
    }
}

/// Which kind of statement an anchor supports: a line of production code, a line of test, or
/// a committed run record. §10 asks for 「实际代码位置或测试证据」; these are the three shapes
/// this repository's evidence actually takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Code,
    Test,
    RunRecord,
}

impl SourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            SourceKind::Code => "code",
            SourceKind::Test => "test",
            SourceKind::RunRecord => "run_record",
        }
    }
}

/// One checkable pointer. `token` is resolved to a line by the gate, never hand-written here,
/// so the evidence records the position and the reader can open it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Anchor {
    pub file: &'static str,
    pub token: &'static str,
    pub source: SourceKind,
    pub note: &'static str,
}

pub const fn code(file: &'static str, token: &'static str, note: &'static str) -> Anchor {
    Anchor {
        file,
        token,
        source: SourceKind::Code,
        note,
    }
}

pub const fn test(file: &'static str, token: &'static str, note: &'static str) -> Anchor {
    Anchor {
        file,
        token,
        source: SourceKind::Test,
        note,
    }
}

pub const fn record(file: &'static str, token: &'static str, note: &'static str) -> Anchor {
    Anchor {
        file,
        token,
        source: SourceKind::RunRecord,
        note,
    }
}

/// A freshness or invalidation rule: the words the code enforces, how far that enforcement
/// reaches, and where.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Rule {
    pub id: &'static str,
    pub status: ProofStatus,
    pub text: &'static str,
    pub anchors: &'static [Anchor],
}

/// §10's four tiers, kept apart. `holds` is `None` only where the record or the code cannot
/// answer the tier at all — §13's refusal to invent data — while `safe_to_reuse_now` is always
/// written as a definite `true` or `false` in the declarations and never `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Tier {
    pub holds: Option<bool>,
    pub status: ProofStatus,
    pub reason: &'static str,
}

pub const fn tier(holds: Option<bool>, status: ProofStatus, reason: &'static str) -> Tier {
    Tier {
        holds,
        status,
        reason,
    }
}

/// §10's four tiers for one category, plus what is missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ReuseTiers {
    pub duplicate: Tier,
    pub semantically_equivalent: Tier,
    pub reusable_in_principle: Tier,
    pub safe_to_reuse_now: Tier,
    /// A named check that would stop existing if the value were shared. §6: 「不能为了减少 RPC
    /// 而删除执行前安全检查」 — the tier below cannot be true while this is.
    pub check_would_stop_existing: bool,
    pub missing_proof: &'static [&'static str],
}

/// §4's nine roles for one state category. §10's field list, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct StateContract {
    pub kind: StateKind,
    pub producer: Owner,
    pub owner: Owner,
    pub scope: Scope,
    pub identity: IdentityForm,
    pub authority: Authority,
    pub consumers: &'static [Owner],
    pub freshness: Rule,
    pub invalidation: Rule,
    pub ownership_status: ProofStatus,
    pub reuse: ReuseTiers,
    pub reason: &'static str,
    pub anchors: &'static [Anchor],
}

/// §5's lifecycle view: one row per (kind, step) with the transition's own basis.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LifecycleRow {
    pub state_kind: &'static str,
    pub step: &'static str,
    pub status: &'static str,
    pub basis: &'static str,
    /// The anchors the gate resolves to `file:line`. An empty list is a claim of `unknown` or
    /// `not_applicable` — the row says so itself, and the gate refuses a positive row with no
    /// anchor to open.
    pub anchors: &'static [Anchor],
}

// ---------------------------------------------------------------------------
// §4: the eleven declarations
// ---------------------------------------------------------------------------

/// §11's four tables, named. The names live here rather than in the gate so that the module
/// that owns the vocabulary also owns the file set a reader is told to open.
pub const OWNERSHIP_MATRIX_FILE: &str = "ownership-matrix.json";
pub const LIFECYCLE_CONTRACTS_FILE: &str = "lifecycle-contracts.json";
pub const STAGE_DEPENDENCY_MATRIX_FILE: &str = "stage-dependency-matrix.json";
pub const REUSE_VERDICTS_FILE: &str = "reuse-verdicts.json";
pub const README_FILE: &str = "README.md";

/// Everything one assembly of this milestone's model writes.
pub const STATE_OWNERSHIP_FILES: [&str; 5] = [
    README_FILE,
    OWNERSHIP_MATRIX_FILE,
    LIFECYCLE_CONTRACTS_FILE,
    STAGE_DEPENDENCY_MATRIX_FILE,
    REUSE_VERDICTS_FILE,
];

/// The model. Everything a table prints is assembled from here and from committed run
/// records, so there is exactly one place where a claim about the code is written down.
pub fn contracts() -> &'static [StateContract] {
    &CONTRACTS
}

static CONTRACTS: [StateContract; 11] = [
    // -- §4 row 1: pool reserves ------------------------------------------------------
    StateContract {
        kind: StateKind::PoolReserves,
        producer: Owner::ChainAdapter,
        owner: Owner::StateStore,
        scope: Scope::UpdatePosition,
        identity: IdentityForm::HeightNumber,
        authority: Authority::NodeCanonicalLogs,
        consumers: &[Owner::OpportunityLedger, Owner::GraphSnapshot],
        freshness: Rule {
            id: "restated_at_the_snapshot_block",
            status: ProofStatus::Proven,
            text: "a pool whose reserves were not restated at the snapshot's own applied \
                   position is skipped rather than priced, so a stale reserve cannot reach a \
                   candidate",
            anchors: &[
                code(
                    "crates/graph/src/builder.rs",
                    "The target block is the snapshot's own applied position",
                    "the same-block rule's own statement",
                ),
                code(
                    "crates/state/src/store.rs",
                    "pub struct InMemoryStateStore",
                    "the store that owns the projection",
                ),
            ],
        },
        invalidation: Rule {
            id: "newer_position_replaces_older",
            status: ProofStatus::PartiallyProven,
            text: "a strictly later (block, log) position replaces the value, which is proven; \
                   a rival block at the *same* height is detected as a canonicality conflict \
                   and recorded, never rolled back, so invalidation by reorganization is not \
                   implemented",
            anchors: &[
                code(
                    "crates/live/src/event.rs",
                    "does not resolve reorgs",
                    "v0.1 records the conflict instead of resolving it",
                ),
                code(
                    "crates/live/src/tracker.rs",
                    "kept: format!(\"{hash:?}\"),",
                    "the detection that does exist: a rival hash is kept next to the rejected one",
                ),
            ],
        },
        ownership_status: ProofStatus::PartiallyProven,
        reuse: ReuseTiers {
            duplicate: tier(
                Some(false),
                ProofStatus::Proven,
                "measured: none of M8.4.2's 78 pairs is a reserves read, because reserves are \
                 never fetched per stage — the pair count for this category is zero by the \
                 runs' own rows",
            ),
            semantically_equivalent: tier(
                None,
                ProofStatus::NotApplicable,
                "no cross-stage duplicate exists to compare, so the question has no referent \
                 in these runs",
            ),
            reusable_in_principle: tier(
                Some(true),
                ProofStatus::Proven,
                "the store already serves several consumers of one projection: detection reads \
                 the graph the store produced, and nothing re-reads reserves per stage",
            ),
            safe_to_reuse_now: tier(
                Some(false),
                ProofStatus::PartiallyProven,
                "sharing already happens inside the discovery half, and §4 asks not to call it \
                 a substitute for REVM state: the store holds pool metadata and reserves, and \
                 no balance, code, nonce or storage",
            ),
            check_would_stop_existing: false,
            missing_proof: &[
                "the graph snapshot carries a block number and no block hash, so a snapshot \
                 cannot show its block is the canonical one",
                "no code path rewinds the store's position, so a same-height rival block \
                 leaves the projection in place",
            ],
        },
        reason: "a projection of the node's canonical logs, owned by the store, valid at the \
                 position it was last restated at, and uninvalidated by a reorganization",
        anchors: &[
            code(
                "crates/core/src/pool.rs",
                "pub struct PoolState",
                "what the store actually holds: reserve0, reserve1, block_number, log_index",
            ),
            code(
                "crates/state/src/update.rs",
                "The only way anything may enter the store.",
                "replay and live feed the store through the same door",
            ),
            code(
                "crates/pipeline/src/engine.rs",
                "fn on_canonical",
                "the single entry point that turns a canonical block into store updates",
            ),
            code(
                "crates/graph/src/snapshot.rs",
                "pub struct GraphSnapshot {",
                "a snapshot's identity is its chain id and a block number — the fields on \
                 this definition, and no hash",
            ),
            record(
                "data/evidence/m8/cross-stage/duplicate-summary.json",
                "\"cross_stage_pairs\"",
                "the measured pair counts this row reports as zero for this category",
            ),
        ],
    },
    // -- §4 row 2: contract code ------------------------------------------------------
    StateContract {
        kind: StateKind::ContractCode,
        producer: Owner::ChainAdapter,
        owner: Owner::SimulationStateProvider,
        scope: Scope::Simulation,
        identity: IdentityForm::HeightNumber,
        authority: Authority::NodeAtHeight,
        consumers: &[Owner::SimulationStateProvider],
        freshness: Rule {
            id: "keyed_by_chain_block_address",
            status: ProofStatus::Proven,
            text: "a cached code answer is keyed by chain, height and address, so it can only \
                   be served for the same block it was fetched at; there is no expiry and \
                   none is needed inside one simulation",
            anchors: &[code(
                "crates/simulation/src/state.rs",
                "pub struct StateReadKey",
                "the key that fixes the scope",
            )],
        },
        invalidation: Rule {
            id: "dropped_with_the_provider",
            status: ProofStatus::Proven,
            text: "the cache is a field of one `RpcStateProvider` and there is no process-wide \
                   map for a later run to find; entries are never evicted because the scope \
                   ends before expiry could matter",
            anchors: &[
                code(
                    "crates/simulation/src/state.rs",
                    "no process-wide map",
                    "the boundary M8.3.1 wrote and this row cites",
                ),
                code(
                    "crates/simulation/src/state.rs",
                    "struct StateReadCache {",
                    "the cache itself",
                ),
                test(
                    "crates/simulation/tests/state_reuse_ab.rs",
                    "a_second_simulation_over_the_same_endpoint_pays_for_its_own_reads",
                    "the isolation that makes the scope a claim rather than a hope",
                ),
            ],
        },
        ownership_status: ProofStatus::Proven,
        reuse: ReuseTiers {
            duplicate: tier(
                Some(false),
                ProofStatus::Proven,
                "measured: no `eth_getCode` pair is among M8.4.2's 78 — every code read in \
                 these runs came from inside one simulation, where the cache already serves it",
            ),
            semantically_equivalent: tier(
                None,
                ProofStatus::NotApplicable,
                "no cross-stage pair to compare",
            ),
            reusable_in_principle: tier(
                Some(true),
                ProofStatus::Proven,
                "reused within its owner already, and provably not outside it",
            ),
            safe_to_reuse_now: tier(
                Some(false),
                ProofStatus::PartiallyProven,
                "safe inside one simulation, which is where it is reused today; unsafe across \
                 stages, because the only owner the code gives it dies with that simulation",
            ),
            check_would_stop_existing: false,
            missing_proof: &[
                "no component outside one simulation can hold a code answer, so cross-stage \
                 reuse has no owner to delegate to",
                "code cannot be fetched by hash at all, so a hash-verified identity is \
                 unavailable on this path",
            ],
        },
        reason: "the cache's identity is the key, the owner is one provider, and both end when \
                 the simulation ends",
        anchors: &[
            code(
                "crates/simulation/src/state.rs",
                "get_code(key.block, key.address)",
                "the code read names the block carried by the canonical key, and nothing else",
            ),
            code(
                "crates/simulation/src/engine.rs",
                "code_by_hash_async",
                "the REVM hook this build refuses to serve",
            ),
        ],
    },
    // -- §4 row 3: storage slot ------------------------------------------------------
    StateContract {
        kind: StateKind::StorageSlot,
        producer: Owner::ChainAdapter,
        owner: Owner::SimulationStateProvider,
        scope: Scope::Simulation,
        identity: IdentityForm::HeightNumber,
        authority: Authority::NodeAtHeight,
        consumers: &[Owner::SimulationStateProvider],
        freshness: Rule {
            id: "keyed_by_chain_block_address_slot",
            status: ProofStatus::Proven,
            text: "the storage key adds the slot to the account key, and reuse needs every \
                   field of it",
            anchors: &[
                code(
                    "crates/simulation/src/state.rs",
                    "pub struct StorageCacheKey",
                    "chain, block, address, slot",
                ),
                test(
                    "crates/simulation/src/state.rs",
                    "storage_reuse_needs_every_field_of_its_key",
                    "the test that pins the key's necessity",
                ),
            ],
        },
        invalidation: Rule {
            id: "dropped_with_the_provider",
            status: ProofStatus::Proven,
            text: "as code: no eviction, and the scope ends with one simulation",
            anchors: &[code(
                "crates/simulation/src/state.rs",
                "get_storage_at(self.pin.number",
                "the read names the pinned height, not a tag",
            )],
        },
        ownership_status: ProofStatus::Proven,
        reuse: ReuseTiers {
            duplicate: tier(
                Some(false),
                ProofStatus::Proven,
                "measured: no `eth_getStorageAt` pair among the 78, for the same reason as code",
            ),
            semantically_equivalent: tier(
                None,
                ProofStatus::NotApplicable,
                "no cross-stage pair to compare",
            ),
            reusable_in_principle: tier(
                Some(true),
                ProofStatus::PartiallyProven,
                "reusable within one simulation, and M8.4.1 measured the reads inside one \
                 simulation as almost entirely ordered rather than independent — 19 of 20 per \
                 run — so an ordered chain has few repeats to remove",
            ),
            safe_to_reuse_now: tier(
                Some(false),
                ProofStatus::PartiallyProven,
                "the ordering dependency is the other half of the answer: a slot read that a \
                 later read depends on cannot be answered from a different stage's memory \
                 without re-deriving the order",
            ),
            check_would_stop_existing: false,
            missing_proof: &[
                "no owner exists outside one simulation",
                "the dependency order M8.4.1 measured is not a field a consumer could check",
            ],
        },
        reason: "identity is proven and ownership is proven narrow; cross-stage reuse fails on \
                 the order the reads were taken in, not on the value",
        anchors: &[record(
            "data/evidence/m8/storage-dependency/dependency-map.json",
            "\"ordered\"",
            "the dependency classes this row's ordering claim comes from",
        )],
    },
    // -- §4 row 4: native balance ----------------------------------------------------
    StateContract {
        kind: StateKind::NativeBalance,
        producer: Owner::ChainAdapter,
        owner: Owner::NoOwnerInCode,
        scope: Scope::HeadMoment,
        identity: IdentityForm::Tag,
        authority: Authority::NodeAtHeight,
        consumers: &[
            Owner::LivePreflightReads,
            Owner::SequenceStage,
            Owner::SimulationStateProvider,
        ],
        freshness: Rule {
            id: "no_rule_declared",
            status: ProofStatus::Unknown,
            text: "the measured pairs come in two shapes, and in both the build asks again at \
                   the height the earlier stage used: preflight reads the sender's balance at \
                   the head it has just read and the build's before-snapshot asks that same \
                   height, while the simulation reads at the pinned block and the build's gate \
                   leg asks that same height. Nothing compares the two answers and nothing \
                   states how old a balance may be",
            anchors: &[
                code(
                    "crates/execution/src/giwa/preflight_facts.rs",
                    "fn read_head",
                    "the head the preflight balance is asked at",
                ),
                code(
                    "crates/execution/src/sequence.rs",
                    "before-snapshot: native",
                    "the build's second balance ask, at preflight's height",
                ),
                code(
                    "crates/execution/src/sequence.rs",
                    "gate — native balance",
                    "the build's funding leg, at the intent's pinned height",
                ),
            ],
        },
        invalidation: Rule {
            id: "re_read_per_send",
            status: ProofStatus::PartiallyProven,
            text: "invalidation is handled by refusing to rely on an earlier answer at all — \
                   the balance is re-read per send rather than assumed — which detects a \
                   change without modelling it",
            anchors: &[
                code(
                    "crates/execution/src/sequence.rs",
                    "re-read per send",
                    "the code's own words for this rule",
                ),
                code(
                    "crates/execution/src/gate.rs",
                    "pub enum BalanceEvidence",
                    "a per-step verdict, not a held value",
                ),
            ],
        },
        ownership_status: ProofStatus::PartiallyProven,
        reuse: ReuseTiers {
            duplicate: tier(
                Some(true),
                ProofStatus::Proven,
                "measured: 6 of M8.4.2's 42 exact candidates are `eth_getBalance` — \
                 preflight to build and simulation to build, three runs each",
            ),
            semantically_equivalent: tier(
                Some(false),
                ProofStatus::Unknown,
                "each measured pair shares a method, an address and a height, and the two sides \
                 do not ask the same question: the earlier answer decides whether the attempt \
                 may go on, the later one is the before-half of a diff that measures what the \
                 sequence actually moved. §10's 「same value != same state identity」 is this \
                 row",
            ),
            reusable_in_principle: tier(
                Some(false),
                ProofStatus::Proven,
                "no owner holds a balance between stages, and §6 forbids removing a \
                 pre-submission check to make the value shareable",
            ),
            safe_to_reuse_now: tier(
                Some(false),
                ProofStatus::PartiallyProven,
                "refused: the build-side read is a named gate leg, and sharing would delete \
                 the check rather than the cost",
            ),
            check_would_stop_existing: true,
            missing_proof: &[
                "a field saying which question each balance answer answers",
                "an owner that could invalidate a held balance when a transaction moves it",
            ],
        },
        reason: "duplicate in the record, different in purpose, and deliberately re-read",
        anchors: &[
            code(
                "crates/execution/src/stage.rs",
                "re-reads nothing about pools",
                "the lane's own statement of what it re-confirms",
            ),
            record(
                "data/evidence/m8/cross-stage/reuse-candidates.json",
                "\"step 1: gate — native balance\"",
                "the measured consumer role this row hangs on",
            ),
        ],
    },
    // -- §4 row 5: nonce -------------------------------------------------------------
    StateContract {
        kind: StateKind::Nonce,
        producer: Owner::ChainAdapter,
        owner: Owner::ExecutionLane,
        scope: Scope::PendingAccount,
        identity: IdentityForm::Tag,
        authority: Authority::NodeAtHeight,
        consumers: &[
            Owner::LivePreflightReads,
            Owner::SequenceStage,
            Owner::ExecutionLane,
        ],
        freshness: Rule {
            id: "pending_view_is_the_next_nonce",
            status: ProofStatus::Proven,
            text: "the next nonce is the pending count, and the confirmed count is what the \
                   chain has executed; the two are read as a pair so a moved account is visible \
                   to the reader rather than inferred",
            anchors: &[
                code(
                    "crates/execution/src/nonce.rs",
                    "the pending count is wha",
                    "the pending-versus-confirmed rule in the module's own words",
                ),
                code(
                    "crates/execution/src/giwa/sequencer_direct.rs",
                    "fn nonce",
                    "the read that returns both views",
                ),
            ],
        },
        invalidation: Rule {
            id: "any_mined_or_accepted_transaction",
            status: ProofStatus::PartiallyProven,
            text: "the lane allocates the next nonce itself, so within a sequence the value is \
                   owned and advanced in one place; outside it, another transaction from the \
                   same sender moves the pending count and the build notices only because it \
                   asks again",
            anchors: &[code(
                "crates/execution/src/lifecycle.rs",
                "fn allocate",
                "the only in-process nonce advance",
            )],
        },
        ownership_status: ProofStatus::PartiallyProven,
        reuse: ReuseTiers {
            duplicate: tier(
                Some(true),
                ProofStatus::Proven,
                "measured: 3 of 42 exact candidates are `eth_getTransactionCount`, preflight to \
                 build, plus 6 `eth_getBlockByNumber` asks whose consumer role is the nonce \
                 track, so the nonce path is 9 of 42",
            ),
            semantically_equivalent: tier(
                Some(false),
                ProofStatus::Proven,
                "both sides ask a tag, and a tag names whichever head the node has when it \
                 answers: two identical asks are not two reads of one state",
            ),
            reusable_in_principle: tier(
                Some(false),
                ProofStatus::Proven,
                "the lane's own allocation makes the value advance between steps by design, so \
                 an answer held across steps is wrong for the next step",
            ),
            safe_to_reuse_now: tier(
                Some(false),
                ProofStatus::Proven,
                "refused: a stale nonce is not a cheaper transaction, it is a different \
                 transaction",
            ),
            check_would_stop_existing: true,
            missing_proof: &[
                "nothing that would let a held nonce be proved still current; the current \
                 design's answer is to ask again",
            ],
        },
        reason: "the one category this build deliberately owns on a moving target, and the \
                 reason it re-reads is that the target is the point",
        anchors: &[
            code(
                "crates/execution/src/nonce.rs",
                "pub fn next(&self) -> u64 {",
                "the accessor that turns a view into a nonce",
            ),
            record(
                "data/evidence/m8/cross-stage/reuse-candidates.json",
                "\"step 1: pending and latest nonces\"",
                "the measured caller label both sides of the pair carry",
            ),
        ],
    },
    // -- §4 row 6: fee parameters ----------------------------------------------------
    StateContract {
        kind: StateKind::FeeParameters,
        producer: Owner::ChainAdapter,
        owner: Owner::ChainAdapter,
        scope: Scope::HeadMoment,
        identity: IdentityForm::NoBlockTerm,
        authority: Authority::NodeAtHeight,
        consumers: &[Owner::LivePreflightReads, Owner::SequenceStage],
        freshness: Rule {
            id: "compared_at_head_not_expired",
            status: ProofStatus::PartiallyProven,
            text: "a fee reading carries the block number and hash it was taken at, and the \
                   preflight gate compares the fee at the pin against the fee at the head, so a \
                   moved base fee is a finding; no expiry or deadline exists anywhere in the \
                   build",
            anchors: &[
                code(
                    "crates/execution/src/fee.rs",
                    "pub struct FeeReading",
                    "the value that does carry an identity",
                ),
                code(
                    "crates/execution/src/giwa/sequencer_direct.rs",
                    "fn base_fee_at",
                    "the pinned half of the pair",
                ),
            ],
        },
        invalidation: Rule {
            id: "next_block_base_fee",
            status: ProofStatus::PartiallyProven,
            text: "invalidation is the head comparison, which detects a moved base fee inside \
                   preflight; the build re-reads the fee at the pin and never checks the \
                   preflight value against it",
            anchors: &[code(
                "crates/execution/src/sequence.rs",
                "fee at pinned block",
                "the re-read this row is about",
            )],
        },
        ownership_status: ProofStatus::PartiallyProven,
        reuse: ReuseTiers {
            duplicate: tier(
                Some(true),
                ProofStatus::Proven,
                "measured: 6 of 42 exact candidates are `eth_maxPriorityFeePerGas` — preflight \
                 to build, and preflight to itself at the head — three runs each",
            ),
            semantically_equivalent: tier(
                Some(false),
                ProofStatus::Proven,
                "the method carries no block term at all, so the answer is a function of the \
                 moment the node served it; two identical asks are two samples",
            ),
            reusable_in_principle: tier(
                Some(false),
                ProofStatus::PartiallyProven,
                "a fee reading could in principle be carried with its block identity, and the \
                 build does not do that: it asks for its own number because the number is a \
                 pre-submission check",
            ),
            safe_to_reuse_now: tier(
                Some(false),
                ProofStatus::PartiallyProven,
                "refused: the fee leg of §26 exists to catch a moved fee, and sharing the value \
                 would leave the leg with nothing to compare",
            ),
            check_would_stop_existing: true,
            missing_proof: &[
                "a stated validity window: no expiry, deadline or TTL field exists in any crate",
                "an owner that holds one reading for both the gate and the build",
            ],
        },
        reason: "the value is versioned by the block it names, the ask is not, and the second \
                 read is the check",
        anchors: &[
            code(
                "crates/chain/src/rpc.rs",
                "fn block_param",
                "the only block term this build sends on the state path: a number",
            ),
            record(
                "data/evidence/m8/cross-stage/reuse-candidates.json",
                "\"fee at head\"",
                "the second measured fee flow inside one stage",
            ),
        ],
    },
    // -- §4 row 7: block header ------------------------------------------------------
    StateContract {
        kind: StateKind::BlockHeader,
        producer: Owner::ChainAdapter,
        owner: Owner::Node,
        scope: Scope::Block,
        identity: IdentityForm::HeightHash,
        authority: Authority::NodeAtHeight,
        consumers: &[
            Owner::OpportunityLedger,
            Owner::LivePreflightReads,
            Owner::SequenceStage,
            Owner::SimulationStateProvider,
        ],
        freshness: Rule {
            id: "number_and_hash_must_both_match",
            status: ProofStatus::Proven,
            text: "a pinned block is verified by height and by hash at three separate production \
                   points — dispatch, plan, and the provider round trip — so a header answer \
                   that belongs to a different block is refused rather than noticed later",
            anchors: &[
                code(
                    "crates/pipeline/src/runner.rs",
                    "state_unavailable",
                    "the dispatch check: hash of the announced block against the ledger's",
                ),
                code(
                    "crates/pipeline/src/sim.rs",
                    "state_version_mismatch",
                    "the plan check: header height against the finding's own state version",
                ),
                code(
                    "crates/simulation/src/request.rs",
                    "fn check_pin",
                    "the provider round trip, comparing number and hash",
                ),
            ],
        },
        invalidation: Rule {
            id: "same_height_different_hash",
            status: ProofStatus::PartiallyProven,
            text: "a reorganization is detected — the binding read returns a distinct `Reorged` \
                   case — and the response is to stop, not to re-resolve, so invalidation is \
                   proven as a decision and not as a recovery",
            anchors: &[
                code(
                    "crates/execution/src/gate.rs",
                    "pub enum BlockBinding",
                    "Confirmed, Reorged, Unverified",
                ),
                code(
                    "crates/execution/src/chain_read.rs",
                    "fn read_binding",
                    "the read that produces it",
                ),
            ],
        },
        ownership_status: ProofStatus::Proven,
        reuse: ReuseTiers {
            duplicate: tier(
                Some(true),
                ProofStatus::Proven,
                "measured: the largest family in M8.4.2 — 21 of 42 exact candidates are \
                 `eth_getBlockByNumber`, 15 with a numbered height, 6 on the nonce track's tags",
            ),
            semantically_equivalent: tier(
                Some(true),
                ProofStatus::PartiallyProven,
                "for the 15 numbered asks: one height names one block, and every one of them \
                 was measured at the same height on both sides; the hash that would make it \
                 unambiguous is verified by the consumers but is not carried in the shared \
                 value",
            ),
            reusable_in_principle: tier(
                Some(true),
                ProofStatus::Proven,
                "a header at a pinned height is immutable once the height is settled, and this \
                 build already proves it can verify the settlement; the missing piece is a \
                 value that carries both number and hash to the next consumer",
            ),
            safe_to_reuse_now: tier(
                Some(false),
                ProofStatus::PartiallyProven,
                "refused today: each of the 15 reads is a different consumer's own leg \
                 (fee at the pin, the block-binding gate leg, the simulation's header at pin), \
                 and `PreflightReport` — the only type that crosses the boundary — has no \
                 block field to put the answer in",
            ),
            check_would_stop_existing: false,
            missing_proof: &[
                "a carrier: no type that crosses from preflight to build has a block number or \
                 hash field",
                "a stated owner: the node owns the answer, and no component in this build \
                 holds a header for another stage to read",
            ],
        },
        reason: "the best-founded reuse case in the model, and still not safe today because \
                 nothing that crosses stages can hold the value",
        anchors: &[
            code(
                "crates/execution/src/preflight.rs",
                "pub struct PreflightReport",
                "the crossing type, whose fields are attempt id, findings, passed, two numbers, \
                 a reason and a ceiling",
            ),
            code(
                "crates/execution/src/sequence.rs",
                "fn preflight_cleared",
                "the three checks the build actually performs on that report",
            ),
            code(
                "crates/simulation/src/state.rs",
                "pub struct BlockPin",
                "the number-plus-hash the simulation already uses as an identity",
            ),
            record(
                "data/evidence/m8/cross-stage/reuse-candidates.json",
                "\"block binding at pin\"",
                "the measured consumer role on the preflight side of the pair",
            ),
        ],
    },
    // -- §4 row 8: chain identity ----------------------------------------------------
    StateContract {
        kind: StateKind::ChainIdentity,
        producer: Owner::ChainAdapter,
        owner: Owner::ChainAdapterConnection,
        scope: Scope::Chain,
        identity: IdentityForm::NotBlockScoped,
        authority: Authority::FirstAnswerAtConnect,
        consumers: &[
            Owner::ConfigStartup,
            Owner::LivePreflightReads,
            Owner::SequenceStage,
        ],
        freshness: Rule {
            id: "learned_once_per_connection",
            status: ProofStatus::Proven,
            text: "the adapter asks `eth_chainId` before it can answer anything else and keeps \
                   the value for the connection; that is a proven freshness rule with one \
                   consequence — a node that changed its chain mid-connection would not be \
                   noticed by the adapter",
            anchors: &[code(
                "crates/chain/src/rpc.rs",
                "before this type",
                "the connect-time exception the module documents",
            )],
        },
        invalidation: Rule {
            id: "none_until_reconnect",
            status: ProofStatus::PartiallyProven,
            text: "nothing invalidates the held value inside a connection, and the defense the \
                   code actually uses is repetition: five separate components compare their own \
                   configured chain against an answer and refuse, so a mismatch stops the run \
                   even though the cached value never changed",
            anchors: &[
                code(
                    "crates/cli/src/lib.rs",
                    "ChainMismatch",
                    "startup refusal when the registry and the node disagree",
                ),
                code(
                    "crates/execution/src/builder.rs",
                    "mismatch stops",
                    "the build leg, in its own words",
                ),
            ],
        },
        ownership_status: ProofStatus::Proven,
        reuse: ReuseTiers {
            duplicate: tier(
                Some(true),
                ProofStatus::Proven,
                "measured: 6 of 42 exact candidates are `eth_chainId`, both flows reaching the \
                 preflight leg and the build gate leg",
            ),
            semantically_equivalent: tier(
                Some(true),
                ProofStatus::Proven,
                "the answer is not a function of a block or a moment: the chain id is a \
                 property of the endpoint connection, and every later normalized value in this \
                 build carries it",
            ),
            reusable_in_principle: tier(
                Some(true),
                ProofStatus::Proven,
                "an owner exists (`RpcChainAdapter`'s connection), it holds the value from \
                 connect to drop, and its scope covers every consumer in the run",
            ),
            safe_to_reuse_now: tier(
                Some(false),
                ProofStatus::PartiallyProven,
                "refused: the second and third asks are each a named leg of a check, and §6 \
                 forbids trading them for a saved call; the honest statement is that the \
                 value is shareable and the verification is not",
            ),
            check_would_stop_existing: true,
            missing_proof: &[
                "a field that records, per check, whether the leg was satisfied by a fresh read \
                 or by a shared value — nothing distinguishes them today",
                "any detection of a chain change after connect, if a shared value were used",
            ],
        },
        reason: "the only category with a proven owner, a proven scope and a proven \
                 non-dependence on the block — and therefore the one §17 option A would start \
                 with, as a contract that keeps every check and shares only the value",
        anchors: &[
            code(
                "crates/execution/src/giwa/sequencer_direct.rs",
                "pub async fn endpoint_chain_id",
                "the accessor the preflight leg calls instead of a fresh ask",
            ),
            code(
                "crates/pipeline/src/arbitrage.rs",
                "let attempt = GateAttempt::Arbitrage",
                "where the legs are assembled into one gate attempt",
            ),
            record(
                "data/evidence/m8/cross-stage/duplicate-matrix.json",
                "\"chain_identity\"",
                "the read category that keeps this out of the state-read buckets",
            ),
        ],
    },
    // -- §4 row 9: eth_call result ---------------------------------------------------
    StateContract {
        kind: StateKind::EthCallResult,
        producer: Owner::ChainAdapter,
        owner: Owner::NoOwnerInCode,
        scope: Scope::Block,
        identity: IdentityForm::HeightNumber,
        authority: Authority::NodeAtHeight,
        consumers: &[Owner::LivePreflightReads, Owner::SequenceStage],
        freshness: Rule {
            id: "pinned_height_no_state_root",
            status: ProofStatus::PartiallyProven,
            text: "the ask names a height and the result is a function of the whole state at \
                   that height, which the build never receives: no field carries a state root, \
                   so two answers at one height are the same question with an unverifiable \
                   premise",
            anchors: &[code(
                "crates/execution/src/giwa/reads.rs",
                "fn read_pool",
                "the preflight leg's own eth_call reads",
            )],
        },
        invalidation: Rule {
            id: "different_block_at_the_same_slot",
            status: ProofStatus::Proven,
            text: "M8.4.2's §23 rule already does this work: a duplicate pair whose two sides \
                   name different heights is refused with the reason 「different block \
                   identity」, which is where 30 of the 78 pairs ended",
            anchors: &[record(
                "data/evidence/m8/cross-stage/reuse-candidates.json",
                "\"same_target_different_block\"",
                "the measured class this row reports",
            )],
        },
        ownership_status: ProofStatus::PartiallyProven,
        reuse: ReuseTiers {
            duplicate: tier(
                Some(false),
                ProofStatus::Proven,
                "measured: none of the 42 exact candidates is an `eth_call`; the `eth_call` \
                 pairs are the 30 refused for naming different heights, the largest single \
                 flow being opportunity detection to preflight",
            ),
            semantically_equivalent: tier(
                Some(false),
                ProofStatus::Proven,
                "by construction: two calls that name different heights are different \
                 questions about a market",
            ),
            reusable_in_principle: tier(
                Some(false),
                ProofStatus::PartiallyProven,
                "the two calls exist at different heights *because* the second is meant to see \
                 a moved market — detection prices a candidate, preflight re-prices it before \
                 spending",
            ),
            safe_to_reuse_now: tier(
                Some(false),
                ProofStatus::Proven,
                "refused by §23 today and by this row for the same reason: the re-read is the \
                 re-pricing",
            ),
            check_would_stop_existing: true,
            missing_proof: &[
                "a state-identity field the answer could carry; only the height travels",
            ],
        },
        reason: "the category where the duplicate was never a candidate at all: the §23 refusal \
                 and the purpose of the second read agree",
        anchors: &[code(
            "crates/pipeline/src/canonicalization.rs",
            "Self::RefusedByBlockIdentity =>",
            "the refusal this row is about, as the code writes it",
        )],
    },
    // -- §4 row 10: simulation result ------------------------------------------------
    StateContract {
        kind: StateKind::SimulationResult,
        producer: Owner::SimulationStateProvider,
        owner: Owner::SimulationStateProvider,
        scope: Scope::Simulation,
        identity: IdentityForm::HeightHash,
        authority: Authority::NotApplicable,
        consumers: &[Owner::SequenceStage, Owner::OpportunityLedger],
        freshness: Rule {
            id: "pinned_block_carried",
            status: ProofStatus::Proven,
            text: "the result carries its own `BlockPin` — number and hash — and the plan built \
                   from it re-verifies the hash by a binding read, so a result is tied to the \
                   block it ran against",
            anchors: &[
                code(
                    "crates/simulation/src/result.rs",
                    "pub struct SimulationResult",
                    "the value, with its pin",
                ),
                code(
                    "crates/execution/src/chain_read.rs",
                    "fn read_binding",
                    "the verification a later stage performs on it",
                ),
            ],
        },
        invalidation: Rule {
            id: "inputs_not_reproducible",
            status: ProofStatus::PartiallyProven,
            text: "the result does not record which state overrides the run used, so whether a \
                   run was reproducible on the real chain is a declaration made by its caller \
                   rather than a property of the result",
            anchors: &[
                code(
                    "crates/simulation/src/request.rs",
                    "pub enum Endowment {",
                    "the declaration that decides reproducibility",
                ),
                code(
                    "crates/simulation/src/state.rs",
                    "pub struct StateOverride",
                    "what a run can override, and what the result does not list",
                ),
                code(
                    "crates/pipeline/src/arbitrage.rs",
                    "must return an empty setup vect",
                    "the live path's refusal of an overridden run",
                ),
            ],
        },
        ownership_status: ProofStatus::PartiallyProven,
        reuse: ReuseTiers {
            duplicate: tier(
                Some(false),
                ProofStatus::Proven,
                "not applicable in the RPC sense: a simulation result is computed, not fetched, \
                 and no pair in M8.4.2 is one",
            ),
            semantically_equivalent: tier(
                None,
                ProofStatus::NotApplicable,
                "the comparison §10 asks for is between fetched values",
            ),
            reusable_in_principle: tier(
                Some(true),
                ProofStatus::Proven,
                "it is reused: the plan is built from the one result of the one run, and §12's \
                 「the plan is the run's transactions in order and nothing else» is the test \
                 that says so",
            ),
            safe_to_reuse_now: tier(
                Some(false),
                ProofStatus::PartiallyProven,
                "not the question this category answers; §14 forbids reading 「结果一致」 as \
                 「生产可安全复用」, and the honest limit is that the fields the build re-derives \
                 — nonce, both fee fields, gas limit, transaction type, access list — are not \
                 checked against the simulation",
            ),
            check_would_stop_existing: false,
            missing_proof: &[
                "an override list recorded in the result, so reproducibility does not depend on \
                 a caller's declaration",
            ],
        },
        reason: "one owner, one consumer, a carried identity, and an unrecorded premise",
        anchors: &[
            test(
                "crates/execution/tests/sequence.rs",
                "the_plan_is_the_run_s_transactions_in_order_and_nothing_else",
                "the binding that does exist",
            ),
            code(
                "crates/execution/src/intent.rs",
                "pub enum SenderFunding",
                "the caller-supplied declaration this row reports as a gap",
            ),
        ],
    },
    // -- §4 row 11: transaction intent -----------------------------------------------
    StateContract {
        kind: StateKind::TransactionIntent,
        producer: Owner::SequenceStage,
        owner: Owner::SequenceStage,
        scope: Scope::Attempt,
        identity: IdentityForm::HeightHash,
        authority: Authority::Configuration,
        consumers: &[Owner::ExecutionLane, Owner::SequenceStage],
        freshness: Rule {
            id: "attempt_id_is_the_only_binding",
            status: ProofStatus::PartiallyProven,
            text: "what the build verifies about the preflight verdict is that one exists, that \
                   it passed, and that its attempt id equals this route's — three checks, none \
                   of them a height, a hash or a moment",
            anchors: &[
                code(
                    "crates/execution/src/sequence.rs",
                    "fn preflight_cleared",
                    "the three checks, in order, in one function",
                ),
                code(
                    "crates/execution/src/gate.rs",
                    "pub enum Freshness",
                    "a freshness leg the gate can fail — and that both production paths fill in \
                     with a constant",
                ),
            ],
        },
        invalidation: Rule {
            id: "lifecycle_staleness_decided_earlier",
            status: ProofStatus::PartiallyProven,
            text: "staleness is real and enforced, but at a different point: the ledger moves a \
                   moved finding out of its active set, and the dispatch path returns before an \
                   inactive finding is ever built — the gate's own freshness leg is then stated \
                   rather than measured",
            anchors: &[
                code(
                    "crates/opportunity/src/lifecycle.rs",
                    "pub fn simmable(",
                    "the filter that cannot yield a stale entry",
                ),
                code(
                    "crates/pipeline/src/runner.rs",
                    "freshness: Freshness::Active",
                    "the constant, with the comment above it explaining why the author considers \
                     it established",
                ),
            ],
        },
        ownership_status: ProofStatus::PartiallyProven,
        reuse: ReuseTiers {
            duplicate: tier(
                None,
                ProofStatus::NotApplicable,
                "an intent is not a fetched state; §4 lists it because its *inputs* are the \
                 reuse question, not because it duplicates anything",
            ),
            semantically_equivalent: tier(None, ProofStatus::NotApplicable, "as above"),
            reusable_in_principle: tier(
                Some(true),
                ProofStatus::Proven,
                "the intent is itself the shared carrier between simulation and build, and it \
                 already carries a hash-bound risk decision id",
            ),
            safe_to_reuse_now: tier(
                Some(false),
                ProofStatus::PartiallyProven,
                "the carrier exists; what it does not carry is any of the preflight facts, and \
                 that is the concrete gap §6 asks about",
            ),
            check_would_stop_existing: false,
            missing_proof: &[
                "no version, epoch, validity token or deadline exists on any type that crosses \
                 from preflight to build",
                "the gate's freshness leg is a constant on both production paths, so it cannot \
                 be the evidence for a shared value",
            ],
        },
        reason: "the type where a contract would have to live, and the place the missing \
                 fields are provable rather than arguable",
        anchors: &[
            code(
                "crates/execution/src/intent.rs",
                "pub struct TransactionIntent",
                "the carrier",
            ),
            code(
                "crates/execution/src/preflight.rs",
                "pub struct PreflightReport",
                "what crosses today",
            ),
            code(
                "crates/opportunity/src/lifecycle.rs",
                "pub pinned_block_hash: B256,",
                "the hash that is kept on the finding and not on the verdict",
            ),
        ],
    },
];

/// §5's lifecycle view for one kind: which of the five steps the code performs, and where the
/// transition's basis is. Written as a table so a reader can disagree with one cell instead of
/// a paragraph.
pub fn lifecycle_rows() -> Vec<LifecycleRow> {
    let mut rows = Vec::new();
    for contract in contracts() {
        for step in [
            LifecycleStep::Acquired,
            LifecycleStep::Validated,
            LifecycleStep::Published,
            LifecycleStep::Consumed,
            LifecycleStep::InvalidatedOrExpired,
        ] {
            let (status, basis, anchors) = lifecycle_cell(contract, step);
            rows.push(LifecycleRow {
                state_kind: contract.kind.as_str(),
                step: step.as_str(),
                status: status.as_str(),
                basis,
                anchors,
            });
        }
    }
    rows
}

/// The lifecycle table itself. A step is `not_applicable` where this build has no such stage
/// — publishing is the case for almost everything, because no component here hands a value to
/// another component — and `unknown` where the code does not say.
///
/// Three of the five steps point at the same anchor set on purpose: the declaration's lines
/// are where the value is first brought in, where it is held, and where it is read back, and
/// the row that says otherwise would need a line the code does not have. The two steps that
/// can differ — validated and invalidated — point at their own rule's anchors, so a reader who
/// disputes a freshness claim has one list to check, not the whole declaration.
fn lifecycle_cell(
    contract: &StateContract,
    step: LifecycleStep,
) -> (ProofStatus, &'static str, &'static [Anchor]) {
    let acquired = (
        ProofStatus::Proven,
        "the producer names it: the ask or the computation that first brings the value into \
         this run",
        contract.anchors,
    );
    let validated = (
        contract.freshness.status,
        "the validation is the freshness rule itself; where that rule has no enforcement, the \
         step did not happen either",
        contract.freshness.anchors,
    );
    let published = match contract.owner {
        Owner::SimulationStateProvider => (
            ProofStatus::NotApplicable,
            "nothing publishes it: the value's scope is the component that fetched it, and no \
             other component can ask for it",
            &[][..],
        ),
        Owner::StateStore | Owner::GraphSnapshot | Owner::OpportunityLedger => (
            ProofStatus::Proven,
            "published inside the process: a later component reads the same store, snapshot or \
             ledger entry rather than a second copy",
            contract.anchors,
        ),
        _ => (
            ProofStatus::Unknown,
            "no component hands this value to another one, so the step has no code to point at",
            &[][..],
        ),
    };
    let consumed = (
        ProofStatus::Proven,
        "each consumer in the contract's list is a place the value is read, and the pair \
         counts in §11 are the measured form of that reading",
        contract.anchors,
    );
    let invalidated = (
        contract.invalidation.status,
        "the invalidation rule is the transition; a rule with no enforcement leaves the value \
         in place",
        contract.invalidation.anchors,
    );
    match step {
        LifecycleStep::Acquired => acquired,
        LifecycleStep::Validated => validated,
        LifecycleStep::Published => published,
        LifecycleStep::Consumed => consumed,
        LifecycleStep::InvalidatedOrExpired => invalidated,
    }
}

// ---------------------------------------------------------------------------
// §11: the measured input
// ---------------------------------------------------------------------------

/// One M8.4.2 candidate row, reduced to the fields this milestone reads.
///
/// Deliberately primitives only: this module opens no file, so the gate that does the reading
/// is the single place a schema change can show up. Every field here is a field of a row in
/// `data/evidence/m8/cross-stage/reuse-candidates.json`, named as that file names it; the
/// three fields the row does *not* carry — the two answers' bytes, a block hash, and any
/// statement of why a component asked again — are absent on purpose, and [`ReuseEvidence`]
/// records each of them as an unknown rather than as a `false`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeasuredCandidate {
    pub run: String,
    pub candidate_id: String,
    pub method: String,
    pub category: String,
    /// `cross_stage` or `intra_stage`, as M8.4.2 scopes it.
    pub scope: String,
    pub producer_caller: Option<String>,
    pub consumer_caller: Option<String>,
    pub producer_stage: Option<String>,
    pub consumer_stage: String,
    pub duplicate_type: String,
    pub block_relation: String,
    pub producer_block_form: String,
    pub consumer_block_form: String,
    /// The raw block term each side sent: a height string, a tag name, or nothing.
    pub producer_block: Option<String>,
    pub consumer_block: Option<String>,
    /// The record's own two verdicts, kept so a table can show them beside this milestone's
    /// rather than silently replacing them.
    pub record_reusable: String,
    pub record_safe_to_reuse: bool,
}

/// Which of the eleven §4 categories a measured ask belongs to.
///
/// Decided by the method first and the read category second, never by a keyword in a caller
/// label — §4 forbids lumping these together, and a caller string is the one field a rename
/// can change without touching a semantic fact. An ask matching no rule returns `None`, which
/// the gate reports as an unmapped row instead of filing it somewhere convenient.
pub fn kind_of(measured: &MeasuredCandidate) -> Option<StateKind> {
    let method = measured.method.as_str();
    if method == "eth_chainId" {
        return Some(StateKind::ChainIdentity);
    }
    if method == "eth_getBalance" {
        return Some(StateKind::NativeBalance);
    }
    if method == "eth_getTransactionCount" {
        return Some(StateKind::Nonce);
    }
    if method == "eth_maxPriorityFeePerGas" || method == "eth_gasPrice" {
        return Some(StateKind::FeeParameters);
    }
    if method == "eth_getBlockByNumber" {
        // A header ask issued on the way to a nonce read is still a header ask; §6 asks the
        // two questions separately, and [`purpose_of`] keeps them separable without this
        // function having to guess from a caller label.
        return Some(StateKind::BlockHeader);
    }
    if method == "eth_call" {
        return Some(StateKind::EthCallResult);
    }
    if method == "eth_getCode" {
        return Some(StateKind::ContractCode);
    }
    if method == "eth_getStorageAt" {
        return Some(StateKind::StorageSlot);
    }
    match measured.category.as_str() {
        "state_read" => Some(StateKind::StorageSlot),
        "block_read" => Some(StateKind::BlockHeader),
        "chain_identity" => Some(StateKind::ChainIdentity),
        "transaction_preparation" => Some(StateKind::FeeParameters),
        _ => None,
    }
}

/// What the measured pair's consumer was doing, so that a header read feeding a nonce check is
/// not averaged into the plain header family in a table.
pub fn purpose_of(measured: &MeasuredCandidate) -> &'static str {
    let caller = measured.consumer_caller.as_deref().unwrap_or("");
    if caller.contains("nonces") {
        "nonce_track"
    } else if caller.contains("fee") {
        "fee_pricing"
    } else if caller.contains("binding") {
        "block_binding"
    } else if caller.contains("header at pin") {
        "simulation_input"
    } else if caller.contains("balance") || caller.contains("snapshot") {
        "funding_check"
    } else if caller.contains("chain id") {
        "chain_check"
    } else {
        "other"
    }
}

// ---------------------------------------------------------------------------
// §10: the four tiers, as rules rather than as adjectives
// ---------------------------------------------------------------------------

/// Which *kind* of block term one side of a pair sent: the record's own `block_form` column,
/// read as an enum. The classification stops at the kind on purpose. Which tag word a tag row
/// used is echoed verbatim from that row's raw term into its `source_record`, because no file
/// under a crate's `src/` may write the head tag as a string literal — that is the §20 ban
/// `crates/pipeline/tests/state_is_always_pinned.rs` scans for, and a diagnosis of a recorded
/// call is a file under `src/` like any other, not an exemption. The rules ask one question of a
/// term anyway, `names_a_height`, and the kind answers it: only an explicit height can.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockTerm {
    /// An explicit height: one number names one block, though not which of two rival blocks
    /// at that height the node considers canonical.
    Number,
    /// A tag: the node chooses the block, so two asks can send the same word and name two
    /// different heights. §5 asks about the head tag and about `pending` separately; the echoed
    /// raw term is what keeps them apart here.
    Tag,
    /// No block term at all: the answer is a property of the endpoint's current state.
    Absent,
}

impl BlockTerm {
    pub const fn as_str(self) -> &'static str {
        match self {
            BlockTerm::Number => "number",
            BlockTerm::Tag => "tag",
            BlockTerm::Absent => "absent",
        }
    }

    pub const fn names_a_height(self) -> bool {
        matches!(self, BlockTerm::Number)
    }

    /// The record's `block_form` value. A word this model has never seen is read as a tag, which
    /// is the refusal-prone direction: it cannot name a height, so no tier can be answered
    /// positively from it.
    pub fn from_form(form: &str) -> Self {
        match form {
            "number" => BlockTerm::Number,
            "absent" => BlockTerm::Absent,
            _ => BlockTerm::Tag,
        }
    }
}

/// Everything the tier rules ask about, in the shapes a record or a reader can actually answer.
///
/// `Default` is the most favourable evidence this build can name — one ask, one height, one
/// hash-verified block, one component, one lifecycle, no detected change in between — because
/// §12's tests are about which single condition breaks it. A field is `Option` exactly where
/// a recorded call cannot answer it, and never as a placeholder for a judgement nobody made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReuseEvidence {
    pub duplicate: bool,
    pub same_chain_id: bool,
    pub same_endpoint: bool,
    /// The two answers' bytes. No row in M8.4.2 carries them: the pair is one *ask*, proven by
    /// the recorded request terms, never one *answer*.
    pub same_value: Option<bool>,
    pub same_block_number: Option<bool>,
    pub same_block_hash: Option<bool>,
    pub producer_term: BlockTerm,
    pub consumer_term: BlockTerm,
    pub same_producing_component: bool,
    pub same_lifecycle_scope: bool,
    /// Whether the chain's state moved between the two asks. `None` is the common case because
    /// nothing in this build compares two moments.
    pub state_changed_in_between: Option<bool>,
    pub depends_on_state_override: bool,
    pub sim_and_build_fields_match: bool,
    pub check_would_stop_existing: bool,
}

impl Default for ReuseEvidence {
    fn default() -> Self {
        Self {
            duplicate: true,
            same_chain_id: true,
            same_endpoint: true,
            same_value: Some(true),
            same_block_number: Some(true),
            same_block_hash: Some(true),
            producer_term: BlockTerm::Number,
            consumer_term: BlockTerm::Number,
            same_producing_component: true,
            same_lifecycle_scope: true,
            state_changed_in_between: Some(false),
            depends_on_state_override: false,
            sim_and_build_fields_match: true,
            check_would_stop_existing: false,
        }
    }
}

/// The measured row's own evidence, with nothing invented to fill a gap: the three questions a
/// record cannot answer stay `None`, which is what makes a measured pair incapable of reaching
/// the fourth tier by arithmetic alone.
pub fn evidence_from_measured(measured: &MeasuredCandidate) -> ReuseEvidence {
    let number_relation = match measured.block_relation.as_str() {
        "same_block" => Some(true),
        "different_block" | "tag_against_height" | "same_target_different_block" => Some(false),
        _ => None,
    };
    ReuseEvidence {
        duplicate: measured.duplicate_type == "exact_duplicate",
        // The pairing rule requires one identity, and the identity string carries the chain id
        // and the endpoint digest; both are therefore properties of how rows may be paired at
        // all, not of the two answers.
        same_chain_id: true,
        same_endpoint: true,
        same_value: None,
        same_block_number: number_relation,
        same_block_hash: None,
        producer_term: BlockTerm::from_form(&measured.producer_block_form),
        consumer_term: BlockTerm::from_form(&measured.consumer_block_form),
        // Whether an earlier stage's component is the one that owns the later stage's answer is
        // the question M8.4.2 could not check from a record, and a record still cannot check
        // it; the declaration answers it from code, and this field stays in the negative.
        same_producing_component: false,
        same_lifecycle_scope: false,
        state_changed_in_between: None,
        depends_on_state_override: false,
        sim_and_build_fields_match: true,
        check_would_stop_existing: false,
    }
}

/// A refusal, named. §10 asks for the missing proof conditions to be listed rather than
/// summarised, and a finite vocabulary is what lets a table be diffed between runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocker {
    NotADuplicate,
    ChainIdDiffers,
    EndpointDiffers,
    ValueDiffers,
    AnswerBytesNotRecorded,
    BlockNumberDiffers,
    BlockNumberNotComparable,
    BlockHashDiffers,
    BlockHashNotRecorded,
    ProducerNamesNoHeight,
    ConsumerNamesNoHeight,
    SameValueDifferentSource,
    DifferentLifecycleScope,
    StateMovedBetweenReads,
    StateMovementNotDetectable,
    StateOverrideNotReproducible,
    SimulationBuildFieldsDiffer,
    CheckWouldStopExisting,
    ContractCheckWouldStopExisting,
    NoOwnerInCode,
    OwnershipNotProven,
    FreshnessRuleNotProven,
    InvalidationRuleNotProven,
    ContractDeclaresNotReusable,
    ContractCannotSayReusable,
    ContractListsMissingProof,
}

impl Blocker {
    pub const fn as_str(self) -> &'static str {
        match self {
            Blocker::NotADuplicate => "not_a_duplicate",
            Blocker::ChainIdDiffers => "chain_id_differs",
            Blocker::EndpointDiffers => "endpoint_differs",
            Blocker::ValueDiffers => "value_differs",
            Blocker::AnswerBytesNotRecorded => "answer_bytes_not_recorded",
            Blocker::BlockNumberDiffers => "block_number_differs",
            Blocker::BlockNumberNotComparable => "block_number_not_comparable",
            Blocker::BlockHashDiffers => "block_hash_differs",
            Blocker::BlockHashNotRecorded => "block_hash_not_recorded",
            Blocker::ProducerNamesNoHeight => "producer_names_no_height",
            Blocker::ConsumerNamesNoHeight => "consumer_names_no_height",
            Blocker::SameValueDifferentSource => "same_value_different_source",
            Blocker::DifferentLifecycleScope => "different_lifecycle_scope",
            Blocker::StateMovedBetweenReads => "state_moved_between_reads",
            Blocker::StateMovementNotDetectable => "state_movement_not_detectable",
            Blocker::StateOverrideNotReproducible => "state_override_not_reproducible",
            Blocker::SimulationBuildFieldsDiffer => "simulation_build_fields_differ",
            Blocker::CheckWouldStopExisting => "check_would_stop_existing",
            Blocker::ContractCheckWouldStopExisting => "contract_check_would_stop_existing",
            Blocker::NoOwnerInCode => "no_owner_in_code",
            Blocker::OwnershipNotProven => "ownership_not_proven",
            Blocker::FreshnessRuleNotProven => "freshness_rule_not_proven",
            Blocker::InvalidationRuleNotProven => "invalidation_rule_not_proven",
            Blocker::ContractDeclaresNotReusable => "contract_declares_not_reusable",
            Blocker::ContractCannotSayReusable => "contract_cannot_say_reusable",
            Blocker::ContractListsMissingProof => "contract_lists_missing_proof",
        }
    }
}

/// The four tiers, resolved against one piece of evidence, with every refusal named.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReuseAssessment {
    pub state_kind: &'static str,
    pub ownership_status: &'static str,
    pub duplicate: bool,
    pub semantically_equivalent: Option<bool>,
    pub reusable_in_principle: Option<bool>,
    pub safe_to_reuse_now: bool,
    pub blockers: Vec<&'static str>,
    pub missing_proof: Vec<&'static str>,
}

/// Look a category up. The eleven declarations are a static array, so a missing category is a
/// programming error rather than a runtime question; `None` is still returned rather than
/// panicking, because a table that drops a row is more honest than one that aborts.
pub fn contract_for(kind: StateKind) -> Option<&'static StateContract> {
    contracts().iter().find(|contract| contract.kind == kind)
}

/// §10's tier rules, applied. The tiers are computed in a strict order — identity, then
/// ownership and lifecycle, then the contract's own declarations — so that a `true` at a later
/// tier always means every earlier tier was answered positively, and never means «the value
/// looked the same».
pub fn assess(kind: StateKind, evidence: &ReuseEvidence) -> ReuseAssessment {
    let contract = contract_for(kind);
    let mut blockers: Vec<&'static str> = Vec::new();
    let push = |blocker: Blocker, blockers: &mut Vec<&'static str>| {
        if !blockers.contains(&blocker.as_str()) {
            blockers.push(blocker.as_str());
        }
    };

    // -- tier 1: duplicate — one ask, two calls, as the record defines it.
    if !evidence.duplicate {
        push(Blocker::NotADuplicate, &mut blockers);
    }
    if !evidence.same_chain_id {
        push(Blocker::ChainIdDiffers, &mut blockers);
    }
    if !evidence.same_endpoint {
        push(Blocker::EndpointDiffers, &mut blockers);
    }

    // -- tier 2: semantically equivalent — the two asks name one state, at one version.
    match evidence.same_value {
        Some(false) => push(Blocker::ValueDiffers, &mut blockers),
        Some(true) => {}
        None => push(Blocker::AnswerBytesNotRecorded, &mut blockers),
    }
    match evidence.same_block_number {
        Some(false) => push(Blocker::BlockNumberDiffers, &mut blockers),
        Some(true) => {}
        None => push(Blocker::BlockNumberNotComparable, &mut blockers),
    }
    match evidence.same_block_hash {
        Some(false) => push(Blocker::BlockHashDiffers, &mut blockers),
        Some(true) => {}
        None => push(Blocker::BlockHashNotRecorded, &mut blockers),
    }
    if !evidence.producer_term.names_a_height() {
        push(Blocker::ProducerNamesNoHeight, &mut blockers);
    }
    if !evidence.consumer_term.names_a_height() {
        push(Blocker::ConsumerNamesNoHeight, &mut blockers);
    }

    let identity_negative = !evidence.duplicate
        || !evidence.same_chain_id
        || !evidence.same_endpoint
        || evidence.same_value == Some(false)
        || evidence.same_block_number == Some(false)
        || evidence.same_block_hash == Some(false)
        || !evidence.producer_term.names_a_height()
        || !evidence.consumer_term.names_a_height();
    let identity_incomplete = evidence.same_value.is_none() || evidence.same_block_hash.is_none();
    let semantically_equivalent = if identity_negative {
        Some(false)
    } else if identity_incomplete {
        None
    } else {
        Some(true)
    };

    // -- tier 3: reusable in principle — one owner, one lifecycle, nothing else in the way.
    if !evidence.same_producing_component {
        push(Blocker::SameValueDifferentSource, &mut blockers);
    }
    if !evidence.same_lifecycle_scope {
        push(Blocker::DifferentLifecycleScope, &mut blockers);
    }
    if evidence.depends_on_state_override {
        push(Blocker::StateOverrideNotReproducible, &mut blockers);
    }
    if !evidence.sim_and_build_fields_match {
        push(Blocker::SimulationBuildFieldsDiffer, &mut blockers);
    }
    match contract.map(|c| c.reuse.reusable_in_principle.holds) {
        Some(Some(false)) => push(Blocker::ContractDeclaresNotReusable, &mut blockers),
        Some(Some(true)) => {}
        _ => push(Blocker::ContractCannotSayReusable, &mut blockers),
    }

    let principle_negative = semantically_equivalent != Some(true)
        || !evidence.same_producing_component
        || !evidence.same_lifecycle_scope
        || evidence.depends_on_state_override
        || !evidence.sim_and_build_fields_match
        || contract.map(|c| c.reuse.reusable_in_principle.holds) == Some(Some(false));
    let reusable_in_principle = if semantically_equivalent.is_none()
        || contract.map(|c| c.reuse.reusable_in_principle.holds) == Some(None)
    {
        None
    } else if principle_negative {
        Some(false)
    } else {
        Some(true)
    };

    // -- tier 4: safe now — ownership, freshness, invalidation, and no check lost.
    match evidence.state_changed_in_between {
        Some(true) => push(Blocker::StateMovedBetweenReads, &mut blockers),
        Some(false) => {}
        None => push(Blocker::StateMovementNotDetectable, &mut blockers),
    }
    if evidence.check_would_stop_existing {
        push(Blocker::CheckWouldStopExisting, &mut blockers);
    }
    if let Some(contract) = contract {
        if contract.reuse.check_would_stop_existing {
            push(Blocker::ContractCheckWouldStopExisting, &mut blockers);
        }
        if contract.owner == Owner::NoOwnerInCode {
            push(Blocker::NoOwnerInCode, &mut blockers);
        }
        if contract.ownership_status != ProofStatus::Proven {
            push(Blocker::OwnershipNotProven, &mut blockers);
        }
        if contract.freshness.status != ProofStatus::Proven {
            push(Blocker::FreshnessRuleNotProven, &mut blockers);
        }
        if contract.invalidation.status != ProofStatus::Proven {
            push(Blocker::InvalidationRuleNotProven, &mut blockers);
        }
        // The declaration's own list of what is still missing is a refusal as concrete as an
        // unproven rule, so it appears in the blocker column too: the invariant the evidence
        // gate checks is «not safe ⇒ at least one named condition», and a row that left it
        // empty while its verdict was false would be §10's failure mode.
        if !contract.reuse.missing_proof.is_empty() {
            push(Blocker::ContractListsMissingProof, &mut blockers);
        }
    } else {
        push(Blocker::OwnershipNotProven, &mut blockers);
    }

    let safe_to_reuse_now = reusable_in_principle == Some(true)
        && evidence.state_changed_in_between == Some(false)
        && !evidence.check_would_stop_existing
        && contract.map(|c| c.reuse.check_would_stop_existing) == Some(false)
        && contract.map(|c| c.ownership_status) == Some(ProofStatus::Proven)
        && contract.map(|c| c.freshness.status) == Some(ProofStatus::Proven)
        && contract.map(|c| c.invalidation.status) == Some(ProofStatus::Proven)
        && contract.map(|c| c.reuse.missing_proof.is_empty()) == Some(true);

    ReuseAssessment {
        state_kind: kind.as_str(),
        ownership_status: contract
            .map(|c| c.ownership_status.as_str())
            .unwrap_or("undeclared"),
        duplicate: evidence.duplicate,
        semantically_equivalent,
        reusable_in_principle,
        safe_to_reuse_now,
        missing_proof: contract
            .map(|c| c.reuse.missing_proof.to_vec())
            .unwrap_or_default(),
        blockers,
    }
}

/// One measured row, its evidence, and this milestone's verdict beside M8.4.2's own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AssessedCandidate {
    pub candidate_id: String,
    pub run: String,
    pub scope: String,
    pub state_kind: &'static str,
    pub purpose: &'static str,
    pub method: String,
    pub category: String,
    pub producer_stage: Option<String>,
    pub consumer_stage: String,
    pub producer_term: &'static str,
    pub consumer_term: &'static str,
    pub block_relation: String,
    pub record_reusable: String,
    pub record_safe_to_reuse: bool,
    pub evidence: ReuseEvidence,
    pub assessment: ReuseAssessment,
}

/// The whole measured→verdict step, in one call so the gate cannot reorder it. A row whose
/// method matches no category is returned as `None` and reported as unmapped rather than
/// averaged into a neighbour.
pub fn assess_measured(measured: &MeasuredCandidate) -> Option<AssessedCandidate> {
    let kind = kind_of(measured)?;
    let evidence = evidence_from_measured(measured);
    let assessment = assess(kind, &evidence);
    Some(AssessedCandidate {
        candidate_id: measured.candidate_id.clone(),
        run: measured.run.clone(),
        scope: measured.scope.clone(),
        state_kind: kind.as_str(),
        purpose: purpose_of(measured),
        method: measured.method.clone(),
        category: measured.category.clone(),
        producer_stage: measured.producer_stage.clone(),
        consumer_stage: measured.consumer_stage.clone(),
        producer_term: evidence.producer_term.as_str(),
        consumer_term: evidence.consumer_term.as_str(),
        block_relation: measured.block_relation.clone(),
        record_reusable: measured.record_reusable.clone(),
        record_safe_to_reuse: measured.record_safe_to_reuse,
        evidence,
        assessment,
    })
}

/// The categories that exist, so a table can state which of the eleven §4 rows a run exercised
/// and which it did not — without inventing a row for the ones it did not.
pub fn kinds() -> Vec<&'static str> {
    ALL_KINDS.iter().map(|kind| kind.as_str()).collect()
}

/// §17: the only four things the evidence can support deciding about one stage edge. The
/// variants are the task book's options A to D, so a row cannot drift into a fifth answer and
/// cannot invent "safe" as a decision: `safe_to_reuse_now` stays a tier of its own (§10), and
/// option A only says a controlled experiment is definable next phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeDecision {
    ControlledExperimentDefinable,
    ContractDesignFirst,
    MustRefetch,
    InsufficientEvidence,
}

impl EdgeDecision {
    pub const fn letter(self) -> &'static str {
        match self {
            Self::ControlledExperimentDefinable => "A",
            Self::ContractDesignFirst => "B",
            Self::MustRefetch => "C",
            Self::InsufficientEvidence => "D",
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ControlledExperimentDefinable => "controlled_experiment_definable",
            Self::ContractDesignFirst => "contract_design_first",
            Self::MustRefetch => "must_refetch",
            Self::InsufficientEvidence => "insufficient_evidence",
        }
    }
}

/// §6's nine sub-questions, asked of every edge so no row answers fewer than the task book
/// asks. `None` is only used where neither the code nor a record decides it; it never means
/// "assumed", and a row with a `None` can still reach a decision — §14 forbids reading a
/// silence as safety, not as a conclusion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct EdgeAnswers {
    pub what_the_producer_read: &'static str,
    pub what_the_consumer_read: &'static str,
    pub same_semantics: Option<bool>,
    pub safety_constraint_served: &'static str,
    pub producer_result_carried: Option<bool>,
    pub consumer_has_own_duty: Option<bool>,
    pub external_change_between: Option<bool>,
    pub version_or_token_exists: Option<bool>,
    pub minimal_addition: &'static str,
}

/// One investigated edge: the two stages, what crosses it, what M8.4.2 measured there, and
/// which of §17's four options the evidence supports. `producer_stage` and `consumer_stage`
/// spell stages the way a run record spells them (`stage_label`), so a table can be reconciled
/// against the rows without a translation layer; `method: None` marks an edge that carries a
/// question but no measured duplicate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StageEdge {
    pub id: &'static str,
    pub section: &'static str,
    pub producer_stage: &'static str,
    pub consumer_stage: &'static str,
    pub state_kind: &'static str,
    pub method: Option<&'static str>,
    pub producer_caller: Option<&'static str>,
    pub consumer_caller: Option<&'static str>,
    pub measured_pairs: u32,
    pub question: &'static str,
    pub answers: EdgeAnswers,
    pub contract: ProofStatus,
    pub decision: EdgeDecision,
    pub reasons: &'static [&'static str],
    pub anchors: &'static [Anchor],
}

static EDGES: [StageEdge; 23] = [
    // -- §6 A: preflight → build, the twelve measured pairs -------------------------
    StageEdge {
        id: "s6.chain_id.connection_to_preflight",
        section: "6",
        producer_stage: "unstamped",
        consumer_stage: "preflight",
        state_kind: StateKind::ChainIdentity.as_str(),
        method: Some("eth_chainId"),
        producer_caller: None,
        consumer_caller: Some("endpoint chain id"),
        measured_pairs: 3,
        question: "can preflight's chain leg consume the answer the connection already took?",
        answers: EdgeAnswers {
            what_the_producer_read:
                "one eth_chainId on the connection, before any stage is named: the record \
                 leaves its stage empty and labels it unstamped",
            what_the_consumer_read: "preflight's own eth_chainId, stamped `endpoint chain id`",
            same_semantics: Some(true),
            safety_constraint_served: "the §26 chain leg — the endpoint answers as the chain \
                                       the run was configured for",
            producer_result_carried: Some(false),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(false),
            version_or_token_exists: Some(false),
            minimal_addition: "a field saying a connection has learned its chain id, and one \
                               saying which connection an answer came from",
        },
        contract: ProofStatus::PartiallyProven,
        decision: EdgeDecision::ContractDesignFirst,
        reasons: &[
            "the answer belongs to a connection and cannot move inside one, which is why a \
             contract is designable here at all",
            "no type that crosses into preflight carries a chain id, so the value would have \
             to be invented rather than found",
            "preflight's leg stays whichever way this goes: comparing an answer against \
             configuration is not a fetch",
        ],
        anchors: &[
            code(
                "crates/execution/src/giwa/sequencer_direct.rs",
                "pub async fn endpoint_chain_id",
                "the connection's own ask, read rather than remembered",
            ),
            code(
                "crates/execution/src/giwa/preflight_facts.rs",
                "endpoint chain id",
                "the preflight stamp that makes this pair measurable",
            ),
            record(
                "data/evidence/m8/cross-stage/reuse-candidates.json",
                "\"unstamped\"",
                "the spelling a run record uses for the stage-less producer",
            ),
        ],
    },
    StageEdge {
        id: "s6.chain_id.connection_to_build",
        section: "6",
        producer_stage: "unstamped",
        consumer_stage: "build",
        state_kind: StateKind::ChainIdentity.as_str(),
        method: Some("eth_chainId"),
        producer_caller: None,
        consumer_caller: Some("step 1: gate — endpoint chain id"),
        measured_pairs: 3,
        question: "does the build's chain leg need its own ask, or a carried answer?",
        answers: EdgeAnswers {
            what_the_producer_read:
                "the connection's eth_chainId, the same single ask as the row above",
            what_the_consumer_read: "the build's gate leg, stamped `step 1: gate — endpoint \
                                     chain id`",
            same_semantics: Some(true),
            safety_constraint_served: "the same chain id checked again at the moment a build \
                                       starts, against what the run was configured for",
            producer_result_carried: Some(false),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(false),
            version_or_token_exists: Some(false),
            minimal_addition: "none this milestone may make: keeping the leg costs the ask, \
                               and §6 forbids trading the leg away",
        },
        contract: ProofStatus::Proven,
        decision: EdgeDecision::MustRefetch,
        reasons: &[
            "this read is a named gate leg, and §6's last line forbids deleting a \
             pre-submission check to make a value shareable",
            "the state itself is the best-founded in the model: chain identity is the one \
             category whose ownership this build proves",
            "so the refusal is about the check, not about the state — which is exactly the \
             distinction §6 asks for",
        ],
        anchors: &[
            code(
                "crates/execution/src/sequence.rs",
                "gate — endpoint chain id",
                "the leg, by the name the run records",
            ),
            record(
                "data/evidence/m8/cross-stage/reuse-candidates.json",
                "\"step 1: gate — endpoint chain id\"",
                "the measured consumer role on the build side",
            ),
        ],
    },
    StageEdge {
        id: "s6.nonce.preflight_to_build",
        section: "6",
        producer_stage: "preflight",
        consumer_stage: "build",
        state_kind: StateKind::Nonce.as_str(),
        method: Some("eth_getTransactionCount"),
        producer_caller: Some("pending and latest nonces"),
        consumer_caller: Some("step 1: pending and latest nonces"),
        measured_pairs: 3,
        question: "is preflight's nonce a snapshot the build may spend?",
        answers: EdgeAnswers {
            what_the_producer_read: "eth_getTransactionCount for the sender, pending and latest \
                                     in one call, stamped `pending and latest nonces`",
            what_the_consumer_read: "the same ask again at the step it is about to send, stamped \
                                     `step 1: pending and latest nonces`",
            same_semantics: Some(true),
            safety_constraint_served: "the §26 nonce leg — the number the node will accept next",
            producer_result_carried: Some(false),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(true),
            version_or_token_exists: Some(false),
            minimal_addition: "an attempt-scoped nonce reservation, which this build does not \
                               have and §3 forbids adding",
        },
        contract: ProofStatus::Proven,
        decision: EdgeDecision::MustRefetch,
        reasons: &[
            "the code states its own rule: re-read per send",
            "any mined or accepted transaction moves the answer between the two asks, and \
             nothing detects that except asking",
            "the nonce an intent carries is the simulation's assumption about the account, not \
             the node's next number",
        ],
        anchors: &[
            code(
                "crates/execution/src/giwa/preflight_facts.rs",
                "pending and latest nonces",
                "the preflight half of the pair",
            ),
            code(
                "crates/execution/src/sequence.rs",
                "pending and latest nonces",
                "the build half, at its own step",
            ),
            code(
                "crates/execution/src/nonce.rs",
                "pub fn next(&self) -> u64 {",
                "the pending view this leg reads",
            ),
            record(
                "data/evidence/m8/cross-stage/reuse-candidates.json",
                "\"step 1: pending and latest nonces\"",
                "the measured pair",
            ),
        ],
    },
    StageEdge {
        id: "s6.native_balance.preflight_to_before_snapshot",
        section: "6",
        producer_stage: "preflight",
        consumer_stage: "build",
        state_kind: StateKind::NativeBalance.as_str(),
        method: Some("eth_getBalance"),
        producer_caller: Some("native balance of the sender"),
        consumer_caller: Some("before-snapshot: native and input-token balances"),
        measured_pairs: 3,
        question: "the build asks the balance twice; may the preflight answer serve either?",
        answers: EdgeAnswers {
            what_the_producer_read: "eth_getBalance for the sender at the head preflight had \
                                     just read, stamped `native balance of the sender`",
            what_the_consumer_read: "eth_getBalance again inside the before-snapshot — one of \
                                     the build's two balance asks, the other being the gate leg \
                                     below — stamped `before-snapshot: native and input-token \
                                     balances`",
            same_semantics: Some(false),
            safety_constraint_served: "not a gate: the before-snapshot is the baseline the real \
                                       asset delta is measured against afterwards",
            producer_result_carried: Some(false),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(true),
            version_or_token_exists: Some(false),
            minimal_addition: "an owner for a balance between stages, plus a field saying which \
                               question an answer answers — §4's balance row names both",
        },
        contract: ProofStatus::PartiallyProven,
        decision: EdgeDecision::ContractDesignFirst,
        reasons: &[
            "in all three runs both asks name the same height, and they still ask different \
             questions: can it pay, versus what did it hold",
            "nothing holds a balance after its read returns, so sharing would mean inventing a \
             holder rather than reading one",
            "this row is not covered by §6's ban — the before-snapshot is an audit baseline, not \
             a pre-submission check — which is why it lands on B while the gate row below lands \
             on C",
        ],
        anchors: &[
            code(
                "crates/execution/src/giwa/preflight_facts.rs",
                "native balance of the sender",
                "the preflight stamp",
            ),
            code(
                "crates/execution/src/sequence.rs",
                "before-snapshot: native",
                "the build-side half of a diff, not a check",
            ),
            record(
                "data/evidence/m8/cross-stage/reuse-candidates.json",
                "\"native balance of the sender\"",
                "the measured producer caller",
            ),
        ],
    },
    StageEdge {
        id: "s6.fee.preflight_to_build",
        section: "6",
        producer_stage: "preflight",
        consumer_stage: "build",
        state_kind: StateKind::FeeParameters.as_str(),
        method: Some("eth_maxPriorityFeePerGas"),
        producer_caller: Some("fee at pinned block"),
        consumer_caller: Some("step 1: fee at pinned block"),
        measured_pairs: 3,
        question: "may the build price itself from preflight's fee answer?",
        answers: EdgeAnswers {
            what_the_producer_read: "eth_maxPriorityFeePerGas, stamped `fee at pinned block`",
            what_the_consumer_read: "eth_maxPriorityFeePerGas again at the step, stamped \
                                     `step 1: fee at pinned block`",
            same_semantics: Some(true),
            safety_constraint_served: "the §26 fee leg — a moved fee changes the profit the \
                                       sequence is being sent for",
            producer_result_carried: Some(false),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(true),
            version_or_token_exists: Some(false),
            minimal_addition: "a stated validity window — no expiry, deadline or TTL field \
                               exists in any crate",
        },
        contract: ProofStatus::Proven,
        decision: EdgeDecision::MustRefetch,
        reasons: &[
            "the method carries no block term at all, so an answer is a sample of the moment \
             the node served it",
            "the leg exists to catch a moved fee; handing it the moved number leaves it \
             nothing to compare",
            "this is a proven negative, not an unknown: the asks are identical and the \
             conclusion is that the second one is the check",
        ],
        anchors: &[
            code(
                "crates/execution/src/giwa/preflight_facts.rs",
                "fee at pinned block",
                "the preflight half of the pair",
            ),
            code(
                "crates/execution/src/sequence.rs",
                "fee at pinned block",
                "the build half, inside the leg",
            ),
            code(
                "crates/execution/src/fee.rs",
                "pub struct FeeReading",
                "the value that does carry a block identity, next to the ask that does not",
            ),
        ],
    },
    StageEdge {
        id: "s6.fee.intra_preflight_head_pair",
        section: "6",
        producer_stage: "preflight",
        consumer_stage: "preflight",
        state_kind: StateKind::FeeParameters.as_str(),
        method: Some("eth_maxPriorityFeePerGas"),
        producer_caller: Some("fee at pinned block"),
        consumer_caller: Some("fee at head"),
        measured_pairs: 3,
        question: "two identical asks inside one stage — is one of them wasted?",
        answers: EdgeAnswers {
            what_the_producer_read: "eth_maxPriorityFeePerGas for the pinned block",
            what_the_consumer_read: "eth_maxPriorityFeePerGas for the head, stamped \
                                     `fee at head`",
            same_semantics: Some(false),
            safety_constraint_served: "the head-versus-pin fee comparison inside preflight",
            producer_result_carried: Some(false),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(true),
            version_or_token_exists: Some(false),
            minimal_addition: "nothing: the pair is the mechanism, and §6 asks that it not be \
                               traded away",
        },
        contract: ProofStatus::Proven,
        decision: EdgeDecision::MustRefetch,
        reasons: &[
            "the two reads are the two halves of one check; removing either removes the check",
            "M8.4.2 counted this as a duplicate because the asks are identical, not because \
             they are redundant — the record cannot tell the two apart and this row says which \
             is which",
            "a same-stage pair is the cheapest reuse question in the model and still comes out \
             as C, which is the clearest evidence that duplicate counts do not imply waste",
        ],
        anchors: &[
            code(
                "crates/execution/src/giwa/preflight_facts.rs",
                "fee at head",
                "the head half of the comparison",
            ),
            record(
                "data/evidence/m8/cross-stage/reuse-candidates.json",
                "\"fee at head\"",
                "the measured same-stage pair",
            ),
        ],
    },
    StageEdge {
        id: "s6.header.nonce_track_tag_preflight_to_build",
        section: "6",
        producer_stage: "preflight",
        consumer_stage: "build",
        state_kind: StateKind::BlockHeader.as_str(),
        method: Some("eth_getBlockByNumber"),
        producer_caller: Some("head at latest"),
        consumer_caller: Some("step 1: pending and latest nonces"),
        measured_pairs: 3,
        question: "the header asks on the nonce track: does a carried `latest` mean the same block?",
        answers: EdgeAnswers {
            what_the_producer_read: "eth_getBlockByNumber for `latest`, stamped `head at latest`",
            what_the_consumer_read: "eth_getBlockByNumber for `latest` and `pending` again at \
                                     the step, to resolve the tags its nonce leg asks in",
            same_semantics: Some(false),
            safety_constraint_served: "resolving the tags the nonce leg speaks, at the moment \
                                       that leg runs",
            producer_result_carried: Some(false),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(true),
            version_or_token_exists: Some(false),
            minimal_addition: "a record of which height a tag resolved to — neither ask names \
                               a height, so no shared value would carry an identity a consumer \
                               could check",
        },
        contract: ProofStatus::Proven,
        decision: EdgeDecision::MustRefetch,
        reasons: &[
            "both sides ask a tag, and a tag is a moment rather than an identity — §10's \
             `latest is not an explicit height` is this row",
            "the height `latest` named at preflight can be a different height at the build, \
             and neither record says which it got",
            "these are 6 of M8.4.2's 21 header candidates, and they are not the family the \
             header rows below decide about",
        ],
        anchors: &[
            code(
                "crates/execution/src/giwa/preflight_facts.rs",
                "head at latest",
                "the tag the preflight side asks",
            ),
            code(
                "crates/chain/src/rpc.rs",
                "fn block_param",
                "where a tag becomes the term the node sees",
            ),
            record(
                "data/evidence/m8/cross-stage/reuse-candidates.json",
                "\"head at latest\"",
                "the measured tag-against-tag pair",
            ),
        ],
    },
    StageEdge {
        id: "s6.header.nonce_track_tag_intra_preflight",
        section: "6",
        producer_stage: "preflight",
        consumer_stage: "preflight",
        state_kind: StateKind::BlockHeader.as_str(),
        method: Some("eth_getBlockByNumber"),
        producer_caller: Some("head at latest"),
        consumer_caller: Some("pending and latest nonces"),
        measured_pairs: 3,
        question: "inside preflight, could one resolved height serve both asks?",
        answers: EdgeAnswers {
            what_the_producer_read: "eth_getBlockByNumber for `latest`, the head preflight \
                                     reports its other reads at",
            what_the_consumer_read: "the same tag asked again for the nonce leg, stamped \
                                     `pending and latest nonces`",
            same_semantics: Some(true),
            safety_constraint_served: "nothing of its own: the second ask is the same question \
                                       asked again a few calls later",
            producer_result_carried: Some(false),
            consumer_has_own_duty: Some(false),
            external_change_between: Some(true),
            version_or_token_exists: Some(false),
            minimal_addition: "a per-run note of the height `latest` resolved to, kept by \
                               whoever stamps the ask — that one field would make the second \
                               ask checkable rather than merely equal",
        },
        contract: ProofStatus::PartiallyProven,
        decision: EdgeDecision::ContractDesignFirst,
        reasons: &[
            "inside one stage's own run the ownership question does not arise, so this is the \
             one measured pair in the table whose blocker is purely a missing record",
            "it is still not safe today: two asks a few calls apart can straddle a block, and \
             nothing says which height either got",
            "§3 forbids widening M8.3.1's cache to cover it, so the finding stays a design item",
        ],
        anchors: &[
            code(
                "crates/execution/src/giwa/preflight_facts.rs",
                "pending and latest nonces",
                "the second ask, in the same stage",
            ),
            code(
                "crates/simulation/src/state.rs",
                "struct StateReadCache {",
                "the cache whose scope §3 forbids changing",
            ),
        ],
    },
    StageEdge {
        id: "s6.header.observation_to_build_fee_input",
        section: "6",
        producer_stage: "observation",
        consumer_stage: "build",
        state_kind: StateKind::BlockHeader.as_str(),
        method: Some("eth_getBlockByNumber"),
        producer_caller: Some("head and header that fixed the pin"),
        consumer_caller: Some("step 1: fee at pinned block"),
        measured_pairs: 3,
        question: "may the build's fee input be handed over instead of re-read?",
        answers: EdgeAnswers {
            what_the_producer_read: "eth_getBlockByNumber at the pinned height, the read that \
                                     fixed the pin",
            what_the_consumer_read: "eth_getBlockByNumber at the same height, for the base fee \
                                     the step prices against",
            same_semantics: Some(true),
            safety_constraint_served: "the header here is an input to a computation, not a \
                                       check — the check is the fee comparison and the binding \
                                       leg, both separate rows",
            producer_result_carried: Some(false),
            consumer_has_own_duty: Some(false),
            external_change_between: Some(false),
            version_or_token_exists: Some(true),
            minimal_addition: "a carrier that moves number and hash together, and a consumer \
                               that verifies the hash against its pin before using the value",
        },
        contract: ProofStatus::Proven,
        decision: EdgeDecision::ControlledExperimentDefinable,
        reasons: &[
            "this is the only family in the model where identity, freshness and ownership are \
             all proven: a numbered block is settled, and §4's header row shows three \
             production points that verify it",
            "what is missing is a field, not a rule — `PreflightReport`, the only type that \
             crosses the boundary, carries a verdict and free text and no state value",
            "A here is a decision about the next phase and not a claim of safety today: \
             `safe_to_reuse_now` stays false for all 42 measured candidates",
        ],
        anchors: &[
            code(
                "crates/simulation/src/state.rs",
                "pub struct BlockPin",
                "the number-plus-hash a carrier would move",
            ),
            code(
                "crates/execution/src/preflight.rs",
                "pub struct PreflightReport",
                "the crossing type, and why it cannot hold the value today",
            ),
            code(
                "crates/execution/src/sequence.rs",
                "fee at pinned block",
                "the consumer that would receive it",
            ),
        ],
    },
    StageEdge {
        id: "s6.header.observation_to_build_binding_gate",
        section: "6",
        producer_stage: "observation",
        consumer_stage: "build",
        state_kind: StateKind::BlockHeader.as_str(),
        method: Some("eth_getBlockByNumber"),
        producer_caller: Some("head and header that fixed the pin"),
        consumer_caller: Some("step 1: gate — block binding at pin"),
        measured_pairs: 3,
        question: "same state, same height — so may the binding leg consume it?",
        answers: EdgeAnswers {
            what_the_producer_read: "eth_getBlockByNumber at the pinned height, once, when the \
                                     pin was fixed",
            what_the_consumer_read: "eth_getBlockByNumber at the same height again, to learn \
                                     whether the node still calls that height the same block",
            same_semantics: Some(true),
            safety_constraint_served: "the §26 block-binding leg: a reorganisation between the \
                                       pin and the build",
            producer_result_carried: Some(false),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(true),
            version_or_token_exists: Some(true),
            minimal_addition: "none: the leg's authority is that it is an independent read at \
                               the moment it guards",
        },
        contract: ProofStatus::Proven,
        decision: EdgeDecision::MustRefetch,
        reasons: &[
            "a handed-over header would make the comparison check the pin against an answer \
             taken before the pin was questioned",
            "§6's ban on deleting a pre-submission check decides this row whatever the row \
             above concludes — and the two rows differ only in which consumer asked",
            "this is §6's clearest demonstration that one state can be shareable for one \
             consumer and unshareable for another",
        ],
        anchors: &[
            code(
                "crates/execution/src/sequence.rs",
                "gate — block binding at pin",
                "the leg",
            ),
            code(
                "crates/execution/src/gate.rs",
                "pub enum BlockBinding",
                "Confirmed, Reorged, Unverified — the verdict the leg produces",
            ),
            code(
                "crates/execution/src/chain_read.rs",
                "fn read_binding",
                "the read that produces it",
            ),
            record(
                "data/evidence/m8/cross-stage/reuse-candidates.json",
                "\"step 1: gate — block binding at pin\"",
                "the measured consumer role",
            ),
        ],
    },
    StageEdge {
        id: "s6.header.observation_to_preflight_binding",
        section: "6",
        producer_stage: "observation",
        consumer_stage: "preflight",
        state_kind: StateKind::BlockHeader.as_str(),
        method: Some("eth_getBlockByNumber"),
        producer_caller: Some("head and header that fixed the pin"),
        consumer_caller: Some("block binding at pin"),
        measured_pairs: 3,
        question: "preflight binds the pin too; is that a second check or a duplicate read?",
        answers: EdgeAnswers {
            what_the_producer_read: "eth_getBlockByNumber at the pinned height, when the pin \
                                     was fixed",
            what_the_consumer_read: "eth_getBlockByNumber at the same height, as preflight's \
                                     own binding fact, stamped `block binding at pin`",
            same_semantics: Some(true),
            safety_constraint_served: "preflight's §26 binding leg — the pin is still the block \
                                      it was when the finding was priced",
            producer_result_carried: Some(false),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(true),
            version_or_token_exists: Some(true),
            minimal_addition: "nothing on this edge either: a check that consumes an earlier \
                               copy of its own answer is not a check",
        },
        contract: ProofStatus::Proven,
        decision: EdgeDecision::MustRefetch,
        reasons: &[
            "preflight and the build each bind the pin at their own moment, and those moments \
             are the point of the two legs",
            "the pair is identical in the record and different in purpose, so §6's \
             per-item rule keeps this row separate from the fee-input row",
        ],
        anchors: &[
            code(
                "crates/execution/src/giwa/preflight_facts.rs",
                "block binding at pin",
                "preflight's binding stamp",
            ),
            code(
                "crates/execution/src/giwa/sequencer_direct.rs",
                "fn base_fee_at",
                "the sibling read taken at the same pin",
            ),
        ],
    },
    StageEdge {
        id: "s6.header.observation_to_preflight_fee_input",
        section: "6",
        producer_stage: "observation",
        consumer_stage: "preflight",
        state_kind: StateKind::BlockHeader.as_str(),
        method: Some("eth_getBlockByNumber"),
        producer_caller: Some("head and header that fixed the pin"),
        consumer_caller: Some("fee at pinned block"),
        measured_pairs: 3,
        question: "the same header feeds preflight's fee pair; same conclusion as the build's?",
        answers: EdgeAnswers {
            what_the_producer_read: "eth_getBlockByNumber at the pinned height, once",
            what_the_consumer_read: "eth_getBlockByNumber at the same height for the base fee \
                                     the fee pair compares",
            same_semantics: Some(true),
            safety_constraint_served: "none by itself: the fee comparison is the check, and \
                                       this read is an input to it",
            producer_result_carried: Some(false),
            consumer_has_own_duty: Some(false),
            external_change_between: Some(false),
            version_or_token_exists: Some(true),
            minimal_addition: "the same carrier the build-side row names — one field serving \
                               both consumers",
        },
        contract: ProofStatus::Proven,
        decision: EdgeDecision::ControlledExperimentDefinable,
        reasons: &[
            "with the fee-input leg in both stages, the experiment's shape is fixed: one \
             header per pinned height, verified by whichever consumer uses it",
            "the binding legs stay independent reads, so the experiment never touches a check",
            "the pair is 3 of the 15 numbered-height header candidates M8.4.2 measured",
        ],
        anchors: &[
            code(
                "crates/execution/src/giwa/preflight_facts.rs",
                "fee at pinned block",
                "the preflight consumer of the same header",
            ),
            code(
                "crates/pipeline/src/sim.rs",
                "state_version_mismatch",
                "the plan-side check that a height and a hash must both still agree",
            ),
        ],
    },
    StageEdge {
        id: "s7.header.observation_to_simulation_pin_read",
        section: "7",
        producer_stage: "observation",
        consumer_stage: "simulation",
        state_kind: StateKind::BlockHeader.as_str(),
        method: Some("eth_getBlockByNumber"),
        producer_caller: Some("head and header that fixed the pin"),
        consumer_caller: Some("header at pin"),
        measured_pairs: 3,
        question: "the simulation's own header at pin: an extra read or the pin's evidence?",
        answers: EdgeAnswers {
            what_the_producer_read: "eth_getBlockByNumber at the pinned height, from the \
                                     observation that fixed the pin",
            what_the_consumer_read: "eth_getBlockByNumber at the same height, as the first of \
                                     the 39 calls one simulation makes, stamped `header at pin`",
            same_semantics: Some(true),
            safety_constraint_served: "the provider round trip's pin check: the run is about \
                                       the block it claims to be about",
            producer_result_carried: Some(false),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(true),
            version_or_token_exists: Some(true),
            minimal_addition: "nothing: this read is how the simulation learns its pin held",
        },
        contract: ProofStatus::Proven,
        decision: EdgeDecision::MustRefetch,
        reasons: &[
            "the check compares the pin the request carries against what the node answers at \
             that height now; a handed-over answer removes the second half",
            "§7 asks whether the simulation re-fetches canonical state: this row is one of the \
             39 calls that are the answer",
            "the header family therefore splits three ways on this edge: A for a fee input, C \
             for two binding legs, C for the simulation's pin",
        ],
        anchors: &[
            code(
                "crates/simulation/src/request.rs",
                "fn check_pin",
                "the round trip that compares number and hash",
            ),
            record(
                "data/evidence/m8/cross-stage/reuse-candidates.json",
                "\"header at pin\"",
                "the measured simulation caller",
            ),
        ],
    },
    // -- §6 B: the item on the list that turns out not to be a read ------------------
    StageEdge {
        id: "s6.gas_limit.not_a_chain_read",
        section: "6",
        producer_stage: "preflight",
        consumer_stage: "build",
        state_kind: StateKind::FeeParameters.as_str(),
        method: None,
        producer_caller: None,
        consumer_caller: None,
        measured_pairs: 0,
        question: "does either stage pay the node for a gas limit or an estimate?",
        answers: EdgeAnswers {
            what_the_producer_read: "nothing: preflight stamps no gas ask in any of the three \
                                     runs",
            what_the_consumer_read: "nothing from the node: the limit is resolved from the \
                                     simulation's measured gas plus a margin, then bounded by a \
                                     configured ceiling and re-checked against the measurement",
            same_semantics: None,
            safety_constraint_served: "the ceiling keeps a limit inside a block a node could \
                                       actually include",
            producer_result_carried: None,
            consumer_has_own_duty: Some(true),
            external_change_between: None,
            version_or_token_exists: None,
            minimal_addition: "nothing: there is no chain answer on this edge to carry",
        },
        contract: ProofStatus::NotApplicable,
        decision: EdgeDecision::MustRefetch,
        reasons: &[
            "§6 lists gas limit or gas estimate as an item to analyse, and the analysis is that \
             it is not a state read here — that is a conclusion, not a gap",
            "the three runs asked nine methods in 246 calls; no eth_estimateGas and no \
             eth_gasPrice is among them, which the recompute gate re-derives rather than quotes",
            "C on this row means there is nothing on the edge to reuse, not that a read has to \
             be kept: §17 offers no not-applicable option, so the row says which half of C it \
             means",
        ],
        anchors: &[
            code(
                "crates/execution/src/builder.rs",
                "policy.gas.resolve(policy.simulated_gas_used)",
                "the limit comes from this run's own measurement",
            ),
            code(
                "crates/execution/src/builder.rs",
                "maximum_gas_limit: 60_000_000",
                "the ceiling, a configured number from one measured real block",
            ),
            record(
                "data/evidence/m8/cross-stage/runs/route-91342-37700740-1791045857463/rpc-summary.json",
                "\"eth_getCode\"",
                "one run's method list; the gate test asserts this list holds no estimate and \
                 no gas price",
            ),
        ],
    },
    // -- §7: opportunity → simulation -------------------------------------------------
    StageEdge {
        id: "s7.pool_reserves.finding_to_simulation",
        section: "7",
        producer_stage: "opportunity",
        consumer_stage: "simulation",
        state_kind: StateKind::PoolReserves.as_str(),
        method: None,
        producer_caller: None,
        consumer_caller: None,
        measured_pairs: 0,
        question: "may the simulation run on the reserves the finding was priced from?",
        answers: EdgeAnswers {
            what_the_producer_read: "no RPC: a finding is priced from pool states the graph \
                                     holds, each restated at an applied position",
            what_the_consumer_read: "code, storage slots, balances and nonces for itself at the \
                                     pinned height — 38 of the 39 calls one simulation makes",
            same_semantics: Some(false),
            safety_constraint_served: "the simulation must be about the state a real node holds \
                                       at the pin, not about a view derived from logs",
            producer_result_carried: Some(true),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(true),
            version_or_token_exists: Some(true),
            minimal_addition: "nothing: §7 forbids treating the graph as a REVM state source, \
                               and the pin check that guards the difference already exists",
        },
        contract: ProofStatus::Proven,
        decision: EdgeDecision::MustRefetch,
        reasons: &[
            "a GraphSnapshot is built from logs the store applied; it carries an applied \
             position and no state root, no storage, no code",
            "one block number does not prove one state — §10's first inequality is this row, \
             and the finding does carry the hash that closes it",
            "what crosses the edge is the identity and the path, never the state itself",
        ],
        anchors: &[
            code(
                "crates/state/src/store.rs",
                "pub struct InMemoryStateStore",
                "where the log-derived view lives",
            ),
            code(
                "crates/graph/src/snapshot.rs",
                "pub struct GraphSnapshot {",
                "the view a finding is priced from, with no state root in it",
            ),
            code(
                "crates/simulation/src/state.rs",
                "get_code(key.block, key.address)",
                "the canonical read that replaces it",
            ),
            code(
                "crates/pipeline/src/sim.rs",
                "state_version_mismatch",
                "the check that keeps the two views from being confused",
            ),
            record(
                "data/evidence/m8/cross-stage/runs/route-91342-37700740-1791045857463/rpc-summary.json",
                "\"eth_getStorageAt\"",
                "the bulk of one simulation's 39 calls",
            ),
        ],
    },
    StageEdge {
        id: "s7.block_identity.finding_pin_is_verified",
        section: "7",
        producer_stage: "opportunity",
        consumer_stage: "simulation",
        state_kind: StateKind::BlockHeader.as_str(),
        method: None,
        producer_caller: None,
        consumer_caller: None,
        measured_pairs: 0,
        question: "is the identity a finding carries enough to describe the state it came from?",
        answers: EdgeAnswers {
            what_the_producer_read: "an applied position: the height the ledger stopped at, and \
                                     the hash recorded beside it",
            what_the_consumer_read: "a pin verified by number and by hash at three separate \
                                     points — dispatch, plan, provider round trip",
            same_semantics: None,
            safety_constraint_served: "the pin is what makes a simulation about a past block a \
                                       claim about that block",
            producer_result_carried: Some(true),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(true),
            version_or_token_exists: Some(true),
            minimal_addition: "nothing for the identity; the open item is a value that carries \
                               number and hash together to a consumer that wants a header",
        },
        contract: ProofStatus::Proven,
        decision: EdgeDecision::MustRefetch,
        reasons: &[
            "a height alone is not an identity — a reorganisation replaces one block with \
             another at the same number; a height plus a hash is, and the code enforces the pair",
            "this is the finding that licenses §6's two header rows: an experiment may share a \
             header, never a bare height",
            "C on this row is the half of §7 that says the pin's own read stays wherever a \
             consumer verifies it — the carrier question is already recorded on the edges that \
             have a shareable consumer",
        ],
        anchors: &[
            code(
                "crates/opportunity/src/lifecycle.rs",
                "pub pinned_block_hash: B256,",
                "the hash a finding carries, not only its height",
            ),
            code(
                "crates/pipeline/src/runner.rs",
                "state_unavailable",
                "the dispatch check",
            ),
            code(
                "crates/simulation/src/request.rs",
                "fn check_pin",
                "the provider round trip's check",
            ),
        ],
    },
    StageEdge {
        id: "s7.opportunity_staleness.decided_before_repricing",
        section: "7",
        producer_stage: "opportunity",
        consumer_stage: "build",
        state_kind: StateKind::TransactionIntent.as_str(),
        method: None,
        producer_caller: None,
        consumer_caller: None,
        measured_pairs: 0,
        question: "after a finding, does a state change mean expiry, re-simulation or re-detection?",
        answers: EdgeAnswers {
            what_the_producer_read: "the finding's state version against the ledger's applied \
                                     position",
            what_the_consumer_read: "preflight and the gate legs re-read at their own moments, \
                                     and the intent carries the identities of the run it came from",
            same_semantics: None,
            safety_constraint_served: "§7's distinction between favourable at detection and \
                                       still favourable at submission",
            producer_result_carried: Some(true),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(true),
            version_or_token_exists: Some(true),
            minimal_addition: "nothing: expiry, re-simulation and re-detection are already \
                               three separate decisions in code",
        },
        contract: ProofStatus::Proven,
        decision: EdgeDecision::MustRefetch,
        reasons: &[
            "expiry is decided by the ledger before a simulation is even asked for, so a stale \
             finding never reaches the edge that could be tempted to reuse it",
            "a re-simulation is a new run with a new fingerprint, not a reuse of the old one",
            "still favourable at submission is what the gate legs answer, which is why they \
             stay — §7's question is answered by keeping the stages' separate answers",
        ],
        anchors: &[
            code(
                "crates/opportunity/src/lifecycle.rs",
                "pub fn simmable(",
                "the ledger's own decision point",
            ),
            code(
                "crates/execution/src/sequence.rs",
                "fn preflight_cleared",
                "the three checks the build performs on preflight's verdict",
            ),
            code(
                "crates/execution/src/gate.rs",
                "pub enum Freshness",
                "the vocabulary for still-good-at-submission",
            ),
        ],
    },
    // -- §8: state update → simulation ------------------------------------------------
    StageEdge {
        id: "s8.pool_reserves.snapshot_lacks_a_hash",
        section: "8",
        producer_stage: "state_store",
        consumer_stage: "graph",
        state_kind: StateKind::PoolReserves.as_str(),
        method: None,
        producer_caller: None,
        consumer_caller: None,
        measured_pairs: 0,
        question: "does the path from an event to a snapshot produce something a consumer can \
                   bind to, and do replay and live travel it together?",
        answers: EdgeAnswers {
            what_the_producer_read: "an event applied to the store through the one entry point \
                                     the store has, ordered by the update path",
            what_the_consumer_read: "a snapshot carrying the position it applied to, rebuilt \
                                     each block",
            same_semantics: None,
            safety_constraint_served: "ordering: a snapshot only means something as the \
                                       position it stopped at",
            producer_result_carried: Some(true),
            consumer_has_own_duty: Some(false),
            external_change_between: Some(false),
            version_or_token_exists: Some(false),
            minimal_addition: "a block hash beside the applied position, so a consumer of a \
                               snapshot can state which block it means",
        },
        contract: ProofStatus::PartiallyProven,
        decision: EdgeDecision::ContractDesignFirst,
        reasons: &[
            "the store has exactly one way in and the builder states the position it applied \
             to, and replay and live meet at the same canonical function",
            "what is missing is not determinism but identity: a height with no hash cannot be \
             checked against a node, and the snapshot type has no hash field",
            "so §8's authority matrix records this edge as designable and not yet written \
             down — which is a finding, not a refusal",
        ],
        anchors: &[
            code(
                "crates/state/src/update.rs",
                "The only way anything may enter the store.",
                "the single entry point",
            ),
            code(
                "crates/graph/src/builder.rs",
                "The target block is the snapshot's own applied position",
                "the position a snapshot names",
            ),
            code(
                "crates/pipeline/src/engine.rs",
                "fn on_canonical",
                "where replay and live meet",
            ),
            code(
                "crates/graph/src/snapshot.rs",
                "pub struct GraphSnapshot {",
                "the type that has a height and no hash",
            ),
            record(
                "data/evidence/m8/storage-dependency/dependency-map.json",
                "\"ordered\"",
                "the recorded ordering of one run's reads",
            ),
        ],
    },
    StageEdge {
        id: "s8.canonical_state.store_is_not_an_evm_source",
        section: "8",
        producer_stage: "graph",
        consumer_stage: "simulation",
        state_kind: StateKind::StorageSlot.as_str(),
        method: None,
        producer_caller: None,
        consumer_caller: None,
        measured_pairs: 0,
        question: "which of the states this path maintains may be a simulation input?",
        answers: EdgeAnswers {
            what_the_producer_read: "reserves and fee figures derived from logs, and the pool \
                                     states the graph rebuilt from them",
            what_the_consumer_read: "REVM's canonical reads: code, storage slots, balances and \
                                     nonces at a height, each keyed by that height",
            same_semantics: Some(false),
            safety_constraint_served: "the simulation must execute against what the chain holds, \
                                       so its reads go to the node",
            producer_result_carried: Some(false),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(true),
            version_or_token_exists: Some(true),
            minimal_addition: "nothing: §8 asks for the authority matrix as the output, and the \
                               matrix's answer is that the two sources are not interchangeable",
        },
        contract: ProofStatus::Proven,
        decision: EdgeDecision::MustRefetch,
        reasons: &[
            "the store answers what the pools did; REVM asks what the chain holds — the two \
             questions have different producers, owners and authorities",
            "a simulation's reads are one run's own: the cache is scoped to it and no \
             process-wide map exists, which is what the dependency evidence records",
            "this row is the §8 statement of §7's, seen from the source side rather than the \
             finding side",
        ],
        anchors: &[
            code(
                "crates/simulation/src/state.rs",
                "struct StateReadCache {",
                "the per-simulation scope",
            ),
            code(
                "crates/simulation/src/state.rs",
                "no process-wide map",
                "the code's own words for the boundary",
            ),
            record(
                "data/evidence/m8/storage-dependency/dependency-map.json",
                "\"ordered\"",
                "one run's read order, the evidence for its self-containedness",
            ),
        ],
    },
    // -- §9: simulation → build -------------------------------------------------------
    StageEdge {
        id: "s9.native_balance.build_gate_leg",
        section: "9",
        producer_stage: "simulation",
        consumer_stage: "build",
        state_kind: StateKind::NativeBalance.as_str(),
        method: Some("eth_getBalance"),
        producer_caller: Some("account: sender"),
        consumer_caller: Some("step 1: gate — native balance"),
        measured_pairs: 3,
        question: "the simulation already read the sender's balance; may the gate consume it?",
        answers: EdgeAnswers {
            what_the_producer_read: "eth_getBalance for the sender as one of the simulation's \
                                     canonical reads, stamped `account: sender`",
            what_the_consumer_read: "eth_getBalance again at the pinned height as the §26 \
                                     balance leg, stamped `step 1: gate — native balance`",
            same_semantics: Some(false),
            safety_constraint_served: "the pre-submission balance check: the real wallet can \
                                       pay for this sequence",
            producer_result_carried: Some(false),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(true),
            version_or_token_exists: Some(false),
            minimal_addition: "none that §6 would allow: the leg is a pre-submission check",
        },
        contract: ProofStatus::Proven,
        decision: EdgeDecision::MustRefetch,
        reasons: &[
            "the simulation's read answered whether an endowment could pay inside a run; the \
             gate answers whether the real wallet can pay before sending",
            "§6's ban on deleting a pre-submission check decides this row directly",
            "this is the balance family's third shape: B for the audit baseline, C for the \
             gate leg, C for the nonce track's sibling — three conclusions, one category",
        ],
        anchors: &[
            code(
                "crates/execution/src/sequence.rs",
                "gate — native balance",
                "the leg",
            ),
            code(
                "crates/execution/src/gate.rs",
                "pub enum BalanceEvidence",
                "a per-step verdict rather than a held value",
            ),
            record(
                "data/evidence/m8/cross-stage/reuse-candidates.json",
                "\"step 1: gate — native balance\"",
                "the measured consumer role",
            ),
        ],
    },
    StageEdge {
        id: "s9.transaction_intent.fields_agree_by_construction",
        section: "9",
        producer_stage: "simulation",
        consumer_stage: "build",
        state_kind: StateKind::TransactionIntent.as_str(),
        method: None,
        producer_caller: None,
        consumer_caller: None,
        measured_pairs: 0,
        question: "which simulation-to-build fields are bound by a check rather than by the \
                   code that wrote them?",
        answers: EdgeAnswers {
            what_the_producer_read: "a simulation result, fingerprinted over its own serialised \
                                     fields",
            what_the_consumer_read: "an intent naming that fingerprint and the simulation's \
                                     nonce, gas, calldata and target; the build then compares \
                                     nine fields of its own bytes against the intent",
            same_semantics: None,
            safety_constraint_served: "§14's round trip: the bytes that get signed must \
                                       describe the intent that was approved",
            producer_result_carried: Some(true),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(false),
            version_or_token_exists: Some(true),
            minimal_addition: "a check that re-derives the intent's fields from the simulation \
                               result it names: today nine fields are compared after the build, \
                               against the intent, and none against the simulation",
        },
        contract: ProofStatus::PartiallyProven,
        decision: EdgeDecision::ContractDesignFirst,
        reasons: &[
            "chain id, target, value, calldata, nonce, gas limit, fee parameters and the sender \
             are carried from the run by construction — `from_simulated_step` is the only way \
             an intent is made — so their agreement is a property of one function, not of a check",
            "the identities that do get compared are the opportunity, the simulation \
             fingerprint and the risk decision, which is what makes an intent evidence about a \
             run rather than a claim",
            "B because the missing mechanism is a field and a comparison, not an RPC question",
        ],
        anchors: &[
            code(
                "crates/execution/src/builder.rs",
                "pub fn round_trip",
                "the nine-field comparison, intent against bytes",
            ),
            code(
                "crates/execution/src/intent.rs",
                "pub simulation_id: B256,",
                "the fingerprint that binds an intent to one run",
            ),
            code(
                "crates/execution/src/intent.rs",
                "pub fn from_simulated_step(",
                "the construction that makes agreement automatic rather than verified",
            ),
            test(
                "crates/execution/tests/sequence.rs",
                "the_plan_is_the_run_s_transactions_in_order_and_nothing_else",
                "the existing test that a plan carries only what the run decided",
            ),
        ],
    },
    StageEdge {
        id: "s9.simulation_result.override_dependence_is_refused",
        section: "9",
        producer_stage: "simulation",
        consumer_stage: "build",
        state_kind: StateKind::SimulationResult.as_str(),
        method: None,
        producer_caller: None,
        consumer_caller: None,
        measured_pairs: 0,
        question: "if the run needed a state override, can the real chain reproduce it?",
        answers: EdgeAnswers {
            what_the_producer_read: "whatever the provider was given: canonical reads at the \
                                     pin, plus any override the request carried",
            what_the_consumer_read: "an intent that declares how its sender was funded, and a \
                                     build that refuses an override-funded one before signing",
            same_semantics: Some(false),
            safety_constraint_served: "§33 and §34: no real account could send a transaction \
                                       that only works inside an overridden environment",
            producer_result_carried: Some(true),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(false),
            version_or_token_exists: Some(true),
            minimal_addition: "nothing: the edge is closed by rule, and §9's demand that an \
                               override-dependent run not count as proof of executability is \
                               enforced rather than assumed",
        },
        contract: ProofStatus::Proven,
        decision: EdgeDecision::MustRefetch,
        reasons: &[
            "the refusal is in the builder, names the funding it refused, and fires before any \
             signature is taken",
            "a run that needed an override is evidence about an environment, not about a block \
             — which is exactly what §9 forbids confusing",
            "the intent records its funding instead of inferring it, because a simulation \
             result does not carry the sender's balance",
        ],
        anchors: &[
            code(
                "crates/execution/src/builder.rs",
                "policy.require_unoverridden_state && intent.funding.derived_from_overridden_state()",
                "the refusal",
            ),
            code(
                "crates/simulation/src/state.rs",
                "pub struct StateOverride",
                "the environment the run may have leaned on",
            ),
            code(
                "crates/execution/src/intent.rs",
                "pub enum SenderFunding",
                "the declaration the refusal reads",
            ),
        ],
    },
    StageEdge {
        id: "s9.transaction_intent.multi_step_plan_is_refused",
        section: "9",
        producer_stage: "simulation",
        consumer_stage: "build",
        state_kind: StateKind::TransactionIntent.as_str(),
        method: None,
        producer_caller: None,
        consumer_caller: None,
        measured_pairs: 0,
        question: "does a favourable multi-step run entitle one transaction?",
        answers: EdgeAnswers {
            what_the_producer_read: "a sequence of steps, each executed inside the same \
                                     simulation, where step two only works if step one landed",
            what_the_consumer_read: "one transaction, carrying the step count it came from and a \
                                     sequence position when one exists",
            same_semantics: Some(false),
            safety_constraint_served: "§4: an intent that describes step 1 of a multi-step plan \
                                       without a position has no executor committed to the rest",
            producer_result_carried: Some(true),
            consumer_has_own_duty: Some(true),
            external_change_between: Some(false),
            version_or_token_exists: Some(true),
            minimal_addition: "nothing here either: the refusal exists, and the remaining \
                               question is which executor contract a sequence needs — outside \
                               this milestone",
        },
        contract: ProofStatus::Proven,
        decision: EdgeDecision::MustRefetch,
        reasons: &[
            "a simulation's profit is a property of the whole sequence, so a single \
             transaction's buildability is not implied by it",
            "the build refuses on the step count and the absent position, naming both",
            "this is §9's field-by-field answer for route and swap steps: they are carried, and \
             carrying them is not the same as being allowed to send one",
        ],
        anchors: &[
            code(
                "crates/execution/src/builder.rs",
                "describes step 1 of a {}-step sequence",
                "the refusal and the field it reads",
            ),
            code(
                "crates/execution/src/intent.rs",
                "pub simulated_steps: usize,",
                "the count that makes the refusal possible",
            ),
        ],
    },
];

/// The edges, in table order: §6's measured pairs first, then §7, §8 and §9.
pub fn stage_edges() -> &'static [StageEdge] {
    &EDGES
}

#[cfg(test)]
mod tests;
