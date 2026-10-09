//! §26–§30: what the risk layer answers about a multi-hop simulation, and what a simulation has
//! to prove before it may become a plan.
//!
//! §26 fixes the shape of the question — a `SimulatedOpportunity` in, an `Accept`/`Reject` out,
//! and a `Reject` that carries its reason. §27 lists nine checks it must at least ask. §28 forbids
//! this layer from reaching a signer, a send, or a submitter. §29 says the plan that comes out the
//! other side is M10's own `ArbitrageExecutionPlan` and not a new type. §30 says the plan has to
//! *prove* it is the same execution as the simulation — two calldata hashes equal, three route
//! identities one — and refuse otherwise.
//!
//! ```text
//! SimulatedOpportunity
//!   -> MultihopRiskPolicy::evaluate   eleven named checks, first answer wins
//!   -> MultihopRiskDecision            Accept(figures) | Reject(check|detail) | Unknown(check|detail)
//!   -> plan_from_simulation          §29's eleven fields + §30's two equalities
//!   -> ExecutablePlan                M10's own validate(), unchanged
//! ```
//!
//! ## How a check is proven to exist
//!
//! A checklist is worth nothing unless every line of it can be made to answer. Each of the eleven
//! [`RiskCheck`]s is driven here by a record built to fail *that* check, and one more test
//! collects the labels those records produced and requires all eleven to appear. The negative
//! controls are the same shape from the other side: a floor one lower accepts, a ceiling one higher
//! accepts, an absent fact answers `Unknown` rather than quietly passing.
//!
//! ## What is being edited, and why that is the point
//!
//! Several cases below take a well-formed record and change one public field afterwards. That is
//! not a shortcut — every field of a `SimulatedOpportunity` is public, so a record that has been
//! changed after the run is a record a later layer can actually receive, and §30's execution claim
//! rests on noticing exactly that. Each such test says which check is supposed to see the edit and
//! asserts the edit is visible nowhere else in the answer.
//!
//! Nothing in this file signs, broadcasts, or holds a key, and the acceptance an `Accept` states is
//! a judgement about a simulation — §28's boundary is asserted structurally, by the dependency
//! list of `crates/risk`, in `risk_reaches_no_network_and_no_key`.

mod multihop_market;

use std::collections::BTreeSet;

use alloy_primitives::{address, Address, U256};

use evm_core::{BlockNumber, ChainId};
use evm_execution::{
    executable_plan, plan_from_simulation, ExecutionBinding, MarketKind, MultihopBinding,
    MultihopPlanContext, MultihopPlanRefusal, ProfitDenomination, SenderFunding, SimulationOutcome,
};
use evm_opportunity::Gross;
use evm_protocol::ExecutorCall;
use evm_risk::{
    MarketFacts, MultihopAcceptance, MultihopRiskDecision, MultihopRiskPolicy, RiskCheck, RiskRule,
};
use evm_simulation::result::StepStatus;
use evm_simulation::SimulatedOpportunity;

use multihop_market::{
    at_amount, candidate, delivered, four_hop_candidate, outcome_of, pool, pool_revert, request,
    triangle_route, u, with_quote, A, BLOCK, CHAIN, EXECUTOR, OPERATOR, P3,
};

/// The burn `multihop_market::delivered` reports for a hand-written success, restated here so a
/// gas ceiling can be written as "this number, plus or minus one" rather than as a magic constant
/// that has to be re-read from the helper every time it is used.
const GAS_USED: u64 = 180_000;

/// An account no fixture runs as, for the §29 sender check.
const OUTSIDER: Address = address!("0x3000000000000000000000000000000000000030");

/// §27's list, in the task's own words, so the test that says "all nine are asked" is comparing
/// against the requirement rather than against a paraphrase written next to the assertion.
const SECTION_27: [&str; 9] = [
    "simulation success",
    "input > 0",
    "output > input",
    "min profit",
    "max gas",
    "simulation freshness",
    "state freshness",
    "route validity",
    "executor validity",
];

// ---------------------------------------------------------------------------
// The record under judgement
// ---------------------------------------------------------------------------

/// A delivered three-leg run of the synthetic triangle, filed the way §23 files it.
fn filed() -> SimulatedOpportunity {
    let priced = candidate();
    let run = request(&priced);
    SimulatedOpportunity::new(&priced, &run, delivered(&priced)).expect("the fixture record files")
}

/// The same candidate, answered with `status`. The input, guard and recipient still come out of
/// the request, so only the ending is being varied.
fn with_status(status: StepStatus, paid: Option<U256>) -> SimulatedOpportunity {
    let priced = candidate();
    let run = request(&priced);
    let outcome = outcome_of(&run, status, paid, GAS_USED);
    SimulatedOpportunity::new(&priced, &run, outcome).expect("a non-delivery files too")
}

/// A reverted run, at the pool's own `Error(string)` rather than one of the executor's errors.
fn reverted() -> SimulatedOpportunity {
    with_status(StepStatus::Reverted(pool_revert("K")), None)
}

/// The round trip's gain, read off the record rather than restated by a test.
fn gain(simulated: &SimulatedOpportunity) -> U256 {
    simulated
        .gross
        .and_then(|gross| gross.gain())
        .expect("the fixture record delivers more than it spends")
}

// ---------------------------------------------------------------------------
// The policy, the facts and the plan context
// ---------------------------------------------------------------------------

/// Thresholds the fixture record passes with room to spare, so every rejection test moves exactly
/// one thing and can name which one it moved.
fn policy() -> MultihopRiskPolicy {
    MultihopRiskPolicy {
        minimum_gross_profit: u(0),
        maximum_gas: GAS_USED + 120_000,
        maximum_simulation_age: 5,
        maximum_state_age: 5,
        executor: EXECUTOR,
        chain_id: CHAIN,
        provenance: "M11's fixture thresholds: a floor of 0 and a ceiling well above the measured \
                     burn, so every rejection below is caused by the one field the test moved"
            .to_string(),
    }
}

/// The two block facts a caller supplies, with a provenance sentence attached.
fn facts(head: Option<u64>, state_version: Option<u64>) -> MarketFacts {
    MarketFacts {
        head: head.map(BlockNumber),
        state_version: state_version.map(BlockNumber),
        provenance: "hand-written by this fixture: no node was asked (§28)".to_string(),
    }
}

/// Both facts present and equal to the record's own pinned block: age zero, so no freshness check
/// fires unless a test moves one of them.
fn fresh() -> MarketFacts {
    facts(Some(BLOCK), Some(BLOCK))
}

