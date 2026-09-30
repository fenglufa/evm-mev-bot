//! M4's execution gate: the same route, the same block, the same deployed
//! bytecode — run against the state the archive node served at the pin, replayed
//! from the dump that recording froze (§62's fixture provider).
//!
//! Everything here is offline, so the four validation gates run it on every change.
//! What it proves is the part a re-derived formula never could: that the
//! *contracts* answer, that their answers are stable across runs (§37), that the
//! amount they pay is below what M3's formula predicted because a transfer tax is
//! decided by execution rather than assumed (§23), and that the gas bill §30
//! computes out of `gas_used × effective_gas_price` is the same number the EVM's
//! own native accounting took out of the sender's purse (§31, §32).
//!
//! The route is reached through the same stages as the live run — the shared
//! `support` module replays the captured block, rebuilds the graph, and calls M3's
//! detector — so a drift between this file and `real_chain.rs` shows up as a
//! failure here, not as a fixture that quietly stopped matching anything.

use std::sync::Arc;

use alloy_primitives::{Address, U256};

use evm_simulation::{
    engine::run, Binding, DumpStateProvider, ExecutionStatus, GasCharge, GrossMovement, NetProfit,
    SimulationError, SimulationResult, StateProvider,
};

mod support;
use support::{request, Fixture, CHAIN};

/// §37: two runs of the same request over the same frozen state are the same
/// result, field for field — including the logs and the state diff — and a
/// provider freshly loaded from the file is a third. A mismatch would mean the
/// engine carries something between runs, which is the one thing a simulation that
/// informs a decision cannot be guilty of.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_frozen_fixture_replays_itself_exactly() {
    let fixture = Fixture::load().await;
    let first = fixture.run_at(U256::ONE).await;
    let second = fixture.run_at(U256::ONE).await;
    assert_eq!(first, second, "the same request ran twice, differently");
    assert_eq!(first.fingerprint(), second.fingerprint());

    // A provider loaded fresh from the same file is not a second run of the same
    // process state: it is the run a later checkout gets.
    let path = support::dump_path();
    let fresh = Arc::new(DumpStateProvider::from_file(&path).expect("the fixture reloads"));
    let source = fresh.source();
    let route = fixture.route.clone();
    let header = fixture.header.clone();
    let third = run(
        fresh as Arc<dyn StateProvider>,
        &request(route, header, U256::ONE, source),
    )
    .await
    .expect("a freshly loaded fixture runs");
    assert_eq!(
        third.fingerprint(),
        first.fingerprint(),
        "replaying the fixture tomorrow is not replaying it today"
    );

    support::report("offline floor ask", &first);
    assert!(first.status.completed(), "{}", first.summary());
}

