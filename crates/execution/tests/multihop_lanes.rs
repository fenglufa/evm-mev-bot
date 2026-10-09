//! §45's Lane rows and §46's NC12–NC15: the ledger that keeps two candidates from holding one
//! nonce, one capital pool, or one plan hash at the same time.
//!
//! Nothing in this file touches REVM, an endpoint, or a private key. That is the point of §31–§36: the
//! ledger's whole job is to say which of two lanes may hold a thing, and the answers are decided by
//! arithmetic and a state table, so a scripted number is not a shortcut here — it is the input the
//! module is defined on. §2's "no new Signer framework, no new Receipt framework" is honoured by
//! construction: the signer is a 20-byte constant and `Included` is a state the caller asserts, exactly
//! as `crates/execution/src/lanes.rs`'s header says both arrive.
//!
//! The negative controls follow the milestone's rule: a refusal is only demonstrated when the same call
//! is also shown to succeed on the value next to it. NC12 therefore not only fails on a nonce a settled
//! lane already spent, it reserves the number one past it; §36's turn-away not only refuses the loser,
//! it lets the winner through the identical arithmetic; and §33's pair refusal is paired with the same
//! number under a second signer, which is legal.

use alloy_primitives::{Address, B256, U256};
use evm_execution::{
    CandidateLane, LaneFailure, LaneId, LaneLedger, LaneRefusal, LaneStanding, LaneState,
    NonceStage, ReservationDisposition,
};

/// §35's one domain, opened at a round number so every subtraction is quotable by hand.
const CAPACITY: u64 = 1_000;
/// The input a lane claims — §35's own example figure.
const INPUT: u64 = 100;
const CHAIN: u64 = 91_342;

fn signer() -> Address {
    Address::from_slice(&[0x6bu8; 20])
}

/// A second wallet, for the one case where two signers legitimately hold the same number.
fn other_signer() -> Address {
    Address::from_slice(&[0x7cu8; 20])
}

fn plan(n: u8) -> B256 {
    B256::left_padding_from(&[n])
}

fn sim_id(n: u8) -> String {
    format!("m11-sim-{CHAIN}-37486792-{n:02x}")
}

fn amount(n: u64) -> U256 {
    U256::from(n)
}

fn ledger() -> LaneLedger {
    LaneLedger::new("m11-one-domain", amount(CAPACITY))
}

/// Drive a lane to `Ready` the way the ladder does it: `Created → Simulating`, the run attached, then
/// `RiskChecking` and `Ready`. What comes back is reservable and nothing more.
fn ready_lane(ledger: &mut LaneLedger, candidate: &str, plan_hash: B256) -> LaneId {
    let lane_id = ledger.open(candidate).expect("a fresh candidate id opens");
    ledger
        .begin_simulation(lane_id)
        .expect("Created advances to Simulating");
    ledger
        .record_simulation(lane_id, sim_id(1), plan_hash)
        .expect("a working lane takes its run");
    ledger
        .advance(lane_id, LaneState::RiskChecking, "risk asked")
        .expect("Simulating advances to RiskChecking");
    ledger
        .advance(lane_id, LaneState::Ready, "risk accepted")
        .expect("RiskChecking advances to Ready with its run recorded");
    lane_id
}

/// The same lane, one step further: §36's ranking has picked it.
fn winner_lane(ledger: &mut LaneLedger, candidate: &str, plan_hash: B256, gain: u64) -> LaneId {
    let lane_id = ready_lane(ledger, candidate, plan_hash);
    let winner = ledger
        .choose_winner(&[LaneStanding::new(lane_id, amount(gain))])
        .expect("a single accepted lane can be ranked");
    assert_eq!(winner, lane_id, "the only standing wins");
    lane_id
}

/// Walk a reserved lane to the arrow that precedes §34's commit.
fn send(ledger: &mut LaneLedger, lane_id: LaneId) {
    for next in [
        LaneState::Submitting,
        LaneState::Submitted,
        LaneState::Included,
    ] {
        ledger
            .advance(lane_id, next, "in flight")
            .expect("the send arrows are all in §32's table");
    }
}

fn state_of(ledger: &LaneLedger, lane_id: LaneId) -> LaneState {
    ledger
        .lane(lane_id)
        .expect("the lane is in this ledger")
        .state
}

fn held_stage(ledger: &LaneLedger, lane_id: LaneId) -> NonceStage {
    ledger
        .lane(lane_id)
        .expect("the lane is in this ledger")
        .nonce_reservation
        .expect("the lane holds a pair")
        .stage
}

// ---------------------------------------------------------------------------
// §31 — the record
// ---------------------------------------------------------------------------

/// §31 lists seven fields for the lane record. All seven are present at open, the four that describe
/// things that have not happened are `None` rather than placeholders, and the eighth is the trail §32
/// promises.
#[test]
fn a_lane_opens_with_the_seven_fields_of_31() {
    let mut ledger = ledger();
    let lane_id = ledger.open("cand-0001").expect("one lane per candidate");

    let lane: &CandidateLane = ledger.lane(lane_id).expect("the lane is in this ledger");
    assert_eq!(lane.lane_id, lane_id);
    assert_eq!(lane.candidate_id, "cand-0001");
    assert_eq!(lane.plan_hash, None, "no plan exists before the run");
    assert_eq!(
        lane.simulation_id, None,
        "no run exists before the simulation"
    );
    assert_eq!(lane.state, LaneState::Created);
    assert_eq!(lane.nonce_reservation, None, "§33: nothing claimed yet");
    assert_eq!(lane.capital_reservation, None, "§35: nothing claimed yet");
    assert!(lane.history.is_empty(), "Created is where the trail starts");
}

/// §31 mints ids in ascending order and never reuses one, so a `lane-2` in an evidence row names one
/// record for the whole run — including after that record has ended.
#[test]
fn lane_ids_are_minted_in_order_and_never_reused() {
    let mut ledger = ledger();
    let first = ledger.open("cand-a").expect("fresh");
    let second = ledger.open("cand-b").expect("fresh");
    assert_eq!((first, second), (LaneId(0), LaneId(1)));

    let _ = ledger.end(first, LaneFailure::Cancelled("dropped".to_string()));
    assert_eq!(
        ledger.open("cand-c").expect("fresh"),
        LaneId(2),
        "a closed lane's id is not handed out again"
    );
}

// ---------------------------------------------------------------------------
// §32 — the machine
// ---------------------------------------------------------------------------

/// The whole ladder, walked once, with both of §31's books read at the far end: the states arrive in
/// §32's order, §34's commit happened, and §35's reservation was settled rather than lost.
#[test]
fn the_ladder_walks_created_through_settled_and_commits_on_the_way_out() {
    let mut ledger = ledger();
    let lane_id = winner_lane(&mut ledger, "cand-full-ladder", plan(1), 50);
    let pair = ledger
        .reserve_for(lane_id, signer(), 7, amount(INPUT))
        .expect("the winner reserves");
    assert_eq!((pair.nonce.nonce, pair.capital.amount), (7, amount(INPUT)));

    for next in [
        LaneState::Submitting,
        LaneState::Submitted,
        LaneState::Included,
    ] {
        ledger
            .advance(lane_id, next, "the caller's step")
            .expect("the forward arrows are in the table");
    }
    ledger.settle(lane_id).expect("§34: Included settles");

    let lane = ledger.lane(lane_id).expect("the lane is here");
    let walked: Vec<&str> = lane.history.iter().map(|m| m.to.code()).collect();
    assert_eq!(
        walked,
        vec![
            "simulating",
            "risk_checking",
            "ready",
            "reserved",
            "submitting",
            "submitted",
            "included",
            "settled",
        ],
        "every arrow the lane crossed, in order, and no others"
    );
    assert_eq!(
        lane.nonce_reservation.expect("kept").stage,
        NonceStage::Committed,
        "§34: the number was spent"
    );
    assert_eq!(lane.capital_reservation, None, "the reservation is gone");
    assert_eq!(ledger.capital().settled_input(), amount(INPUT));
    assert!(ledger.capital().invariant_holds());
}

