//! §45's three checks, in the order a reader would ask them.

use alloy_primitives::U256;
use evm_simulation::{ExecutionStatus, NetProfit, SimulationResult};
use serde::Serialize;

use crate::decision::{RiskDecision, RiskRule, NO_BROADCAST};

/// The first-stage policy is a pair of numbers and one question execution already
/// answered. Serialised with every `Accept` it grants, so a decision can be re-derived
/// from the record it travelled with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RiskThresholds {
    /// The smallest net profit, in wei of the gas currency, this layer will call
    /// acceptable. Compared strictly: a run whose net figure *equals* the minimum is
    /// not above it, and §46's Accept says `> minimum`.
    pub minimum_net_profit_wei: U256,
    /// The largest `gas_used` this layer will look at further. This is the §28
    /// measured figure, not a §28 limit: a run that was stopped by its own allowance
    /// has already failed the first check.
    pub maximum_gas: u64,
}

/// §45's trait, with one input instead of two.
///
/// The task's sketch handed the policy an `Opportunity` and a `SimulationResult`;
/// this takes only the result, because the result already carries M3's numbers
/// ([`evm_simulation::PricedRoute`]'s `analytical_output`, `analytical_gross_profit`
/// and `priced_by` travel inside it) and a policy that read both would be able to
/// decide about a finding and a run that are not the same run.
pub trait RiskPolicy {
    fn evaluate(&self, run: &SimulationResult) -> RiskDecision;
}

impl RiskPolicy for RiskThresholds {
    fn evaluate(&self, run: &SimulationResult) -> RiskDecision {
        // §45's `simulation_success` first: nothing else about a run that did not
        // happen is worth comparing. Its answer is a Reject and not an Unknown
        // because §34's status says exactly what occurred — the run is evidence of a
        // failure, not an absence of evidence.
        if !run.status.completed() {
            return RiskDecision::Reject {
                rule: RiskRule::SimulationSuccess,
                reason: describe(&run.status),
            };
        }

        // Then the cheap check, before any profit question: a run over the ceiling is
        // out regardless of what it earned. Gas *measured* is what is compared, so an
        // unpriced run (§31) is still subject to this rule — the EVM weighed it even
        // when no price could be attached.
        if run.gas_used() > self.maximum_gas {
            return RiskDecision::Reject {
                rule: RiskRule::MaximumGas,
                reason: format!(
                    "the run measured {} of gas, above the ceiling of {}",
                    run.gas_used(),
                    self.maximum_gas
                ),
            };
        }

        match &run.net_profit {
            NetProfit::Gain {
                amount,
                gross,
                gas_cost,
                ..
            } => {
                let gas_used = run.gas_used();
                if *amount > self.minimum_net_profit_wei {
                    RiskDecision::Accept {
                        net_profit_wei: *amount,
                        gross_profit_wei: *gross,
                        gas_cost_wei: *gas_cost,
                        gas_used,
                        minimum_net_profit_wei: self.minimum_net_profit_wei,
                        maximum_gas: self.maximum_gas,
                        reason: format!(
                            "{NO_BROADCAST} Completed on {}, measured {gas_used} of gas \
                             costing {gas_cost} wei, and netted {amount} against a minimum of {}",
                            run.block, self.minimum_net_profit_wei
                        ),
                    }
                } else {
                    RiskDecision::Reject {
                        rule: RiskRule::MinimumNetProfit,
                        reason: format!(
                            "the run netted {amount}, which is not above the minimum of {}",
                            self.minimum_net_profit_wei
                        ),
                    }
                }
            }
            NetProfit::BreakEven { gas_cost, .. } => RiskDecision::Reject {
                rule: RiskRule::MinimumNetProfit,
                reason: format!(
                    "the run came back exactly even after a gas bill of {gas_cost}, so its net \
                     profit is 0 — not above the minimum of {}",
                    self.minimum_net_profit_wei
                ),
            },
            NetProfit::Loss {
                amount,
                gross,
                gas_cost,
                ..
            } => RiskDecision::Reject {
                rule: RiskRule::MinimumNetProfit,
                reason: format!(
                    "the route earned {gross} before gas and the bill was {gas_cost}, so it is \
                     {amount} short — profitable on M3's arithmetic and not on this chain's"
                ),
            },
            NetProfit::Shortfall {
                before_gas,
                gas_cost,
                amount,
                ..
            } => RiskDecision::Reject {
                rule: RiskRule::MinimumNetProfit,
                reason: format!(
                    "the sequence came back {before_gas} short of what it spent before gas and \
                     the bill was {gas_cost}, so it is {amount} under water — there was never a \
                     gross profit for gas to eat into"
                ),
            },
            // §46's third answer, and the one that matters most here: the run
            // completed, the gas was weighed, and the net figure still does not
            // exist — because no price was declared (§29) or the profit's unit could
            // not be shown to be the unit gas is paid in (§31/§32). The reason is
            // quoted rather than paraphrased: it names the missing fact, which is the
            // next milestone's work list.
            NetProfit::NotComputable { reason } => RiskDecision::Unknown {
                rule: RiskRule::MinimumNetProfit,
                reason: format!(
                    "the run completed and spent {} of gas, but there is no net figure to \
                     compare against the minimum of {}: {reason}",
                    run.gas_used(),
                    self.minimum_net_profit_wei
                ),
            },
        }
    }
}