/// The acceptance the policy grants over this record, or a panic that quotes the answer it gave
/// instead — which is how a fixture that stopped passing would show up.
fn acceptance(simulated: &SimulatedOpportunity) -> MultihopAcceptance {
    match policy().evaluate(simulated, &fresh()) {
        MultihopRiskDecision::Accept(figures) => figures,
        other => panic!("the fixture record was not accepted: {other}"),
    }
}

/// The deployment the plan is bound to: the same chain and executor the run was made against.
fn binding() -> ExecutionBinding {
    ExecutionBinding {
        chain_id: CHAIN.0,
        executor: EXECUTOR,
    }
}

/// The four facts a simulation structurally cannot know, at the values a controlled M11 ladder
/// would use. §29's list does not include them, and the module doc in the execution crate says why.
fn context() -> MultihopPlanContext {
    MultihopPlanContext {
        sender: OPERATOR,
        correlation_id: "m11-risk-plan".to_string(),
        max_block_age: 3,
        validity_provenance: "§8's window as this fixture declares it".to_string(),
        funding: SenderFunding::RealState {
            source:
                "§34: a hand-written request has no state behind it, so this field records the \
                     fixture's own claim, not a reading"
                    .to_string(),
        },
        market: MarketKind::ControlledFixture {
            proves: "the risk-to-plan translation works on a synthetic route".to_string(),
        },
        state_fingerprint: format!("{BLOCK}:0"),
        floor_provenance: "§12's floor, mirrored from the guard the call carries".to_string(),
    }
}

/// The answer's check as a label, panicking if the answer was an `Accept`.
fn label(decision: &MultihopRiskDecision) -> &'static str {
    decision
        .check()
        .map(RiskCheck::label)
        .unwrap_or_else(|| panic!("expected a named check, got: {decision}"))
}

// ---------------------------------------------------------------------------
// §27 — the checklist is a checklist, not a wish
// ---------------------------------------------------------------------------

/// All nine of §27's lines are asked, by name, and the two additions are labelled as additions.
/// `SECTION_27` is quoted from the task; the mapping from its prose to a [`RiskCheck`] is this
/// test's, and it is written out pair by pair so a rename has to be a decision rather than a drift.
#[test]
fn every_line_of_twenty_sevens_list_is_a_named_check() {
    let asked: BTreeSet<&str> = RiskCheck::ALL.iter().map(|check| check.label()).collect();
    let from_the_task: [(&str, RiskCheck); 9] = [
        (SECTION_27[0], RiskCheck::SimulationSuccess),
        (SECTION_27[1], RiskCheck::InputPositive),
        (SECTION_27[2], RiskCheck::OutputAboveInput),
        (SECTION_27[3], RiskCheck::MinimumGrossProfit),
        (SECTION_27[4], RiskCheck::MaximumGas),
        (SECTION_27[5], RiskCheck::SimulationFreshness),
        (SECTION_27[6], RiskCheck::StateFreshness),
        (SECTION_27[7], RiskCheck::RouteValidity),
        (SECTION_27[8], RiskCheck::ExecutorValidity),
    ];
    for (words, check) in from_the_task {
        assert!(
            asked.contains(check.label()),
            "§27's \"{words}\" maps to `{}` and no such check is asked",
            check.label(),
        );
    }
    assert_eq!(
        asked.len(),
        RiskCheck::ALL.len(),
        "two checks share a label, so an evidence row could not count them apart",
    );
    assert_eq!(RiskCheck::ALL.len(), 11);
    // The two this module adds, spelled out because they are not in §27's list.
    assert!(asked.contains(RiskCheck::ChainValidity.label()));
    assert!(asked.contains(RiskCheck::FinalGuard.label()));
}

/// The labels are the vocabulary M4 already established where the two layers ask the same
/// question, and M4's own answer type is still reachable beside M11's.
#[test]
fn m11s_labels_agree_with_m4s_where_the_questions_are_the_same() {
    assert_eq!(
        RiskCheck::SimulationSuccess.label(),
        RiskRule::SimulationSuccess.label(),
    );
    assert_eq!(RiskCheck::MaximumGas.label(), RiskRule::MaximumGas.label());
    // §26 says the output is a RiskDecision. M4 owns that name and M7/M8's evidence rows quote it,
    // so M11's answer is a second type — and the two cannot be confused for one another.
    let m11: MultihopRiskDecision = policy().evaluate(&filed(), &fresh());
    assert!(m11.accepted());
    assert_eq!(m11.name(), "accept");
}

// ---------------------------------------------------------------------------
// §26/§27 — the Accept, and the figures it is a claim about
// ---------------------------------------------------------------------------

/// An `Accept` carries the numbers it passed, not a summary of them, and every one of them is the
/// record's own (§24 keeps the bill beside the profit, so the gross is a gross).
#[test]
fn an_accept_echoes_the_records_own_figures() {
    let simulated = filed();
    let figures = acceptance(&simulated);
    assert_eq!(figures.input_amount, simulated.input_amount);
    assert_eq!(
        figures.delivered,
        simulated.status.delivered().expect("the fixture delivers"),
    );
    assert_eq!(figures.gross_profit, gain(&simulated));
    assert_eq!(figures.gas_used, simulated.gas_used);
    assert_eq!(figures.min_final_output, simulated.min_final_output);
    assert_eq!(figures.simulation_block, simulated.simulation_block.number);
    assert_eq!(figures.minimum_gross_profit, policy().minimum_gross_profit);
    assert_eq!(figures.maximum_gas, policy().maximum_gas);
    // The two facts came from the caller, and the Accept says which ones it used.
    assert_eq!(figures.head, Some(BlockNumber(BLOCK)));
    assert_eq!(figures.state_version, Some(BlockNumber(BLOCK)));
}

/// §24's rule that an unpriced bill is never spelled as zero: the synthetic request carries
/// `GasPricing::Unresolved`, so the Accept says "unpriced" and holds `None`.
#[test]
fn an_accept_never_invents_a_gas_price() {
    let simulated = filed();
    let figures = acceptance(&simulated);
    assert_eq!(
        figures.gas_charge_wei, None,
        "the run measured gas and no price was attached to it",
    );
    let detail = policy().evaluate(&simulated, &fresh()).detail().to_string();
    assert!(
        detail.contains("unpriced"),
        "the sentence should say the bill has no price: {detail}",
    );
    assert!(
        !detail.contains(" None "),
        "an absent number must be spelled, not printed as a Rust option: {detail}",
    );
}