/// §32's five terminal states are graves: no arrow leaves them, so a caller that changed its mind
/// cannot resurrect a lane, and the §43 table can treat a terminal state as an end of the trail.
#[test]
fn a_terminal_state_has_no_outgoing_arrow() {
    let terminal: Vec<&str> = LaneState::ALL
        .iter()
        .filter(|state| state.is_terminal())
        .map(|state| state.code())
        .collect();
    assert_eq!(
        terminal,
        vec!["settled", "rejected", "cancelled", "expired", "failed"],
        "Settled plus §32's four failure states"
    );
    for state in LaneState::ALL {
        for next in LaneState::ALL {
            if state.is_terminal() {
                assert!(
                    !state.allows(next),
                    "§32: terminal `{}` must not allow `{}`",
                    state.code(),
                    next.code()
                );
            }
        }
    }
}

/// §32's table as written, checked against the states §32 names: thirteen of them, one label each, and
/// the four that hold a nonce are exactly the span §34 puts between `reserve` and `commit`.
#[test]
fn the_state_table_names_thirteen_states_and_the_four_that_hold_a_nonce() {
    assert_eq!(LaneState::ALL.len(), 13);
    let mut codes: Vec<&str> = LaneState::ALL.iter().map(|state| state.code()).collect();
    codes.sort_unstable();
    assert_eq!(codes.len(), 13, "one label per state");
    assert_eq!(
        codes,
        vec![
            "cancelled",
            "created",
            "expired",
            "failed",
            "included",
            "ready",
            "rejected",
            "reserved",
            "risk_checking",
            "settled",
            "simulating",
            "submitted",
            "submitting",
        ]
    );

    let holders: Vec<&str> = LaneState::ALL
        .iter()
        .filter(|state| state.holds_nonce())
        .map(|state| state.code())
        .collect();
    assert_eq!(
        holders,
        vec!["reserved", "submitting", "submitted", "included"],
        "§34: the nonce stays held from the reserve arrow until the lane settles, `Included` included \
         — and in §32's ladder order, because `ALL` is the ladder"
    );
}

/// A lane whose bytes are already with the node cannot be `Cancelled` or `Expired`: the operator's
/// window no longer decides what the chain does with them. It has one end left, and a caller gets the
/// rule quoted rather than a bare refusal.
#[test]
fn a_lane_that_has_been_sent_cannot_be_cancelled_or_expired() {
    let mut ledger = ledger();
    let lane_id = winner_lane(&mut ledger, "cand-sent", plan(2), 50);
    ledger
        .reserve_for(lane_id, signer(), 7, amount(INPUT))
        .expect("reserved");
    for next in [LaneState::Submitting, LaneState::Submitted] {
        ledger
            .advance(lane_id, next, "sent")
            .expect("the ladder's send arrows");
    }

    let error = ledger
        .end(
            lane_id,
            LaneFailure::Cancelled("operator withdrew".to_string()),
        )
        .expect_err("a sent lane cannot be withdrawn");
    assert_eq!(error.code(), "illegal_transition");
    let text = format!("{error}");
    assert!(
        text.contains("has been sent") && text.contains("submitted"),
        "the refusal names the rule and the state it found: {text}"
    );
    assert_eq!(state_of(&ledger, lane_id), LaneState::Submitted);

    ledger
        .end(
            lane_id,
            LaneFailure::SubmissionProvenRejected("node refused".to_string()),
        )
        .expect("Failed is the one end a sent lane has");
    assert_eq!(state_of(&ledger, lane_id), LaneState::Failed);
    assert_eq!(
        ledger.nonces().outstanding(),
        0,
        "and §34 gives the pair back"
    );
}

/// `advance` crosses the arrows the caller owns. The three it does not — `Reserved`, `Settled`, and the
/// terminal states — are owned by a method that moves the two books in the same step, so a lane
/// declared into one of them would hold a nonce or a grave nobody gave it. Each refusal quotes its
/// owner, which is what turns "illegal" into a diagnosis.
#[test]
fn advance_refuses_the_states_another_method_owns() {
    let mut ledger = ledger();
    let lane_id = ready_lane(&mut ledger, "cand-advance", plan(3));

    for (to, owner) in [
        (LaneState::Reserved, "reserve_for"),
        (LaneState::Settled, "settle"),
        (LaneState::Rejected, "end"),
    ] {
        let error = ledger
            .advance(lane_id, to, "the caller says so")
            .expect_err("advance does not own this arrow");
        assert_eq!(error.code(), "illegal_transition");
        assert!(
            format!("{error}").contains(owner),
            "the refusal for `{}` names {owner}: {error}",
            to.code()
        );
    }
    assert_eq!(state_of(&ledger, lane_id), LaneState::Ready);
    assert_eq!(
        ledger.nonces().outstanding(),
        0,
        "a refused arrow took nothing"
    );
    assert_eq!(ledger.capital().reserved_capital, U256::ZERO);
}

/// §30's binding is what makes `Ready` mean something, so a lane cannot reach it without the run behind
/// the claim — and the refusal says which half is missing rather than leaving the caller to guess
/// whether it forgot the simulation id or the plan hash.
#[test]
fn ready_refuses_a_lane_that_recorded_nothing() {
    let mut ledger = ledger();
    let lane_id = ledger.open("cand-unrecorded").expect("fresh");
    ledger.begin_simulation(lane_id).expect("Simulating");
    ledger
        .advance(lane_id, LaneState::RiskChecking, "risk asked")
        .expect("Simulating -> RiskChecking");

    let error = ledger
        .advance(lane_id, LaneState::Ready, "risk accepted")
        .expect_err("a Ready lane must name the run it is about");
    assert_eq!(error.code(), "unrecorded_lane");
    assert!(
        format!("{error}").contains("simulation id"),
        "the first missing half is the one quoted: {error}"
    );
    assert_eq!(state_of(&ledger, lane_id), LaneState::RiskChecking);

    ledger
        .record_simulation(lane_id, sim_id(43), plan(43))
        .expect("RiskChecking is still a working step");
    ledger
        .advance(lane_id, LaneState::Ready, "risk accepted")
        .expect("and the arrow opens");
}

// ---------------------------------------------------------------------------
// §31/§32 — recording the run
// ---------------------------------------------------------------------------

/// A lane records one run. A second `plan_hash` would make the record a claim about a simulation that
/// is not the one §30 bound, and §43's table would carry two hashes for one lane with no way to say
/// which execution was judged.
#[test]
fn a_lane_records_its_run_once() {
    let mut ledger = ledger();
    let lane_id = ledger.open("cand-record").expect("fresh");
    ledger.begin_simulation(lane_id).expect("Simulating");
    ledger
        .record_simulation(lane_id, sim_id(4), plan(4))
        .expect("the first run attaches");

    let error = ledger
        .record_simulation(lane_id, sim_id(5), plan(5))
        .expect_err("a second run does not overwrite the first");
    assert_eq!(error.code(), "already_recorded");
    assert!(
        format!("{error}").contains(&plan(4).to_string()),
        "the refusal quotes the hash that stayed: {error}"
    );
    assert_eq!(ledger.lane(lane_id).expect("here").plan_hash, Some(plan(4)));
    assert_eq!(
        ledger.lane(lane_id).expect("here").simulation_id,
        Some(sim_id(4)),
        "and the simulation id it came with"
    );
}

