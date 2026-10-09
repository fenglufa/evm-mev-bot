//! §31–§36: the lane ledger — where each candidate stands in the ladder, and which two lanes are
//! not allowed to exist at the same time.
//!
//! §31 calls multi-lane new infrastructure and hands over a record shape. §33–§36 are the only
//! prohibitions that come with it, and each is a type here rather than a paragraph in a report:
//!
//! ```text
//! §33  same signer + same nonce in two lanes   -> NonceManager::reserve refuses the second
//! §34  reserve() / commit() / release()        -> LaneLedger::reserve_for / settle / end
//! §35  two lanes each believing they can pay   -> CapitalDomain's available / reserved split
//! §36  ranking -> winner -> reserve -> submit  -> reserve_for refuses a lane it never picked
//! ```
//!
//! ## What this module never touches
//!
//! No RPC, no signing, no sending, no receipt decoding. §2 forbids a second signer framework and a
//! second receipt framework and §44 forbids a new hot-path read, so the ledger verifies nothing for
//! itself: it records the transitions the caller's M10 stage already made and arbitrates the two
//! shared resources between them. The nonce arrives as an argument because
//! [`crate::nonce::NonceReading`] is what reads one off an endpoint; `Included` arrives as an
//! argument because [`crate::receipt::ReceiptTracker`] is what learns it; the input amount arrives as
//! an argument because [`evm_risk::MultihopAcceptance`] is what granted it (§29: M11 adds no monetary
//! model of its own). A ledger that re-read any of the three would be a second of those frameworks
//! wearing a different name.
//!
//! ## Why §33's parallelism costs nothing here
//!
//! §33 allows A/B/C simulations and A/B/C risk passes at the same time. Nothing has to permit that,
//! because nothing here is a lock: the two facts lanes compete for are a nonce and a capital
//! reservation, and neither is claimed before `Reserved`. Any number of lanes may sit in `Simulating`
//! or `RiskChecking`, and a collision becomes possible exactly where §33 says it must be refused.
//!
//! ## Naming
//!
//! §31 suggests `ExecutionLane` for the per-candidate record. That name belongs to M6
//! ([`crate::lifecycle::ExecutionLane`]) — the single-lane nonce holder of §11, whose name is quoted
//! by `sequence.rs`, `stage.rs`, `pipeline::Owner::ExecutionLane` and M6–M10's committed evidence.
//! Renaming it would reach four milestones of evidence to buy nothing, so the new record is
//! [`CandidateLane`] and §31's seven fields are carried verbatim. M6's `LANES == 1` stays the answer
//! to "how many transactions may this build have in flight"; this ledger is about candidates, and §36
//! is what keeps the two counts from quietly meaning the same thing.
//!
//! ## Why an unanswered submission is not a failure state
//!
//! §25's rule — an answer we cannot interpret is not a failure — has a consequence here that is easy
//! to get wrong: [`LaneFailure`] has no `SubmissionUnknown` arm, on purpose. A lane whose submission
//! went unanswered has not ended. It stays `Submitted`, keeps its nonce, and the only ways out are
//! [`LaneLedger::settle`] once a receipt binds or [`LaneLedger::end`] with
//! [`LaneFailure::SubmissionProvenRejected`] once an answer proves nothing is in flight. Every other
//! arm releases, because §34 names simulation failure, risk rejection and plan expiry as the cases
//! that *must* let go — and §34's third case is why `Expired` is reachable from `Reserved` rather
//! than only from before it.
//!
//! `result_large_err` is allowed for the whole module: the largest arm is §35's
//! [`LaneRefusal::InsufficientCapital`], which carries the four figures of the subtraction and the
//! domain's name as text so §43's table can print the refusal instead of reconstructing it. Boxing
//! those would put a deref between a lane and the reason it was turned away, on a value created a
//! handful of times per window at the reservation step — and §44's hot path is RPC reads, which this
//! ledger performs none of. M11's risk layer makes the same trade for the same reason at
//! `crates/risk/src/multihop.rs:241`.
#![allow(clippy::result_large_err)]

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fmt;

use alloy_primitives::{Address, B256, U256};
use serde::Serialize;

/// §31's lane identity.
///
/// Minted by [`LaneLedger::open`] in ascending order and never reused, so a lane id in an evidence
/// row names one record for the whole run. Ordered, because the ledger keys itself by it and §47's
/// determinism wants iteration order to be a property of the data rather than of a hash map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct LaneId(pub u64);

impl fmt::Display for LaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lane-{}", self.0)
    }
}

/// §32's thirteen states: nine forward arrows and four terminal failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneState {
    Created,
    Simulating,
    RiskChecking,
    Ready,
    Reserved,
    Submitting,
    Submitted,
    Included,
    Settled,
    /// A judgement said no: the simulation did not deliver, risk rejected, or §30's binding refused a
    /// plan that risk had already accepted.
    Rejected,
    /// The operator withdrew it — the usual fate of a candidate that lost §36's ranking.
    Cancelled,
    /// The window closed. Nobody said no; time did, which is the distinction §43's rows exist for.
    Expired,
    /// The execution attempt itself did not complete: proven not accepted, or reverted in a block.
    Failed,
}

/// What §34 does to a lane's reservations when the lane ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationDisposition {
    /// Give the nonce and the capital back so the next candidate can use them.
    Release,
    /// The run is in a block, so the number is spent. A reverted transaction consumed a real nonce;
    /// releasing it would hand a used number to another lane.
    Commit,
}

impl LaneState {
    /// §32's list, in ladder order, so a table can enumerate the states without restating them.
    pub const ALL: [LaneState; 13] = [
        Self::Created,
        Self::Simulating,
        Self::RiskChecking,
        Self::Ready,
        Self::Reserved,
        Self::Submitting,
        Self::Submitted,
        Self::Included,
        Self::Settled,
        Self::Rejected,
        Self::Cancelled,
        Self::Expired,
        Self::Failed,
    ];

