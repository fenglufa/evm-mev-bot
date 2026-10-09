//! M12-B §5: the state-invalidation policy for one node restarting, reconnecting, or
//! rolling its head back.
//!
//! # What this file is
//!
//! §5 asks for 「明确的状态失效策略」 over a list of state it then enumerates, and it names
//! three node events: 重启 / 重新连接 / 链头倒退. A policy that only says what *should*
//! happen is a wish, so this file states the rule per (state class × event) and points at
//! the production line that already enforces it — a file plus a token, resolved to a line
//! number when the table is assembled, never hand-written.
//!
//! 八类状态 × 三个事件 = 二十四条, and every cell is a row: [`StateClass`] is §5's own list
//! in §5's own order, [`NodeEvent`] is its three events. `ALL_CLASSES × ALL_EVENTS` is
//! compared against the table by [`every_class_meets_every_event_once`], so a new state
//! class cannot arrive without a ruling and an existing class cannot lose one.
//!
//! # What the table found
//!
//! The repository already enforces §5's four 基本原则, and it does it in three places
//! rather than one:
//!
//! - **The decision is never carried over.** Every attempt re-asks the endpoint for the
//!   block its intent pins (`read_binding` in `crates/execution/src/stage.rs`), and
//!   a binding the node cannot confirm — error, missing height, or a different hash —
//!   blocks the gate. That is 「不能仅因为重新连上 RPC 就继续执行」 in the only form that
//!   matters, because the gate is the last thing a send passes through.
//!   `crates/execution/tests/stage_matrix.rs`'s `each_attempt_asks_the_chain_for_its_pin_again`
//!   counts those re-reads.
//! - **A hole is never a zero.** A canonical source that stops filling blocks ends the
//!   session (`PipelineError::GapUnrecovered`); readiness is asked again at bootstrap
//!   before any block is read.
//! - **Bytes that left are not failures and not resends.** A submission answer that leaves
//!   the transaction possibly in flight holds the lane and keeps the record off the rung the
//!   node never granted, and the release path asks `may_be_in_flight` before freeing a
//!   number or a unit of capital. That is §5's 「沿用现有 transaction hash / receipt 生命周期」.
//! - **Two claims, one refusal.** A nonce pair another lane holds is refused by name
//!   (`LaneRefusal::NonceHeld`), as is capital the arithmetic no longer has
//!   (`InsufficientCapital`), so 「恢复后重复占用同一 nonce 或同一笔资本」 has no path.
//!
//! # The cells this table does *not* claim to enforce
//!
//! §5 also asks for 失效, and there are two places where the honest answer is 「the event is
//! recorded, not acted on」 or 「no node event reaches this state at all」, plus one rule that
//! is deliberately only a metric:
//!
//! - `CanonicalityConflict` keeps both hashes and discards nothing — v0.1 states that it
//!   does not resolve reorgs. The row is graded `recorded_only`, and the compensation is
//!   named in it: the block-identity rows are what actually stop a send.
//! - The simulation state cache is keyed by chain, height and address — not by the header
//!   hash — so a rollback landing *between* the pin check and a later cached read is not
//!   detected by the cache. `not_enforced`, with the reason and the compensating control
//!   written into the row. This is a residual risk of the v0.1 design, recorded rather than
//!   papered over; §9 and §10 of the milestone forbid reworking that cache here.
//! - `SourceStatus::Failed` on a canonical source bumps `canonical_source_failed_during_run`
//!   and does not stop the session. That is reviewed and kept: §3 requires that one source's
//!   failure must not read as "no opportunities", and the rule that does stop a run is the
//!   gap rule, which the first row of the table names.
//!
//! The two graded-not-enforced rows are pinned by [`the_rows_that_are_not_enforced_are_exactly_the_named_ones`]:
//! an unenforced row cannot be added quietly, because a new row with one of those grades
//! fails the test until it is named.
//!
//! # Prohibitions, tested rather than promised
//!
//! §5 forbids 清空全部状态 as a strategy, forbids treating a committed transaction as a
//! failure or a resend, and forbids double occupancy of a nonce or capital. The first is
//! tested as a *scan*: the execution crate's production source contains no reference to a
//! transport event at all, which is what "no blanket clear is wired to a node event" means
//! in code — the lane is node-event-blind, and the only inputs that reach it are answers.
//! The other two are tested against the refusal arms and against `may_be_in_flight`.
//!
//! # No new RPC, no real node
//!
//! This file opens no socket. §5 keeps real-node restart verification as a follow-up
//! infrastructure item, so every claim here is about code that a reader can open, and
//! `SELF_HOSTED_NODE = NOT_RUN` still holds.
//!
//! # How the evidence file changes
//!
//! `M12B_NODE_RESET_POLICY_REFRESH=1` re-assembles `data/evidence/m12/b/node-reset-policy.json`
//! from this table plus the current source, resolving each anchor to its line. The committed
//! file's identity is `(file, token)` — a pair that must still hit exactly one production
//! line — so the check outlives a line shift; the recorded line number is display only.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// vocabulary
// ---------------------------------------------------------------------------

/// §5's three node events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NodeEvent {
    /// The process behind the endpoint went away and came back with a cold cache.
    Restart,
    /// The transport dropped and was re-established against the same process.
    Reconnect,
    /// The same height now names a different hash, or `pending` moved backwards.
    HeadRollback,
}

impl NodeEvent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Restart => "restart",
            Self::Reconnect => "reconnect",
            Self::HeadRollback => "head_rollback",
        }
    }
}