/// The run attaches while the lane is working. A `Created` lane has not started the step, and a lane
/// that has been ranked has already made its decision — attaching to either would be a record of a step
/// that is not happening.
#[test]
fn a_run_attaches_only_to_a_working_lane() {
    let mut ledger = ledger();
    let lane_id = ledger.open("cand-window").expect("fresh");
    let error = ledger
        .record_simulation(lane_id, sim_id(6), plan(6))
        .expect_err("Created has not started the step yet");
    assert_eq!(error.code(), "not_recordable");
    assert!(
        format!("{error}").contains("created"),
        "the refusal quotes the state it found: {error}"
    );

    ledger.begin_simulation(lane_id).expect("Simulating");
    ledger
        .record_simulation(lane_id, sim_id(6), plan(6))
        .expect("Simulating is a working step");

    let other = ledger.open("cand-second-window").expect("fresh");
    ledger.begin_simulation(other).expect("Simulating");
    ledger
        .advance(other, LaneState::RiskChecking, "risk asked")
        .expect("RiskChecking is still working");
    ledger
        .record_simulation(other, sim_id(7), plan(7))
        .expect("a lane records on the risk arrow too");
}

/// §45's duplicate-plan row, and §46 `NC14`'s second half: two lanes carrying one `plan_hash` are two
/// claims to execute a plan whose hash says there is one execution. The control beside it binds a
/// different hash to the second lane and succeeds.
#[test]
fn nc14_one_plan_hash_binds_one_lane() {
    let mut ledger = ledger();
    let first = ledger.open("cand-one").expect("fresh");
    let second = ledger.open("cand-two").expect("fresh");
    for lane_id in [first, second] {
        ledger.begin_simulation(lane_id).expect("Simulating");
    }
    ledger
        .record_simulation(first, sim_id(8), plan(8))
        .expect("the first claim");

    let error = ledger
        .record_simulation(second, sim_id(9), plan(8))
        .expect_err("the same hash cannot bind a second lane");
    assert_eq!(error.code(), "duplicate_plan");
    assert!(
        format!("{error}").contains(&first.to_string()),
        "the refusal names the holder: {error}"
    );
    assert_eq!(ledger.lane(second).expect("here").plan_hash, None);

    ledger
        .record_simulation(second, sim_id(9), plan(9))
        .expect("a different hash is a different plan");
    assert_eq!(ledger.lane(second).expect("here").plan_hash, Some(plan(9)));
}

/// §46 `NC14`, first half: §31's record is per candidate. A second lane for one candidate id would be
/// two claims on one finding, and the two would then compete for the same nonce as strangers.
#[test]
fn nc14_one_candidate_gets_one_lane() {
    let mut ledger = ledger();
    let first = ledger.open("cand-dup").expect("fresh");
    let error = ledger
        .open("cand-dup")
        .expect_err("the same candidate does not get a second lane");
    assert_eq!(error.code(), "duplicate_lane");
    assert!(
        format!("{error}").contains(&first.to_string()),
        "the refusal names the lane that already speaks for it: {error}"
    );
    assert_eq!(ledger.lanes().count(), 1);
}

// ---------------------------------------------------------------------------
// §33 — parallel by allowance, exclusive only where it must be
// ---------------------------------------------------------------------------

/// §33 allows A/B/C to simulate and to pass risk at the same time. Nothing here has to permit that,
/// because nothing is claimed before `Reserved`: three lanes sit in `Simulating`, the nonce book is
/// empty, and the capital pool has not moved.
#[test]
fn three_lanes_simulate_in_parallel_and_hold_nothing() {
    let mut ledger = ledger();
    let mut lanes = Vec::new();
    for (index, candidate) in ["cand-a", "cand-b", "cand-c"].iter().enumerate() {
        let lane_id = ledger.open(*candidate).expect("fresh");
        ledger.begin_simulation(lane_id).expect("Simulating");
        ledger
            .record_simulation(lane_id, sim_id(index as u8 + 1), plan(index as u8 + 10))
            .expect("each lane records its own run");
        lanes.push(lane_id);
    }

    assert_eq!(
        lanes,
        ledger.simulating(),
        "§33: all three at once, in lane order"
    );
    assert_eq!(
        ledger.nonces().outstanding(),
        0,
        "no simulation has claimed a nonce"
    );
    assert_eq!(ledger.capital().reserved_capital, U256::ZERO);
    assert_eq!(
        ledger.capital().available_capital,
        amount(CAPACITY),
        "parallel work costs the pool nothing"
    );

    for lane_id in &lanes {
        ledger
            .advance(*lane_id, LaneState::RiskChecking, "risk asked")
            .expect("parallel risk pass");
        ledger
            .advance(*lane_id, LaneState::Ready, "risk accepted")
            .expect("parallel accept");
    }
    assert_eq!(ledger.in_state(LaneState::Ready).len(), 3);
    assert_eq!(
        ledger.nonces().outstanding(),
        0,
        "risk passing claims nothing either — §33's parallelism is free at both steps"
    );
}

// ---------------------------------------------------------------------------
// §33/§34/NC12 — the nonce book
// ---------------------------------------------------------------------------

/// NC12: one `(signer, nonce)` pair, one lane. The collision refused here is the one §34's lifecycle
/// actually produces — a settled winner has *spent* number 7, so a second lane asking for 7 is asking
/// for a number a block already holds. The control beside it reserves number 8 and succeeds, which is
/// what makes the refusal a statement about the pair rather than about the caller.
#[test]
fn nc12_a_committed_pair_cannot_be_taken_twice() {
    let mut ledger = ledger();
    let first = winner_lane(&mut ledger, "cand-nonce-holder", plan(11), 90);
    ledger
        .reserve_for(first, signer(), 7, amount(INPUT))
        .expect("the winner reserves");
    send(&mut ledger, first);
    ledger.settle(first).expect("§34 commit");
    assert_eq!(
        ledger.nonces().held_by(signer(), 7),
        Some(first),
        "a spent pair stays in the book"
    );
    assert_eq!(held_stage(&ledger, first), NonceStage::Committed);

    let second = ready_lane(&mut ledger, "cand-nonce-seeker", plan(12));
    let _ = ledger
        .choose_winner(&[LaneStanding::new(second, amount(80))])
        .expect("a settled winner is finished, so the ledger may rank again");

    let error = ledger
        .reserve_for(second, signer(), 7, amount(INPUT))
        .expect_err("number 7 is history");
    assert_eq!(error.code(), "nonce_held");
    let text = format!("{error}");
    assert!(
        text.contains("committed") && text.contains(&first.to_string()),
        "both the stage and the holder are quoted: {text}"
    );
    assert_eq!(
        ledger.nonces().of(second),
        None,
        "the refused lane holds nothing"
    );

    let pair = ledger
        .reserve_for(second, signer(), 8, amount(INPUT))
        .expect("the control: the very next number is free");
    assert_eq!(
        (pair.nonce.nonce, pair.nonce.stage),
        (8, NonceStage::Reserved)
    );
    assert_eq!(state_of(&ledger, second), LaneState::Reserved);
}