    /// The machine label an evidence row groups by.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Simulating => "simulating",
            Self::RiskChecking => "risk_checking",
            Self::Ready => "ready",
            Self::Reserved => "reserved",
            Self::Submitting => "submitting",
            Self::Submitted => "submitted",
            Self::Included => "included",
            Self::Settled => "settled",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
            Self::Failed => "failed",
        }
    }

    /// Is this a state no lane leaves?
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Settled | Self::Rejected | Self::Cancelled | Self::Expired | Self::Failed
        )
    }

    /// Does a lane in this state hold a §34 nonce reservation?
    ///
    /// `Included` is inside the set because §34's lifecycle puts `commit` *after* the receipt: the
    /// nonce is still reserved until the lane settles.
    pub const fn holds_nonce(self) -> bool {
        matches!(
            self,
            Self::Reserved | Self::Submitting | Self::Submitted | Self::Included
        )
    }

    /// Has this state been reached at or past the reservation a §36 winner is given? `Included` is
    /// inside the set and `Settled` is not: a settled lane is finished, so re-ranking away from it
    /// takes nothing away from anybody, while a lane that is merely waiting for a receipt still
    /// means it.
    fn is_at_or_past_reservation(self) -> bool {
        matches!(
            self,
            Self::Reserved | Self::Submitting | Self::Submitted | Self::Included
        )
    }

    /// §32's edge table, as written rather than as inferred.
    ///
    /// The failure arrows are restricted on purpose. A lane whose transaction has been sent cannot be
    /// `Cancelled` or `Expired`, because the operator's window no longer decides what the chain does
    /// with those bytes — it can only end `Failed` or be carried through to `Settled`. That is why the
    /// four failure states are not reachable from everywhere.
    pub fn allows(self, next: LaneState) -> bool {
        use LaneState::*;
        match self {
            Created => matches!(next, Simulating | Cancelled),
            Simulating => matches!(next, RiskChecking | Rejected | Expired | Cancelled),
            RiskChecking => matches!(next, Ready | Rejected | Expired | Cancelled),
            Ready => matches!(next, Reserved | Rejected | Expired | Cancelled),
            Reserved => matches!(next, Submitting | Rejected | Expired | Cancelled),
            Submitting => matches!(next, Submitted | Failed),
            Submitted => matches!(next, Included | Failed),
            Included => matches!(next, Settled | Failed),
            Settled | Rejected | Cancelled | Expired | Failed => false,
        }
    }

    /// Why an arrow is not in the table, in the terms §32 would use. A refusal that only says
    /// "illegal" leaves the caller to rediscover the rule; this names the rule that bit.
    fn refusal_note(from: LaneState, to: LaneState) -> String {
        use LaneState::*;
        if from.is_terminal() {
            return format!(
                "{} is terminal — §32 gives a finished lane no outgoing arrow, so a lane that has \
                 already ended cannot be moved to {}",
                from.code(),
                to.code()
            );
        }
        if to == Reserved {
            return "Reserved is entered through LaneLedger::reserve_for, the one place a §34 nonce \
                    and a §35 capital reservation are taken and the one place §36's winner rule is \
                    asked; a lane cannot be moved into it by declaring the state"
                .to_string();
        }
        if to == Settled {
            return "Settled is entered through LaneLedger::settle, which commits the nonce and \
                    settles the capital reservation in the same step"
                .to_string();
        }
        if to.is_terminal() {
            if matches!(from, Submitting | Submitted | Included) {
                return format!(
                    "a lane at {} has been sent: §32 leaves it only Failed (and only through \
                     LaneLedger::end), never Rejected, Cancelled or Expired, because the operator's \
                     window does not decide what the chain does with bytes it already has",
                    from.code()
                );
            }
            return format!(
                "{} is a failure state and is entered through LaneLedger::end, which applies §34's \
                 rule to whatever the lane holds; it cannot be reached by declaring the state",
                to.code()
            );
        }
        format!(
            "{} is not the next arrow of §32's ladder after {}",
            to.code(),
            from.code()
        )
    }
}

impl fmt::Display for LaneState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// Why a lane ended, and what that end does to what the lane was holding.
///
/// §32 names the four failure states and §34 names the three reasons that must release a reservation
/// (`simulation fails`, `risk rejects`, `plan expires`); M6's §25 adds the shape that must not, which
/// is why that shape is absent here. One arm per reason, so why a lane stopped is a field in the
/// evidence table rather than a sentence a reader reconstructs from a state name.
///
/// Externally tagged, because an internally tagged enum cannot carry a tuple variant's bare `String`,
/// and §43's rows read [`LaneFailure::code`] rather than this encoding anyway.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneFailure {
    /// §34's first must-release. The detail quotes the simulation's own ending.
    SimulationFailed(String),
    /// §34's second must-release. The detail quotes risk's check label — §26's machine name for the
    /// check that produced the rejection.
    RiskRejected(String),
    /// §30/§15–§17: risk accepted and the plan was refused at the execution boundary anyway. The
    /// reservation has to come back here more than anywhere, because this lane got as far as holding a
    /// nonce before its claim failed.
    PlanRefused(String),
    /// §34's third must-release: the window closed, possibly while a reservation was already held.
    PlanExpired(String),
    /// The operator withdrew the lane.
    Cancelled(String),
    /// §25's definite no: the node answered and refused, so nothing is in flight and §34 lets go.
    SubmissionProvenRejected(String),
    /// The transaction is in a block with status 0. The one failure that *commits* the nonce: the run
    /// happened, and only a receipt could say so.
    RevertedInBlock {
        transaction_hash: String,
        block_number: u64,
    },
}

