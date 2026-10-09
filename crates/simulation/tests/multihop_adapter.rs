//! §19–§25: the candidate becomes an M10 request, and the answer comes back as a
//! simulation — not as a second opinion about the same call.
//!
//! §19 is a prohibition with a reason behind it: M10 already runs
//! `EOA → Executor → Pool → Pool → … → EOA` against real bytecode at a pinned header, and
//! that run is the thing the chain would do. A second simulator in this crate would be a
//! second answer about one call, and the two would drift. So the three things checked here
//! are, in order:
//!
//! 1. **The request is built from the quote and nothing else** (§20/§21). One leg per priced
//!    hop, in trade order, carrying the hop's own `amount_in`/`amount_out`, with each floor
//!    equal to its own ask because that is M10's `DeliveryMismatch` rule restated rather
//!    than a new policy. A quote whose chain does not chain is refused, not repaired —
//!    arithmetic on the caller's numbers would be a repair the market never agreed to.
//! 2. **The chain and the block are the candidate's, not the caller's** (§23). `RunConfig`
//!    carries no field for either, and the two mismatch refusals are the proof that a
//!    simulation cannot be re-dated or re-homed after the fact.
//! 3. **Five endings stay five sentences** (§25). Delivered, reverted, out of gas, halted,
//!    and a success that returned nothing: each is named, and none of them is a profit of
//!    zero. `gross` and `final_amount` are `None` for every ending that paid nothing, and
//!    the canonical text spells that `none`, never `0`.
//!
//! The outcomes here are written by hand [`multihop_market::outcome_of`], through the same
//! revert classifier the real run uses, so a test can ask about all five endings without
//! burning five executions each time. What the EVM itself answers — real bytecode, real
//! pools, real reserves — is `tests/multihop_revm.rs`.

mod multihop_market;

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};

use evm_core::{BlockNumber, ChainId};
use evm_opportunity::{Gross, OptimizedCandidate};
use evm_protocol::{decode_calldata, ExecutorCall, MAX_LEGS};
use evm_simulation::executor::ExecutorRun;
use evm_simulation::result::{RevertData, StepStatus};
use evm_simulation::state::BlockPin;
use evm_simulation::{
    executor_run, legs, BalanceRow, ExecutorOutcome, MultiHopBuildError, SimulatedOpportunity,
    SimulationStatus,
};

use multihop_market::{
    at_amount, candidate, config, delivered, four_hop_candidate, pair_route, pentagon_route,
    pool_revert, quote_of, request, request_guarded, square_route, token, triangle_route, u,
    with_best_input, with_quote, A, B, C, CHAIN, D, EXECUTOR, OPERATOR, P1, P2, P3, RECIPIENT,
};

/// A second header at the same height, for the identity tests. Two blocks can share a
/// number, which is exactly why §23 pins the hash as well and why a simulation must not be
/// filable under either one.
fn other_pin() -> BlockPin {
    BlockPin::new(BlockNumber(100), keccak256(b"m11-synthetic-header-2"))
}

/// A delivered outcome for `run`, pinned to `block`, at whatever guard the request carries.
fn delivered_at(run: &ExecutorRun, block: BlockPin) -> ExecutorOutcome {
    let mut outcome = multihop_market::outcome_of(run, StepStatus::Success, None, 180_000);
    let guard = guard_of(run);
    outcome.delivered = Some(guard);
    outcome.block = block;
    outcome
}

/// The final guard a call carries, read out of the call rather than restated by a test.
fn guard_of(run: &ExecutorRun) -> U256 {
    match &run.call {
        ExecutorCall::Execute {
            min_final_amount, ..
        } => *min_final_amount,
        other => panic!("{} carries no final guard", other.signature()),
    }
}

fn sim_of(
    candidate: &OptimizedCandidate,
    run: &ExecutorRun,
    outcome: ExecutorOutcome,
) -> SimulatedOpportunity {
    SimulatedOpportunity::new(candidate, run, outcome).expect("a filing")
}

/// The refusal form of `SimulatedOpportunity::new`. `SimulatedOpportunity` holds an
/// `ExecutorRun` and an `ExecutorOutcome`, neither of which is `PartialEq`, so a test that
/// wants to compare a refusal names the error rather than the whole result.
fn refusal(
    candidate: &OptimizedCandidate,
    run: &ExecutorRun,
    outcome: ExecutorOutcome,
) -> MultiHopBuildError {
    SimulatedOpportunity::new(candidate, run, outcome).expect_err("the filing was accepted")
}