/// §33's contested resource is a *pair*, not a number: two wallets may each hold number 7, and a book
/// keyed on the nonce alone would refuse a second operator's lane for no reason. The spent pair stays
/// spent under its own signer while the second signer takes the same number — two holdings, one book.
#[test]
fn a_pair_is_a_signer_and_a_number_not_a_number_alone() {
    let mut ledger = ledger();
    let holder = winner_lane(&mut ledger, "cand-signer-one", plan(13), 90);
    ledger
        .reserve_for(holder, signer(), 7, amount(INPUT))
        .expect("reserved");
    send(&mut ledger, holder);
    ledger.settle(holder).expect("committed");

    let seeker = ready_lane(&mut ledger, "cand-signer-two", plan(14));
    let _ = ledger
        .choose_winner(&[LaneStanding::new(seeker, amount(80))])
        .expect("re-rank past a settled winner");
    let pair = ledger
        .reserve_for(seeker, other_signer(), 7, amount(INPUT))
        .expect("§33's pair is (signer, nonce), so another wallet may hold the same number");
    assert_eq!((pair.nonce.signer, pair.nonce.nonce), (other_signer(), 7));
    assert_eq!(
        ledger.nonces().held_by(signer(), 7),
        Some(holder),
        "the spent pair stays spent"
    );
    assert_eq!(ledger.nonces().held_by(other_signer(), 7), Some(seeker));
    assert_eq!(
        ledger.nonces().outstanding(),
        2,
        "two holdings, one per lane"
    );
    assert!(ledger.capital().invariant_holds());
}

/// A lane that ends before it ever reserved releases nothing, because it held nothing — and the outcome
/// says so, rather than a reader inferring it from an empty ledger.
#[test]
fn a_lane_that_never_reserved_ends_with_nothing_to_release() {
    let mut ledger = ledger();
    let lane_id = ready_lane(&mut ledger, "cand-no-reservation", plan(15));
    let outcome = ledger
        .end(
            lane_id,
            LaneFailure::RiskRejected("floor not met".to_string()),
        )
        .expect("Ready ends Rejected");

    assert_eq!(outcome.lane, lane_id);
    assert_eq!(
        (outcome.from.as_str(), outcome.to.as_str()),
        ("ready", "rejected")
    );
    assert_eq!(outcome.reason, "risk_rejected");
    assert_eq!(outcome.disposition, ReservationDisposition::Release);
    assert_eq!(outcome.released_nonce, None, "no pair was ever taken");
    assert_eq!(outcome.released_capital, None, "no capital was ever taken");
    assert_eq!(ledger.nonces().outstanding(), 0);
    assert_eq!(ledger.capital().available_capital, amount(CAPACITY));
}

/// §34's three must-release reasons, each from the state the ladder puts them in: `SimulationFailed`
/// mid-simulation, `RiskRejected` at the accept decision, `PlanRefused` while both resources are held.
/// Only the last has anything to give back, and the outcome distinguishes them.
#[test]
fn the_release_reasons_of_34_give_back_exactly_what_the_lane_held() {
    let mut ledger = ledger();
    let simulating = ledger.open("cand-sim-fail").expect("fresh");
    ledger.begin_simulation(simulating).expect("Simulating");
    let outcome = ledger
        .end(
            simulating,
            LaneFailure::SimulationFailed("second leg reverted".to_string()),
        )
        .expect("Simulating ends Rejected");
    assert_eq!(
        (outcome.to.as_str(), outcome.reason.as_str()),
        ("rejected", "simulation_failed")
    );
    assert_eq!(outcome.released_nonce, None);
    assert_eq!(outcome.released_capital, None);

    let rejected = ready_lane(&mut ledger, "cand-risk-fail", plan(16));
    let outcome = ledger
        .end(
            rejected,
            LaneFailure::RiskRejected("stale block".to_string()),
        )
        .expect("Ready ends Rejected");
    assert_eq!(outcome.reason, "risk_rejected");
    assert_eq!(
        outcome.released_capital, None,
        "a Ready lane had not reserved yet"
    );

    let holder = winner_lane(&mut ledger, "cand-held-then-refused", plan(17), 50);
    ledger
        .reserve_for(holder, signer(), 7, amount(INPUT))
        .expect("reserved");
    let outcome = ledger
        .end(
            holder,
            LaneFailure::PlanRefused("§30 calldata hash mismatch".to_string()),
        )
        .expect("Reserved ends Rejected");
    let nonce = outcome.released_nonce.expect("the pair came back");
    assert_eq!(
        (nonce.signer, nonce.nonce, nonce.stage),
        (signer(), 7, NonceStage::Reserved),
        "released, never committed"
    );
    assert_eq!(
        outcome
            .released_capital
            .expect("the capital came back")
            .amount,
        amount(INPUT)
    );
    assert_eq!(ledger.nonces().outstanding(), 0, "the pair is free again");
    assert_eq!(ledger.nonces().held_by(signer(), 7), None);
    assert_eq!(ledger.capital().available_capital, amount(CAPACITY));
    assert!(ledger.capital().invariant_holds());
}

// ---------------------------------------------------------------------------
// §35/NC13 and §36/NC36 — the domain and the winner rule
// ---------------------------------------------------------------------------

/// NC13: the winner asks the domain for more than the domain has. The refusal quotes all four figures
/// of the subtraction, because "insufficient" without the numbers is the sentence §35 exists to
/// replace — and the lane is left holding nothing, which is what makes the refusal safe rather than
/// merely loud.
#[test]
fn nc13_a_winner_that_cannot_pay_is_refused_and_keeps_nothing() {
    let mut ledger = ledger();
    let lane_id = winner_lane(&mut ledger, "cand-oversized", plan(18), 50);
    let error = ledger
        .reserve_for(lane_id, signer(), 7, amount(CAPACITY + 1))
        .expect_err("the domain is smaller than the ask");
    assert_eq!(error.code(), "insufficient_capital");
    let text = format!("{error}");
    for figure in [
        (CAPACITY + 1).to_string(),
        CAPACITY.to_string(),
        "m11-one-domain".to_string(),
    ] {
        assert!(
            text.contains(figure.as_str()),
            "the refusal quotes {figure}: {text}"
        );
    }
    assert!(
        text.contains("only 1000") && text.contains("0 already reserved"),
        "the free and reserved halves are both stated: {text}"
    );
    assert_eq!(
        ledger.nonces().outstanding(),
        0,
        "capital is asked first, so a refusal never took a nonce either"
    );
    assert_eq!(ledger.capital().reserved_capital, U256::ZERO);
    assert_eq!(ledger.capital().available_capital, amount(CAPACITY));
    assert_eq!(
        state_of(&ledger, lane_id),
        LaneState::Ready,
        "the lane did not move"
    );
}