impl LaneFailure {
    /// §32's state this reason ends the lane in.
    pub const fn lane_state(&self) -> LaneState {
        match self {
            Self::SimulationFailed(_) | Self::RiskRejected(_) | Self::PlanRefused(_) => {
                LaneState::Rejected
            }
            Self::PlanExpired(_) => LaneState::Expired,
            Self::Cancelled(_) => LaneState::Cancelled,
            Self::SubmissionProvenRejected(_) | Self::RevertedInBlock { .. } => LaneState::Failed,
        }
    }

    /// §34's rule for what happens to the lane's reservations.
    pub const fn disposition(&self) -> ReservationDisposition {
        match self {
            Self::RevertedInBlock { .. } => ReservationDisposition::Commit,
            _ => ReservationDisposition::Release,
        }
    }

    /// The label an evidence row groups by.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::SimulationFailed(_) => "simulation_failed",
            Self::RiskRejected(_) => "risk_rejected",
            Self::PlanRefused(_) => "plan_refused",
            Self::PlanExpired(_) => "plan_expired",
            Self::Cancelled(_) => "cancelled",
            Self::SubmissionProvenRejected(_) => "submission_proven_rejected",
            Self::RevertedInBlock { .. } => "reverted_in_block",
        }
    }

    /// The caller's own words, or the revert's two facts.
    fn detail(&self) -> String {
        match self {
            Self::SimulationFailed(text)
            | Self::RiskRejected(text)
            | Self::PlanRefused(text)
            | Self::PlanExpired(text)
            | Self::Cancelled(text)
            | Self::SubmissionProvenRejected(text) => text.clone(),
            Self::RevertedInBlock {
                transaction_hash,
                block_number,
            } => format!("reverted in block {block_number} by transaction {transaction_hash}"),
        }
    }
}

/// Which half of §34's lifecycle a pair is in.
///
/// `Committed` is a fact about the chain, not about the ledger: the number appeared in a block, so it
/// is history and no lane may hold it again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NonceStage {
    Reserved,
    Committed,
}

impl NonceStage {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Committed => "committed",
        }
    }

    pub const fn is_committed(self) -> bool {
        matches!(self, Self::Committed)
    }
}

/// §31's `nonce_reservation`: a `(signer, nonce)` pair, whose it is, and how far §34's lifecycle has
/// taken it.
///
/// The holder travels with the pair because §33's rule is a statement about both together, and because
/// §34's release has to distinguish "not yours to release" from "not held at all". `stage` is a value
/// and not a setter: only [`LaneLedger::settle`] and [`LaneLedger::end`] write it, from inside this
/// module, so a lane cannot mark itself committed and then release what it spent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct NonceReservation {
    pub signer: Address,
    pub nonce: u64,
    pub lane_id: LaneId,
    pub stage: NonceStage,
}

/// §31's `capital_reservation`, and §35's answer to "whose 100 is this".
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CapitalReservation {
    pub lane_id: LaneId,
    pub domain_id: String,
    /// The input this execution claims, spelled by the acceptance the caller holds. The ledger does
    /// not derive it — §29 forbids M11 from adding a monetary model, so the number a lane reserves is
    /// the number risk granted over, not a recomputation of it.
    pub amount: U256,
}

/// §34's nonce arbiter: which lane holds which `(signer, nonce)` pair.
///
/// Keyed by lane, not by pair. A lane holds at most one pair, so a pair index would be a second book
/// of one fact, able to drift from the first; `held_by` answers the pair question by scanning the
/// handful of lanes a window produces.
///
/// The three verbs §34 names — `reserve`, `commit`, `release` — are this type's three mutating
/// methods, and they are private to the ledger on purpose: a nonce book a caller could edit behind the
/// lane record's back would be a second answer to §33's question, and §33's refusal has to be one
/// fact, not two.
#[derive(Clone, Debug, Default)]
pub struct NonceManager {
    holds: BTreeMap<LaneId, NonceReservation>,
}

impl NonceManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// §33/§34: take the pair for one lane.
    ///
    /// A lane re-asking for a pair it already holds gets that holding back unchanged — it cannot take
    /// a second one, and the pair it named is already its own. A pair another lane holds is refused
    /// whether it is reserved *or committed*: a spent nonce is a transaction on chain, so §33's
    /// sentence covers history as well as intentions.
    ///
    /// The map is keyed by lane, so one pair per lane is true by construction rather than by a check
    /// here, and the ledger's only caller refuses a lane that is not at `Ready` before reaching this
    /// function — an insert therefore never replaces a holding.
    fn reserve(
        &mut self,
        lane_id: LaneId,
        signer: Address,
        nonce: u64,
    ) -> Result<NonceReservation, LaneRefusal> {
        if let Some(held) = self
            .holds
            .values()
            .find(|held| held.signer == signer && held.nonce == nonce)
        {
            if held.lane_id == lane_id {
                return Ok(*held);
            }
            return Err(LaneRefusal::NonceHeld {
                signer,
                nonce,
                holder: held.lane_id,
                stage: held.stage.label(),
                lane: lane_id,
            });
        }
        let record = NonceReservation {
            signer,
            nonce,
            lane_id,
            stage: NonceStage::Reserved,
        };
        self.holds.insert(lane_id, record);
        Ok(record)
    }

    /// §34: a receipt has arrived, so the number is spent. Reached from [`LaneLedger::settle`] and
    /// from [`LaneLedger::end`] over a revert, and from nowhere else.
    fn commit(&mut self, lane_id: LaneId) {
        if let Some(held) = self.holds.get_mut(&lane_id) {
            held.stage = NonceStage::Committed;
        }
    }

    /// §34: the pair was never spent, so it goes back to the pool.
    ///
    /// A committed pair is not removed. The ledger cannot ask for that — a pair becomes committed only
    /// as its lane reaches a terminal state, and every release comes from a lane that has not — so this
    /// is a belt rather than a branch: un-spending a nonce that a block already holds would be the one
    /// thing this book must never do, and it is not allowed to depend on the call path being careful.
    fn release(&mut self, lane_id: LaneId) {
        if self
            .holds
            .get(&lane_id)
            .is_some_and(|held| !held.stage.is_committed())
        {
            self.holds.remove(&lane_id);
        }
    }

    /// The lane holding this pair, if any.
    pub fn held_by(&self, signer: Address, nonce: u64) -> Option<LaneId> {
        self.holds
            .values()
            .find(|held| held.signer == signer && held.nonce == nonce)
            .map(|held| held.lane_id)
    }

    /// The reservation a lane holds, whichever pair it is.
    pub fn of(&self, lane_id: LaneId) -> Option<&NonceReservation> {
        self.holds.get(&lane_id)
    }

    /// How many lanes hold a pair right now.
    pub fn outstanding(&self) -> usize {
        self.holds.len()
    }

    /// The holdings, in lane order — the shape a §43 table quotes.
    pub fn held(&self) -> impl Iterator<Item = &NonceReservation> {
        self.holds.values()
    }
}