/// §21's `minimum_amount_out` does not work the way a router's `amountOutMin` does,
/// and the difference decides what this milestone is allowed to claim about the
/// route. The sequence calls the pair's own
/// `swap(amount0Out, amount1Out, to, data)`, so the ask *is* the amount the pair
/// transfers and the only question execution answers is how large an ask its
/// invariant still accepts. That question has one right answer, and the way to get it
/// is to ask: bisection over the ask, with both ends of the bracket proven by running
/// them, gives the route's real capacity as the contracts' own number rather than as
/// a figure derived from their reserves.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_highest_ask_the_pair_will_meet_is_the_capacity_it_pays() {
    let fixture = Fixture::load().await;
    let analytical = fixture.route.analytical_output;

    // The bottom of the bracket, and what a met ask actually pays.
    let floor = fixture.run_at(U256::ONE).await;
    assert_eq!(
        floor.outcome.output(),
        Some(U256::ONE),
        "an ask of one wei was paid in one wei: the pair transferred the amount the plan \
         named, so the ask is a price the sender sets and not a gate the pool negotiates",
    );
    assert_eq!(
        floor.slippage.minimum_output,
        U256::ONE,
        "the floor rung asks for one wei, so nothing but the pair's own invariant gates it"
    );

    // The top: M3's own figure, which §23 predicts the tax puts out of reach.
    let greedy = fixture.run_at(analytical).await;
    assert!(
        matches!(greedy.status, ExecutionStatus::Reverted { .. }),
        "M3's analytical {analytical} was expected to be beyond what the pools can pay, \
         and it completed: {}",
        greedy.summary(),
    );

    let mut met = U256::ONE;
    let mut refused = analytical;
    let mut probes = 0u32;
    while met + U256::ONE < refused {
        let mid = (met + refused) / U256::from(2u8);
        let rung = fixture.run_at(mid).await;
        probes += 1;
        if rung.status.completed() {
            assert_eq!(
                rung.outcome.output(),
                Some(mid),
                "the ask at {mid} completed, so the sender ended holding exactly the ask",
            );
            met = mid;
        } else {
            refused = mid;
        }
    }
    let capacity = met;

    // Three rungs at the boundary, every one of them a run rather than an inference:
    // the capacity itself is met, one wei under it is met, one wei over it is refused.
    let below = fixture.run_at(capacity - U256::ONE).await;
    let at = fixture.run_at(capacity).await;
    let above = fixture.run_at(capacity + U256::ONE).await;
    assert!(below.status.completed(), "one under: {}", below.summary());
    assert!(at.status.completed(), "exactly at: {}", at.summary());
    assert_eq!(below.outcome.output(), Some(capacity - U256::ONE));
    assert_eq!(at.outcome.output(), Some(capacity));
    let revert = match &above.status {
        ExecutionStatus::Reverted { revert, .. } => revert,
        other => panic!(
            "one over the capacity the bisection found did not revert: {other:?} | {}",
            above.summary(),
        ),
    };
    assert!(
        !revert.raw.is_empty(),
        "§35: the pools' revert bytes are kept even when they are not decoded"
    );

    support::report("capacity ask", &at);
    println!(
        "capacity {capacity} from {probes} probes of [{}, {analytical}] | the ask at \
         {capacity} completes and at {} the pair reverts | analytical {analytical}",
        U256::ONE,
        capacity + U256::ONE,
    );

    // §23/§26: execution decided the route pays less than the formula, and by how
    // much. The sign is the finding — a transfer tax can only take away.
    assert!(
        capacity < analytical,
        "the pair met an ask of {capacity}, at or above M3's analytical {analytical}",
    );
    assert!(
        at.compared.behind().is_some(),
        "a run below the analytical figure has to report the gap: {}",
        at.compared.delta_text(),
    );
    assert_eq!(
        at.compared.simulated,
        Some(capacity),
        "the comparison the result reports is this run's own output"
    );
    println!(
        "the analytical figure overreaches the capacity by {} ({})",
        analytical - capacity,
        basis_points(analytical - capacity, analytical),
    );
}

/// §35: the raw revert bytes survive, and the standard shape is decoded into the
/// contract's own words rather than paraphrased by this crate.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_pools_revert_reason_is_kept_and_decoded() {
    let fixture = Fixture::load().await;
    let result = fixture.run_at(fixture.route.analytical_output).await;
    let ExecutionStatus::Reverted { step, call, revert } = &result.status else {
        panic!(
            "the analytical ask was expected to revert, got {}",
            result.summary()
        );
    };
    assert!(
        call.contains("swap"),
        "the step that stopped the sequence is named by its signature: {call}"
    );
    // The reverting step is recorded, and it is the last one: nothing after it ran.
    assert_eq!(result.steps.len(), *step + 1);
    assert!(
        matches!(
            result
                .steps
                .last()
                .expect("a reverted run has steps")
                .status,
            evm_simulation::StepStatus::Reverted(_)
        ),
        "the last recorded step is the one that reverted: {:?}",
        result
            .steps
            .last()
            .expect("a reverted run has steps")
            .status,
    );

    let raw = revert.raw.clone();
    println!(
        "step {step} ({call}) reverted with {} raw bytes: 0x{}",
        raw.len(),
        hex::encode(&raw),
    );
    assert_eq!(
        &raw[..4],
        &evm_simulation::ERROR_STRING_SELECTOR,
        "the payload starts with `Error(string)`"
    );
    let message = revert.message.as_ref().unwrap_or_else(|| {
        panic!(
            "a payload with the standard selector did not decode: 0x{}",
            hex::encode(&raw)
        )
    });
    assert!(!message.is_empty(), "the contract said something");
    assert_eq!(revert.reason(), *message, "the report uses the words");

    // §9: this is a simulation that *worked*. It answered "does not execute", and
    // it says so with a status and a gas bill, not with an error.
    assert!(result.gas_used() > 0, "the revert still consumed gas (§28)");
    assert!(
        !result.net_profit.is_computable(),
        "a run that never completed has no net figure: {:?}",
        result.net_profit,
    );
    assert_eq!(
        GrossMovement::of(&result.outcome, fixture.route.input_amount),
        GrossMovement::NotExecuted,
    );
    assert_eq!(result.gross_profit, None);
}