/// §36's prohibition is what makes §35's example unreachable rather than detected: with a winner
/// decision in front of the reservation, Lane B is turned away by name before it ever sees the domain's
/// arithmetic. Both lanes ask the same 100 of a 100-capacity pool; only one of them is told about
/// numbers at all.
#[test]
fn nc36_a_losing_lane_is_turned_away_before_it_sees_the_arithmetic() {
    let mut ledger = LaneLedger::new("m11-one-domain", amount(INPUT));
    let winner = ready_lane(&mut ledger, "cand-w", plan(19));
    let loser = ready_lane(&mut ledger, "cand-l", plan(20));
    let picked = ledger
        .choose_winner(&[
            LaneStanding::new(loser, amount(90)),
            LaneStanding::new(winner, amount(100)),
        ])
        .expect("two accepted lanes rank");
    assert_eq!(picked, winner, "the larger gain wins");

    ledger
        .reserve_for(winner, signer(), 7, amount(INPUT))
        .expect("the winner takes the whole pool");

    let error = ledger
        .reserve_for(loser, signer(), 8, amount(INPUT))
        .expect_err("§36: the loser gets no reservation at all");
    assert_eq!(error.code(), "lane_not_selected");
    let text = format!("{error}");
    assert!(
        text.contains("not the selected winner") && text.contains(&winner.to_string()),
        "the refusal says who was picked: {text}"
    );
    assert_eq!(
        ledger.capital().reserved_capital,
        amount(INPUT),
        "the pool moved once, for the winner only — not twice, and not by the loser"
    );
    assert_eq!(ledger.capital().available_capital, U256::ZERO);
    assert_eq!(
        ledger.nonces().outstanding(),
        1,
        "one live reservation, §36"
    );
    assert_eq!(state_of(&ledger, loser), LaneState::Ready);
    assert_eq!(ledger.lane(loser).expect("here").nonce_reservation, None);
}

/// The rollback the ledger owes §34: capital is taken first, so a nonce refusal has to hand it back on
/// the way out. A build that forgot would shrink the pool by 100 for a lane that never ran, and the
/// next collision would then read as an empty wallet rather than a taken number.
#[test]
fn a_nonce_collision_costs_no_capital() {
    let mut ledger = ledger();
    let first = winner_lane(&mut ledger, "cand-rollback-holder", plan(21), 90);
    ledger
        .reserve_for(first, signer(), 7, amount(INPUT))
        .expect("the first winner reserves");
    send(&mut ledger, first);
    ledger.settle(first).expect("committed");

    let second = ready_lane(&mut ledger, "cand-rollback-seeker", plan(22));
    let _ = ledger
        .choose_winner(&[LaneStanding::new(second, amount(80))])
        .expect("re-rank past a settled winner");
    let error = ledger
        .reserve_for(second, signer(), 7, amount(INPUT))
        .expect_err("the committed pair is refused");
    assert_eq!(error.code(), "nonce_held");

    assert_eq!(
        ledger.capital().reserved_capital,
        U256::ZERO,
        "the capital the lane asked for was handed back on the way out"
    );
    assert_eq!(ledger.capital().available_capital, amount(CAPACITY));
    assert!(
        ledger.capital().invariant_holds(),
        "available + reserved is still the capacity"
    );
    assert_eq!(
        ledger.capital().settled_input(),
        amount(INPUT),
        "the first lane's input stands"
    );
    assert_eq!(state_of(&ledger, second), LaneState::Ready);
    assert_eq!(ledger.lane(second).expect("here").capital_reservation, None);
}

/// NC15: the window closes on a lane that is already holding both resources. §34's third must-release is
/// the only one with something to give back, so this is where the pair and the capital actually return
/// — and the lane that follows gets to use both.
#[test]
fn nc15_expiry_from_reserved_returns_the_pair_and_the_capital() {
    let mut ledger = ledger();
    let first = winner_lane(&mut ledger, "cand-expiring", plan(23), 50);
    ledger
        .reserve_for(first, signer(), 7, amount(INPUT))
        .expect("reserved");

    let outcome = ledger
        .end(
            first,
            LaneFailure::PlanExpired("max block age passed".to_string()),
        )
        .expect("§34: expiry releases");
    assert_eq!(
        (outcome.from.as_str(), outcome.to.as_str()),
        ("reserved", "expired")
    );
    assert_eq!(outcome.reason, "plan_expired");
    assert_eq!(outcome.disposition, ReservationDisposition::Release);
    assert_eq!(outcome.released_nonce.expect("pair").nonce, 7);
    assert_eq!(
        outcome.released_capital.expect("capital").amount,
        amount(INPUT)
    );
    assert_eq!(ledger.nonces().outstanding(), 0);
    assert_eq!(ledger.nonces().held_by(signer(), 7), None);
    assert_eq!(ledger.capital().available_capital, amount(CAPACITY));
    assert_eq!(ledger.capital().settled_input(), U256::ZERO, "nothing ran");
    assert!(ledger
        .lane(first)
        .expect("here")
        .capital_reservation
        .is_none());

    let second = ready_lane(&mut ledger, "cand-successor", plan(24));
    let _ = ledger
        .choose_winner(&[LaneStanding::new(second, amount(50))])
        .expect("the successor ranks");
    let pair = ledger
        .reserve_for(second, signer(), 7, amount(INPUT))
        .expect("the expired lane's pair and pool are both reusable");
    assert_eq!((pair.nonce.nonce, pair.capital.amount), (7, amount(INPUT)));
    assert_eq!(ledger.nonces().outstanding(), 1);
    assert_eq!(ledger.nonces().held_by(signer(), 7), Some(second));
}

// ---------------------------------------------------------------------------
// §34 — commit, and the two ways to reach it
// ---------------------------------------------------------------------------

/// A reverted transaction consumed a real nonce, so §34's commit is about the number and not about
/// whether the run achieved anything. Releasing it would hand a spent number to the next lane, whose
/// first submission would then be the previous lane's transaction. The capital is a ledger fact and
/// does go back — a finished lane has no claim on the pool left to make.
#[test]
fn a_revert_commits_the_nonce_and_returns_the_capital() {
    let mut ledger = ledger();
    let lane_id = winner_lane(&mut ledger, "cand-revert", plan(25), 50);
    ledger
        .reserve_for(lane_id, signer(), 7, amount(INPUT))
        .expect("reserved");
    send(&mut ledger, lane_id);

    let outcome = ledger
        .end(
            lane_id,
            LaneFailure::RevertedInBlock {
                transaction_hash: "0x01".to_string(),
                block_number: 37_486_793,
            },
        )
        .expect("§32: Failed from Included");
    assert_eq!(outcome.to, "failed");
    assert_eq!(outcome.reason, "reverted_in_block");
    assert_eq!(outcome.disposition, ReservationDisposition::Commit);
    assert_eq!(outcome.released_nonce, None, "a spent pair is not released");
    assert_eq!(
        outcome.released_capital.expect("capital").amount,
        amount(INPUT)
    );
    assert_eq!(
        held_stage(&ledger, lane_id),
        NonceStage::Committed,
        "the number stays spent"
    );
    assert_eq!(ledger.nonces().outstanding(), 1);
    assert_eq!(ledger.nonces().held_by(signer(), 7), Some(lane_id));
    assert_eq!(ledger.capital().available_capital, amount(CAPACITY));
    assert_eq!(
        ledger.capital().settled_input(),
        U256::ZERO,
        "a revert is not a settle — only the input's return is counted here"
    );
    assert!(ledger.capital().invariant_holds());
    assert!(
        ledger
            .lane(lane_id)
            .expect("here")
            .history
            .last()
            .expect("an arrow")
            .note
            .contains("reverted_in_block"),
        "the trail carries the reason label"
    );
}