/// §35's one capital domain.
///
/// Two figures, named as the task book names them, with one invariant: `available_capital +
/// reserved_capital == capacity`. `capacity` is what the caller opened the domain with — in a live
/// run, the balance M6/M10's gate already read, never re-read here (§44). Reserving moves capital out
/// of `available` and into `reserved`, which turns §35's collision into arithmetic rather than a race:
/// the second lane asking for the same 100 finds `available` at zero and is refused by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CapitalDomain {
    pub domain_id: String,
    pub capacity: U256,
    pub available_capital: U256,
    pub reserved_capital: U256,
    /// Capital that ran through the domain and came back settled. Outside §35's two required figures,
    /// kept so §43's rows can say how much the lanes moved.
    settled_input: U256,
}

impl CapitalDomain {
    /// Open a domain at the capacity the caller states. Nothing is read: the caller is the stage that
    /// has the balance reading, and this is where it hands the number over.
    pub fn open(domain_id: impl Into<String>, capacity: U256) -> Self {
        Self {
            domain_id: domain_id.into(),
            capacity,
            available_capital: capacity,
            reserved_capital: U256::ZERO,
            settled_input: U256::ZERO,
        }
    }

    /// §35 stated as a check rather than assumed: the two halves still add up to the capacity. A
    /// domain failing this has either double-spent or lost a reservation.
    pub fn invariant_holds(&self) -> bool {
        self.available_capital.saturating_add(self.reserved_capital) == self.capacity
    }

    pub fn settled_input(&self) -> U256 {
        self.settled_input
    }

    fn reserve(
        &mut self,
        lane_id: LaneId,
        amount: U256,
    ) -> Result<CapitalReservation, LaneRefusal> {
        if amount > self.available_capital {
            return Err(LaneRefusal::InsufficientCapital {
                lane: lane_id,
                requested: amount.to_string(),
                free: self.available_capital.to_string(),
                reserved: self.reserved_capital.to_string(),
                capacity: self.capacity.to_string(),
                domain: self.domain_id.clone(),
            });
        }
        self.available_capital -= amount;
        self.reserved_capital += amount;
        Ok(CapitalReservation {
            lane_id,
            domain_id: self.domain_id.clone(),
            amount,
        })
    }

    /// §34's release: the reservation was never spent, so the capital returns to the pool.
    fn release(&mut self, reservation: &CapitalReservation) {
        self.reserved_capital -= reservation.amount;
        self.available_capital += reservation.amount;
    }

    /// The lane settled. §35 says the first version does no capital management, so the input returns
    /// to the pool and only the running total is kept; where the gain ended up is the receipt and
    /// profit layer's question, not this domain's.
    fn settle(&mut self, reservation: &CapitalReservation) {
        self.release(reservation);
        self.settled_input += reservation.amount;
    }
}

/// §31's lane record: the seven fields §31 names, plus the trail §32's machine leaves behind.
///
/// `plan_hash` and `simulation_id` are `Option` because the ladder starts at `Created`, before either
/// exists. Both are filled exactly once, by [`LaneLedger::record_simulation`], on the arrow that
/// describes the run that produced them — so a lane at `Ready` or later always has both (guarded
/// there, not assumed), and a lane that died before the run has neither, which is the honest shape
/// rather than a placeholder standing in for a simulation that did not happen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CandidateLane {
    pub lane_id: LaneId,
    pub candidate_id: String,
    pub plan_hash: Option<B256>,
    pub simulation_id: Option<String>,
    pub state: LaneState,
    pub nonce_reservation: Option<NonceReservation>,
    pub capital_reservation: Option<CapitalReservation>,
    /// Every §32 arrow this lane crossed, in order, with the caller's note. Only
    /// [`LaneLedger::advance`], [`LaneLedger::record_simulation`], [`LaneLedger::reserve_for`],
    /// [`LaneLedger::settle`] and [`LaneLedger::end`] push here, so the history is the ledger's own
    /// account of how the lane got where it is.
    pub history: Vec<LaneMove>,
}

/// One §32 arrow, as the ledger recorded it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LaneMove {
    pub from: LaneState,
    pub to: LaneState,
    pub note: String,
}

/// §36's ranking input: a lane, and what risk said it was worth.
///
/// The caller builds these from the [`evm_risk::MultihopAcceptance`] values it holds. §29 again sets
/// the boundary: the ledger ranks on a number risk granted, it prices nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct LaneStanding {
    pub lane_id: LaneId,
    pub gross_gain: U256,
}

impl LaneStanding {
    pub const fn new(lane_id: LaneId, gross_gain: U256) -> Self {
        Self {
            lane_id,
            gross_gain,
        }
    }
}

/// Both §31 reservations, as [`LaneLedger::reserve_for`] hands them back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReservationPair {
    pub nonce: NonceReservation,
    pub capital: CapitalReservation,
}