/// The refusal form of `executor_run`, for the same reason.
fn build_refusal(
    candidate: &OptimizedCandidate,
    config: &evm_simulation::RunConfig,
    guard: U256,
) -> MultiHopBuildError {
    executor_run(candidate, config, guard).expect_err("the request was built")
}

// ---------------------------------------------------------------------------
// §20/§21 — what the legs carry, and where each number came from
// ---------------------------------------------------------------------------

/// One leg per priced hop, in trade order, with the route's own pools and token pair (§20).
/// The order is asserted against the quote's `hops`, the only list that says which way the
/// route was walked.
#[test]
fn one_leg_per_priced_hop_in_trade_order() {
    let candidate = candidate();
    let built = legs(&candidate).expect("a priced candidate builds legs");
    let quote = quote_of(&candidate);
    assert_eq!(built.len(), quote.hops.len());
    assert_eq!(built.len(), candidate.hop_count());
    for (index, (leg, hop)) in built.iter().zip(quote.hops.iter()).enumerate() {
        assert_eq!(
            leg.pool, hop.pool.address,
            "leg {index} is hop {index}'s pool"
        );
        assert_eq!(leg.token_in, hop.token_in.address);
        assert_eq!(leg.token_out, hop.token_out.address);
    }
    // The triangle's trade order, spelled here so the assertion does not only compare a
    // module against itself.
    assert_eq!(built[0].pool, P1);
    assert_eq!(built[1].pool, P2);
    assert_eq!(built[2].pool, P3);
    assert_eq!(built[0].token_in, A);
    assert_eq!(built[0].token_out, B);
    assert_eq!(built[2].token_in, C);
    assert_eq!(built[2].token_out, A);
}

/// `amount_in` and `amount_out` are copied out of the hop records, not re-derived (§20).
/// This is the difference between an adapter and a second pricing engine: a recompute could
/// agree by luck and disagree in the case that matters.
#[test]
fn the_amounts_are_the_ones_the_quote_recorded() {
    let candidate = candidate();
    let built = legs(&candidate).expect("legs");
    let quote = quote_of(&candidate);
    for (index, leg) in built.iter().enumerate() {
        assert_eq!(leg.amount_in, quote.hops[index].amount_in);
        assert_eq!(leg.amount_out, quote.hops[index].amount_out);
    }
    // And the numbers are not one value written everywhere: the first hop gains, the
    // second loses to its fee, so a module that flattened them would fail here.
    assert!(quote.hops[0].amount_out > quote.hops[0].amount_in);
    assert!(quote.hops[1].amount_out < quote.hops[0].amount_out);
    assert_ne!(built[0].amount_out, built[1].amount_out);
}

/// §21's chain, stated at the level of the request: leg *k*'s input is leg *k-1*'s output,
/// and leg 0 spends the amount the search says it searched for.
#[test]
fn the_amount_chain_chains() {
    let candidate = candidate();
    let built = legs(&candidate).expect("legs");
    for index in 1..built.len() {
        assert_eq!(
            built[index].amount_in,
            built[index - 1].amount_out,
            "leg {index} must spend what leg {} delivered",
            index - 1
        );
    }
    assert_eq!(built[0].amount_in, candidate.search.best_input);
}

/// Each leg's floor is its own quoted output, exactly. The contract requires the pair to
/// deliver the figure the plan claims, so a looser floor is a floor the simulation never
/// tested, and a slippage tolerance is not this module's to invent (§20).
#[test]
fn each_floor_is_its_own_quoted_output_exactly() {
    let candidate = candidate();
    let built = legs(&candidate).expect("legs");
    for leg in &built {
        assert_eq!(leg.min_amount_out, leg.amount_out);
        assert!(leg.min_amount_out > U256::ZERO);
    }
    // The guard on the call is the caller's parameter; the per-leg floors are the only
    // floors the route itself carries, and they are never a fraction of an ask.
    let run = request(&candidate);
    assert_eq!(guard_of(&run), candidate.search.best_output);
}