/// A revert is a receipt's answer, so a lane that never got one cannot claim it. From `Reserved` the
/// ends available are `PlanRefused` and `PlanExpired`, and the refusal leaves the reservation standing
/// rather than half-applying the revert.
#[test]
fn a_revert_is_refused_before_the_lane_is_included() {
    let mut ledger = ledger();
    let lane_id = winner_lane(&mut ledger, "cand-early-revert", plan(26), 50);
    ledger
        .reserve_for(lane_id, signer(), 7, amount(INPUT))
        .expect("reserved");

    let error = ledger
        .end(
            lane_id,
            LaneFailure::RevertedInBlock {
                transaction_hash: "0x02".to_string(),
                block_number: 37_486_793,
            },
        )
        .expect_err("no receipt, no revert");
    assert_eq!(error.code(), "illegal_transition");
    assert!(
        format!("{error}").contains("a revert is known from a receipt"),
        "the refusal says why: {error}"
    );
    assert_eq!(state_of(&ledger, lane_id), LaneState::Reserved);
    assert_eq!(
        ledger.nonces().outstanding(),
        1,
        "the reservation is untouched"
    );
    assert_eq!(ledger.capital().reserved_capital, amount(INPUT));

    ledger
        .end(
            lane_id,
            LaneFailure::PlanExpired("window closed".to_string()),
        )
        .expect("the legal end from Reserved");
    assert_eq!(ledger.nonces().outstanding(), 0);
    assert_eq!(ledger.capital().reserved_capital, U256::ZERO);
}

/// §36's "a winner whose nonce may be in flight cannot be un-picked" is the same rule §25 states for an
/// unanswered submission: not knowing is not a failure. The lane stays `Submitted`, keeps its pair, and
/// the ledger refuses to rank over it. The only ways out are a receipt that settles it or an answer that
/// proves nothing was sent — there is no `SubmissionUnknown` arm in §32's failures, on purpose.
#[test]
fn an_unanswered_submission_holds_its_lane_open() {
    let mut ledger = ledger();
    let holder = winner_lane(&mut ledger, "cand-unanswered", plan(27), 90);
    ledger
        .reserve_for(holder, signer(), 7, amount(INPUT))
        .expect("reserved");
    for next in [LaneState::Submitting, LaneState::Submitted] {
        ledger
            .advance(holder, next, "sent")
            .expect("the send arrows");
    }

    let challenger = ready_lane(&mut ledger, "cand-challenger", plan(28));
    // Two refusals, in the order the ledger asks them. A ranking that names the in-flight lane itself
    // is stopped at the `Ready` check, and a ranking that names only the challenger is stopped by §36's
    // winner rule — the second is the one that matters, because the challenger did nothing wrong.
    let not_ready = ledger
        .choose_winner(&[LaneStanding::new(holder, amount(10))])
        .expect_err("a Submitted lane has no standing to be ranked");
    assert_eq!(not_ready.code(), "lane_not_ready");

    let error = ledger
        .choose_winner(&[LaneStanding::new(challenger, amount(95))])
        .expect_err("the current winner is in flight");
    assert_eq!(error.code(), "winner_reserved");
    let text = format!("{error}");
    assert!(
        text.contains("submitted") && text.contains("nonce may be in flight"),
        "the refusal quotes the state that is holding: {text}"
    );
    assert_eq!(ledger.winner(), Some(holder), "the winner did not change");
    assert_eq!(state_of(&ledger, holder), LaneState::Submitted);
    assert_eq!(
        ledger.nonces().outstanding(),
        1,
        "the pair is still claimed"
    );
    assert_eq!(
        ledger.capital().reserved_capital,
        amount(INPUT),
        "and so is the capital"
    );

    ledger
        .advance(holder, LaneState::Included, "receipt bound")
        .expect("a receipt is the way forward");
    ledger.settle(holder).expect("and the way out");
    let winner = ledger
        .choose_winner(&[LaneStanding::new(challenger, amount(95))])
        .expect("a settled winner is finished, so the ledger may rank again");
    assert_eq!(winner, challenger);
    assert_eq!(
        ledger.nonces().held_by(signer(), 7),
        Some(holder),
        "still spent"
    );
}

// ---------------------------------------------------------------------------
// §36 — ranking is a step
// ---------------------------------------------------------------------------

/// §36's flow is `ranking → winner → reserve → submit`, and the ranking is a total order: gain first,
/// lane id second, so the answer does not depend on the order the acceptances arrived in (§47). Two
/// lanes tied at 100 pick the lower id from either input order.
#[test]
fn winner_selection_is_a_total_order_independent_of_input_order() {
    for reversed in [false, true] {
        let mut ledger = ledger();
        let low = ready_lane(&mut ledger, "cand-low", plan(29));
        let high_a = ready_lane(&mut ledger, "cand-high-a", plan(30));
        let high_b = ready_lane(&mut ledger, "cand-high-b", plan(31));
        let mut standings = vec![
            LaneStanding::new(low, amount(10)),
            LaneStanding::new(high_a, amount(100)),
            LaneStanding::new(high_b, amount(100)),
        ];
        if reversed {
            standings.reverse();
        }
        let winner = ledger
            .choose_winner(&standings)
            .expect("three accepted lanes rank");
        assert_eq!(
            winner, high_a,
            "the two tied at 100 and the lower lane id won, from either order"
        );
        assert_eq!(ledger.winner(), Some(winner));
        assert_eq!(
            ledger.standings(),
            standings.as_slice(),
            "§47 re-runs the standings that decided, not a paraphrase of them"
        );
    }
}

/// Even a single candidate goes through the ranking: §36 lists winner selection as a step, and a build
/// that reaches the same answer by accident cannot show which candidate it decided to send. So
/// `reserve_for` before any ranking is refused, and so is an empty ranking.
#[test]
fn nothing_is_reserved_before_a_winner_exists() {
    let mut ledger = ledger();
    let lane_id = ready_lane(&mut ledger, "cand-alone", plan(32));

    let error = ledger
        .reserve_for(lane_id, signer(), 7, amount(INPUT))
        .expect_err("§36: reservation is gated on the decision");
    assert_eq!(error.code(), "no_winner_selected");
    assert_eq!(ledger.winner(), None);
    assert_eq!(ledger.nonces().outstanding(), 0);
    assert_eq!(ledger.capital().reserved_capital, U256::ZERO);

    let empty = ledger
        .choose_winner(&[])
        .expect_err("an empty ranking has no winner");
    assert_eq!(empty.code(), "no_winner_selected");
    assert!(
        format!("{empty}").contains("ranking is a step"),
        "the refusal states why one candidate is not an accident: {empty}"
    );

    let winner = ledger
        .choose_winner(&[LaneStanding::new(lane_id, amount(1))])
        .expect("one accepted lane is rankable");
    assert_eq!(winner, lane_id);
    ledger
        .reserve_for(lane_id, signer(), 7, amount(INPUT))
        .expect("and after the decision, reservable");
}

/// §36 ranks over accepted lanes only. A lane still working has no gain to compare, and letting it stand
/// would crown a candidate before risk had said anything.
#[test]
fn only_an_accepted_lane_can_stand() {
    let mut ledger = ledger();
    let lane_id = ledger.open("cand-still-working").expect("fresh");
    ledger.begin_simulation(lane_id).expect("Simulating");
    ledger
        .record_simulation(lane_id, sim_id(33), plan(33))
        .expect("recorded");

    let error = ledger
        .choose_winner(&[LaneStanding::new(lane_id, amount(500))])
        .expect_err("Simulating is not Ready");
    assert_eq!(error.code(), "lane_not_ready");
    assert!(
        format!("{error}").contains("simulating"),
        "the refusal quotes the state it found: {error}"
    );
    assert_eq!(ledger.winner(), None);
    assert_eq!(
        ledger.standings().len(),
        0,
        "a refused ranking stores nothing"
    );
}