/// What [`LaneLedger::end`] decided, so §45's release and commit rows can be asserted from the answer
/// rather than by reading the ledger back and guessing which half moved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EndOutcome {
    pub lane: LaneId,
    pub from: String,
    pub to: String,
    pub reason: String,
    pub disposition: ReservationDisposition,
    pub released_nonce: Option<NonceReservation>,
    pub released_capital: Option<CapitalReservation>,
}

/// Why the ledger refused, named by the section that refuses it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LaneRefusal {
    /// §31/NC14: one candidate, one lane. Two lanes for one candidate id would be two claims on one
    /// finding, and the second would either win the nonce race or lose it arbitrarily.
    #[error("§31: candidate `{candidate_id}` already has {existing}; a lane is per candidate")]
    DuplicateLane {
        candidate_id: String,
        existing: LaneId,
    },
    /// §45/NC14's plan half: two lanes carrying one `plan_hash` are two claims to execute the same
    /// bound plan, and §30's binding says that is one execution.
    #[error("§45: plan {plan_hash:#x} is already bound by {existing}; one plan hash is one lane")]
    DuplicatePlan { plan_hash: B256, existing: LaneId },
    /// §33/NC12: the pair is another lane's. The holder's stage is quoted because "lane-2 is waiting
    /// on nonce 7" and "lane-2 already spent nonce 7" are different problems for the caller.
    #[error(
        "§33: nonce {nonce} for signer {signer:#x} is held by {holder} ({stage}); {lane} cannot take \
         the same pair"
    )]
    NonceHeld {
        signer: Address,
        nonce: u64,
        holder: LaneId,
        stage: &'static str,
        lane: LaneId,
    },
    // §34: a committed nonce is not a releasable resource, and there is no arm for refusing to
    // release one on purpose: a pair becomes committed only as its lane reaches a terminal state, and
    // every release comes from a lane that has not, so no caller can reach that refusal.
    // [`NonceManager::release`] keeps the rule as an internal belt instead, and a §45 control that
    // could never go red is not a control.
    //
    // §35/NC13: the second lane's arithmetic. Every figure of the subtraction is quoted, because
    // "insufficient" without the numbers is the sentence this milestone exists to replace.
    #[error(
        "§35: {lane} asks {requested} of domain `{domain}` and only {free} is unreserved of a \
         capacity {capacity} ({reserved} already reserved); another lane's reservation is why"
    )]
    InsufficientCapital {
        lane: LaneId,
        requested: String,
        free: String,
        reserved: String,
        capacity: String,
        domain: String,
    },
    /// §36: the lane was never chosen, so it gets no nonce. This is the arm that makes "全部
    /// broadcast" unrepresentable: reservation is gated on the winner decision, not on the caller
    /// remembering that a decision existed.
    #[error("§36: {lane} is not the selected winner ({winner}), so nothing is reserved for it")]
    LaneNotSelected { lane: LaneId, winner: LaneId },
    /// §36: the previous winner is already at or past its reservation, so it cannot be un-picked —
    /// its nonce may be in flight, and a second winner would be a second claim on the same signer.
    #[error(
        "§36: {winner} was already selected and is at `{state}`; a winner whose reservation is taken \
         cannot be re-ranked, because its nonce may be in flight"
    )]
    WinnerReserved { winner: LaneId, state: LaneState },
    /// §36 with no winner yet. Even one candidate goes through the ranking: §36 lists winner selection
    /// as a step, and a build that reaches the same answer by accident cannot show which candidate it
    /// decided to send.
    #[error(
        "§36: no winner has been selected, so nothing may be reserved; ranking is a step, not an \
         accident of there being one candidate"
    )]
    NoWinnerSelected,
    /// §36: only a lane risk accepted can stand in a ranking.
    #[error("§36: {lane} is `{state}`, not `ready`, so it has no standing to be ranked")]
    LaneNotReady { lane: LaneId, state: LaneState },
    /// §36: one lane ranked twice, which would let a single candidate be counted as two and give a
    /// ranking a winner that does not exist.
    #[error("§36: {lane} appears twice in the ranking")]
    DuplicateStanding { lane: LaneId },
    /// §31/§32: a lane records its run once. A second run would make the lane's `plan_hash` a claim
    /// about a simulation that is not the one §30 bound, and the lane would then hold two hashes with
    /// only one of them recorded.
    #[error(
        "§31: {lane} already records plan {previous_plan:#x}, so a second run cannot be attached"
    )]
    AlreadyRecorded { lane: LaneId, previous_plan: B256 },
    /// §32: the run is attached while the lane is working — after `Created` and before `Ready`.
    /// Attaching it later would rewrite a lane whose decision has already been taken; attaching it
    /// earlier would be a record of a step that has not happened.
    #[error("§32: {lane} is `{state}`, so there is no working step to attach a run to")]
    NotRecordable { lane: LaneId, state: LaneState },
    /// §32: an arrow that is not in the table.
    #[error("§32: {lane} cannot move from `{from}` to `{to}` — {note}")]
    IllegalTransition {
        lane: LaneId,
        from: String,
        to: String,
        note: String,
    },
    /// §30/§32: `Ready` without the run and the plan it would be a claim about.
    #[error("§30: {lane} has no {missing} recorded, so there is nothing to be ready to execute")]
    UnrecordedLane { lane: LaneId, missing: &'static str },
    /// The lane is not in this ledger.
    #[error("§31: no lane {lane} in this ledger")]
    UnknownLane { lane: LaneId },
}