/// The route is a round trip in the legs too: it opens on the candidate's input token and
/// the last leg pays out in that same token.
#[test]
fn the_legs_close_the_circle() {
    let candidate = candidate();
    let built = legs(&candidate).expect("legs");
    let input = candidate.route.input_token().address;
    assert_eq!(built.first().expect("a leg").token_in, input);
    assert_eq!(built.last().expect("a leg").token_out, input);
    for leg in &built {
        assert_ne!(
            leg.token_in, leg.token_out,
            "a leg trades a token for itself"
        );
    }
}

/// Two, three and four hops all build, from the route's count rather than a table of
/// supported lengths.
#[test]
fn two_three_and_four_hop_routes_all_build() {
    for (name, route) in [
        ("pair", pair_route()),
        ("triangle", triangle_route()),
        ("square", square_route()),
    ] {
        let candidate = at_amount(&route, u(1_000));
        let built = legs(&candidate).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(built.len(), route.hop_count(), "{name} leg count");
        assert_eq!(built.len(), candidate.search.quote.hops.len());
    }
}

/// A four-hop route is exactly at `MAX_LEGS`; a five-hop route is refused before any bytes
/// exist. Pricing five hops is legal (§45); asking the contract to carry them is not, and
/// the refusal is what saves a transaction that would revert.
#[test]
fn a_route_one_leg_past_the_contracts_cap_is_refused() {
    let four = four_hop_candidate();
    assert_eq!(legs(&four).expect("four legs").len(), MAX_LEGS as usize);

    let five = at_amount(&pentagon_route(), u(1_000));
    assert_eq!(
        legs(&five),
        Err(MultiHopBuildError::TooManyLegs {
            count: 5,
            maximum: MAX_LEGS
        })
    );
    // A refusal is a refusal to ask: no request exists either.
    assert_eq!(
        build_refusal(&five, &config(), five.search.best_output),
        MultiHopBuildError::TooManyLegs {
            count: 5,
            maximum: MAX_LEGS
        },
        "a five-leg request must not exist"
    );
}

// ---------------------------------------------------------------------------
// §21 — the planted defects, each refused by the name of the number it broke
// ---------------------------------------------------------------------------

#[test]
fn a_broken_amount_chain_is_refused_at_the_leg_that_broke() {
    let base = candidate();
    let broken = with_quote(&base, |quote| quote.hops[1].amount_in = u(999));
    let expected = MultiHopBuildError::AmountChainBroken {
        index: 1,
        expected: base.search.quote.hops[0].amount_out,
        found: u(999),
    };
    assert_eq!(legs(&broken), Err(expected.clone()));
    assert!(
        expected.to_string().contains("amount chain broken"),
        "{expected}"
    );
}

#[test]
fn a_first_leg_spending_another_amount_is_refused() {
    let base = candidate();
    let broken = with_quote(&base, |quote| quote.hops[0].amount_in = u(1_001));
    assert_eq!(
        legs(&broken),
        Err(MultiHopBuildError::InputMismatch {
            expected: base.search.best_input,
            found: u(1_001),
        })
    );
}

/// The mirror of the case above: the candidate's claimed best input moved instead of the
/// quote's. Either way the two disagree, and a plan built at an amount nobody priced is not
/// this candidate.
#[test]
fn a_claimed_input_the_quote_does_not_support_is_refused() {
    let base = candidate();
    let broken = with_best_input(&base, u(2_000));
    assert_eq!(
        legs(&broken),
        Err(MultiHopBuildError::InputMismatch {
            expected: u(2_000),
            found: base.search.quote.hops[0].amount_in,
        })
    );
}

#[test]
fn a_quote_entered_on_the_wrong_token_is_refused() {
    let base = candidate();
    let broken = with_quote(&base, |quote| quote.hops[0].token_in = token(B));
    assert_eq!(
        legs(&broken),
        Err(MultiHopBuildError::WrongInputToken {
            expected: A,
            found: B,
        })
    );
}

#[test]
fn a_route_that_does_not_come_back_is_refused() {
    let base = candidate();
    let broken = with_quote(&base, |quote| quote.hops[2].token_out = token(D));
    assert_eq!(
        legs(&broken),
        Err(MultiHopBuildError::NotACycle {
            expected: A,
            found: D,
        })
    );
}