/// §41 and §34: an allowance that is too small is its own answer, and it must not
/// arrive as `ProviderError` (nothing failed to be read) nor be confused with the
/// revert §35 describes (the contracts did not refuse this ask — the allowance ran
/// out while executing it). The two say opposite things about the route at a real
/// ceiling, which is why the milestone cannot be allowed to merge them.
///
/// The ceiling is derived from what the plan actually spent rather than typed in, so
/// the test says what it means about *this* route: the largest step before the first
/// swap, doubled, and the assertion below checks that this is still under what that
/// swap needed. The doubling is headroom, not padding — an allowance that only just
/// covers a step lets EIP-150's 63/64 rule bite inside it, and then the step that
/// stops the sequence would be an earlier one, for a reason this test is not about.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_gas_allowance_that_is_too_small_is_reported_as_out_of_gas() {
    let fixture = Fixture::load().await;
    let floor = fixture.run_at(U256::ONE).await;
    let first_swap = floor
        .steps
        .iter()
        .position(|step| step.signature.starts_with("swap"))
        .expect("the plan hands tokens to a pair");
    let spent_before = floor.steps[..first_swap]
        .iter()
        .map(|step| step.gas_used)
        .max()
        .expect("steps run before the first swap");
    let swap_needs = floor.steps[first_swap].gas_used;
    let allowance = spent_before * 2;
    assert!(
        allowance < swap_needs,
        "the ceiling this test sets has to sit under the first swap's own spend: \
         {allowance} >= {swap_needs}"
    );

    // A run that refused to answer would be an `Err`, and `run_with_gas` panics with
    // the error in its message — so reaching the next line at all is §41's answer.
    let tight = fixture.run_with_gas(U256::ONE, allowance).await;
    let ExecutionStatus::OutOfGas { step, call } = &tight.status else {
        panic!(
            "{allowance} gas per step was expected to run out, got {}",
            tight.summary()
        );
    };
    assert_eq!(*step, first_swap, "the sequence stopped at: {call}");
    assert!(call.contains("swap"), "the step is named: {call}");
    assert_eq!(
        tight.steps.len(),
        first_swap + 1,
        "nothing after the step that ran out was attempted"
    );
    let stopped = tight.steps.last().expect("the run recorded its steps");
    assert_eq!(
        stopped.gas_used, allowance,
        "§28: an out-of-gas step has spent exactly the allowance it was given, which is \
         the one case where the ceiling and the measurement meet"
    );
    for earlier in &tight.steps[..first_swap] {
        assert!(
            matches!(earlier.status, evm_simulation::StepStatus::Success),
            "step {} ({}) stopped for a reason this test is not about: {:?}",
            earlier.index,
            earlier.signature,
            earlier.status,
        );
    }
    assert!(
        tight.outcome.output().is_none(),
        "a run that never reached its closing measurement has no output to report: {:?}",
        tight.outcome,
    );
    assert!(
        !tight.net_profit.is_computable(),
        "and no net figure either: {:?}",
        tight.net_profit,
    );

    // §34, in one line: the same route, the same ask and the same state report three
    // different things when three different knobs are moved, and no two of them are
    // the same variant.
    assert!(
        floor.status.completed(),
        "the control: the default allowance completes: {}",
        floor.summary(),
    );
    let greedy = fixture.run_at(fixture.route.analytical_output).await;
    assert!(
        matches!(greedy.status, ExecutionStatus::Reverted { .. }),
        "an ask above capacity on the default allowance reverts instead: {}",
        greedy.summary(),
    );

    support::report("tight allowance", &tight);
    println!(
        "allowance {allowance} = the largest pre-swap step ({spent_before}) doubled; the \
         first swap needs {swap_needs} and stopped the sequence at step {step} ({call})",
    );
}