/// A lane twice in the ranking would let one candidate be counted as two, and could hand the ledger a
/// winner that exists as two claims rather than one.
#[test]
fn a_lane_appears_in_the_ranking_once() {
    let mut ledger = ledger();
    let doubled = ready_lane(&mut ledger, "cand-twice", plan(34));
    let other = ready_lane(&mut ledger, "cand-other-standing", plan(35));
    let error = ledger
        .choose_winner(&[
            LaneStanding::new(doubled, amount(10)),
            LaneStanding::new(other, amount(20)),
            LaneStanding::new(doubled, amount(30)),
        ])
        .expect_err("one lane, one standing");
    assert_eq!(error.code(), "duplicate_standing");
    assert!(
        format!("{error}").contains(&doubled.to_string()),
        "the refusal names the doubled lane: {error}"
    );
    assert_eq!(ledger.winner(), None);
}

/// A standing naming a lane this ledger never opened is refused rather than skipped — a ranking built
/// from a stale lane list would otherwise win on a record nothing is tracking.
#[test]
fn a_ranking_cannot_name_a_lane_from_another_ledger() {
    let mut ledger = ledger();
    let error = ledger
        .choose_winner(&[LaneStanding::new(LaneId(42), amount(50))])
        .expect_err("lane-42 was never opened here");
    assert_eq!(error.code(), "unknown_lane");
    assert!(
        format!("{error}").contains("lane-42"),
        "the id is quoted: {error}"
    );
    assert_eq!(ledger.winner(), None);
}

/// A second reservation on one lane is refused by the ladder before the domain is asked, so the pool
/// cannot be charged twice for one lane — the arithmetic §35 would otherwise have to catch late.
#[test]
fn a_lane_cannot_reserve_twice() {
    let mut ledger = ledger();
    let lane_id = winner_lane(&mut ledger, "cand-twice-reserved", plan(36), 50);
    ledger
        .reserve_for(lane_id, signer(), 7, amount(INPUT))
        .expect("the first reservation");

    let error = ledger
        .reserve_for(lane_id, signer(), 8, amount(INPUT))
        .expect_err("Reserved does not arrow to Reserved");
    assert_eq!(error.code(), "illegal_transition");
    assert_eq!(
        ledger.capital().reserved_capital,
        amount(INPUT),
        "charged once, not twice"
    );
    assert_eq!(ledger.nonces().outstanding(), 1);
    assert_eq!(
        ledger
            .lane(lane_id)
            .expect("here")
            .nonce_reservation
            .expect("held")
            .nonce,
        7,
        "the first pair stands"
    );
    assert_eq!(ledger.nonces().of(lane_id).expect("held").nonce, 7);
}

/// Both books and the lane record agree at the reservation, because a §43 table that reads only one of
/// them would be a claim about the ledger rather than about the run.
#[test]
fn the_reservation_is_written_to_both_books_and_the_lane() {
    let mut ledger = ledger();
    let lane_id = winner_lane(&mut ledger, "cand-pair", plan(37), 50);
    let pair = ledger
        .reserve_for(lane_id, signer(), 7, amount(INPUT))
        .expect("reserved");

    let lane = ledger.lane(lane_id).expect("here");
    assert_eq!(lane.nonce_reservation, Some(pair.nonce));
    assert_eq!(lane.capital_reservation, Some(pair.capital.clone()));
    assert_eq!(ledger.nonces().of(lane_id), Some(&pair.nonce));
    assert_eq!(ledger.nonces().held_by(signer(), 7), Some(lane_id));
    assert_eq!(
        ledger.capital().available_capital,
        amount(CAPACITY - INPUT),
        "the pool moved by exactly the input"
    );
    assert_eq!(ledger.capital().reserved_capital, amount(INPUT));
    assert!(ledger.capital().invariant_holds());
    assert_eq!(
        (pair.capital.domain_id.as_str(), pair.capital.lane_id),
        ("m11-one-domain", lane_id)
    );

    let last = lane.history.last().expect("the reservation arrow");
    assert_eq!(last.to, LaneState::Reserved);
    assert!(
        last.note.contains("nonce 7") && last.note.contains("signer"),
        "the note carries both resources: {}",
        last.note
    );
}

/// §35's first version does no capital management, and the invariant is what makes that safe to say:
/// after one lane settles and two release, the two halves still add up to the capacity, the settled
/// total equals the capital that actually ran, and only the committed pair is left in the book.
#[test]
fn the_domain_invariant_survives_a_win_a_settle_and_two_releases() {
    let mut ledger = ledger();
    let winner = winner_lane(&mut ledger, "cand-first-settled", plan(38), 90);
    ledger
        .reserve_for(winner, signer(), 7, amount(INPUT))
        .expect("reserved");
    send(&mut ledger, winner);
    ledger.settle(winner).expect("settled");

    let second = ready_lane(&mut ledger, "cand-second-expired", plan(39));
    let _ = ledger
        .choose_winner(&[LaneStanding::new(second, amount(80))])
        .expect("re-rank past a settled winner");
    ledger
        .reserve_for(second, signer(), 8, amount(INPUT))
        .expect("reserved");
    let _ = ledger.end(
        second,
        LaneFailure::PlanExpired("window closed".to_string()),
    );

    let third = ready_lane(&mut ledger, "cand-third-cancelled", plan(40));
    let _ = ledger
        .choose_winner(&[LaneStanding::new(third, amount(70))])
        .expect("re-rank again");
    let _ = ledger.end(
        third,
        LaneFailure::Cancelled("operator withdrew".to_string()),
    );

    let capital = ledger.capital();
    assert!(
        capital.invariant_holds(),
        "available + reserved == capacity"
    );
    assert_eq!(
        capital.reserved_capital,
        U256::ZERO,
        "one settled, two released"
    );
    assert_eq!(capital.available_capital, amount(CAPACITY));
    assert_eq!(
        capital.settled_input(),
        amount(INPUT),
        "exactly one input ran through"
    );
    assert_eq!(
        ledger.nonces().outstanding(),
        1,
        "only the committed pair stays"
    );
    assert_eq!(ledger.nonces().held_by(signer(), 7), Some(winner));
    assert_eq!(
        ledger.nonces().held_by(signer(), 8),
        None,
        "the expired lane's 8 is free"
    );
    assert_eq!(
        ledger
            .nonces()
            .held()
            .map(|record| (record.lane_id, record.nonce, record.stage))
            .collect::<Vec<_>>(),
        vec![(winner, 7, NonceStage::Committed)],
        "the book holds one record, in lane order"
    );
    assert_eq!(
        ledger
            .lanes()
            .map(|lane| lane.state.code())
            .collect::<Vec<_>>(),
        vec!["settled", "expired", "cancelled"],
        "§32's three endings, one per lane, in lane order"
    );
}