/// The AMM's own floor can stop a quote short of the route: a hop that buys less than one
/// whole unit ends the round trip and the hops it never reached are not in the record. There
/// is no leg to build for a hop that never ran.
#[test]
fn a_quote_stopped_short_of_the_route_is_refused() {
    let base = candidate();
    let broken = with_quote(&base, |quote| {
        quote.hops.truncate(2);
        quote.truncated = true;
    });
    assert_eq!(
        legs(&broken),
        Err(MultiHopBuildError::TruncatedQuote {
            route: 3,
            quoted: 2,
            stopped_at: 2,
        })
    );
    assert_eq!(
        legs(&with_quote(&base, |quote| quote.hops.clear())),
        Err(MultiHopBuildError::TruncatedQuote {
            route: 3,
            quoted: 0,
            stopped_at: 0,
        })
    );
}

// ---------------------------------------------------------------------------
// §19/§23 — the request
// ---------------------------------------------------------------------------

/// The request is an M10 `ExecutorRun` and nothing else: chain and block out of the
/// candidate, legs out of the quote, and the caller supplying only addresses, gas and the
/// final guard.
#[test]
fn the_request_is_the_existing_m10_shape() {
    let candidate = candidate();
    let run = request(&candidate);
    assert_eq!(run.chain_id, candidate.chain_id);
    assert_eq!(run.priced_at, candidate.target_block);
    let ExecutorCall::Execute {
        legs: found,
        input_token,
        amount_in,
        min_final_amount,
        recipient,
    } = &run.call
    else {
        panic!("the request is not an execute call");
    };
    assert_eq!(*found, legs(&candidate).expect("legs"));
    assert_eq!(*input_token, candidate.route.input_token().address);
    assert_eq!(*amount_in, candidate.search.best_input);
    assert_eq!(*min_final_amount, candidate.search.best_output);
    assert_eq!(*recipient, RECIPIENT);
    assert_eq!(run.executor, EXECUTOR);
    assert_eq!(run.operator, OPERATOR);
}

/// §23 as a type fact: `RunConfig` has no chain field and no block field, so a caller cannot
/// put a route on one chain and its state on another. The Debug spelling of the config is
/// scanned for the field names, with their `=`, so the only way those two numbers reach a
/// run is through the candidate.
#[test]
fn the_caller_has_no_field_to_restate_the_chain_or_the_block() {
    let spelled = format!("{:?}", config());
    for forbidden in ["chain_id=", "priced_at=", "target_block=", "block_number="] {
        assert!(
            !spelled.contains(forbidden),
            "RunConfig must not carry {forbidden}: {spelled}"
        );
    }
    let candidate = candidate();
    let run = request(&candidate);
    assert_eq!(
        (run.chain_id, run.priced_at),
        (candidate.chain_id, candidate.target_block)
    );
}

/// A final guard above the priced output is a floor no route can meet: a bug at the call
/// site rather than a fact about the market, and refused with both numbers in the message.
/// The boundary is inclusive — the guard at the quote is the tightest legal ask.
#[test]
fn a_guard_above_the_priced_output_is_refused_and_at_it_is_asked() {
    let candidate = candidate();
    let over = candidate.search.best_output + U256::ONE;
    assert_eq!(
        build_refusal(&candidate, &config(), over),
        MultiHopBuildError::UnreachableGuard {
            min_final_output: over,
            quoted: candidate.search.best_output,
        }
    );
    let run = request_guarded(&candidate, candidate.search.best_output);
    assert_eq!(guard_of(&run), candidate.search.best_output);
    // Below the quote is a looser ask, still legal: the route may deliver less and the
    // guard is where the caller says so.
    let loose = request_guarded(&candidate, U256::ZERO);
    assert_eq!(guard_of(&loose), U256::ZERO);
}

/// The call the request carries decodes back to itself through the protocol crate's own
/// decoder — the same four bytes a submission would carry (§24's calldata evidence).
#[test]
fn the_calldata_round_trips_through_the_protocol_decoder() {
    let candidate = candidate();
    let run = request(&candidate);
    let encoded = run.call.encode();
    let decoded = decode_calldata(&encoded).expect("the encoder's own output decodes");
    assert_eq!(decoded, run.call);
    let ExecutorCall::Execute { legs: found, .. } = decoded else {
        panic!("decoded as something else");
    };
    assert_eq!(found, legs(&candidate).expect("legs"));
    assert_eq!(found.len(), candidate.hop_count());
}

// ---------------------------------------------------------------------------
// §23/§25 — the outcome read back
// ---------------------------------------------------------------------------