impl LaneRefusal {
    /// The machine label §43's rows group by.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::DuplicateLane { .. } => "duplicate_lane",
            Self::DuplicatePlan { .. } => "duplicate_plan",
            Self::NonceHeld { .. } => "nonce_held",
            Self::InsufficientCapital { .. } => "insufficient_capital",
            Self::LaneNotSelected { .. } => "lane_not_selected",
            Self::WinnerReserved { .. } => "winner_reserved",
            Self::NoWinnerSelected => "no_winner_selected",
            Self::LaneNotReady { .. } => "lane_not_ready",
            Self::DuplicateStanding { .. } => "duplicate_standing",
            Self::AlreadyRecorded { .. } => "already_recorded",
            Self::NotRecordable { .. } => "not_recordable",
            Self::IllegalTransition { .. } => "illegal_transition",
            Self::UnrecordedLane { .. } => "unrecorded_lane",
            Self::UnknownLane { .. } => "unknown_lane",
        }
    }
}

/// §31–§36's ledger: the lanes, the one nonce book, the one capital domain, the one winner.
///
/// No `Serialize` derive, on purpose: both books are keyed by [`LaneId`], a newtyped integer, and
/// serde_json refuses such a map key. A caller that wants the run as rows collects
/// [`LaneLedger::lanes`] and [`NonceManager::held`] into an array — lane-ordered, which is the shape
/// §47's byte comparison wants anyway.
#[derive(Clone, Debug)]
pub struct LaneLedger {
    lanes: BTreeMap<LaneId, CandidateLane>,
    next_lane_id: u64,
    nonces: NonceManager,
    capital: CapitalDomain,
    winner: Option<LaneId>,
    /// §36's ranking, kept so §47's determinism check can re-run over the standings that decided the
    /// winner rather than over a paraphrase of them.
    standings: Vec<LaneStanding>,
}

impl LaneLedger {
    /// Open a ledger whose capital pool is one domain at `capacity` — §35's first version, which asks
    /// for one domain and no portfolio management.
    pub fn new(domain_id: impl Into<String>, capacity: U256) -> Self {
        Self {
            lanes: BTreeMap::new(),
            next_lane_id: 0,
            nonces: NonceManager::new(),
            capital: CapitalDomain::open(domain_id, capacity),
            winner: None,
            standings: Vec::new(),
        }
    }

    /// §31: one lane per candidate. The candidate id is the key the finding carries, and a second lane
    /// for the same id is refused before it can compete for anything.
    pub fn open(&mut self, candidate_id: impl Into<String>) -> Result<LaneId, LaneRefusal> {
        let candidate_id = candidate_id.into();
        if let Some(existing) = self
            .lanes
            .values()
            .find(|lane| lane.candidate_id == candidate_id)
        {
            return Err(LaneRefusal::DuplicateLane {
                candidate_id,
                existing: existing.lane_id,
            });
        }
        let lane_id = LaneId(self.next_lane_id);
        self.next_lane_id += 1;
        self.lanes.insert(
            lane_id,
            CandidateLane {
                lane_id,
                candidate_id,
                plan_hash: None,
                simulation_id: None,
                state: LaneState::Created,
                nonce_reservation: None,
                capital_reservation: None,
                history: Vec::new(),
            },
        );
        Ok(lane_id)
    }

    /// §33's allowance, expressed as the count a caller can assert on: every lane in a state, in lane
    /// order.
    pub fn in_state(&self, state: LaneState) -> Vec<LaneId> {
        self.lanes
            .values()
            .filter(|lane| lane.state == state)
            .map(|lane| lane.lane_id)
            .collect()
    }

    /// §33's parallel-simulation row, spelled out: the lanes that are simulating right now. A caller
    /// asserts on this to show that three candidates ran simulations at the same time and none of
    /// them needed a lock, because none of them had claimed a nonce.
    pub fn simulating(&self) -> Vec<LaneId> {
        self.in_state(LaneState::Simulating)
    }

    /// The lane behind an id, or [`LaneRefusal::UnknownLane`]. Mirrors the private
    /// [`LaneLedger::lane_mut`] so a caller's `?` reads the same on both sides of a `&mut self`.
    pub fn lane(&self, lane_id: LaneId) -> Result<&CandidateLane, LaneRefusal> {
        match self.lanes.get(&lane_id) {
            Some(lane) => Ok(lane),
            None => Err(LaneRefusal::UnknownLane { lane: lane_id }),
        }
    }

    /// The lanes, in lane order — the deterministic iteration §47 needs.
    pub fn lanes(&self) -> impl Iterator<Item = &CandidateLane> {
        self.lanes.values()
    }

    pub fn nonces(&self) -> &NonceManager {
        &self.nonces
    }

    pub fn capital(&self) -> &CapitalDomain {
        &self.capital
    }

    pub fn winner(&self) -> Option<LaneId> {
        self.winner
    }

    /// §36's ranking, as supplied.
    pub fn standings(&self) -> &[LaneStanding] {
        &self.standings
    }

    /// §33: enter the ladder's first arrow. Nothing about a simulation is known yet, so nothing is
    /// recorded beyond the state.
    pub fn begin_simulation(&mut self, lane_id: LaneId) -> Result<(), LaneRefusal> {
        self.move_to(
            lane_id,
            LaneState::Simulating,
            "simulation started".to_string(),
        )
    }