/// §30/§31/§32 read together: the net figure has to be the number the chain's own
/// native accounting says. `gas_used × effective_gas_price` is computed from a
/// measurement and a declared price; the sender's purse is what the EVM actually
/// did to it. Either they agree, or one of them is wrong.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_net_figure_is_the_number_the_purse_actually_lost() {
    let fixture = Fixture::load().await;
    let result = fixture.run_at(U256::ONE).await;

    let start = result
        .measurement(Binding::SenderNativeStart)
        .expect("a completed run reads native at both ends");
    let end = result
        .measurement(Binding::SenderNativeEnd)
        .expect("a completed run reads native at both ends");
    let gas_paid = result
        .gas_charge
        .wei()
        .expect("§29 declared a price the header supports");

    let denomination = match &result.net_profit {
        NetProfit::Gain { denomination, .. }
        | NetProfit::Loss { denomination, .. }
        | NetProfit::BreakEven { denomination, .. }
        | NetProfit::Shortfall { denomination, .. } => denomination,
        NetProfit::NotComputable { reason } => panic!(
            "a completed run that unwrapped makes §32's proof, so it has a net figure: {reason}"
        ),
    };
    assert!(
        denomination.is_proved(),
        "the withdraw released exactly what the native ledger moved: {denomination:?}"
    );
    assert_eq!(denomination.native_start, start);
    assert_eq!(denomination.native_end, end);

    let net = match &result.net_profit {
        NetProfit::Gain { amount, .. } => {
            assert!(end > start, "a gain has to show up in the purse");
            assert_eq!(end - start, *amount);
            format!("+{amount}")
        }
        NetProfit::Loss {
            amount,
            gross,
            gas_cost,
            ..
        } => {
            assert_eq!(
                gas_cost - gross,
                *amount,
                "the bill ate the gain and this much"
            );
            assert!(start > end, "a loss has to show up in the purse");
            assert_eq!(start - end, *amount);
            format!("-{amount} (gross {gross} eaten by {gas_cost} of gas)")
        }
        NetProfit::Shortfall {
            amount, before_gas, ..
        } => {
            assert!(start > end, "a shortfall has to show up in the purse");
            assert_eq!(start - end, *amount);
            format!("-{amount} ({before_gas} short before gas)")
        }
        NetProfit::BreakEven { gas_cost, .. } => {
            assert_eq!(start, end, "break-even means the purse did not move");
            format!("0 (gas {gas_cost})")
        }
        NetProfit::NotComputable { .. } => unreachable!("matched above"),
    };

    println!("{}", result.summary());
    println!(
        "native {start} → {end}, {gas_paid} of it gas, net {net} | the pools paid {} \
         against an input of {} | gas used {} of {} allowed per step",
        result.outcome.output().expect("completed"),
        fixture.route.input_amount,
        result.gas_used(),
        request(
            fixture.route.clone(),
            fixture.header.clone(),
            U256::ONE,
            fixture.source.clone(),
        )
        .transaction
        .gas_limit_per_step,
    );

    // §29/§43: the price came from the pinned header, not from a constant. With no
    // tip declared, the header's base fee times the gas used *is* the bill.
    let GasCharge::Priced {
        effective_gas_price,
        base_fee_per_gas,
        gas_used,
        ..
    } = &result.gas_charge
    else {
        panic!(
            "the declared model prices this run: {:?}",
            result.gas_charge
        );
    };
    let header_base_fee = fixture.header.base_fee_per_gas.expect("1559 block");
    assert_eq!(
        u128::from(*gas_used) * effective_gas_price,
        gas_paid.to::<u128>(),
        "§30's product, in exact integers"
    );
    assert_eq!(
        base_fee_per_gas,
        &Some(header_base_fee),
        "the charge names the header fee it priced against"
    );
    assert_eq!(
        effective_gas_price, &header_base_fee,
        "with no tip, the effective price is the header's own base fee"
    );
}