/// Filing a candidate's outcome under a run that is not that candidate's is refused in every
/// direction the three arguments can disagree: another chain, another block, another set of
/// legs, another order, another call entirely.
#[test]
fn a_run_must_belong_to_the_candidate_it_is_filed_under() {
    let candidate = candidate();
    let ok = request(&candidate);
    let outcome = delivered(&candidate);

    let mut other_chain = ok.clone();
    other_chain.chain_id = ChainId(8);
    assert_eq!(
        refusal(&candidate, &other_chain, outcome.clone()),
        MultiHopBuildError::RunChainMismatch {
            expected: CHAIN,
            found: ChainId(8),
        }
    );

    let mut other_block = ok.clone();
    other_block.priced_at = BlockNumber(101);
    assert_eq!(
        refusal(&candidate, &other_block, outcome.clone()),
        MultiHopBuildError::BlockMismatch {
            priced: candidate.target_block,
            pinned: BlockNumber(101),
        }
    );

    let mut short = ok.clone();
    if let ExecutorCall::Execute { legs: found, .. } = &mut short.call {
        found.pop();
    }
    assert_eq!(
        refusal(&candidate, &short, outcome.clone()),
        MultiHopBuildError::RunLegMismatch { index: 3 }
    );

    // Same legs, different order. The same route walked in another order is a different set
    // of transactions, so a comparison that ignored position would be a bug with evidence.
    let mut swapped = ok.clone();
    if let ExecutorCall::Execute { legs: found, .. } = &mut swapped.call {
        found.swap(0, 1);
    }
    assert_eq!(
        refusal(&candidate, &swapped, outcome.clone()),
        MultiHopBuildError::RunLegMismatch { index: 0 }
    );

    // A withdrawal moves a balance and carries no route; its outcome cannot be filed as a
    // simulation of an arbitrage.
    let mut withdraw_run = ok.clone();
    withdraw_run.call = ExecutorCall::Withdraw {
        token: A,
        to: RECIPIENT,
        amount: u(1),
    };
    match SimulatedOpportunity::new(&candidate, &withdraw_run, outcome) {
        Err(MultiHopBuildError::NotAnExecuteCall { signature }) => {
            assert!(signature.starts_with("withdraw("), "{signature}");
        }
        other => panic!("a withdrawal was accepted: {other:?}"),
    }
}

/// A delivery is an amount, and that amount is compared with the input in the input token's
/// own units (§24). Here the contract delivered exactly what the pricing claimed.
#[test]
fn a_delivery_reports_its_amount_and_its_gross() {
    let candidate = candidate();
    let run = request(&candidate);
    let sim = sim_of(&candidate, &run, delivered(&candidate));
    assert_eq!(
        sim.status,
        SimulationStatus::Delivered {
            final_amount: candidate.search.best_output
        }
    );
    assert_eq!(sim.status.name(), "delivered");
    assert_eq!(sim.status.delivered(), Some(candidate.search.best_output));
    assert_eq!(sim.final_amount, Some(candidate.search.best_output));
    assert_eq!(sim.input_amount, candidate.search.best_input);
    assert_eq!(sim.min_final_output, candidate.search.best_output);
    assert_eq!(sim.matches_pricing(), Some(true));
    assert_eq!(
        sim.priced_against_delivered(),
        Some(Gross::Even),
        "a delivery at the priced output is neither a gain nor a loss against the pricing"
    );
    assert!(
        sim.gross.is_some(),
        "a delivery always has a profit answer, even when it is a loss"
    );
}

/// §4's three states, at the level of the simulation: more, exactly, less. `Even` and a
/// failure are different words, and a loss is never the same number as either.
#[test]
fn gain_even_and_loss_are_three_different_answers() {
    let candidate = candidate();
    let input = candidate.search.best_input;
    for (paid, word) in [
        (input + U256::from(7u8), "gain"),
        (input, "even"),
        (input - U256::from(7u8), "loss"),
    ] {
        let run = request(&candidate);
        let outcome = multihop_market::outcome_of(&run, StepStatus::Success, Some(paid), 180_000);
        let sim = sim_of(&candidate, &run, outcome);
        assert_eq!(sim.gross.expect("a delivery has a gross").name(), word);
        assert_eq!(sim.final_amount, Some(paid));
        assert_eq!(
            sim.matches_pricing(),
            Some(paid == candidate.search.best_output)
        );
        // The comparison against the pricing keeps the same three states, in the other
        // direction: what the quote promised against what the contract delivered.
        assert_eq!(
            sim.priced_against_delivered(),
            Some(Gross::between(paid, candidate.search.best_output))
        );
    }
}