/// §34's status in one sentence.
///
/// The step is quoted from [`ExecutionStatus`]'s own `call` text rather than
/// reformatted here, because that text already names the index, the nonce, the
/// sender, the target and the selector — the evidence §55 asks a decision to point at.
/// Repeating the index in front of it would be a second, contradictable way of saying
/// the same thing.
fn describe(status: &ExecutionStatus) -> String {
    match status {
        ExecutionStatus::Completed => "the sequence completed".to_string(),
        ExecutionStatus::Reverted { call, revert, .. } => {
            format!("{call}: reverted — {}", revert.reason())
        }
        ExecutionStatus::OutOfGas { call, .. } => {
            format!("{call}: spent its whole gas allowance")
        }
        ExecutionStatus::Halted { call, reason, .. } => {
            format!("{call}: stopped by the EVM — {reason}")
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, b256, Address, Bytes, U256};
    use evm_core::{BlockNumber, ChainId, TokenId};
    use evm_simulation::{
        BlockPin, Denomination, GasCharge, GasPricing, GrossMovement, OutputComparison,
        PlanSummary, RevertData, SimulatedOutcome, SlippagePolicy, SlippageRecord, StateChanges,
    };

    use super::*;

    // Nothing in this module is the block M4 ran on: the chain, the height, the
    // hash and the three addresses are invented labels, chosen so that a fixture
    // for a rule cannot be mistaken for evidence about a real market.
    const CHAIN: ChainId = ChainId(1);
    const BLOCK: BlockNumber = BlockNumber(1);
    const PIN: BlockPin = BlockPin::new(
        BLOCK,
        b256!("0x0000000000000000000000000000000000000000000000000000000000000001"),
    );
    const WETH: Address = address!("0x00000000000000000000000000000000000000a1");
    const SENDER: Address = address!("0x00000000000000000000000000000000000000b2");

    /// What the invented run spends, and what the sender's purse holds before it:
    /// big enough that a fixture which pays its gas bill ends above zero, so the
    /// denomination below can add up without a checked sub needing a special case.
    const INPUT: u64 = 1_000_000;
    const PURSE: u64 = 1_000_000;
    const PRICE: u128 = 300;

    /// A charge whose wei figure is the product the §30 definition says it is,
    /// computed rather than typed.
    fn priced(gas_used: u64, price: u128) -> GasCharge {
        GasCharge::Priced {
            gas_used,
            effective_gas_price: price,
            base_fee_per_gas: Some(price),
            wei: U256::from(gas_used) * U256::from(price),
            pricing: GasPricing::Eip1559 {
                priority_fee_per_gas: 0,
                provenance: "a test fixture's declared price".to_string(),
            },
        }
    }

    fn unpriced(gas_used: u64) -> GasCharge {
        GasCharge::Unpriced {
            gas_used,
            reason: "the test declares no price for this run".to_string(),
        }
    }

    /// The wrapped/native ledger for a run that ends holding `output` of the input
    /// token: `converted + native_start == native_end + native_spent + gas_paid`,
    /// which is [`Denomination::is_proved`]'s own equality read off these numbers.
    fn denomination(output: U256, gas_paid: U256) -> Denomination {
        let native_end = output
            .checked_add(U256::from(PURSE))
            .and_then(|sum| sum.checked_sub(U256::from(INPUT)))
            .and_then(|sum| sum.checked_sub(gas_paid))
            .expect(
                "an invented run whose gas bill outweighs its purse has no denomination to \
                     prove either, so it cannot stand in for a test about net profit",
            );
        Denomination {
            token: TokenId::new(CHAIN, WETH),
            converted: output,
            native_start: U256::from(PURSE),
            native_end,
            native_spent: U256::from(INPUT),
            gas_paid,
            proved_by: "a test fixture's deposit and withdraw".to_string(),
        }
    }

    /// One run, with the three numbers the policy reads and nothing else filled in
    /// beyond inert data.
    fn record(
        status: ExecutionStatus,
        charge: GasCharge,
        net_profit: NetProfit,
    ) -> SimulationResult {
        SimulationResult {
            chain_id: CHAIN,
            block: PIN,
            state_source: "test".to_string(),
            sender: SENDER,
            status,
            steps: Vec::new(),
            gas_charge: charge,
            measurements: Vec::new(),
            compared: OutputComparison {
                analytical: U256::from(INPUT),
                simulated: None,
            },
            outcome: SimulatedOutcome::NotExecuted,
            gross_profit: None,
            gross_loss: None,
            net_profit,
            state_changes: StateChanges::default(),
            slippage: SlippageRecord {
                expected_output: U256::from(INPUT),
                policy: SlippagePolicy::Exact,
                minimum_output: U256::from(INPUT),
            },
            plan_summary: PlanSummary {
                steps: Vec::new(),
                evm_rules: "test".to_string(),
                pools: Vec::new(),
                input_token: WETH,
                input_amount: U256::from(INPUT),
                analytical_mid_amount: U256::ZERO,
                analytical_output: U256::from(INPUT),
                priced_by: "a test fixture".to_string(),
            },
        }
    }

    /// A run built the way the engine builds one: the net figure is
    /// [`NetProfit::compute`]'s answer for this movement, this charge and a
    /// denomination that adds up — so no test here can be satisfied by a combination
    /// of numbers no execution could have produced.
    fn completed(movement: GrossMovement, gas_used: u64, price: Option<u128>) -> SimulationResult {
        let charge = match price {
            Some(price) => priced(gas_used, price),
            None => unpriced(gas_used),
        };
        let output = match movement {
            GrossMovement::Gained(by) => U256::from(INPUT) + by,
            GrossMovement::Even => U256::from(INPUT),
            GrossMovement::Lost(by) => U256::from(INPUT) - by,
            GrossMovement::NotExecuted => U256::ZERO,
        };
        let gas_paid = charge.wei().unwrap_or(U256::ZERO);
        let unit = (movement != GrossMovement::NotExecuted).then(|| denomination(output, gas_paid));
        let net_profit = NetProfit::compute(movement, &charge, unit.as_ref());
        record(ExecutionStatus::Completed, charge, net_profit)
    }

    fn thresholds(minimum: u64, maximum_gas: u64) -> RiskThresholds {
        RiskThresholds {
            minimum_net_profit_wei: U256::from(minimum),
            maximum_gas,
        }
    }

    #[test]
    fn a_run_that_never_completed_is_rejected_by_the_first_rule() {
        // §46's own example, and the ordering has to be proven rather than assumed:
        // this run is also far over the ceiling and has no net figure, so if the
        // rules were checked in any other order the reason would name gas.
        let reverted = record(
            ExecutionStatus::Reverted {
                step: 7,
                // The engine's own step description: the index, the nonce and the
                // target come from there, not from a reformatting here.
                call: "step 7 (nonce 6) to 0xb2: swap(uint256,uint256,address,bytes)".to_string(),
                revert: RevertData::new(Bytes::from_static(&[0x00, 0x01])),
            },
            priced(500_000, PRICE),
            NetProfit::compute(GrossMovement::NotExecuted, &priced(500_000, PRICE), None),
        );
        let decision = thresholds(0, 100).evaluate(&reverted);
        assert_eq!(decision.rule(), Some(RiskRule::SimulationSuccess));
        assert!(matches!(decision, RiskDecision::Reject { .. }));
        assert!(
            decision
                .reason()
                .contains("step 7 (nonce 6) to 0xb2: swap("),
            "the reason carries the engine's own words for the step, so the decision points              at evidence: {}",
            decision.reason(),
        );
        assert!(
            !decision.reason().contains("ceiling"),
            "the gas rule was not consulted: {}",
            decision.reason(),
        );

        // The other two §34 answers that are not a revert are still the same rule.
        for (status, words) in [
            (
                ExecutionStatus::OutOfGas {
                    step: 4,
                    call: "step 4 (nonce 3) to 0xa1: swap(uint256,uint256,address,bytes)"
                        .to_string(),
                },
                "gas allowance",
            ),
            (
                ExecutionStatus::Halted {
                    step: 2,
                    call: "step 2 (nonce 1) to 0xa1: deposit()".to_string(),
                    reason: "NotEnoughFunds".to_string(),
                },
                "NotEnoughFunds",
            ),
        ] {
            let stopped = record(
                status,
                unpriced(10),
                NetProfit::compute(GrossMovement::NotExecuted, &unpriced(10), None),
            );
            let decision = thresholds(0, 100_000).evaluate(&stopped);
            assert_eq!(decision.rule(), Some(RiskRule::SimulationSuccess));
            assert!(decision.reason().contains(words), "{}", decision.reason());
        }
    }

    #[test]
    fn the_gas_ceiling_is_answered_before_any_profit_question() {
        // The same run twice, with only the ceiling moved: this is the control that
        // shows the rule is on the path that decides, rather than an unprofitable
        // run being rejected for a reason that had nothing to do with its size.
        let run = completed(
            GrossMovement::Gained(U256::from(500_000u64)),
            1_000,
            Some(PRICE),
        );
        let tight = thresholds(0, 999).evaluate(&run);
        assert_eq!(tight.rule(), Some(RiskRule::MaximumGas));
        assert!(
            tight.reason().contains("1000") && tight.reason().contains("999"),
            "both the measured figure and the ceiling are in the answer: {}",
            tight.reason(),
        );
        let roomy = thresholds(0, 1_000).evaluate(&run);
        assert!(roomy.accepted(), "{roomy}");

        // An unpriced run is measured, so the ceiling still applies to it (§28's
        // gas_used is a fact even when §29's price is not).
        let weighing = completed(GrossMovement::Gained(U256::from(500_000u64)), 1_000, None);
        assert_eq!(
            thresholds(0, 999).evaluate(&weighing).rule(),
            Some(RiskRule::MaximumGas),
            "an unpriced run escaped the ceiling",
        );
    }

    #[test]
    fn an_accept_carries_the_numbers_it_was_granted_on() {
        let run = completed(
            GrossMovement::Gained(U256::from(500_000u64)),
            1_000,
            Some(PRICE),
        );
        let decision = thresholds(1_000, 1_000).evaluate(&run);
        let RiskDecision::Accept {
            net_profit_wei,
            gross_profit_wei,
            gas_cost_wei,
            gas_used,
            minimum_net_profit_wei,
            maximum_gas,
            reason,
        } = &decision
        else {
            panic!("expected an Accept, got {decision}");
        };
        assert_eq!(*gross_profit_wei, U256::from(500_000u64));
        assert_eq!(*gas_cost_wei, U256::from(1_000u64) * U256::from(PRICE));
        assert_eq!(
            *net_profit_wei,
            U256::from(500_000u64) - *gas_cost_wei,
            "and the net figure is the difference of the two numbers above, not a fourth one"
        );
        assert_eq!(*gas_used, run.gas_used());
        assert_eq!(*minimum_net_profit_wei, U256::from(1_000u64));
        assert_eq!(*maximum_gas, 1_000);
        assert!(
            reason.contains("no broadcast"),
            "§47 rides with the decision: {reason}"
        );
        assert!(reason.contains(&PIN.to_string()), "{reason}");
    }

    #[test]
    fn a_gain_exactly_equal_to_the_minimum_is_not_above_it() {
        let run = completed(
            GrossMovement::Gained(U256::from(500_000u64)),
            1_000,
            Some(PRICE),
        );
        let net = match run.net_profit {
            NetProfit::Gain { amount, .. } => amount,
            ref other => panic!("expected a Gain, got {other:?}"),
        };
        let at_line = thresholds(net.to::<u64>(), 1_000).evaluate(&run);
        assert_eq!(at_line.rule(), Some(RiskRule::MinimumNetProfit));
        assert!(matches!(at_line, RiskDecision::Reject { .. }));
        let one_below = thresholds(net.to::<u64>() - 1, 1_000).evaluate(&run);
        assert!(
            one_below.accepted(),
            "the same run accepted at a minimum of one less: {one_below}"
        );
    }

    #[test]
    fn the_three_ways_to_be_unprofitable_say_which_one_they_are() {
        let gas_cost = 1_000u64 * PRICE as u64;
        let cases = [
            // Gross exactly the bill: the route worked and the chain took all of it.
            (GrossMovement::Gained(U256::from(gas_cost)), "exactly even"),
            // Gross positive but smaller than the bill: profitable on the curve.
            (GrossMovement::Gained(U256::from(gas_cost / 2)), "earned"),
            // No gross at all: the route itself came back short, and gas is added to
            // a hole that was already there.
            (GrossMovement::Lost(U256::from(1_000u64)), "before gas"),
        ];
        for (movement, words) in cases {
            let run = completed(movement, 1_000, Some(PRICE));
            let decision = thresholds(0, 100_000).evaluate(&run);
            assert_eq!(decision.rule(), Some(RiskRule::MinimumNetProfit));
            assert!(
                matches!(decision, RiskDecision::Reject { .. }),
                "a run with no net profit is a Reject, not an Unknown: {decision}"
            );
            assert!(
                decision.reason().contains(words),
                "{movement:?} should be answered as {words}: {}",
                decision.reason(),
            );
        }
    }

    #[test]
    fn a_missing_net_figure_is_unknown_and_quotes_what_is_missing() {
        // Three runs that completed and still have no number to compare: no declared
        // price (§29), no provable unit (§31/§32), and each is Unknown rather than
        // Reject — the run may well have been profitable.
        let run = completed(GrossMovement::Gained(U256::from(500_000u64)), 1_000, None);
        let unpriced = thresholds(0, 100_000).evaluate(&run);
        assert_eq!(unpriced.rule(), Some(RiskRule::MinimumNetProfit));
        assert!(matches!(unpriced, RiskDecision::Unknown { .. }));
        assert!(
            unpriced.reason().contains("gas cost is not computable"),
            "the reason execution gave is quoted, not paraphrased: {}",
            unpriced.reason(),
        );

        let charge = priced(1_000, PRICE);
        let no_unit =
            NetProfit::compute(GrossMovement::Gained(U256::from(500_000u64)), &charge, None);
        assert!(
            matches!(no_unit, NetProfit::NotComputable { .. }),
            "compute itself has to be the thing that refuses: {no_unit:?}"
        );
        let undecided =
            thresholds(0, 100_000).evaluate(&record(ExecutionStatus::Completed, charge, no_unit));
        assert!(matches!(undecided, RiskDecision::Unknown { .. }));
        assert!(
            undecided.reason().contains("1 ETH = 1 WETH"),
            "§32's prohibition is the reason and it travels into the decision: {}",
            undecided.reason(),
        );

        // The paired control: the same movement, the same price, a denomination that
        // adds up — and the answer is a number.
        let priced_run = completed(
            GrossMovement::Gained(U256::from(500_000u64)),
            1_000,
            Some(PRICE),
        );
        assert!(
            thresholds(0, 100_000).evaluate(&priced_run).accepted(),
            "the missing fact, not the run, was what made it Unknown"
        );
    }

    #[test]
    fn a_decision_displays_as_one_line_that_names_the_rule() {
        let run = completed(
            GrossMovement::Lost(U256::from(1_000u64)),
            1_000,
            Some(PRICE),
        );
        let text = thresholds(0, 100_000).evaluate(&run).to_string();
        assert_eq!(
            text.lines().count(),
            1,
            "the report prints one line per decision: {text}"
        );
        assert!(text.starts_with("Reject on minimum_net_profit: "), "{text}");
    }
}