/// The no-broadcast sentence survives the extra checks: §26's answer is a judgement about a
/// simulation, and deciding to send is a different layer's question.
#[test]
fn an_accept_says_it_is_not_a_broadcast() {
    let decision = policy().evaluate(&filed(), &fresh());
    let detail = decision.detail();
    assert!(detail.starts_with("no broadcast"), "{detail}");
    assert!(detail.contains("execution layer"), "{detail}");
}

// ---------------------------------------------------------------------------
// §27 — one test per line, each driven by a record built to fail that line
// ---------------------------------------------------------------------------

/// Check 1: §45's `simulation_success`, asked of all four non-deliveries §25 can report. Each
/// gets its own word in the detail, because "reverted" and "out of gas" are two facts.
#[test]
fn a_run_that_delivered_nothing_is_rejected_before_anything_else() {
    let cases: [(&str, SimulatedOpportunity); 4] = [
        ("reverted", reverted()),
        ("out_of_gas", with_status(StepStatus::OutOfGas, None)),
        (
            "halted",
            with_status(
                StepStatus::Halted("the fixture halted it".to_string()),
                None,
            ),
        ),
        ("no_return", with_status(StepStatus::Success, None)),
    ];
    let mut spellings = BTreeSet::new();
    for (word, simulated) in &cases {
        let decision = policy().evaluate(simulated, &fresh());
        assert_eq!(decision.name(), "reject", "{word} should be a rejection");
        assert_eq!(label(&decision), "simulation_success", "{word}");
        assert!(
            decision.detail().contains(word),
            "the detail should name the ending `{word}`: {}",
            decision.detail(),
        );
        spellings.insert(decision.detail().to_string());
    }
    assert_eq!(
        spellings.len(),
        cases.len(),
        "four different endings produced fewer than four different sentences",
    );
}

/// Check 2 (§27's `input > 0`). A zero-input run has a gross, and the gross is a statement about
/// nothing, so this is refused on its own line rather than on the profit line.
#[test]
fn a_run_that_moved_nothing_is_rejected_on_input() {
    let mut simulated = filed();
    simulated.input_amount = U256::ZERO;
    let decision = policy().evaluate(&simulated, &fresh());
    assert_eq!(label(&decision), "input_positive");
    assert!(decision.detail().contains("0 of the input token"));
}

/// Check 3 (§27's `output > input`): an even round trip is a loss of the route's own cost, not a
/// small profit, and it is answered on this line rather than on the floor below it.
#[test]
fn an_even_round_trip_is_rejected_as_no_gain() {
    let spent = {
        let priced = candidate();
        priced.search.best_input
    };
    let simulated = with_status(StepStatus::Success, Some(spent));
    assert_eq!(
        simulated.gross,
        Some(Gross::Even),
        "the fixture's delivery equals its input, so this is the even case",
    );
    let decision = policy().evaluate(&simulated, &fresh());
    assert_eq!(label(&decision), "output_above_input");
    assert!(decision.detail().contains("gross is even"));
}

/// A strictly worse-than-even trip is refused on the same line, for the same reason: §27 asks
/// `output > input`, and a loss is the same answer as a break.
#[test]
fn a_losing_round_trip_is_rejected_on_the_same_line() {
    let spent = {
        let priced = candidate();
        priced.search.best_input
    };
    let decision = policy().evaluate(
        &with_status(StepStatus::Success, Some(spent - u(1))),
        &fresh(),
    );
    assert_eq!(label(&decision), "output_above_input");
}

/// Check 4 (§27's `min profit`), asked strictly: a gain exactly at the floor is not above it. The
/// pair of cases is the point — one below the floor accepts, so the rejection is about the
/// comparison and not about the record.
#[test]
fn the_profit_floor_is_compared_strictly() {
    let simulated = filed();
    let at = gain(&simulated);
    let on_the_floor = MultihopRiskPolicy {
        minimum_gross_profit: at,
        ..policy()
    };
    let decision = on_the_floor.evaluate(&simulated, &fresh());
    assert_eq!(decision.name(), "reject");
    assert_eq!(label(&decision), "minimum_gross_profit");
    assert!(decision.detail().contains(&at.to_string()));

    let just_under = MultihopRiskPolicy {
        minimum_gross_profit: at - u(1),
        ..policy()
    };
    assert!(
        just_under.evaluate(&simulated, &fresh()).accepted(),
        "the same record one unit under its own gain is a profit the policy allows",
    );
}

/// Check 5 (§27's `max gas`), inclusive at the ceiling for the same reason M4's is: the ceiling is
/// "no more than this", and the boundary case is the last one a caller means to allow.
#[test]
fn the_gas_ceiling_rejects_above_and_allows_at() {
    let simulated = filed();
    let over = MultihopRiskPolicy {
        maximum_gas: GAS_USED - 1,
        ..policy()
    };
    let decision = over.evaluate(&simulated, &fresh());
    assert_eq!(label(&decision), "maximum_gas");
    assert!(decision.detail().contains(&GAS_USED.to_string()));

    let exact = MultihopRiskPolicy {
        maximum_gas: GAS_USED,
        ..policy()
    };
    assert!(exact.evaluate(&simulated, &fresh()).accepted());
}

/// Check 6, added by this module: the delivery against the guard the same call carries. The
/// contract reverts short of its own guard, so a record that delivers under it was not produced by
/// that call — it is an edited artifact, and §30's execution claim cannot sit on one.
#[test]
fn a_delivery_under_its_own_guard_is_refused_as_an_edited_record() {
    let mut simulated = filed();
    let delivered = simulated.status.delivered().expect("the fixture delivers");
    simulated.min_final_output = delivered + u(1);
    let decision = policy().evaluate(&simulated, &fresh());
    assert_eq!(label(&decision), "final_guard");
    assert!(decision
        .detail()
        .contains("the record and the call disagree"));
}

/// Check 7 (§27's `route validity`), re-asked here: the candidate is swapped for one whose quote
/// prices a different middle pool, while the call keeps the pools that ran.
#[test]
fn legs_that_are_not_the_priced_routes_are_refused() {
    let simulated = filed();
    let mut edited = simulated.clone();
    edited.candidate = with_quote(&simulated.candidate, |quote| {
        quote.hops[1].pool = pool(P3);
    });
    let decision = policy().evaluate(&edited, &fresh());
    assert_eq!(label(&decision), "route_validity");
    // The refusal quotes the adapter's own error, so the two layers cannot disagree about which
    // leg is the one that moved.
    assert!(decision.detail().contains("leg 1"), "{}", decision.detail(),);
}