/// §24's warning, enforced: gas is paid in the chain's native token and this route settles
/// in an ERC-20, so a net number is not computable here and no field pretends to be one.
/// Two runs that differ only in what gas cost report the same profit.
#[test]
fn gas_never_enters_gross() {
    let candidate = candidate();
    let run = request(&candidate);
    let cheap = multihop_market::outcome_of(&run, StepStatus::Success, Some(u(1_085)), 60_000);
    let dear = multihop_market::outcome_of(&run, StepStatus::Success, Some(u(1_085)), 2_900_000);
    let cheap_sim = sim_of(&candidate, &run, cheap);
    let dear_sim = sim_of(&candidate, &run, dear);

    assert_eq!(cheap_sim.gross, dear_sim.gross);
    assert_eq!(cheap_sim.final_amount, dear_sim.final_amount);
    assert_ne!(cheap_sim.gas_used, dear_sim.gas_used);
    // The bill is kept beside the profit, in wei, and never folded into it.
    assert_ne!(cheap_sim.gas_charge, dear_sim.gas_charge);
    assert!(cheap_sim.canonical_text().contains("gas_used=60000"));
    assert!(dear_sim.canonical_text().contains("gas_used=2900000"));
    // There is no net-profit field to fold it into, in either direction.
    assert_eq!(cheap_sim.input_amount, candidate.search.best_input);
}

/// A pool's `Error(string)` is an answer about the market, not the contract's own rejection:
/// §25 keeps the two apart, and a revert has no profit attached to it.
#[test]
fn a_revert_keeps_the_words_it_came_with_and_no_profit() {
    let candidate = candidate();
    let run = request(&candidate);
    let outcome = multihop_market::outcome_of(
        &run,
        StepStatus::Reverted(pool_revert("INSUFFICIENT_OUTPUT_AMOUNT")),
        None,
        210_000,
    );
    let sim = sim_of(&candidate, &run, outcome);
    let SimulationStatus::Reverted {
        contract_error,
        revert_kind,
        reason,
    } = &sim.status
    else {
        panic!("a pool revert read as {:?}", sim.status);
    };
    assert_eq!(*revert_kind, Some("Error(string)"));
    // Not one of the executor's 22 named errors, so no contract rejection is claimed.
    assert!(contract_error.is_none(), "{contract_error:?}");
    assert_eq!(reason, "INSUFFICIENT_OUTPUT_AMOUNT");
    assert_eq!(sim.status.name(), "reverted");
    assert_eq!(sim.gross, None);
    assert_eq!(sim.final_amount, None);
    assert_eq!(sim.matches_pricing(), None);
    assert_eq!(sim.priced_against_delivered(), None);
}

/// Out of gas and halted are different facts about the same call, and neither is the other's
/// spelling.
#[test]
fn out_of_gas_and_a_halt_are_not_each_other() {
    let candidate = candidate();
    for (status, name, note) in [
        (StepStatus::OutOfGas, "out_of_gas", None),
        (
            StepStatus::Halted("the state limit stopped the call".to_string()),
            "halted",
            Some("the state limit stopped the call"),
        ),
    ] {
        let run = request(&candidate);
        let outcome = multihop_market::outcome_of(&run, status, None, 3_000_000);
        let sim = sim_of(&candidate, &run, outcome);
        assert_eq!(sim.status.name(), name);
        assert_eq!(sim.gross, None);
        assert_eq!(sim.final_amount, None);
        if let Some(wanted) = note {
            let SimulationStatus::Halted { reason } = &sim.status else {
                panic!("not a halt");
            };
            assert_eq!(reason.as_str(), wanted);
        }
    }
    assert_ne!(
        SimulationStatus::OutOfGas,
        SimulationStatus::Halted {
            reason: "x".to_string()
        }
    );
}

/// `execute` is declared to answer with what it delivered. A success that returns no bytes
/// therefore did not do what its signature says, and that is its own ending rather than a
/// delivery of zero.
#[test]
fn a_success_with_no_return_data_is_not_a_zero_delivery() {
    let candidate = candidate();
    let run = request(&candidate);
    let outcome = multihop_market::outcome_of(&run, StepStatus::Success, None, 180_000);
    let sim = sim_of(&candidate, &run, outcome);
    assert_eq!(sim.status, SimulationStatus::NoReturn);
    assert_eq!(sim.status.name(), "no_return");
    assert_eq!(sim.status.delivered(), None);
    assert_eq!(sim.gross, None);
    assert_eq!(sim.final_amount, None);
}

