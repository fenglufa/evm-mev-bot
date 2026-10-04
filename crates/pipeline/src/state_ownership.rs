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
    /// Not determinable from the code.
    Unscoped,
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
            Scope::Unscoped => "unknown",
        }
    }
}

/// §4's Authority: which source wins when two disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
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
}

/// §5's five lifecycle steps, in the order the task book writes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
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

/// The block term one side of a pair used. `Number` and `Absent` are M8.4.2's own two forms;
/// the tag form is split here because §5 asks about `latest` and `pending` separately, and the
/// record's raw term answers that even though its `block_form` column does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum BlockTerm {
    /// An explicit height: one number names one block, though not which of two rival blocks
    /// at that height the node considers canonical.
    Number,
    /// `latest` — whichever head the node held when it answered.
    Latest,
    /// `pending` — a head that includes transactions not yet in any canonical block.
    Pending,
    /// No block term at all: the answer is a property of the endpoint's current state.
    Absent,
    /// A tag the record names but this model does not recognise; treated as a tag.
    Unknown,
}

impl BlockTerm {
    pub const fn as_str(self) -> &'static str {
        match self {
            BlockTerm::Number => "number",
            BlockTerm::Latest => "latest",
            BlockTerm::Pending => "pending",
            BlockTerm::Absent => "absent",
            BlockTerm::Unknown => "unknown_tag",
        }
    }

    pub const fn names_a_height(self) -> bool {
        matches!(self, BlockTerm::Number)
    }

    /// The record's pair of columns, resolved: `block_form` says which *kind* of term was sent
    /// and the raw term says which tag, when it was a tag.
    pub fn from_record(form: &str, raw: Option<&str>) -> Self {
        match form {
            "number" => BlockTerm::Number,
            "absent" => BlockTerm::Absent,
            _ => match raw {
                Some("pending") => BlockTerm::Pending,
                Some("latest") => BlockTerm::Latest,
                Some(_) | None => BlockTerm::Unknown,
            },
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
        producer_term: BlockTerm::from_record(
            &measured.producer_block_form,
            measured.producer_block.as_deref(),
        ),
        consumer_term: BlockTerm::from_record(
            &measured.consumer_block_form,
            measured.consumer_block.as_deref(),
        ),
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

#[cfg(test)]
mod tests;