/// §29/§30/§31 with the price taken away. A run nobody can price is not a run that
/// failed: the EVM still measured the gas, the pools still paid the same amount, and
/// the only thing that goes missing is the net figure — which is why `Unpriced` is an
/// arm of [`GasCharge`] rather than a zero, and why acceptance L asks for
/// `NotComputable` instead of a number computed from an invention.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_run_with_no_declared_price_measures_gas_and_reports_no_net_figure() {
    let fixture = Fixture::load().await;
    let priced = fixture.run_at(U256::ONE).await;
    let unpriced = fixture.run_unpriced(U256::ONE).await;

    assert!(
        unpriced.status.completed(),
        "the absence of a price is not a refusal to execute: {}",
        unpriced.summary(),
    );
    let GasCharge::Unpriced { gas_used, reason } = &unpriced.gas_charge else {
        panic!(
            "nothing was declared, so nothing should have priced it: {:?}",
            unpriced.gas_charge
        );
    };
    assert!(
        !reason.is_empty(),
        "an unpriced run carries the reason it is unpriced"
    );
    assert!(
        unpriced.gas_charge.wei().is_none(),
        "and no wei figure escapes from it"
    );
    assert_eq!(
        *gas_used,
        priced.gas_used(),
        "the measurement is untouched by the absence of a price: §28's gas and §30's cost \
         are different objects"
    );
    assert_eq!(
        unpriced.outcome.output(),
        priced.outcome.output(),
        "what the pools paid does not depend on the price either"
    );
    assert!(
        matches!(unpriced.net_profit, NetProfit::NotComputable { .. },),
        "so the net figure is the one thing that has to be refused: {:?}",
        unpriced.net_profit,
    );
    assert!(
        priced.net_profit.is_computable(),
        "the control: the same run with the header's own fee does get a net figure: {:?}",
        priced.net_profit,
    );

    // §59 read against §31: with no fee declared the EVM charged nothing, so the
    // purse moved by exactly the route's own shortfall and not one wei more. That is
    // the number the priced run's `before_gas` reports, and it is why the two runs
    // differ by the gas bill and nothing else.
    let start = unpriced
        .measurement(Binding::SenderNativeStart)
        .expect("a completed run reads native at both ends");
    let end = unpriced
        .measurement(Binding::SenderNativeEnd)
        .expect("a completed run reads native at both ends");
    support::report("unpriced", &unpriced);
    println!(
        "no price declared: gas used {gas_used} (identical to the priced run's {}), native \
         {start} → {end}, output {}, net refused",
        priced.gas_used(),
        unpriced.outcome.output().expect("completed"),
    );
    assert_eq!(
        end + fixture.route.input_amount,
        start + unpriced.outcome.output().expect("a completed output"),
        "with no fee the only native movements in this sequence are `deposit(input)` out of \
         the purse and `withdraw(output)` back into it, so those two terms are the whole \
         ledger: {start} → {end} against an input of {} and an output of {}",
        fixture.route.input_amount,
        unpriced.outcome.output().expect("a completed output"),
    );
}

/// §36: the diff is the audit trail, and the amounts the run reports have to be
/// readable in it. A balance measurement is a `balanceOf()` call this crate made;
/// the diff is what the EVM's own journal says the storage did. They are two
/// different paths to the same number, and the tax is the place where they are
/// allowed to disagree — because the pool that sent a taxed token paid out one
/// amount and the pool that received it booked a smaller one.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_state_diff_records_what_the_sequence_touched() {
    let fixture = Fixture::load().await;
    let result = fixture.run_at(U256::ONE).await;

    let mut touched = result.state_changes.touched();
    touched.sort();
    for pool in fixture.route.pools() {
        assert!(
            touched.contains(&pool.address),
            "a pool that traded has to appear in the diff: {pool:?}"
        );
    }
    for token in [
        fixture.route.input_token.address,
        fixture.route.mid_token().address,
    ] {
        assert!(
            touched.contains(&token),
            "both tokens' books moved: {token}"
        );
    }
    assert!(
        touched.contains(&result.sender),
        "the sender's own account moved"
    );

    let paid = result.outcome.output().expect("a completed run pays");
    let received_mid = result
        .measurement(Binding::SenderMidReceived)
        .expect("the plan measures what the first pool actually delivered");

    // The input token: one pool was credited the plan's transfer, one paid out what
    // the route delivered. `input` and `paid` are different numbers, so finding both
    // in the diff is a check rather than a restatement.
    let input_book = bookkeeping(&result, fixture.route.input_token.address);
    println!(
        "input-token balance deltas: +{:?} -{:?}",
        input_book.increases, input_book.decreases
    );
    assert!(
        input_book.increases.contains(&fixture.route.input_amount),
        "a pool received the {} the plan transferred: {:?}",
        fixture.route.input_amount,
        input_book.increases,
    );
    assert!(
        input_book.decreases.contains(&paid),
        "a pool paid out the {paid} the route delivered: {:?}",
        input_book.decreases,
    );

    // The mid token: the sender is credited what the first pool's transfer leaves it,
    // and the second pool books what the sender's transfer leaves *it*. Three numbers
    // travel through those two legs — what the pair paid out, what the sender held,
    // what the next pool took — and the gaps between them are the tax §23 refuses to
    // assume.
    let mid_book = bookkeeping(&result, fixture.route.mid_token().address);
    println!(
        "mid-token balance deltas: +{:?} -{:?}",
        mid_book.increases, mid_book.decreases
    );
    assert!(
        mid_book
            .decreases
            .contains(&fixture.route.analytical_mid_amount),
        "the first pool let go of exactly M3's predicted {} — the pair's own math and the \
         detector's math agree about the payout, so the divergence between the two routes \
         is the transfer and not the curve: {:?}",
        fixture.route.analytical_mid_amount,
        mid_book.decreases,
    );
    let booked = *mid_book
        .increases
        .iter()
        .max()
        .unwrap_or_else(|| panic!("no pool booked any of the mid token: {mid_book:?}"));
    let tax = received_mid - booked;
    println!(
        "the first pool paid out {}, the sender was credited {received_mid} and the second \
         pool booked {booked} — the token kept {tax} ({})",
        fixture.route.analytical_mid_amount,
        basis_points(tax, received_mid),
    );
    assert!(
        tax > U256::ZERO,
        "the sender held {received_mid} and the pool it transferred to booked {booked}: \
         the transfer did not deliver what it sent"
    );
    assert!(
        received_mid < fixture.route.analytical_mid_amount,
        "M3 predicted {} of the mid token and the transfer delivered {received_mid}",
        fixture.route.analytical_mid_amount,
    );
}

