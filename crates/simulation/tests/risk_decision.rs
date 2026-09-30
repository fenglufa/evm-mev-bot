//! The last arrow of §67's chain: `SimulationResult → RiskDecision`, asked of the
//! runs M4 actually has.
//!
//! [`evm_risk`]'s own tests decide over results it builds itself, which proves the
//! rule table and nothing else. This file runs the three thresholds against the real
//! route's own runs — the ones in `dump_replay.rs`, over the deployed bytecode and the
//! frozen state — so the milestone's conclusion ("M3's opportunity does not survive
//! execution") is stated in the vocabulary the next stage will read, with the numbers
//! the chain produced rather than in prose.
//!
//! It also puts §47's guarantee where it can be seen: the executors are called with
//! these real decisions, and neither of them can do anything with them.

use alloy_primitives::U256;

use evm_risk::{
    DryRunExecutor, ExecutionRequest, ExecutionResult, Executor, NullExecutor, RiskDecision,
    RiskPolicy, RiskRule, RiskThresholds,
};
use evm_simulation::{ExecutionStatus, NetProfit, SimulationResult};

mod support;
use support::Fixture;

/// Generous on purpose: the questions here are which rule fires and why, and a
/// threshold that was already tight would answer both at once.
fn any_profit_and_room() -> RiskThresholds {
    RiskThresholds {
        minimum_net_profit_wei: U256::ZERO,
        maximum_gas: u64::MAX,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_two_real_rungs_are_rejected_by_two_different_rules() {
    let fixture = Fixture::load().await;

    // The zero-slippage form of §21: ask M3's analytical output. Execution answers
    // with the pool's own revert, so §45's first rule is the one that speaks.
    let greedy = fixture.run_at(fixture.route.analytical_output).await;
    let greedy_decision = any_profit_and_room().evaluate(&greedy);
    assert!(
        matches!(greedy.status, ExecutionStatus::Reverted { .. }),
        "the run this decision is about has to be the reverting one: {:?}",
        greedy.status,
    );
    assert_eq!(greedy_decision.rule(), Some(RiskRule::SimulationSuccess));
    assert!(
        matches!(greedy_decision, RiskDecision::Reject { .. }),
        "{greedy_decision}"
    );

    // The rung that does complete: the pair pays, the tax takes its part, and what
    // comes back is less than the route spent — so the profit floor is the rule that
    // fires, and it fires on a number execution measured.
    let floor = fixture.run_at(U256::ONE).await;
    assert!(floor.status.completed(), "{}", floor.summary());
    let floor_decision = any_profit_and_room().evaluate(&floor);
    assert_eq!(floor_decision.rule(), Some(RiskRule::MinimumNetProfit));
    assert!(
        matches!(floor_decision, RiskDecision::Reject { .. }),
        "a route that came back short is a Reject with a figure, not an Unknown: \
         {floor_decision}"
    );
    assert!(
        floor_decision.reason().contains("before gas"),
        "the reason keeps §31's two-part loss intact: {}",
        floor_decision.reason(),
    );
    println!("greedy (M3's own ask) → {greedy_decision}");
    println!("floor  (ask of one)   → {floor_decision}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn no_threshold_this_route_could_satisfy_turns_it_into_an_accept() {
    // The claim M4's conclusion rests on is stronger than "these thresholds happen to
    // be tight": on this block, with this bytecode, no minimum net profit and no gas
    // ceiling makes either rung acceptable — because the completed run's net figure is
    // a loss and the reverting one has no figure at all, and a Reject is the only
    // answer a loss can get.
    let fixture = Fixture::load().await;
    let runs = [
        (
            "analytical ask",
            fixture.run_at(fixture.route.analytical_output).await,
        ),
        ("ask of one", fixture.run_at(U256::ONE).await),
        (
            "ask of one, unpriced",
            fixture.run_unpriced(U256::ONE).await,
        ),
    ];
    for (label, run) in runs {
        for minimum in [0u64, 1, 1_000] {
            let decision = RiskThresholds {
                minimum_net_profit_wei: U256::from(minimum),
                maximum_gas: run.gas_used(),
            }
            .evaluate(&run);
            assert!(
                !decision.accepted(),
                "{label} at a minimum of {minimum} was accepted: {decision}"
            );
            // Which of the two negative answers this is depends on §45's order, not on
            // the shape of `net_profit`: a reverting run has no net figure either, but
            // its status is the finding and §46 says a revert is a Reject.
            match (run.status.completed(), &run.net_profit) {
                (false, _) => assert!(
                    matches!(decision, RiskDecision::Reject { .. }),
                    "{label}: a run that never executed is a Reject, whatever its profit \
                     field says: {decision}"
                ),
                (true, NetProfit::NotComputable { .. }) => assert!(
                    matches!(decision, RiskDecision::Unknown { .. }),
                    "{label}: a run that completed with no net figure must not be called \
                     a failure: {decision}"
                ),
                (true, _) => assert!(
                    matches!(decision, RiskDecision::Reject { .. }),
                    "{label}: {decision}"
                ),
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_gas_ceiling_is_on_the_path_that_decides() {
    // A paired control on one run: move only the ceiling across the run's own measured
    // gas and the answer changes rule. If the ceiling were decoration, both sides
    // would name the profit floor.
    let fixture = Fixture::load().await;
    let run = fixture.run_at(U256::ONE).await;
    let measured = run.gas_used();
    assert!(
        measured > 0,
        "a completed run measured gas: {}",
        run.summary()
    );

    let roomy = RiskThresholds {
        minimum_net_profit_wei: U256::ZERO,
        maximum_gas: measured,
    }
    .evaluate(&run);
    let tight = RiskThresholds {
        minimum_net_profit_wei: U256::ZERO,
        maximum_gas: measured - 1,
    }
    .evaluate(&run);
    assert_eq!(roomy.rule(), Some(RiskRule::MinimumNetProfit));
    assert_eq!(tight.rule(), Some(RiskRule::MaximumGas));
    assert!(
        tight.reason().contains(&measured.to_string()),
        "the refusal quotes the figure the EVM measured: {}",
        tight.reason(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_real_run_without_a_declared_price_is_unknown_and_says_what_is_missing() {
    // §46's third answer, reached on real data: the sequence completes and the gas is
    // weighed, and there is still no net figure to compare, because §29's price was
    // never declared. This is the one rung that must not be reported as a failure.
    let fixture = Fixture::load().await;
    let priced = fixture.run_at(U256::ONE).await;
    let unpriced = fixture.run_unpriced(U256::ONE).await;

    assert!(unpriced.status.completed(), "{}", unpriced.summary());
    assert_eq!(
        unpriced.gas_used(),
        priced.gas_used(),
        "the only thing that moved is the price, so the decision can be attributed to it"
    );
    let decision = any_profit_and_room().evaluate(&unpriced);
    let rule = match &decision {
        RiskDecision::Unknown { rule, .. } => *rule,
        other => panic!("expected Unknown, got {other}"),
    };
    assert_eq!(rule, RiskRule::MinimumNetProfit);
    assert!(
        decision.reason().contains("no price"),
        "the reason names the missing fact: {}",
        decision.reason(),
    );
    // And the priced twin of the same run is a Reject, which is what makes Unknown a
    // statement about knowledge rather than about the trade.
    assert!(matches!(
        any_profit_and_room().evaluate(&priced),
        RiskDecision::Reject { .. }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn an_execution_request_built_from_a_real_run_describes_that_run_and_nothing_else() {
    let fixture = Fixture::load().await;
    let run: SimulationResult = fixture.run_at(U256::ONE).await;
    let decision = any_profit_and_room().evaluate(&run);
    let request = ExecutionRequest::from_run(&run, decision.clone());

    assert_eq!(request.chain_id, run.chain_id);
    assert_eq!(request.block, run.block, "the pin the state came from");
    assert_eq!(request.sender, run.sender);
    assert_eq!(
        request.targets,
        vec![
            fixture.route.legs[0].address(),
            fixture.route.legs[1].address()
        ],
        "the two pools, in leg order — §55's targets are the contracts the run called"
    );
    assert_eq!(request.input_amount, fixture.route.input_amount);
    assert_eq!(request.gas_estimate, run.gas_used());

    // Neither executor can act on it, and the reason each gives is its own.
    let dry = DryRunExecutor {
        destination: "data/simulation-m4/dry-run.ndjson".to_string(),
    };
    let refused = dry.execute(&request).await;
    assert!(
        matches!(refused, ExecutionResult::Refused { .. }),
        "the policy rejected this run, so the dry run must refuse it: {refused:?}"
    );
    let discarded = NullExecutor.execute(&request).await;
    assert!(
        matches!(discarded, ExecutionResult::Discarded { .. }),
        "{discarded:?}"
    );
    println!("request  → {}", request.gas_estimate);
    println!("dry run  → {}", refused.note());
    println!("null     → {}", discarded.note());
}