    /// Record the run this lane is about.
    ///
    /// This is a fact, not an arrow: it attaches §31's `simulation_id` and `plan_hash` to a lane that
    /// is working (`Simulating` or `RiskChecking`) and moves nothing, so the refusal "reached `Ready`
    /// with no run behind it" stays a real answer rather than something the machine makes impossible
    /// by construction. A `plan_hash` already bound by another lane is refused: two lanes with one hash
    /// would each claim a different execution of a plan whose hash says there is one (§45's
    /// duplicate-plan row).
    pub fn record_simulation(
        &mut self,
        lane_id: LaneId,
        simulation_id: impl Into<String>,
        plan_hash: B256,
    ) -> Result<(), LaneRefusal> {
        let simulation_id = simulation_id.into();
        if let Some(existing) = self
            .lanes
            .values()
            .filter(|lane| lane.lane_id != lane_id)
            .find(|lane| lane.plan_hash == Some(plan_hash))
        {
            return Err(LaneRefusal::DuplicatePlan {
                plan_hash,
                existing: existing.lane_id,
            });
        }
        let lane = self.lane(lane_id)?;
        if let Some(previous) = lane.plan_hash {
            return Err(LaneRefusal::AlreadyRecorded {
                lane: lane_id,
                previous_plan: previous,
            });
        }
        if !matches!(lane.state, LaneState::Simulating | LaneState::RiskChecking) {
            return Err(LaneRefusal::NotRecordable {
                lane: lane_id,
                state: lane.state,
            });
        }
        let lane = self.lane_mut(lane_id)?;
        lane.simulation_id = Some(simulation_id);
        lane.plan_hash = Some(plan_hash);
        Ok(())
    }

    /// §36's winner selection.
    ///
    /// The rule is a total order — greater gross gain wins, then the lower lane id — so the answer does
    /// not depend on the order standings arrive in (§47). Every standing must name a lane this ledger
    /// knows and a lane in `Ready`; a ranking over candidates risk did not accept is not a ranking, and
    /// §36's "全部 broadcast" is exactly the case where a lane that never won is given a nonce anyway.
    /// Re-ranking is refused once the previous winner is at or past `Reserved`, because that would take
    /// away a nonce that may be in flight.
    pub fn choose_winner(&mut self, standings: &[LaneStanding]) -> Result<LaneId, LaneRefusal> {
        if standings.is_empty() {
            return Err(LaneRefusal::NoWinnerSelected);
        }
        for standing in standings {
            if standings
                .iter()
                .filter(|other| other.lane_id == standing.lane_id)
                .count()
                > 1
            {
                return Err(LaneRefusal::DuplicateStanding {
                    lane: standing.lane_id,
                });
            }
            let lane = self.lane(standing.lane_id)?;
            if lane.state != LaneState::Ready {
                return Err(LaneRefusal::LaneNotReady {
                    lane: standing.lane_id,
                    state: lane.state,
                });
            }
        }
        if let Some(previous) = self.winner {
            let lane = self.lane(previous)?;
            if lane.state.is_at_or_past_reservation() {
                return Err(LaneRefusal::WinnerReserved {
                    winner: previous,
                    state: lane.state,
                });
            }
        }
        let mut best: Option<&LaneStanding> = None;
        for standing in standings {
            let better = match best {
                None => true,
                Some(current) => {
                    (standing.gross_gain, Reverse(standing.lane_id.0))
                        > (current.gross_gain, Reverse(current.lane_id.0))
                }
            };
            if better {
                best = Some(standing);
            }
        }
        let winner = match best {
            Some(standing) => standing.lane_id,
            None => return Err(LaneRefusal::NoWinnerSelected),
        };
        self.standings = standings.to_vec();
        self.winner = Some(winner);
        Ok(winner)
    }

    /// §34/§35/§36's reservation step: capital first, then the nonce, with the capital handed back if
    /// the nonce is refused.
    ///
    /// Capital is asked first because it is the cheaper arithmetic and because a lane that cannot pay
    /// should not be told about a nonce collision it never reached. §36 is asked before both, and that
    /// order is what §35's sentence really needs: with one winner there is only ever one live
    /// reservation, so the two-lanes-each-believing-they-can-execute case is not detected here, it is
    /// made unreachable — a losing lane is turned away by `LaneNotSelected` before it ever sees the
    /// domain's arithmetic. The domain still refuses a winner that cannot pay, which is the half §35
    /// can decide on its own, and §45's capital control asserts both halves separately.
    pub fn reserve_for(
        &mut self,
        lane_id: LaneId,
        signer: Address,
        nonce: u64,
        input: U256,
    ) -> Result<ReservationPair, LaneRefusal> {
        let winner = self.winner.ok_or(LaneRefusal::NoWinnerSelected)?;
        if winner != lane_id {
            return Err(LaneRefusal::LaneNotSelected {
                lane: lane_id,
                winner,
            });
        }
        // Every check that can refuse runs before anything is taken, so a refusal never leaves capital
        // or a nonce reserved by a lane that was never reserved. The recorded run is not re-asked
        // here: `Ready` is the only state this arrow leaves from and require_edge already guards it.
        self.require_edge(lane_id, LaneState::Reserved)?;
        let capital = self.capital.reserve(lane_id, input)?;
        let nonce_record = match self.nonces.reserve(lane_id, signer, nonce) {
            Ok(record) => record,
            Err(refusal) => {
                // §34's release, applied to the half that was taken and could not be kept. This is the
                // rollback the NC12 control reads: a nonce collision must not cost capital.
                self.capital.release(&capital);
                return Err(refusal);
            }
        };
        self.move_to(
            lane_id,
            LaneState::Reserved,
            format!("nonce {nonce} for signer {signer:#x} and capital {input} reserved"),
        )?;
        let lane = self.lane_mut(lane_id)?;
        lane.nonce_reservation = Some(nonce_record);
        lane.capital_reservation = Some(capital.clone());
        Ok(ReservationPair {
            nonce: nonce_record,
            capital,
        })
    }

    /// §32's plain forward arrows: `Created → Simulating`, `RiskChecking → Ready`, `Ready → …`,
    /// `Submitting → Submitted → Included`, with the caller's reason for the step.
    ///
    /// `Reserved` and the four failure states are not reachable from here — the first because taking a
    /// reservation is [`LaneLedger::reserve_for`], the rest because ending a lane is
    /// [`LaneLedger::end`] — and `Settled` is excluded because committing a nonce is
    /// [`LaneLedger::settle`]. A lane cannot be *told* into holding a nonce or into a grave; that is
    /// the difference between a state machine and a field.
    pub fn advance(
        &mut self,
        lane_id: LaneId,
        to: LaneState,
        note: impl Into<String>,
    ) -> Result<(), LaneRefusal> {
        if to == LaneState::Reserved || to == LaneState::Settled || to.is_terminal() {
            let from = self.lane(lane_id)?.state;
            return Err(LaneRefusal::IllegalTransition {
                lane: lane_id,
                from: from.code().to_string(),
                to: to.code().to_string(),
                note: LaneState::refusal_note(from, to),
            });
        }
        self.move_to(lane_id, to, note.into())
    }