/// Check 8: a route on another chain is refused on its own line, before the executor address is
/// asked — on the wrong network the address would name a different contract entirely.
#[test]
fn a_run_on_the_wrong_chain_is_refused_before_the_address_is_asked() {
    let other = MultihopRiskPolicy {
        chain_id: ChainId(8),
        ..policy()
    };
    let decision = other.evaluate(&filed(), &fresh());
    assert_eq!(label(&decision), "chain_validity");
    assert!(decision.detail().contains("chain 8"));
}

/// Check 9 (§27's `executor validity`): the allowlist is a field of the policy, and the only
/// address it is compared against is the one the run was made against.
#[test]
fn an_executor_this_policy_does_not_allow_is_refused() {
    let suspicious = MultihopRiskPolicy {
        executor: OUTSIDER,
        ..policy()
    };
    let decision = suspicious.evaluate(&filed(), &fresh());
    assert_eq!(label(&decision), "executor_validity");
    let detail = decision.detail();
    assert!(
        detail.contains(&format!("{EXECUTOR:#x}")) && detail.contains(&format!("{OUTSIDER:#x}")),
        "both addresses should be quotable from the refusal: {detail}",
    );
}

/// Check 10 (§27's `simulation freshness`): §27 says an over-old `simulation_block` is a Reject,
/// and the bound is the caller's. The two sides of the boundary are both here.
#[test]
fn a_run_behind_the_head_by_more_than_the_bound_is_rejected() {
    let stale = MultihopRiskPolicy {
        maximum_simulation_age: 2,
        ..policy()
    };
    let decision = stale.evaluate(&filed(), &facts(Some(BLOCK + 3), Some(BLOCK)));
    assert_eq!(label(&decision), "simulation_freshness");
    assert!(
        decision.detail().contains("3 blocks"),
        "{}",
        decision.detail(),
    );

    assert!(
        stale
            .evaluate(&filed(), &facts(Some(BLOCK + 2), Some(BLOCK)))
            .accepted(),
        "the bound is inclusive, so three would be a rejection and two is not",
    );
}

/// A run pinned *ahead* of the caller's head is not a fresh run — it describes a block that has
/// not happened. Neither subtraction's sign is left to a fallback.
#[test]
fn a_run_ahead_of_the_head_is_rejected_not_wrapped_around() {
    let decision = policy().evaluate(&filed(), &facts(Some(BLOCK - 4), Some(BLOCK)));
    assert_eq!(label(&decision), "simulation_freshness");
    assert!(
        decision.detail().contains("has not happened"),
        "{}",
        decision.detail(),
    );
}

/// Check 11 (§27's `state freshness`) is a separate line from check 10 even when both bounds are
/// the same number, because a pipeline whose ingestion has fallen behind has a head that looks
/// fine and a state that does not.
#[test]
fn state_that_has_moved_past_the_run_is_its_own_rejection() {
    let head_is_fine = facts(Some(BLOCK + 1), Some(BLOCK + 10));
    let decision = MultihopRiskPolicy {
        maximum_simulation_age: 5,
        maximum_state_age: 3,
        ..policy()
    }
    .evaluate(&filed(), &head_is_fine);
    assert_eq!(label(&decision), "state_freshness");
    assert!(
        decision.detail().contains("10 blocks"),
        "{}",
        decision.detail()
    );
}

// ---------------------------------------------------------------------------
// §41's philosophy at this layer: an absent fact is Unknown, never a pass, never a fail
// ---------------------------------------------------------------------------

/// No head, so no age. The answer is `Unknown` — a Reject would claim the run is old, an Accept
/// would claim it is not, and the layer does not know either.
#[test]
fn an_absent_head_is_unknown_rather_than_a_stale_rejection() {
    let decision = policy().evaluate(&filed(), &facts(None, Some(BLOCK)));
    assert_eq!(decision.name(), "unknown");
    assert_eq!(label(&decision), "simulation_freshness");
    assert!(!decision.accepted(), "Unknown is not a pass");
    assert!(
        decision.detail().contains("no head block"),
        "{}",
        decision.detail(),
    );
}

/// A record with a delivery and no gross figure is an incomplete artifact, and this layer says so
/// on the profit line rather than reading the absence as a loss.
#[test]
fn a_delivery_with_no_gross_figure_is_unknown() {
    let mut simulated = filed();
    simulated.gross = None;
    let decision = policy().evaluate(&simulated, &fresh());
    assert_eq!(decision.name(), "unknown");
    assert_eq!(label(&decision), "output_above_input");
    assert!(decision.detail().contains("carries no gross figure"));
}

/// `Unknown` and `Reject` are two answers even when they name the same check: the distinction is
/// "the fact was absent" versus "the fact said no", and an evidence table has to keep them apart.
#[test]
fn unknown_and_reject_stay_two_answers_about_the_same_check() {
    let no_state = policy().evaluate(&filed(), &facts(Some(BLOCK), None));
    let moved_state = policy().evaluate(&filed(), &facts(Some(BLOCK), Some(BLOCK + 99)));
    assert_eq!(no_state.check(), moved_state.check());
    assert_eq!(
        no_state.check().map(RiskCheck::label),
        Some("state_freshness")
    );
    assert_eq!(no_state.name(), "unknown");
    assert_eq!(moved_state.name(), "reject");
    assert_eq!(
        no_state.reason().map(|reason| reason.describe()),
        Some(format!("state_freshness|{}", no_state.detail())),
    );
}

/// An absent `state_version` is the third line §41's rule covers here.
#[test]
fn an_absent_state_version_is_unknown() {
    let decision = policy().evaluate(&filed(), &facts(Some(BLOCK), None));
    assert_eq!(decision.name(), "unknown");
    assert_eq!(label(&decision), "state_freshness");
}

// ---------------------------------------------------------------------------
// The walk: one pass, first answer wins, and the whole checklist is reachable
// ---------------------------------------------------------------------------