/// §25 as one sweep: for every ending that paid nothing, no field reads as a profit of zero
/// and the canonical text says `none`.
#[test]
fn no_failure_is_ever_written_as_profit_zero() {
    let candidate = candidate();
    let endings = [
        StepStatus::OutOfGas,
        StepStatus::Halted("halt".to_string()),
        StepStatus::Success,
        StepStatus::Reverted(RevertData::new(Bytes::new())),
    ];
    for status in endings {
        let run = request(&candidate);
        let outcome = multihop_market::outcome_of(&run, status, None, 1);
        let sim = sim_of(&candidate, &run, outcome);
        let text = sim.canonical_text();
        assert_eq!(sim.gross, None, "{} wrote a gross", sim.status.name());
        assert_eq!(
            sim.final_amount,
            None,
            "{} wrote an amount",
            sim.status.name()
        );
        assert!(text.contains("gross=none"), "{text}");
        assert!(text.contains("final_amount=none"), "{text}");
        // The words that would mean a zero profit. A failure may not borrow any of them.
        for forbidden in ["gain:0", "loss:0", "even:0", "final_amount=0", "gross=0"] {
            assert!(
                !text.contains(forbidden),
                "{} says {forbidden}",
                sim.status.name()
            );
        }
    }
}

/// M10's residue answer, forwarded rather than re-derived: the simulation reports what the
/// run measured about reserves and balances, so §27's check has one source.
#[test]
fn market_moved_comes_from_the_run_not_from_the_status() {
    let candidate = candidate();
    let run = request(&candidate);
    let mut outcome = delivered(&candidate);
    assert!(outcome.state_changes.is_empty());
    assert!(!sim_of(&candidate, &run, outcome.clone()).market_moved());
    outcome.balances.push(BalanceRow {
        token: A,
        holder: OPERATOR,
        before: u(1_000),
        after: U256::ZERO,
    });
    assert!(sim_of(&candidate, &run, outcome).market_moved());
}

// ---------------------------------------------------------------------------
// §23 — the identity
// ---------------------------------------------------------------------------

/// The identity is `m11-sim-{chain}-{block}-{hash}` and the hash is taken over the canonical
/// text, so a report row and a hash cannot disagree about which simulation is being quoted.
#[test]
fn the_identity_names_the_chain_the_block_and_a_hash_over_the_text() {
    let candidate = candidate();
    let run = request(&candidate);
    let sim = sim_of(&candidate, &run, delivered(&candidate));
    let identity = sim.identity();
    assert!(
        identity.starts_with(&format!("m11-sim-{}-{}-", CHAIN.0, 100)),
        "{identity}"
    );
    let hash = sim.identity_hash();
    assert_eq!(
        hash,
        keccak256(sim.canonical_text().as_bytes()),
        "the identity is a hash of the text, not of a parallel structure"
    );
    assert!(identity.ends_with(&format!("-{hash}")));
    // The pinned header's hash is inside the text: two blocks at height 100 are two
    // simulations, and the line that says so is visible in a report.
    assert!(sim
        .canonical_text()
        .contains(&format!("block_hash={}", sim.simulation_block.hash)));
    let moved = sim_of(&candidate, &run, delivered_at(&run, other_pin()));
    assert_ne!(moved.canonical_text(), sim.canonical_text());
    assert_ne!(moved.identity_hash(), sim.identity_hash());
}