/// Every accessor that takes a lane id answers with the lane it could not find rather than an empty
/// option a caller would have to invent a failure for — §49's zero production panics is a property of
/// these paths. `reserve_for` is the exception, and the exception is the honest order: §36's winner
/// question is asked before the ledger looks the lane up at all.
#[test]
fn an_unseen_lane_is_refused_by_name_and_never_panics() {
    let mut ledger = ledger();
    let missing = LaneId(777);
    let paths = [
        ("lane", ledger.lane(missing).expect_err("no lane")),
        (
            "begin_simulation",
            ledger
                .begin_simulation(missing)
                .expect_err("nothing to simulate"),
        ),
        (
            "record_simulation",
            ledger
                .record_simulation(missing, sim_id(41), plan(41))
                .expect_err("nothing to record"),
        ),
        (
            "advance",
            ledger
                .advance(missing, LaneState::RiskChecking, "nope")
                .expect_err("nothing to advance"),
        ),
        (
            "settle",
            ledger.settle(missing).expect_err("nothing to settle"),
        ),
        (
            "end",
            ledger
                .end(missing, LaneFailure::Cancelled("gone".to_string()))
                .expect_err("nothing to end"),
        ),
    ];
    for (name, error) in paths {
        assert_eq!(error.code(), "unknown_lane", "{name} answered otherwise");
        assert!(
            format!("{error}").contains("lane-777"),
            "{name} quoted the id: {error}"
        );
    }

    let error = ledger
        .reserve_for(missing, signer(), 7, amount(INPUT))
        .expect_err("§36 is asked first");
    assert_eq!(error.code(), "no_winner_selected");
    assert_eq!(ledger.lanes().count(), 0, "and the ledger stayed empty");
    assert_eq!(ledger.nonces().outstanding(), 0);
    assert_eq!(ledger.capital().available_capital, amount(CAPACITY));
}

// ---------------------------------------------------------------------------
// §43/§46 — the refusal and failure vocabularies
// ---------------------------------------------------------------------------

/// §43's rows group by a machine label, so every arm of [`LaneRefusal`] needs one and two arms must not
/// share it. The list is the label set §45's matrix produces, spelled out — and every refusal opens with
/// the section it enforces, because a caller counting refusals needs a rule, not a sentence.
#[test]
fn every_refusal_carries_a_unique_label_and_a_section() {
    let refusals = vec![
        LaneRefusal::DuplicateLane {
            candidate_id: "c".to_string(),
            existing: LaneId(0),
        },
        LaneRefusal::DuplicatePlan {
            plan_hash: plan(1),
            existing: LaneId(0),
        },
        LaneRefusal::NonceHeld {
            signer: signer(),
            nonce: 7,
            holder: LaneId(0),
            stage: "committed",
            lane: LaneId(1),
        },
        LaneRefusal::InsufficientCapital {
            lane: LaneId(1),
            requested: "1001".to_string(),
            free: "1000".to_string(),
            reserved: "0".to_string(),
            capacity: "1000".to_string(),
            domain: "m11-one-domain".to_string(),
        },
        LaneRefusal::LaneNotSelected {
            lane: LaneId(1),
            winner: LaneId(0),
        },
        LaneRefusal::WinnerReserved {
            winner: LaneId(0),
            state: LaneState::Submitted,
        },
        LaneRefusal::NoWinnerSelected,
        LaneRefusal::LaneNotReady {
            lane: LaneId(0),
            state: LaneState::Simulating,
        },
        LaneRefusal::DuplicateStanding { lane: LaneId(0) },
        LaneRefusal::AlreadyRecorded {
            lane: LaneId(0),
            previous_plan: plan(2),
        },
        LaneRefusal::NotRecordable {
            lane: LaneId(0),
            state: LaneState::Created,
        },
        LaneRefusal::IllegalTransition {
            lane: LaneId(0),
            from: "ready".to_string(),
            to: "reserved".to_string(),
            note: "owned by reserve_for".to_string(),
        },
        LaneRefusal::UnrecordedLane {
            lane: LaneId(0),
            missing: "plan hash",
        },
        LaneRefusal::UnknownLane { lane: LaneId(9) },
    ];
    let mut labels: Vec<&str> = refusals.iter().map(LaneRefusal::code).collect();
    labels.sort_unstable();
    let counted = labels.len();
    labels.dedup();
    assert_eq!(labels.len(), counted, "one label per arm, {labels:?}");
    assert_eq!(
        labels,
        vec![
            "already_recorded",
            "duplicate_lane",
            "duplicate_plan",
            "duplicate_standing",
            "illegal_transition",
            "insufficient_capital",
            "lane_not_ready",
            "lane_not_selected",
            "no_winner_selected",
            "nonce_held",
            "not_recordable",
            "unknown_lane",
            "unrecorded_lane",
            "winner_reserved",
        ],
        "the §43 grouping labels, sorted"
    );
    for refusal in &refusals {
        let text = format!("{refusal}");
        assert!(
            text.starts_with('§') && !text.ends_with(' '),
            "every refusal cites its section and is quotable: {}",
            refusal.code()
        );
    }
}

/// §32 names four failure states and §34 splits the ends into two dispositions, so the reason a lane
/// ended is the caller's only input: an arm that means `Rejected` cannot be filed as `Cancelled`, and
/// the one arm that commits is the revert a receipt told us about. §25's unanswered submission has no
/// arm at all — the lane stays `Submitted`, which is why this table has seven rows and not eight.
#[test]
fn every_failure_names_its_state_and_its_disposition() {
    let cases = [
        (
            LaneFailure::SimulationFailed("x".to_string()),
            LaneState::Rejected,
            ReservationDisposition::Release,
            "simulation_failed",
        ),
        (
            LaneFailure::RiskRejected("x".to_string()),
            LaneState::Rejected,
            ReservationDisposition::Release,
            "risk_rejected",
        ),
        (
            LaneFailure::PlanRefused("x".to_string()),
            LaneState::Rejected,
            ReservationDisposition::Release,
            "plan_refused",
        ),
        (
            LaneFailure::PlanExpired("x".to_string()),
            LaneState::Expired,
            ReservationDisposition::Release,
            "plan_expired",
        ),
        (
            LaneFailure::Cancelled("x".to_string()),
            LaneState::Cancelled,
            ReservationDisposition::Release,
            "cancelled",
        ),
        (
            LaneFailure::SubmissionProvenRejected("x".to_string()),
            LaneState::Failed,
            ReservationDisposition::Release,
            "submission_proven_rejected",
        ),
        (
            LaneFailure::RevertedInBlock {
                transaction_hash: "0x03".to_string(),
                block_number: 37_486_794,
            },
            LaneState::Failed,
            ReservationDisposition::Commit,
            "reverted_in_block",
        ),
    ];
    let mut labels: Vec<&str> = cases.iter().map(|(_, _, _, code)| *code).collect();
    labels.sort_unstable();
    let counted = labels.len();
    labels.dedup();
    assert_eq!(labels.len(), counted, "one label per reason: {labels:?}");
    assert_eq!(
        labels,
        vec![
            "cancelled",
            "plan_expired",
            "plan_refused",
            "reverted_in_block",
            "risk_rejected",
            "simulation_failed",
            "submission_proven_rejected",
        ],
        "§32's reasons in full, sorted — seven, because §25's unanswered submission has no arm"
    );
    assert_eq!(
        cases
            .iter()
            .filter(|(_, _, disposition, _)| *disposition == ReservationDisposition::Commit)
            .count(),
        1,
        "§34: exactly one reason commits, and it is the one a receipt told us"
    );

    for (failure, state, disposition, code) in cases {
        assert_eq!(failure.lane_state(), state, "{code}");
        assert_eq!(failure.code(), code);
        assert_eq!(failure.disposition(), disposition, "{code}");
        assert!(
            state.is_terminal(),
            "{code} ends in `{}`, which §32 must treat as an end",
            state.code()
        );
    }
}