pub const ALL_EVENTS: [NodeEvent; 3] = [
    NodeEvent::Restart,
    NodeEvent::Reconnect,
    NodeEvent::HeadRollback,
];

/// §5's state list, in the order the task book writes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StateClass {
    /// 「当前 GraphSnapshot / canonical head」
    CanonicalHeadAndGraph,
    /// 「block identity / pending」
    BlockIdentity,
    /// 「Flashblocks 视图缓存」
    FlashblocksViewCache,
    /// 「尚未完成的候选与仿真结果」
    OutstandingCandidatesAndSimulations,
    /// 「Ready 但尚未提交的 ExecutablePlan」
    ReadyUnsubmittedPlan,
    /// 「nonce reservation」
    NonceReservation,
    /// 「capital reservation」
    CapitalReservation,
    /// 「已提交但尚未确认的交易」
    SubmittedUnconfirmedTransaction,
}

impl StateClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CanonicalHeadAndGraph => "canonical_head_and_graph",
            Self::BlockIdentity => "block_identity",
            Self::FlashblocksViewCache => "flashblocks_view_cache",
            Self::OutstandingCandidatesAndSimulations => "outstanding_candidates_and_simulations",
            Self::ReadyUnsubmittedPlan => "ready_unsubmitted_plan",
            Self::NonceReservation => "nonce_reservation",
            Self::CapitalReservation => "capital_reservation",
            Self::SubmittedUnconfirmedTransaction => "submitted_unconfirmed_transaction",
        }
    }

    /// The words §5 itself uses for this item, kept next to the machine name.
    pub const fn task_book_words(self) -> &'static str {
        match self {
            Self::CanonicalHeadAndGraph => "current GraphSnapshot / canonical head",
            Self::BlockIdentity => "block identity / pending",
            Self::FlashblocksViewCache => "Flashblocks view cache",
            Self::OutstandingCandidatesAndSimulations => {
                "outstanding candidates and simulation results"
            }
            Self::ReadyUnsubmittedPlan => "Ready but unsubmitted ExecutablePlan",
            Self::NonceReservation => "nonce reservation",
            Self::CapitalReservation => "capital reservation",
            Self::SubmittedUnconfirmedTransaction => "submitted but unconfirmed transaction",
        }
    }
}

pub const ALL_CLASSES: [StateClass; 8] = [
    StateClass::CanonicalHeadAndGraph,
    StateClass::BlockIdentity,
    StateClass::FlashblocksViewCache,
    StateClass::OutstandingCandidatesAndSimulations,
    StateClass::ReadyUnsubmittedPlan,
    StateClass::NonceReservation,
    StateClass::CapitalReservation,
    StateClass::SubmittedUnconfirmedTransaction,
];

/// What happens to the class when the event occurs. Five words, and none of them is
/// "continue as before".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// Held answers are dropped or marked unusable before anything can consume them.
    Invalidated,
    /// Nothing is carried over: the question is asked of the node again before it can act.
    ReRead,
    /// Neither failed nor resent; the transaction hash and the receipt lifecycle own it.
    HeldForReceipt,
    /// The session cannot continue on this fact and stops fail-closed.
    RunEnds,
    /// One holder at a time, by construction, and freed only when nothing is in flight.
    SingleOccupancy,
    /// No node event reaches this state; the row says why that is safe, or what covers it.
    UntouchedByNodeEvents,
}

impl Disposition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Invalidated => "invalidated",
            Self::ReRead => "re_read",
            Self::HeldForReceipt => "held_for_receipt",
            Self::RunEnds => "run_ends",
            Self::SingleOccupancy => "single_occupancy",
            Self::UntouchedByNodeEvents => "untouched_by_node_events",
        }
    }

    /// `true` for the two dispositions a run may *not* quietly take when the state carries
    /// a canonical block identity.
    pub const fn leaves_the_old_identity_usable(self) -> bool {
        matches!(self, Self::UntouchedByNodeEvents)
    }
}

/// How far the row's claim reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strength {
    /// Production code enforces it, on a path a run takes.
    Enforced,
    /// The event is recorded and a human reads the record; nothing is discarded or refused.
    RecordedOnly,
    /// Reviewed, and no mechanism exists — the row carries the reason and the compensation.
    NotEnforced,
}

impl Strength {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enforced => "enforced",
            Self::RecordedOnly => "recorded_only",
            Self::NotEnforced => "not_enforced",
        }
    }
}

/// One pointer into production code: the file, and a token that must occur on exactly one
/// of its production lines. A line number is never written here, because the number is
/// what moves when code moves.
#[derive(Clone, Copy, Debug)]
pub struct Anchor {
    pub file: &'static str,
    pub token: &'static str,
}

/// One cell of the policy.
#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub class: StateClass,
    pub event: NodeEvent,
    pub disposition: Disposition,
    /// The rule, named the way the code names it.
    pub mechanism: &'static str,
    pub strength: Strength,
    /// Which sentence of §5 this row answers.
    pub clause: &'static str,
    pub anchor: Anchor,
    pub note: &'static str,
}

/// One row, spelled in the order the table prints it. Nine arguments is the width of the
/// table, not a function that should have been a struct: the call sites stay readable because
/// the columns appear in the same order in all 24 of them.
#[allow(clippy::too_many_arguments)]
const fn row(
    class: StateClass,
    event: NodeEvent,
    disposition: Disposition,
    mechanism: &'static str,
    strength: Strength,
    clause: &'static str,
    file: &'static str,
    token: &'static str,
    note: &'static str,
) -> Row {
    Row {
        class,
        event,
        disposition,
        mechanism,
        strength,
        clause,
        anchor: Anchor { file, token },
        note,
    }
}