/// §38 and acceptance H, at the level where the comparison is clean.
///
/// The route as M3 found it cannot be compared end to end against the formula,
/// because its middle token takes a fee on every transfer and the formula knows
/// nothing about that — that is §39's finding, not a discrepancy to explain away.
/// What *can* be compared, exactly and on deployed bytecode, is one leg at a time,
/// with each leg's input taken from what that pool actually booked rather than from
/// what the previous hop was supposed to send:
///
/// ```text
/// leg 1: what the pair transferred out   vs  M3's quote for the route's input
/// leg 2: what the pair pays at that ask  vs  M3's quote for the balance it booked
/// ```
///
/// Neither comparison has a tax inside it — the tax sits between the two legs, where
/// it is measured — so if the pairs and the formula disagree on either one, it is the
/// engine, the formula or the reserves that are wrong, and this says which. The
/// attribution is by amount rather than by slot owner: `SlotChange.address` is the
/// token whose ledger moved, and a balance slot maps back to its holder through a
/// layout this crate deliberately never reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn each_leg_pays_what_the_formula_pays_for_the_amount_the_pair_actually_got() {
    let fixture = Fixture::load().await;
    let floor = fixture.run_at(U256::ONE).await;
    let mid = fixture.route.mid_token().address;
    let input = fixture.route.input_token.address;
    let (first, second) = (fixture.route.legs[0], fixture.route.legs[1]);

    // Leg 1's payout: exactly one slot of the mid token fell by M3's quote, and the
    // balance it fell from is the reserve M3 priced against. The other fall is the
    // token's own accounting — its fee, burned on transfer — which is what makes
    // "exactly one at this amount" the assertion rather than "the largest".
    let mid_changes = floor.state_changes.storage_of(mid);
    let paid = mid_changes
        .iter()
        .filter(|change| change.after < change.before)
        .filter(|change| change.before - change.after == fixture.route.analytical_mid_amount)
        .collect::<Vec<_>>();
    let [sent] = paid.as_slice() else {
        panic!(
            "one balance of the mid token is expected to fall by M3's quoted hop amount; \
             the diff says {:?}",
            mid_changes
                .iter()
                .map(|c| (c.before, c.after))
                .collect::<Vec<_>>(),
        );
    };
    assert_eq!(
        sent.before, first.reserve_out,
        "§36 against §26: the pool that paid leg 1 held exactly the reserve M3 quoted it \
         against"
    );
    let sent_by_first = sent.before - sent.after;

    // Leg 2's input: the only balance of the mid token that grew is the second pool's,
    // and it grew by less than leg 1 sent, because the token kept a fee on the way.
    let booked = mid_changes
        .iter()
        .filter(|change| change.after > change.before)
        .map(|change| change.after - change.before)
        .collect::<Vec<_>>();
    let [booked] = booked.as_slice() else {
        panic!("exactly one mid-token balance grows: {booked:?}");
    };
    let credited = floor
        .measurement(Binding::SenderMidReceived)
        .expect("the run reads what the sender was handed");
    assert!(
        sent_by_first > credited,
        "leg 1 sent {sent_by_first} and the sender was credited {credited}: the tax this \
         test is not about has to be the difference"
    );
    assert!(
        *booked < credited,
        "the second pool booked {booked} of the {credited} the sender passed on"
    );
    assert!(
        *booked < sent_by_first,
        "the second pool booked {booked} against leg 1's payout of {sent_by_first}"
    );

    // Leg 2, paid out in the input token, which carries no fee of its own. Ask the
    // pair for precisely the formula's answer for the balance it booked: if it pays
    // it, the AMM math and the deployed bytecode agree to the wei.
    let second_paid = |result: &SimulationResult| {
        result
            .state_changes
            .storage_of(input)
            .iter()
            .find(|change| change.before == second.reserve_out)
            .map(|change| change.before - change.after)
            .unwrap_or_else(|| {
                panic!(
                    "the second pool's {} balance, as the token reports it, is not M3's \
                     reserve {}: {:?}",
                    input, second.reserve_out, second.reserve_in,
                )
            })
    };
    let quoted =
        evm_opportunity::swap_exact_in(second.reserve_in, second.reserve_out, second.fee, *booked)
            .expect("the second hop quotes for the amount that arrived");
    let at_quote = fixture.run_at(quoted).await;
    assert!(
        at_quote.status.completed(),
        "leg 2 at M3's quote for its own measured input: {}",
        at_quote.summary(),
    );
    assert_eq!(
        at_quote.outcome.output(),
        Some(quoted),
        "leg 2: the pair paid the formula's figure for the balance it booked"
    );
    assert_eq!(
        second_paid(&at_quote),
        quoted,
        "and the token's own ledger says the second pool handed over that same amount — the \
         result's output number and the state diff are two readings of one payout"
    );

    println!(
        "leg 1: pair paid {sent_by_first} = formula {}; sender credited {credited}, second \
         pool booked {booked} (token kept {} between the two hops) | leg 2: formula for \
         {booked} in is {quoted}, pair paid {}",
        fixture.route.analytical_mid_amount,
        credited - booked,
        second_paid(&at_quote),
    );
}