/// Building the same simulation twice produces the same identity, and every fact a reader
/// would quote moves it. This is the property §30's binding rests on: a stale claim is a
/// different number, not a paragraph a reader can reinterpret.
#[test]
fn the_identity_is_stable_for_one_run_and_moves_for_every_fact() {
    let candidate = candidate();
    let run = request(&candidate);
    let base = sim_of(&candidate, &run, delivered(&candidate));
    let again = sim_of(&candidate, &run, delivered(&candidate));
    assert_eq!(base.identity_hash(), again.identity_hash());
    assert_eq!(base.canonical_text(), again.canonical_text());

    let mut variants: Vec<SimulatedOpportunity> = Vec::new();

    // A different final guard is a different claim about what the route must deliver.
    let guarded = request_guarded(&candidate, base.candidate.search.best_output - U256::ONE);
    let mut guarded_outcome = delivered(&candidate);
    guarded_outcome.min_final_amount = guard_of(&guarded);
    variants.push(sim_of(&base.candidate, &guarded, guarded_outcome));

    // Another header at the same height (§23).
    variants.push(sim_of(&candidate, &run, delivered_at(&run, other_pin())));
    // A delivery at the input instead of at the priced output.
    let mut flat = delivered(&candidate);
    flat.delivered = Some(flat.amount_in);
    variants.push(sim_of(&candidate, &run, flat));
    // The same call taking more gas.
    let mut slower = delivered(&candidate);
    slower.gas_used = 240_000;
    variants.push(sim_of(&candidate, &run, slower));
    // A different payee, a different deployment, a different gas limit.
    let mut other_payee = delivered(&candidate);
    other_payee.recipient = Address::with_last_byte(0x77);
    variants.push(sim_of(&candidate, &run, other_payee));
    let mut moved_executor = run.clone();
    moved_executor.executor = Address::with_last_byte(0x11);
    let mut executor_outcome = delivered(&candidate);
    executor_outcome.executor = moved_executor.executor;
    variants.push(sim_of(&candidate, &moved_executor, executor_outcome));
    let mut other_limit = run.clone();
    other_limit.gas_limit = 1_500_000;
    let mut limit_outcome = delivered(&candidate);
    limit_outcome.gas_limit = other_limit.gas_limit;
    variants.push(sim_of(&candidate, &other_limit, limit_outcome));
    // A reverted ending at the same numbers.
    let mut ended = delivered(&candidate);
    ended.status = StepStatus::Reverted(pool_revert("no"));
    ended.delivered = None;
    variants.push(sim_of(&candidate, &run, ended));

    let base_hash = base.identity_hash();
    let mut hashes: Vec<B256> = vec![base_hash];
    for variant in &variants {
        let hash = variant.identity_hash();
        assert_ne!(
            hash,
            base_hash,
            "the identity did not move for:\n{}",
            variant.canonical_text()
        );
        assert!(
            !hashes.contains(&hash),
            "two different facts produced the same identity:\n{}",
            variant.canonical_text()
        );
        hashes.push(hash);
    }
}

/// The route line is spelled from the route's own edge identity, and one line per leg
/// carries its three amounts, so a report can name the cycle without re-deriving it from a
/// `Debug` implementation.
#[test]
fn the_canonical_text_names_the_route_by_its_edges() {
    let candidate = candidate();
    let run = request(&candidate);
    let text = sim_of(&candidate, &run, delivered(&candidate)).canonical_text();
    let edges = candidate.route.identity().edges();
    assert!(
        text.contains(&format!("route={} hops:", edges.len())),
        "{text}"
    );
    for edge in edges {
        assert!(
            text.contains(&format!("{:#x}", edge.pool.address)),
            "the route's pool {} is missing from the text",
            edge.pool.address
        );
    }
    for (index, leg) in legs(&candidate).expect("legs").iter().enumerate() {
        let line = format!(
            "leg[{index}]|pool={:#x}|token_in={:#x}|token_out={:#x}|amount_in={}|amount_out={}|min_amount_out={}",
            leg.pool, leg.token_in, leg.token_out, leg.amount_in, leg.amount_out,
            leg.min_amount_out
        );
        assert!(text.contains(&line), "{line} is missing");
    }
}

/// The bytes a §30 binding would quote are the request's own, and a longer route at the same
/// guard is a different hash.
#[test]
fn the_calldata_hash_is_a_hash_of_the_requests_bytes() {
    let candidate = candidate();
    let run = request(&candidate);
    let sim = sim_of(&candidate, &run, delivered(&candidate));
    assert_eq!(sim.calldata(), run.call.encode());
    assert_eq!(sim.calldata_hash(), keccak256(sim.calldata().as_ref()));
    assert!(sim
        .canonical_text()
        .contains(&sim.calldata_hash().to_string()));
    let four = four_hop_candidate();
    let four_sim = sim_of(&four, &request(&four), delivered(&four));
    assert_ne!(four_sim.calldata_hash(), sim.calldata_hash());
    assert_eq!(four_sim.canonical_text().matches("leg[").count(), 4);
}