/// §27's list is walked in one fixed order: the earliest thing wrong is what gets reported, so a
/// run that never happened is never reported as a run that lost money.
#[test]
fn the_walk_reports_the_earliest_thing_wrong() {
    let broken = {
        let mut simulated = reverted();
        simulated.input_amount = U256::ZERO;
        simulated
    };
    let against_everything = MultihopRiskPolicy {
        chain_id: ChainId(8),
        executor: OUTSIDER,
        minimum_gross_profit: u(1_000_000),
        maximum_gas: 1,
        ..policy()
    };
    let decision =
        against_everything.evaluate(&broken, &facts(Some(BLOCK + 900), Some(BLOCK + 900)));
    assert_eq!(
        label(&decision),
        "simulation_success",
        "the record is broken in six ways and the only one a caller can act on first is that it \
         did not run",
    );

    // The same facts and the same over-everything policy, with the one defect that check 1 is
    // about removed: the walk moves down to the next line it can answer. That is what makes the
    // ordering a measured fact rather than a claim about whichever `if` comes first.
    let chain_only = MultihopRiskPolicy {
        chain_id: ChainId(8),
        ..policy()
    };
    let clean_but_wrong_chain =
        chain_only.evaluate(&filed(), &facts(Some(BLOCK + 900), Some(BLOCK + 900)));
    assert_eq!(
        label(&clean_but_wrong_chain),
        "chain_validity",
        "a delivered record with one wrong field is judged on that field",
    );
}

/// Every one of the eleven lines can be made to answer, by a record built for it, and the labels
/// collected from those answers are exactly the eleven. A checklist with an unreachable line is a
/// checklist that was not implemented.
#[test]
fn all_eleven_lines_reach_an_answer() {
    let mut reached: BTreeSet<&str> = BTreeSet::new();

    let mut zero_input = filed();
    zero_input.input_amount = U256::ZERO;
    let mut no_gross = filed();
    no_gross.gross = None;
    let mut edited_guard = filed();
    let paid = edited_guard.status.delivered().expect("delivers");
    edited_guard.min_final_output = paid + u(1);
    let mut edited_route = filed();
    edited_route.candidate = with_quote(&edited_route.candidate, |quote| {
        quote.hops[1].pool = pool(P3);
    });

    let cases: Vec<MultihopRiskDecision> = vec![
        policy().evaluate(&reverted(), &fresh()),
        policy().evaluate(&zero_input, &fresh()),
        policy().evaluate(&no_gross, &fresh()),
        MultihopRiskPolicy {
            minimum_gross_profit: U256::MAX,
            ..policy()
        }
        .evaluate(&filed(), &fresh()),
        MultihopRiskPolicy {
            maximum_gas: 0,
            ..policy()
        }
        .evaluate(&filed(), &fresh()),
        policy().evaluate(&edited_guard, &fresh()),
        policy().evaluate(&edited_route, &fresh()),
        MultihopRiskPolicy {
            chain_id: ChainId(8),
            ..policy()
        }
        .evaluate(&filed(), &fresh()),
        MultihopRiskPolicy {
            executor: OUTSIDER,
            ..policy()
        }
        .evaluate(&filed(), &fresh()),
        policy().evaluate(&filed(), &facts(Some(BLOCK + 90), Some(BLOCK))),
        policy().evaluate(&filed(), &facts(Some(BLOCK), Some(BLOCK + 90))),
    ];
    for decision in &cases {
        reached.insert(label(decision));
    }

    let expected: BTreeSet<&str> = RiskCheck::ALL.iter().map(|check| check.label()).collect();
    assert_eq!(
        reached,
        expected,
        "{} of {} lines were reached",
        reached.len(),
        expected.len(),
    );
    assert_eq!(reached.len(), 11);
}

/// A total function over borrowed data answers the same way twice, and spells itself the same way
/// twice — the second half is what §43's evidence rows compare.
#[test]
fn one_record_one_policy_one_answer_every_time() {
    let simulated = filed();
    let first = policy().evaluate(&simulated, &fresh());
    let second = policy().evaluate(&simulated, &fresh());
    assert_eq!(first, second);
    assert_eq!(
        serde_json::to_string(&first).expect("a decision is serializable"),
        serde_json::to_string(&second).expect("a decision is serializable"),
    );
}