/// Rows whose enforcement is a record or a review rather than a mechanism. This list and
/// the table must agree exactly; see the test that names it.
pub const UNENFORCED_MECHANISMS: [&str; 2] = [
    "canonicality_conflict_keeps_both_hashes",
    "cache_keyed_by_height_not_hash",
];

// ---------------------------------------------------------------------------
// the policy: 8 classes × 3 events = 24 rows
// ---------------------------------------------------------------------------

pub const POLICY: [Row; 24] = [
    // §5 item 1: 当前 GraphSnapshot / canonical head.
    row(
        StateClass::CanonicalHeadAndGraph,
        NodeEvent::Restart,
        Disposition::RunEnds,
        "gap_never_becomes_zero",
        Strength::Enforced,
        "未验证的节点状态不能把缺块当成「没有机会」",
        "crates/pipeline/src/runner.rs",
        "fatal = Some(PipelineError::GapUnrecovered",
        "a restart is first seen as unread heights; if the hole is not filled the session ends as a failure with the missing count in it, which is the §3 rule that an unverified node must not be reported as an empty market. A canonical source that gives up outright is recorded (`canonical_source_failed_during_run`) and does not by itself stop the session — the gap rule is what stops it, deliberately, so that one source's failure cannot look like an absence of opportunities",
    ),
    row(
        StateClass::CanonicalHeadAndGraph,
        NodeEvent::Reconnect,
        Disposition::Invalidated,
        "graph_is_built_per_sealed_block",
        Strength::Enforced,
        "旧 canonical identity 上的视图不能续用",
        "crates/pipeline/src/engine.rs",
        "let build = self.graph.build_traced(&self.replay.snapshot())?;",
        "there is no graph carried between blocks to invalidate — each sealed block rebuilds the graph from the store, so a resumed read starts from the block the chain actually has rather than from the one the old session remembered",
    ),
    row(
        StateClass::CanonicalHeadAndGraph,
        NodeEvent::HeadRollback,
        Disposition::RunEnds,
        "canonicality_conflict_keeps_both_hashes",
        Strength::RecordedOnly,
        "链头倒退要有明确规则",
        "crates/live/src/tracker.rs",
        "kept: format!(\"{hash:?}\"),",
        "v0.1 does not resolve reorgs: the tracker names both hashes and discards nothing, which is a record and not an invalidation. What actually stops work on the old identity is the block-identity row below — a send dies at the gate, not here",
    ),
    // §5 item 2: block identity / pending.
    row(
        StateClass::BlockIdentity,
        NodeEvent::Restart,
        Disposition::ReRead,
        "binding_read_is_a_failed_call_not_a_pass",
        Strength::Enforced,
        "重新连上不等于可以继续",
        "crates/execution/src/chain_read.rs",
        "Err(error) => BlockBinding::Unverified(error.to_string()),",
        "while the node is unreachable the read fails and the binding becomes `Unverified` with the endpoint's own words; there is no branch that keeps an earlier `Confirmed`.",
    ),
    row(
        StateClass::BlockIdentity,
        NodeEvent::Reconnect,
        Disposition::ReRead,
        "height_the_node_does_not_have_is_unverified",
        Strength::Enforced,
        "旧 pending 视图必须按明确规则失效",
        "crates/execution/src/chain_read.rs",
        "BlockBinding::Unverified(format!(\"the endpoint holds no block {} at all\", number.0))",
        "a reconnected node that has not reached the pinned height answers `None`, and `None` becomes `Unverified` rather than a silent pass — absence of the block is not proof of it",
    ),
    row(
        StateClass::BlockIdentity,
        NodeEvent::HeadRollback,
        Disposition::Invalidated,
        "binding_mismatch_is_named_as_a_reorg",
        Strength::Enforced,
        "依赖旧 canonical identity 的候选不能续用",
        "crates/execution/src/chain_read.rs",
        "Ok(Some(hash)) => BlockBinding::Reorged {",
        "the same height under a different hash is detected here, by comparing the read hash against the hash the intent carries — the detection the gate's refusal then acts on",
    ),
    // §5 item 3: Flashblocks 视图缓存.
    row(
        StateClass::FlashblocksViewCache,
        NodeEvent::Restart,
        Disposition::Invalidated,
        "candidate_below_highest_canonical_expires",
        Strength::Enforced,
        "旧 pending 视图在 reset 后必须按明确规则失效",
        "crates/live/src/flashblocks.rs",
        "number.0 < self.stats.highest_canonical",
        "the candidate cache's expiry rule is a number rule, so it does not need to be told a restart happened: once the canonical path has named a height, every held candidate below it expires on the next tick and is reported as expired rather than forgotten — `crates/live/tests/node_reset_pending.rs::a_pending_number_behind_the_head_dies_in_the_cycle_that_opened_it` runs it with the age rules switched off, so the head is the only possible cause",
    ),
    row(
        StateClass::FlashblocksViewCache,
        NodeEvent::Reconnect,
        Disposition::Invalidated,
        "open_views_invalidated_at_disconnect",
        Strength::Enforced,
        "重新连接后不得假装接着同一个视图",
        "crates/live/src/preconf_radar.rs",
        "pub fn on_disconnect",
        "M9.4's radar drops every open view on a transport drop, because this endpoint has no cursor to resume against; the M5 pending-poll cache above is the same rule expressed as numbers instead of an event",
    ),
    row(
        StateClass::FlashblocksViewCache,
        NodeEvent::HeadRollback,
        Disposition::Invalidated,
        "a_rewinding_pending_is_not_resumed",
        Strength::Enforced,
        "链头倒退要有明确规则",
        "crates/live/src/flashblocks.rs",
        "`pending` moved backwards",
        "a candidate source that rewinds cannot be reconciled, so the number is recorded and ignored and the high-water mark does not move backwards — `crates/live/tests/node_reset_pending.rs` shows the pre-rollback view is never re-emitted as new",
    ),
    // §5 item 4: 尚未完成的候选与仿真结果.
    row(
        StateClass::OutstandingCandidatesAndSimulations,
        NodeEvent::Restart,
        Disposition::Invalidated,
        "pin_checked_against_the_loaded_header",
        Strength::Enforced,
        "依赖旧 identity 的仿真结果不能继续执行",
        "crates/simulation/src/engine.rs",
        "request.check_pin(loaded)?;",
        "a job that starts after the node comes back loads the header at its pin and stops as `StateMismatch` if the height is no longer the block it was priced against.",
    ),
    row(
        StateClass::OutstandingCandidatesAndSimulations,
        NodeEvent::Reconnect,
        Disposition::Invalidated,
        "pin_mismatch_names_both_numbers",
        Strength::Enforced,
        "重新连上 RPC 不等于结果又有效了",
        "crates/simulation/src/request.rs",
        "pub fn check_pin",
        "the check compares height and hash and reports which of the two moved, so a result that stopped being valid is refused with the two identities in the reason.",
    ),
    row(
        StateClass::OutstandingCandidatesAndSimulations,
        NodeEvent::HeadRollback,
        Disposition::UntouchedByNodeEvents,
        "cache_keyed_by_height_not_hash",
        Strength::NotEnforced,
        "已完成的仿真结果带着自己的 state version",
        "crates/simulation/src/state.rs",
        "pub struct StateReadKey",
        "residual risk, recorded rather than fixed: the cache key is (chain, height, address), so a rollback landing between the pin check and a later cached read is not detected by the cache itself. It is covered downstream — the answer is only ever consumed by an intent that re-reads its binding at the gate — and §9/§10 of this milestone forbid reworking that cache here",
    ),
    // §5 item 5: Ready 但尚未提交的 ExecutablePlan.
    row(
        StateClass::ReadyUnsubmittedPlan,
        NodeEvent::Restart,
        Disposition::ReRead,
        "chain_legs_are_read_per_attempt",
        Strength::Enforced,
        "任何依赖旧 canonical block identity 的候选或仿真结果，不能仅因为重新连上 RPC 就继续执行",
        "crates/execution/src/stage.rs",
        "let binding = read_binding(&*chain, intent.block_number, intent.block_hash).await;",
        "the plan is not marked stale by a node event — M5's `Staleness` has one variant, `PoolChanged` — because the stronger rule is that the attempt never carries the identity over: chain id, binding, balance and nonce are asked again on every attempt, and a freshness nobody re-confirmed arrives at `crates/execution/src/gate.rs` as `Freshness::Unknown`, which is a gate failure rather than a pass. Graded by `crates/execution/tests/stage_matrix.rs::each_attempt_asks_the_node_rather_than_reusing_an_earlier_answer`",
    ),
    row(
        StateClass::ReadyUnsubmittedPlan,
        NodeEvent::Reconnect,
        Disposition::Invalidated,
        "unread_fact_blocks_the_gate",
        Strength::Enforced,
        "未验证不能当成通过",
        "crates/execution/src/gate.rs",
        "BlockBinding::Unverified(why) => failures.push(GateFailure {",
        "after a reconnect the endpoint may answer nothing, and nothing is a failure of `block_binding_valid` rather than a pass — the run stops before a key is read",
    ),
    row(
        StateClass::ReadyUnsubmittedPlan,
        NodeEvent::HeadRollback,
        Disposition::Invalidated,
        "a_reorg_blocks_before_signing",
        Strength::Enforced,
        "依赖旧 identity 的候选不能执行",
        "crates/execution/src/gate.rs",
        "BlockBinding::Reorged {",
        "graded at the stage level in `crates/execution/tests/stage_matrix.rs::a_pin_the_chain_no_longer_holds_blocks_before_signing`: the attempt ends at the gate with zero bytes sent",
    ),
    // §5 item 6: nonce reservation.
    row(
        StateClass::NonceReservation,
        NodeEvent::Restart,
        Disposition::SingleOccupancy,
        "only_an_unsent_pair_is_given_back",
        Strength::Enforced,
        "不允许通过简单清空全部状态破坏正在进行的交易跟踪",
        "crates/execution/src/lifecycle.rs",
        "pub fn release_unsent",
        "the release path is asked of the lane's own rung, not of the endpoint: a restart is not a reason to free a nonce, only a proven absence of bytes on the wire is.",
    ),
    row(
        StateClass::NonceReservation,
        NodeEvent::Reconnect,
        Disposition::SingleOccupancy,
        "a_held_pair_is_refused_by_name",
        Strength::Enforced,
        "不允许恢复后重复占用同一 nonce",
        "crates/execution/src/lanes.rs",
        "return Err(LaneRefusal::NonceHeld {",
        "two lanes cannot hold one (signer, nonce) pair, and the refusal quotes the holder and its stage, so a resumed lane that reaches for the same number is told who has it.",
    ),
    row(
        StateClass::NonceReservation,
        NodeEvent::HeadRollback,
        Disposition::UntouchedByNodeEvents,
        "nonce_evidence_is_re_read",
        Strength::Enforced,
        "链头倒退不改变账户 nonce 的归属",
        "crates/execution/src/gate.rs",
        "NonceEvidence::Differs",
        "the reservation is keyed by signer and number and carries no block identity, so a rollback is not a fact about it; the chain's own view is still re-read every attempt and a pending nonce that differs from the intent's blocks the gate.",
    ),
    // §5 item 7: capital reservation.
    row(
        StateClass::CapitalReservation,
        NodeEvent::Restart,
        Disposition::SingleOccupancy,
        "capital_is_freed_from_a_terminal_lane",
        Strength::Enforced,
        "不允许清空全部状态",
        "crates/execution/src/lanes.rs",
        "fn release(&mut self, reservation: &CapitalReservation) {",
        "the domain's only release paths are a lane settling or ending; no transport event is one, so a restart cannot hand the same capital back to the pool while a lane still holds it.",
    ),
    row(
        StateClass::CapitalReservation,
        NodeEvent::Reconnect,
        Disposition::SingleOccupancy,
        "the_second_lane_is_refused_by_arithmetic",
        Strength::Enforced,
        "不允许恢复后重复占用同一笔资本",
        "crates/execution/src/lanes.rs",
        "return Err(LaneRefusal::InsufficientCapital {",
        "`available + reserved == capacity` is the invariant, and a lane asking for capital another lane reserved is refused with all four figures quoted.",
    ),
    row(
        StateClass::CapitalReservation,
        NodeEvent::HeadRollback,
        Disposition::UntouchedByNodeEvents,
        "reservation_has_no_block_identity",
        Strength::Enforced,
        "链头倒退与资本归属无关",
        "crates/execution/src/lanes.rs",
        "pub struct CapitalReservation",
        "the reservation names a domain and an amount, and is keyed by lane; nothing in it is derived from which block is canonical, so there is no stale state for a rollback to invalidate.",
    ),
    // §5 item 8: 已提交但尚未确认的交易.
    row(
        StateClass::SubmittedUnconfirmedTransaction,
        NodeEvent::Restart,
        Disposition::HeldForReceipt,
        "an_unknown_answer_is_not_a_failure",
        Strength::Enforced,
        "已提交交易不能因节点重启而直接当作失败或重新提交",
        "crates/execution/src/stage.rs",
        "SubmissionOutcome::Unknown { reason, .. } => {",
        "no acknowledgement grants no rung: the record stays where the chain left it, the lane stays held, and the bytes are named in the report so the hash lifecycle can pick them up.",
    ),
    row(
        StateClass::SubmittedUnconfirmedTransaction,
        NodeEvent::Reconnect,
        Disposition::HeldForReceipt,
        "in_flight_is_the_release_condition",
        Strength::Enforced,
        "沿用现有 transaction hash / receipt 生命周期进行恢复",
        "crates/execution/src/receipt.rs",
        "pub fn may_be_in_flight(self) -> bool",
        "the receipt lifecycle is what decides, and it asks whether the answer leaves the transaction possibly in flight — a reconnect that produces no answer keeps the record held instead of failing it",
    ),
    row(
        StateClass::SubmittedUnconfirmedTransaction,
        NodeEvent::HeadRollback,
        Disposition::HeldForReceipt,
        "a_may_be_in_flight_status_is_never_released",
        Strength::Enforced,
        "不能因节点事件当作失败或重发",
        "crates/execution/src/lifecycle.rs",
        "if !status.may_be_in_flight() {",
        "the release branch is guarded by the same question, so a rolled-back head cannot free a nonce or a reservation that a real transaction still occupies; receipt tracking follows the hash, and `eth_getTransactionReceipt` being null spends an attempt rather than ending the run.",
    ),
];