    /// §34: the run is in a block and the lane is finished. Commits the nonce (the number was spent,
    /// whatever the transaction achieved) and settles the capital reservation.
    pub fn settle(&mut self, lane_id: LaneId) -> Result<(), LaneRefusal> {
        self.require_edge(lane_id, LaneState::Settled)?;
        let capital = self.lane(lane_id)?.capital_reservation.clone();
        self.nonces.commit(lane_id);
        if let Some(reservation) = capital {
            self.capital.settle(&reservation);
        }
        self.move_to(
            lane_id,
            LaneState::Settled,
            "receipt bound; §34 commit".to_string(),
        )?;
        let lane = self.lane_mut(lane_id)?;
        if let Some(record) = lane.nonce_reservation.as_mut() {
            record.stage = NonceStage::Committed;
        }
        lane.capital_reservation = None;
        Ok(())
    }

    /// §32/§34: end the lane, and apply §34's rule to whatever it was holding.
    ///
    /// The state comes from the reason and not from the caller: an arm that maps to `Rejected` cannot be
    /// filed as `Cancelled`, because the two answer different questions in §43's tables (who said no,
    /// and whether anybody did). A revert is legal only from `Included`, because a revert is something a
    /// receipt tells you.
    ///
    /// The two dispositions differ about the nonce and not about the capital. §34's commit is a chain
    /// fact — the number appeared in a block, so it is spent and cannot be given back — while a capital
    /// reservation is a ledger fact, and a lane that has ended has no claim on the pool left to make.
    /// Committing a revert and leaving its input reserved would shrink §35's `available_capital` by the
    /// amount every time a transaction failed, which is the opposite of what the figure is for: it is
    /// what the next lane may be told it can spend.
    pub fn end(
        &mut self,
        lane_id: LaneId,
        failure: LaneFailure,
    ) -> Result<EndOutcome, LaneRefusal> {
        let to = failure.lane_state();
        let from = self.lane(lane_id)?.state;
        if matches!(failure, LaneFailure::RevertedInBlock { .. }) && from != LaneState::Included {
            return Err(LaneRefusal::IllegalTransition {
                lane: lane_id,
                from: from.code().to_string(),
                to: to.code().to_string(),
                note: "a revert is known from a receipt, so the lane has to be recorded Included \
                       before a receipt can end it Failed"
                    .to_string(),
            });
        }
        self.require_edge(lane_id, to)?;
        let detail = failure.detail();
        let disposition = failure.disposition();
        let held = self.lane(lane_id)?.nonce_reservation;
        let mut released_nonce = None;
        let mut released_capital = None;
        match disposition {
            ReservationDisposition::Release => {
                if held.is_some() {
                    self.nonces.release(lane_id);
                    released_nonce = held;
                }
            }
            ReservationDisposition::Commit => {
                if held.is_some() {
                    self.nonces.commit(lane_id);
                }
            }
        }
        let capital = self.lane(lane_id)?.capital_reservation.clone();
        if let Some(reservation) = capital {
            self.capital.release(&reservation);
            released_capital = Some(reservation);
        }
        let note = format!("{}: {detail}", failure.code());
        self.move_to(lane_id, to, note)?;
        let lane = self.lane_mut(lane_id)?;
        if disposition == ReservationDisposition::Release {
            lane.nonce_reservation = None;
        } else if let Some(record) = lane.nonce_reservation.as_mut() {
            record.stage = NonceStage::Committed;
        }
        lane.capital_reservation = None;
        Ok(EndOutcome {
            lane: lane_id,
            from: from.code().to_string(),
            to: to.code().to_string(),
            reason: failure.code().to_string(),
            disposition,
            released_nonce,
            released_capital,
        })
    }

    /// §32's table, consulted before anything moves.
    fn require_edge(&self, lane_id: LaneId, to: LaneState) -> Result<LaneState, LaneRefusal> {
        let lane = self.lane(lane_id)?;
        let from = lane.state;
        if !from.allows(to) {
            return Err(LaneRefusal::IllegalTransition {
                lane: lane_id,
                from: from.code().to_string(),
                to: to.code().to_string(),
                note: LaneState::refusal_note(from, to),
            });
        }
        if to == LaneState::Ready {
            if lane.simulation_id.is_none() {
                return Err(LaneRefusal::UnrecordedLane {
                    lane: lane_id,
                    missing: "simulation id",
                });
            }
            if lane.plan_hash.is_none() {
                return Err(LaneRefusal::UnrecordedLane {
                    lane: lane_id,
                    missing: "plan hash",
                });
            }
        }
        Ok(from)
    }

    /// Cross an arrow the table allows and write the history line.
    fn move_to(&mut self, lane_id: LaneId, to: LaneState, note: String) -> Result<(), LaneRefusal> {
        let from = self.require_edge(lane_id, to)?;
        let lane = self.lane_mut(lane_id)?;
        lane.state = to;
        lane.history.push(LaneMove { from, to, note });
        Ok(())
    }

    fn lane_mut(&mut self, lane_id: LaneId) -> Result<&mut CandidateLane, LaneRefusal> {
        match self.lanes.get_mut(&lane_id) {
            Some(lane) => Ok(lane),
            None => Err(LaneRefusal::UnknownLane { lane: lane_id }),
        }
    }
}