/// The signed view of one token's balance slots: who gained and who lost, without
/// naming the holder — the diff records slots, and mapping a slot back to an owner
/// would be a claim about a storage layout this crate does not need to make.
#[derive(Debug)]
struct Bookkeeping {
    increases: Vec<U256>,
    decreases: Vec<U256>,
}

fn bookkeeping(result: &SimulationResult, token: Address) -> Bookkeeping {
    let mut book = Bookkeeping {
        increases: Vec::new(),
        decreases: Vec::new(),
    };
    for slot in result.state_changes.storage_of(token) {
        match slot.after.cmp(&slot.before) {
            std::cmp::Ordering::Greater => book.increases.push(slot.after - slot.before),
            std::cmp::Ordering::Less => book.decreases.push(slot.before - slot.after),
            std::cmp::Ordering::Equal => {}
        }
    }
    book.increases.sort();
    book.decreases.sort();
    book
}

/// `x / y` in basis points, for printing a share a reader can compare against the
/// registry's own fee figures.
fn basis_points(part: U256, whole: U256) -> u64 {
    u64::try_from(part * U256::from(10_000u16) / whole).expect("a share fits in u64")
}

/// §15/§20/§62: a request built against the node cannot be run on the fixture.
/// The two are different objects even when they describe the same block, and the
/// engine is the thing that refuses — before executing a step.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_request_named_for_another_state_source_is_refused_before_it_runs() {
    let fixture = Fixture::load().await;
    let foreign = request(
        fixture.route.clone(),
        fixture.header.clone(),
        U256::ONE,
        format!("rpc:chain-{}", CHAIN.0),
    );
    let error = run(fixture.shared(), &foreign)
        .await
        .expect_err("the dump provider is not the node the request names");
    match error {
        SimulationError::StateMismatch {
            priced,
            pinned,
            reason,
        } => {
            println!("refused before execution: {reason}");
            assert_eq!(priced, fixture.route.block_number);
            assert_eq!(pinned.number, fixture.route.block_number);
        }
        other => panic!("expected a state mismatch, got {other}"),
    }
}