// ---------------------------------------------------------------------------
// anchor resolution
// ---------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The production region of a source file: everything before the inline test module.
///
/// The cut is the same one every other guard in this repository uses, so an anchor cannot
/// be satisfied by a line that only exists inside a test.
fn production_lines(path: &Path) -> Vec<String> {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let code = text.split("\n#[cfg(test)]").next().unwrap_or(text.as_str());
    code.lines().map(|line| line.to_string()).collect()
}

/// One anchor, resolved: the file's production region is scanned for the token, and exactly
/// one hit is a pass. Two hits means the claim does not name one place; zero means the
/// mechanism is gone.
fn resolve_pair(file: &str, token: &str) -> Result<usize, String> {
    let path = workspace_root().join(file);
    let hits = production_lines(&path)
        .into_iter()
        .enumerate()
        .filter(|(_, line)| line.contains(token))
        .map(|(index, _)| index + 1)
        .collect::<Vec<_>>();
    match hits.len() {
        1 => Ok(hits[0]),
        0 => Err(format!("{file}: no production line contains `{token}`")),
        n => Err(format!(
            "{}: {n} production lines contain `{}` (at {}) — an anchor must name one place",
            file,
            token,
            hits.iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn resolve(anchor: &Anchor) -> Result<usize, String> {
    resolve_pair(anchor.file, anchor.token)
}

fn row_key(row: &Row) -> String {
    format!("{}@{}", row.class.as_str(), row.event.as_str())
}

fn ruling_for(class: StateClass, event: NodeEvent) -> Row {
    *POLICY
        .iter()
        .find(|row| row.class == class && row.event == event)
        .unwrap_or_else(|| panic!("no row for {} / {}", class.as_str(), event.as_str()))
}

/// The classes that carry, or act on, a canonical block identity — §5's actual subject.
const IDENTITY_BEARING: [StateClass; 5] = [
    StateClass::CanonicalHeadAndGraph,
    StateClass::BlockIdentity,
    StateClass::FlashblocksViewCache,
    StateClass::OutstandingCandidatesAndSimulations,
    StateClass::ReadyUnsubmittedPlan,
];

/// The table as one JSON document, with every anchor resolved against the source that is
/// on disk right now.
fn assemble() -> Value {
    let mut rows = Vec::new();
    for class in ALL_CLASSES {
        for event in ALL_EVENTS {
            let row = ruling_for(class, event);
            let line = resolve(&row.anchor)
                .unwrap_or_else(|why| panic!("{}: unresolved anchor — {why}", row_key(&row)));
            rows.push(json!({
                "state_class": class.as_str(),
                "task_book_words": class.task_book_words(),
                "node_event": event.as_str(),
                "disposition": row.disposition.as_str(),
                "mechanism": row.mechanism,
                "strength": row.strength.as_str(),
                "clause": row.clause,
                "anchor_file": row.anchor.file,
                "anchor_token": row.anchor.token,
                "anchor_line": line,
                "note": row.note,
            }));
        }
    }
    let mut strengths = json!({});
    for strength in ["enforced", "recorded_only", "not_enforced"] {
        strengths[strength] = POLICY
            .iter()
            .filter(|row| row.strength.as_str() == strength)
            .count()
            .into();
    }
    json!({
        "schema": "m12b-node-reset-policy-v1",
        "source_of_truth": "crates/pipeline/tests/node_reset_policy.rs",
        "task_book": "docs/v0.1/M12B Coding.md §5",
        "node_events": ALL_EVENTS.iter().map(|event| event.as_str()).collect::<Vec<_>>(),
        "state_classes": ALL_CLASSES.iter().map(|class| class.as_str()).collect::<Vec<_>>(),
        "rows": rows.len(),
        "strength_counts": strengths,
        "unenforced_mechanisms": UNENFORCED_MECHANISMS,
        "identity_bearing_classes": IDENTITY_BEARING.iter().map(|class| class.as_str()).collect::<Vec<_>>(),
        "verdicts": {
            "real_node_restart_experiment": "NOT_RUN (§5 keeps this as a follow-up infrastructure item)",
            "new_rpc_methods_in_this_policy": [],
            "production_behaviour_changed_by_this_policy": "none — this table is a test-side model; nothing in the pipeline reads it",
        },
        "table": rows,
    })
}

const EVIDENCE_FILE: &str = "data/evidence/m12/b/node-reset-policy.json";

static TABLE: OnceLock<Value> = OnceLock::new();

fn table() -> &'static Value {
    TABLE.get_or_init(assemble)
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[test]
fn every_class_meets_every_event_once() {
    assert_eq!(POLICY.len(), ALL_CLASSES.len() * ALL_EVENTS.len());
    let mut seen = std::collections::BTreeSet::new();
    for row in &POLICY {
        assert!(
            seen.insert(row_key(row)),
            "{} is ruled on twice",
            row_key(row)
        );
    }
    for class in ALL_CLASSES {
        for event in ALL_EVENTS {
            let row = ruling_for(class, event);
            assert!(
                !row.mechanism.is_empty(),
                "{} names no mechanism",
                row_key(&row)
            );
            assert!(
                !row.clause.is_empty(),
                "{} cites no §5 clause",
                row_key(&row)
            );
            assert!(
                row.note.contains('.') || row.note.contains('—'),
                "{} has no stated reason",
                row_key(&row)
            );
        }
    }
    // §5's list is what the table covers, so the count is the task book's, not a choice.
    assert_eq!(ALL_CLASSES.len(), 8);
    assert_eq!(ALL_EVENTS.len(), 3);
}

#[test]
fn each_ruling_names_exactly_one_line_of_production_code() {
    let mut resolved = Vec::new();
    for row in &POLICY {
        if row.strength == Strength::RecordedOnly || row.strength == Strength::NotEnforced {
            // Still resolved: a row that says "nothing enforces this" must name the place
            // it looked, or the reader cannot check the looking.
            let _ = resolve(&row.anchor).expect("even a `recorded` row names a real line");
        }
        let line = resolve(&row.anchor).unwrap_or_else(|why| panic!("{}: {why}", row_key(row)));
        resolved.push(format!("{}:{} {}", row.anchor.file, line, row_key(row)));
    }
    assert_eq!(resolved.len(), 24);
}

/// The negative control for the whole file: an anchor that names nothing, and an anchor
/// that names two places, must both be refused. Without this, the resolution above would
/// pass on an empty or ambiguous table.
#[test]
fn an_anchor_that_is_ambiguous_or_absent_is_not_a_pass() {
    assert!(
        resolve(&Anchor {
            file: "crates/execution/src/stage.rs",
            token: "this token exists in no file of this workspace",
        })
        .is_err(),
        "an absent mechanism was accepted as a ruling"
    );

    // `graph:` really does occur in the engine — several times, which is the failure.
    let ambiguous = resolve(&Anchor {
        file: "crates/pipeline/src/engine.rs",
        token: "graph:",
    })
    .expect_err("a token hitting several lines must not resolve");
    assert!(
        ambiguous.contains("production lines contain"),
        "{ambiguous}"
    );

    // And a hit that exists only below `#[cfg(test)]` is not production either.
    let test_only = resolve(&Anchor {
        file: "crates/execution/src/gate.rs",
        token: "fn a_fully_evidenced_intent_passes_and_one_missing_field_blocks()",
    })
    .expect_err("a test-only line is not an enforcement site");
    assert!(test_only.contains("no production line"), "{test_only}");
}

/// §5's first 基本原则, stated as a rule over the table rather than as prose: no cell of an
/// identity-bearing class may be disposed of as "the event does not reach it".
#[test]
fn a_node_event_never_leaves_an_identity_bearing_view_as_is() {
    for class in IDENTITY_BEARING {
        for event in ALL_EVENTS {
            let row = ruling_for(class, event);
            if row.disposition.leaves_the_old_identity_usable() {
                assert_eq!(
                    row.strength,
                    Strength::NotEnforced,
                    "{} claims a view is untouched while also claiming a mechanism enforces it",
                    row_key(&row)
                );
                assert!(
                    row.note.contains("covered") || row.note.contains("gate"),
                    "{} says untouched without naming what compensates",
                    row_key(&row)
                );
            }
        }
    }
    // The one cell that is genuinely untouched is named, so it cannot silently grow.
    let untouched = POLICY
        .iter()
        .filter(|row| row.disposition == Disposition::UntouchedByNodeEvents)
        .map(row_key)
        .collect::<Vec<_>>();
    assert_eq!(
        untouched,
        [
            "outstanding_candidates_and_simulations@head_rollback",
            "nonce_reservation@head_rollback",
            "capital_reservation@head_rollback",
        ],
        "which views are untouched by a node event is a decision, not a drift"
    );
    assert!(
        !POLICY
            .iter()
            .any(|row| row.disposition == Disposition::ReRead
                && row.strength != Strength::Enforced),
        "a re-read that nothing enforces is not a re-read"
    );
}

/// §5's third 基本原则: bytes that left are neither a failure nor a resend, and the receipt
/// lifecycle owns them.
#[test]
fn a_transaction_that_left_is_never_called_a_failure_by_a_node_event() {
    for event in ALL_EVENTS {
        let row = ruling_for(StateClass::SubmittedUnconfirmedTransaction, event);
        assert_eq!(
            row.disposition,
            Disposition::HeldForReceipt,
            "{} must hold for the receipt, not decide",
            row_key(&row)
        );
        assert_eq!(row.strength, Strength::Enforced, "{}", row_key(&row));
        assert!(
            row.clause.contains("不能") || row.clause.contains("沿用"),
            "{} does not cite §5's committed-transaction rule",
            row_key(&row)
        );
    }

    // The stronger half of the rule is structural: the two modules that decide whether a
    // nonce or a unit of capital may go back to the pool never ask for a send. A restart
    // cannot turn an unacknowledged transaction into a second one, because the code that
    // would make that decision has no way to make it.
    for file in [
        "crates/execution/src/lifecycle.rs",
        "crates/execution/src/receipt.rs",
    ] {
        let code = production_lines(&workspace_root().join(file)).join("\n");
        assert!(
            !code.contains(".submit(") && !code.contains("sendRawTransaction"),
            "{file} both decides the release and can send — §5's「不能重新提交」is no longer structural"
        );
    }
    // …and the guard that keeps a possibly-in-flight pair held is one line, not three.
    assert_eq!(
        resolve(&Anchor {
            file: "crates/execution/src/lifecycle.rs",
            token: "if !status.may_be_in_flight() {",
        })
        .expect("the release guard exists"),
        resolve(
            &ruling_for(
                StateClass::SubmittedUnconfirmedTransaction,
                NodeEvent::HeadRollback,
            )
            .anchor
        )
        .expect("the row points at that same line")
    );
}

/// §5's 「不允许通过简单清空全部状态破坏正在进行的交易跟踪」 tested as the scan it is:
/// the execution lane is node-event-blind. If a future change wires a transport event to a
/// clear, this goes red on the line that did it.
#[test]
fn no_node_event_is_wired_to_a_state_clear_in_the_execution_crate() {
    let root = workspace_root().join("crates/execution/src");
    let mut files = Vec::new();
    collect_rs(&root, &mut files);
    assert!(files.len() > 12, "the scan found {files:?}");

    let event_words = [
        "reconnect",
        "Disconnected",
        "SourceStatus",
        "transport_reconnect",
    ];
    let clear_words = ["clear()", "retain(", "drain(", "truncate("];
    let mut wired = Vec::new();
    let mut cleared = Vec::new();
    for file in &files {
        let code = production_lines(file).join("\n");
        for word in event_words {
            if code.contains(word) {
                wired.push(format!("{} names {word}", file.display()));
            }
        }
        for word in clear_words {
            if code.contains(word) {
                cleared.push(format!("{} calls {word}", file.display()));
            }
        }
    }
    assert!(
        wired.is_empty(),
        "the execution crate now reads node events, which is how a blanket clear starts: {wired:?}"
    );
    // A clear that exists *and* names an event is the combination §5 forbids; on its own a
    // bounded retain inside a cache is ordinary code, so only the pairing is reported.
    let both = cleared
        .iter()
        .filter(|line| {
            let file = line.split(" calls ").next().expect("a paired line");
            wired.iter().any(|w| w.starts_with(file))
        })
        .collect::<Vec<_>>();
    assert!(
        both.is_empty(),
        "a state clear is wired to a node event: {both:?}"
    );
}

/// §5's fourth 基本原则: no resume may occupy the same nonce or the same capital twice.
#[test]
fn a_reservation_refuses_a_second_claim_by_name() {
    for (file, token, what) in [
        (
            "crates/execution/src/lanes.rs",
            "return Err(LaneRefusal::NonceHeld {",
            "the nonce pair",
        ),
        (
            "crates/execution/src/lanes.rs",
            "return Err(LaneRefusal::InsufficientCapital {",
            "the capital",
        ),
    ] {
        let line = resolve(&Anchor { file, token }).unwrap_or_else(|why| panic!("{what}: {why}"));
        assert!(line > 1, "{what} resolves to {line}");
    }
    // The reservations are rows of their own for all three events, and none of them is a
    // `RecordedOnly` row: a double occupation is refused by code, not by a note.
    for class in [StateClass::NonceReservation, StateClass::CapitalReservation] {
        for event in ALL_EVENTS {
            let row = ruling_for(class, event);
            assert_eq!(row.strength, Strength::Enforced, "{}", row_key(&row));
            assert_ne!(
                row.disposition,
                Disposition::HeldForReceipt,
                "{} is not a receipt question",
                row_key(&row)
            );
        }
    }
}

/// The rows that are records or reviews rather than mechanisms cannot grow quietly.
#[test]
fn the_rows_that_are_not_enforced_are_exactly_the_named_ones() {
    let mut named = POLICY
        .iter()
        .filter(|row| row.strength != Strength::Enforced)
        .map(|row| row.mechanism)
        .collect::<Vec<_>>();
    named.sort();
    let mut expected = UNENFORCED_MECHANISMS.to_vec();
    expected.sort();
    assert_eq!(
        named, expected,
        "the table's unenforced rows are: {named:?}"
    );

    // Each of them says what compensates, so the honest answer is still an answer.
    for row in POLICY.iter().filter(|r| r.strength != Strength::Enforced) {
        assert!(
            row.note.contains("gate")
                || row.note.contains("block-identity")
                || row.note.contains("residual risk"),
            "{} is unenforced and does not say what covers it",
            row_key(row)
        );
    }
}

/// The policy adds no RPC method and changes no production behaviour. Every mechanism it
/// names is a line that already existed, and the only wire method quoted anywhere in the
/// table is the receipt read §5 hands its recovery to.
#[test]
fn the_policy_names_no_new_way_to_ask_the_node() {
    let text = serde_json::to_string(table()).expect("a table that serialises");
    let mut quoted = text
        .split("eth_")
        .skip(1)
        .map(|tail| {
            tail.split_whitespace()
                .next()
                .expect("a word after eth_")
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_string()
        })
        .collect::<Vec<_>>();
    quoted.sort();
    quoted.dedup();
    assert_eq!(
        quoted,
        vec!["getTransactionReceipt".to_string()],
        "the policy must quote only the receipt read; anything else is a new call site"
    );

    // And no anchor is a file this milestone created for another task's gate.
    for row in &POLICY {
        assert!(row.anchor.file.starts_with("crates/"), "{}", row_key(row));
        assert!(
            !row.anchor.file.contains("readiness"),
            "{} rests on the readiness gate, which is §3's subject and has its own tests",
            row_key(row)
        );
    }
    assert!(
        table()["verdicts"]["real_node_restart_experiment"]
            .as_str()
            .expect("a verdict")
            .starts_with("NOT_RUN"),
        "§5 keeps real-node restart verification for the infrastructure follow-up"
    );
}

#[test]
fn the_assembled_table_is_the_one_the_evidence_file_carries() {
    let assembled = table();
    assert_eq!(assembled["rows"].as_i64().expect("a count"), 24);
    assert_eq!(
        assembled["state_classes"]
            .as_array()
            .expect("classes")
            .len(),
        8
    );
    assert_eq!(
        assembled["node_events"].as_array().expect("events").len(),
        3
    );
    let strengths = assembled["strength_counts"].as_object().expect("counts");
    let total = strengths
        .values()
        .map(|count| count.as_u64().expect("a count"))
        .sum::<u64>();
    assert_eq!(total, 24, "every row is graded exactly once");
    assert_eq!(
        strengths.get("enforced").and_then(Value::as_u64),
        Some(22),
        "the two unenforced rows are the named ones: {strengths:?}"
    );

    if std::env::var("M12B_NODE_RESET_POLICY_REFRESH").is_ok() {
        let path = workspace_root().join(EVIDENCE_FILE);
        std::fs::create_dir_all(path.parent().expect("a directory"))
            .expect("create the evidence directory");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&assembled).expect("json") + "\n",
        )
        .expect("write the policy table");
        panic!("refreshed {EVIDENCE_FILE}; run this test again without the environment variable");
    }
}

/// The committed table, re-checked against today's source by its semantic keys: `(file,
/// token)` must still hit exactly one production line. Line numbers are display only, so a
/// shift does not make this red while a deletion does.
#[test]
fn the_committed_table_still_resolves_against_the_source() {
    let path = workspace_root().join(EVIDENCE_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        panic!(
            "{EVIDENCE_FILE} is missing; refresh it with M12B_NODE_RESET_POLICY_REFRESH=1 \
             cargo test -p evm-pipeline --test node_reset_policy"
        );
    };
    let committed: Value = serde_json::from_str(&text).expect("a json table");
    let committed_rows = committed["table"].as_array().expect("a table");
    assert_eq!(committed_rows.len(), POLICY.len());
    for row in committed_rows {
        let class = row["state_class"].as_str().expect("a class");
        let event = row["node_event"].as_str().expect("an event");
        let file = row["anchor_file"].as_str().expect("a file");
        let token = row["anchor_token"].as_str().expect("a token");
        let line = resolve_pair(file, token).unwrap_or_else(|why| panic!("{class}@{event}: {why}"));
        assert!(line >= 1, "{class}@{event} resolves to {line}");
    }
    assert_eq!(
        committed["schema"].as_str(),
        Some("m12b-node-reset-policy-v1")
    );
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("a readable source directory") {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}