/// §26's three answers stay three answers in the JSON a report reads: a tag names the state, and
/// the detail rides beside it rather than being parsed out of a sentence.
#[test]
fn the_three_states_are_distinguishable_in_json() {
    let accept = serde_json::to_string(&policy().evaluate(&filed(), &fresh())).unwrap();
    let reject = serde_json::to_string(&policy().evaluate(&reverted(), &fresh())).unwrap();
    let unknown = serde_json::to_string(&policy().evaluate(&filed(), &facts(None, None))).unwrap();
    assert!(
        accept.starts_with(r#"{"state":"accept","detail":"#),
        "{accept}"
    );
    assert!(
        reject.contains(r#""check":"simulation_success""#),
        "{reject}"
    );
    assert!(
        unknown.contains(r#""check":"simulation_freshness""#),
        "{unknown}"
    );
    assert!(reject.contains(r#""state":"reject""#), "{reject}");
    assert!(unknown.contains(r#""state":"unknown""#), "{unknown}");
}

// ---------------------------------------------------------------------------
// §28 — this layer cannot reach a key, a network, or a submitter
// ---------------------------------------------------------------------------

/// §28's three prohibitions are a property of the dependency list: `crates/risk` builds against
/// no transport, no signer, and no execution crate, so there is nothing in scope to call. The scan
/// reads only `[dependencies]` — the crate's own tests do use a runtime — and the same scan run
/// over `crates/execution/Cargo.toml` finds the very packages it must not, which is what makes the
/// empty result a measurement rather than a typo in the list.
#[test]
fn risk_reaches_no_network_and_no_key() {
    let forbidden = [
        "reqwest",
        "tokio",
        "tungstenite",
        "alloy-transport",
        "alloy-provider",
        "alloy-network",
        "alloy-signer",
        "k256",
        "revm",
        "evm-execution",
        "evm-chain",
        "evm-replay",
        "evm-pipeline",
        "evm-live",
    ];

    let dependencies = |manifest: &str| -> Vec<String> {
        let text =
            std::fs::read_to_string(manifest).unwrap_or_else(|error| panic!("{manifest}: {error}"));
        let mut section = String::new();
        let mut inside = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                inside = trimmed == "[dependencies]";
                continue;
            }
            if inside {
                section.push_str(trimmed);
                section.push('\n');
            }
        }
        section
            .lines()
            .filter_map(|line| line.split('=').next())
            .map(str::trim)
            // `k256.workspace = true` names the package `k256`; a comparison that kept the
            // `.workspace` suffix would match no package name anywhere, in either half of this
            // test — the refusal list above and the control below would both pass on nothing.
            .map(|name| name.split('.').next().unwrap_or(name))
            .filter(|name| !name.is_empty() && !name.starts_with('#'))
            .map(str::to_string)
            .collect()
    };

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let risk = dependencies(&root.join("risk/Cargo.toml").to_string_lossy());
    for package in &forbidden {
        assert!(
            !risk.iter().any(|name| name == *package),
            "§28: crates/risk depends on `{package}`, which can reach a network or a key",
        );
    }

    // The control: the execution crate is allowed to depend on these, and does.
    let execution = dependencies(&root.join("execution/Cargo.toml").to_string_lossy());
    let found: Vec<&str> = forbidden
        .iter()
        .copied()
        .filter(|package| execution.iter().any(|name| name == *package))
        .collect();
    assert!(
        found.contains(&"k256") && found.contains(&"tokio"),
        "the forbidden list would match nothing anywhere, so the result above would be vacuous: \
         execution lists {found:?}",
    );
}

/// §28's required direction, stated at the type level this test can reach: the only thing an
/// `Accept` hands onward is data, and the plan builder is in another crate entirely. A `Reject`
/// carries a reason and no plan, so a caller cannot accidentally execute a rejection.
#[test]
fn a_rejection_carries_no_plan_to_execute() {
    let rejected = policy().evaluate(&reverted(), &fresh());
    let reason = rejected.reason().expect("a rejection names itself");
    assert_eq!(reason.check, RiskCheck::SimulationSuccess);
    // The same record's figures cannot be recovered from a rejection at all — there is no
    // accessor that yields an acceptance, so the only way onward is to invent one.
    assert!(rejected.detail().contains("did not deliver anything"));
}

// ---------------------------------------------------------------------------
// §29 — the plan is M10's plan, field by field
// ---------------------------------------------------------------------------

/// §29's eleven fields, each read out of the record or the acceptance it came from. Nothing here
/// is a number the plan invented: the only arguments the builder took were the sender, the
/// correlation id, the plan's own expiry, the funding claim, the market label and two provenance
/// strings — none of which is monetary.
#[test]
fn the_plan_carries_m10s_eleven_fields_and_adds_no_number() {
    let simulated = filed();
    let granted = acceptance(&simulated);
    let plan = plan_from_simulation(&simulated, &granted, &context()).expect("a bound plan");

    assert_eq!(plan.chain_id, simulated.chain_id.0);
    assert_eq!(plan.executor, simulated.run.executor);
    assert_eq!(plan.sender, context().sender);
    assert_eq!(plan.recipient, simulated.outcome.recipient);
    assert_eq!(plan.input_token, simulated.outcome.input_token);
    assert_eq!(plan.input_token, A, "the triangle round-trips in A");
    assert_eq!(plan.input_amount, simulated.input_amount);
    assert_eq!(plan.min_final_output, simulated.min_final_output);
    assert_eq!(
        plan.min_final_output, granted.min_final_output,
        "the floor the acceptance confirmed is the floor the plan enforces"
    );

    assert_eq!(
        plan.validity.simulated_at_block,
        simulated.simulation_block.number
    );
    assert_eq!(plan.validity.max_block_age, context().max_block_age);

    assert_eq!(
        plan.simulation.block_number,
        simulated.simulation_block.number
    );
    assert_eq!(plan.simulation.block_hash, simulated.simulation_block.hash);
    assert_eq!(
        plan.simulation.simulation_id,
        simulated.identity_hash(),
        "§23's identity is the run's own — the plan does not mint a second one"
    );
    match &plan.simulation.outcome {
        SimulationOutcome::Succeeded {
            gas_used,
            proved_gas_limit,
            final_amount,
        } => {
            assert_eq!(*gas_used, simulated.gas_used);
            assert_eq!(
                *proved_gas_limit, simulated.run.gas_limit,
                "the limit the run executed under, not the burn it reported (M10 §13/§57)"
            );
            assert_eq!(*final_amount, granted.delivered);
        }
        other => panic!("a plan built from a delivery must report a success, got {other:?}"),
    }

    assert_eq!(
        plan.profit.required_final_balance,
        simulated.min_final_output
    );
    match &plan.profit.denomination {
        ProfitDenomination::TokenSettled { token, .. } => {
            assert_eq!(*token, simulated.outcome.input_token);
            assert_eq!(
                *token, A,
                "the floor is denominated in what the route settles in (§34)"
            );
        }
        other => panic!(
            "a token route must not be floored in another unit, got {}",
            other.describe()
        ),
    }

    // §20's legs, one per hop, in trade order, with the derivations M10 requires.
    let legs = match &simulated.run.call {
        ExecutorCall::Execute { legs, .. } => legs.clone(),
        other => panic!("{} is not a route", other.signature()),
    };
    assert_eq!(plan.legs.len(), legs.len());
    for (index, (plan_leg, call_leg)) in plan.legs.iter().zip(legs.iter()).enumerate() {
        assert_eq!(plan_leg.pool, call_leg.pool, "leg {index}");
        assert_eq!(plan_leg.token_in, call_leg.token_in, "leg {index}");
        assert_eq!(plan_leg.token_out, call_leg.token_out, "leg {index}");
        assert_eq!(plan_leg.amount_in, call_leg.amount_in, "leg {index}");
        assert_eq!(plan_leg.amount_out, call_leg.amount_out, "leg {index}");
        assert_eq!(
            plan_leg.min_amount_out, call_leg.min_amount_out,
            "leg {index}"
        );
    }
    assert!(
        plan.validate(&binding()).is_empty(),
        "M10's own gate must accept the plan M11 handed it"
    );
}

/// §29's other half: the frozen object is M10's `ExecutablePlan`, and it is reached through M10's
/// `validate`, not around it. A plan M10 would refuse stays refused when it arrives through M11's
/// door.
#[test]
fn the_frozen_plan_is_m10s_and_freezes_through_m10s_gate() {
    let simulated = filed();
    let granted = acceptance(&simulated);
    let (executable, observed) =
        executable_plan(&simulated, &granted, &context(), &binding()).expect("M10 accepts it");
    let plan = executable.plan();
    assert_eq!(executable.plan_hash(), plan.plan_hash());
    assert_eq!(executable.calldata_hash(), plan.calldata_hash());
    assert_eq!(executable.route_id(), plan.route_id());
    assert_eq!(observed.plan_hash, plan.plan_hash());

    // The same plan, refused by M10 for a binding that names another network. §7's rule is M10's,
    // and M11 forwards it rather than re-answering it.
    let elsewhere = ExecutionBinding {
        chain_id: CHAIN.0 + 1,
        executor: EXECUTOR,
    };
    let refused = executable_plan(&simulated, &granted, &context(), &elsewhere)
        .expect_err("a plan for another chain is not executable here");
    assert_eq!(refused.code(), "plan_rejected");
    assert!(
        refused.to_string().contains("chain"),
        "M10's own sentence should survive the forwarding: {refused}"
    );
}

// ---------------------------------------------------------------------------
// §30 — the binding, and the refusals around it
// ---------------------------------------------------------------------------

/// §30's headline claim, stated as six quotable values: two calldata hashes that are one hash,
/// three route identities that are one identity, and §23's simulation id beside them.
#[test]
fn simulation_and_execution_share_one_calldata_hash_and_one_route_identity() {
    let simulated = filed();
    let granted = acceptance(&simulated);
    let plan = plan_from_simulation(&simulated, &granted, &context()).expect("a bound plan");
    let observed = MultihopBinding::observe(&simulated, &plan);

    assert!(observed.bound(), "{observed:?}");
    assert_eq!(observed.simulation_id, simulated.identity());
    assert_eq!(observed.simulation_calldata_hash, simulated.calldata_hash());
    assert_eq!(observed.execution_calldata_hash, plan.calldata_hash());
    assert_eq!(
        observed.simulation_calldata_hash, observed.execution_calldata_hash,
        "the bytes that would be signed are the bytes that ran"
    );
    assert_eq!(observed.priced_route_id, observed.simulated_route_id);
    assert_eq!(observed.simulated_route_id, observed.execution_route_id);
    assert!(
        observed.execution_route_id.starts_with("m10-"),
        "the route identity is M10's spelling, not a second one invented here: \
         {}",
        observed.execution_route_id,
    );
}

/// The two paths that produce the hash are independent: one encodes the call the run made, the
/// other re-encodes a plan's own field set. The comparison is therefore a comparison — and this
/// test proves the check answers for itself, by pairing a simulation with a plan built from a
/// *different* run.
#[test]
fn the_hash_check_answers_by_itself_when_the_two_sides_are_not_a_pair() {
    let three_leg = filed();
    let four_leg = four_leg_record();
    let four_plan = plan_from_simulation(&four_leg, &acceptance(&four_leg), &context())
        .expect("a four-leg plan");

    let crossed = MultihopBinding::observe(&three_leg, &four_plan);
    assert!(!crossed.bound());
    match crossed.unbound() {
        Some(MultihopPlanRefusal::CalldataHashMismatch {
            simulation,
            execution,
        }) => assert_ne!(
            simulation, execution,
            "the refusal names a mismatch it did not have to compute",
        ),
        other => panic!("expected the hash refusal, got {other:?}"),
    }

    // The same crossed pair, refused by the builder's own entry point rather than by observation.
    let refused = plan_from_simulation(&three_leg, &acceptance(&four_leg), &context())
        .expect_err("a grant over another run is not a grant over this one");
    assert_eq!(refused.code(), "acceptance_mismatch");
}

/// The route-identity check is the other half of §30 and is equally live: with the priced side
/// edited to a different pool, the call and the plan still agree with each other, so the only
/// thing that can refuse this plan is the identity computed from the candidate.
#[test]
fn the_route_identity_check_answers_by_itself_when_the_pricing_moved() {
    let simulated = filed();
    let plan =
        plan_from_simulation(&simulated, &acceptance(&simulated), &context()).expect("bound");

    let mut edited = simulated.clone();
    edited.candidate = with_quote(&simulated.candidate, |quote| {
        quote.hops[1].pool = pool(P3);
    });

    let observed = MultihopBinding::observe(&edited, &plan);
    match observed.unbound() {
        Some(MultihopPlanRefusal::RouteIdMismatch {
            priced,
            simulated: simulated_route,
            execution,
        }) => {
            assert_eq!(
                simulated_route, execution,
                "the run and the plan still walk one route"
            );
            assert_ne!(
                priced, simulated_route,
                "the priced route is the one that moved"
            );
        }
        other => panic!("expected the route refusal, got {other:?}"),
    }

    // The builder asks the same question earlier, as §27's route validity, and refuses before it
    // has a plan to compare hashes with.
    let refused = plan_from_simulation(&edited, &acceptance(&simulated), &context())
        .expect_err("an edited route must not become a plan");
    assert_eq!(refused.code(), "route_validity");
}

/// A stale grant — five figures from one run handed to another — is refused with the field named,
/// because "which side is the old one" is a question only the pair can answer.
#[test]
fn a_grant_from_a_different_run_is_refused_by_field_name() {
    let simulated = filed();
    let record_delivered = simulated.status.delivered().expect("delivers");
    let mut granted = acceptance(&simulated);
    granted.delivered = record_delivered + u(1);
    let refused = plan_from_simulation(&simulated, &granted, &context())
        .expect_err("the acceptance no longer describes this record");
    assert_eq!(refused.code(), "acceptance_mismatch");
    match &refused {
        MultihopPlanRefusal::AcceptanceMismatch {
            field,
            acceptance,
            record,
        } => {
            assert_eq!(field.as_str(), "delivered");
            assert_eq!(
                *acceptance,
                (record_delivered + u(1)).to_string(),
                "the refusal quotes the number that was granted",
            );
            assert_eq!(
                *record,
                record_delivered.to_string(),
                "and the number the record actually carries",
            );
        }
        other => panic!("expected a field-named mismatch, got {other:?}"),
    }
}

/// Each of §29/§30's five granted figures is refused on its own, so a mismatch cannot hide behind
/// whichever field the loop happens to reach first.
#[test]
fn every_granted_figure_is_compared() {
    // A name paired with the one field it moves, so the loop below says which of §29/§30's five
    // figures it is asking about rather than which line of the array it reached.
    type GrantedField = (&'static str, fn(&mut MultihopAcceptance));
    let simulated = filed();
    let fields: [GrantedField; 5] = [
        ("input_amount", |granted| granted.input_amount += u(1)),
        ("delivered", |granted| granted.delivered += u(1)),
        ("min_final_output", |granted| {
            granted.min_final_output += u(1)
        }),
        ("gas_used", |granted| granted.gas_used += 1),
        ("simulation_block", |granted| {
            granted.simulation_block = BlockNumber(granted.simulation_block.0 + 1)
        }),
    ];
    for (name, mutate) in fields {
        let mut granted = acceptance(&simulated);
        mutate(&mut granted);
        let refused = plan_from_simulation(&simulated, &granted, &context())
            .expect_err("a grant that moved one figure is not this record's grant");
        match &refused {
            MultihopPlanRefusal::AcceptanceMismatch { field, .. } => {
                assert_eq!(field.as_str(), name);
            }
            other => panic!("{name}: expected a mismatch, got {other:?}"),
        }
    }
}

/// §30's precondition is asked before anything is built, and a record that never delivered has no
/// delivery to plan an execution around — even when the caller hands it a real grant from a real
/// run, which is how a record edited after the risk pass would arrive.
#[test]
fn a_non_delivery_cannot_be_planned() {
    let simulated = reverted();
    let granted = acceptance(&filed());
    let refused =
        plan_from_simulation(&simulated, &granted, &context()).expect_err("nothing was delivered");
    match refused {
        MultihopPlanRefusal::NotDelivered { status } => assert_eq!(status, "reverted"),
        other => panic!("expected not_delivered, got {other:?}"),
    }
    assert_eq!(refused.code(), "not_delivered");
}

/// §7/§36 at the plan's own edge: the balances, reserves and allowances a run measured are the
/// operator's, so a plan that would be signed by someone else is an execution of a different
/// funding story than the one that was simulated.
#[test]
fn a_sender_that_is_not_the_operator_is_refused() {
    let simulated = filed();
    let granted = acceptance(&simulated);
    let elsewhere = MultihopPlanContext {
        sender: OUTSIDER,
        ..context()
    };
    let refused = plan_from_simulation(&simulated, &granted, &elsewhere)
        .expect_err("the sender is not the account the run funded");
    match refused {
        MultihopPlanRefusal::SenderNotOperator { plan, simulated } => {
            assert_eq!(plan, OUTSIDER);
            assert_eq!(simulated, OPERATOR);
        }
        other => panic!("expected sender_not_operator, got {other:?}"),
    }
}

/// The field-level half of §30: a record whose recipient no longer matches the call that ran
/// produces a plan whose calldata differs from the simulated one. The builder names it as a
/// diverged call — same selector, different arguments — rather than leaving a reader to work it
/// out from a hash.
#[test]
fn a_record_whose_target_moved_diverges_from_the_call_that_ran() {
    let mut simulated = filed();
    simulated.outcome.recipient = OUTSIDER;
    let granted = acceptance(&simulated);
    let refused = plan_from_simulation(&simulated, &granted, &context())
        .expect_err("the plan would send somewhere the run did not");
    match refused {
        MultihopPlanRefusal::CallDiverged {
            plan_signature,
            simulation_signature,
        } => {
            assert_eq!(
                plan_signature, simulation_signature,
                "the selector is the same function — what moved is an argument",
            );
            assert!(
                refused.to_string().contains(plan_signature),
                "the refusal should quote the call it compared: {refused}"
            );
        }
        other => panic!("expected call_diverged, got {other:?}"),
    }
}

/// Building the same plan twice from the same record gives the same three identities and the same
/// plan hash — §30's equality has to be stable, or the evidence row quoting it is quoting a
/// different number each time.
#[test]
fn the_binding_is_stable_across_two_builds() {
    let simulated = filed();
    let granted = acceptance(&simulated);
    let first = plan_from_simulation(&simulated, &granted, &context()).expect("a plan");
    let second = plan_from_simulation(&simulated, &granted, &context()).expect("the same plan");
    assert_eq!(first, second);
    assert_eq!(first.plan_hash(), second.plan_hash());
    assert_eq!(first.calldata_hash(), second.calldata_hash());
    assert_eq!(first.route_id(), second.route_id());

    // And a plan for a different amount is a different plan, while the route identity it walks is
    // still the same route (§55's rule that the identity says which route, not which size).
    let bigger_priced = at_amount(&triangle_route(), u(2_000));
    let bigger_run = request(&bigger_priced);
    let bigger = SimulatedOpportunity::new(&bigger_priced, &bigger_run, delivered(&bigger_priced))
        .expect("a filing at 2 000");
    let bigger_plan = plan_from_simulation(&bigger, &acceptance(&bigger), &context())
        .expect("the bigger run binds too");
    assert_ne!(bigger_plan.plan_hash(), first.plan_hash());
    assert_eq!(bigger_plan.route_id(), first.route_id());
}

/// The refusals are named, so an evidence row counts them by rule. Each variant reachable through
/// this API is asked here by driving one record at it, and the codes are distinct.
#[test]
fn every_refusal_has_a_distinct_code() {
    let simulated = filed();
    let granted = acceptance(&simulated);
    let crossed = four_leg_record();
    let crossed_plan =
        plan_from_simulation(&crossed, &acceptance(&crossed), &context()).expect("a four-leg plan");

    let mut edited_route = simulated.clone();
    edited_route.candidate = with_quote(&simulated.candidate, |quote| {
        quote.hops[1].pool = pool(P3);
    });
    let mut stale = granted.clone();
    stale.delivered += u(1);
    let mut moved_target = simulated.clone();
    moved_target.outcome.recipient = OUTSIDER;

    let refusals: Vec<MultihopPlanRefusal> = vec![
        plan_from_simulation(&reverted(), &granted, &context()).unwrap_err(),
        plan_from_simulation(&simulated, &stale, &context()).unwrap_err(),
        plan_from_simulation(
            &simulated,
            &granted,
            &MultihopPlanContext {
                sender: OUTSIDER,
                ..context()
            },
        )
        .unwrap_err(),
        plan_from_simulation(&edited_route, &granted, &context()).unwrap_err(),
        plan_from_simulation(&moved_target, &acceptance(&moved_target), &context()).unwrap_err(),
        executable_plan(
            &simulated,
            &granted,
            &context(),
            &ExecutionBinding {
                chain_id: CHAIN.0 + 1,
                executor: EXECUTOR,
            },
        )
        .unwrap_err(),
        MultihopBinding::observe(&simulated, &crossed_plan)
            .unbound()
            .expect("a crossed pair is unbound"),
    ];

    let codes: BTreeSet<&str> = refusals.iter().map(MultihopPlanRefusal::code).collect();
    assert_eq!(
        codes.len(),
        refusals.len(),
        "{codes:?} — two refusals share a label, or one of these records does not produce the \
         refusal it was written to produce",
    );
    assert_eq!(codes.len(), 7);
}

/// The four-leg record the crossed-pair tests need, built the same way the three-leg one is.
fn four_leg_record() -> SimulatedOpportunity {
    let priced = four_hop_candidate();
    let run = request(&priced);
    SimulatedOpportunity::new(&priced, &run, delivered(&priced)).expect("a four-leg filing")
}
