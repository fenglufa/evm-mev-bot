//! M10 §55: the executor runs in front of REVM on real bytecode and real state, and the
//! six outcome classes each answer a question a formula could not.
//!
//! ```text
//! cargo test -p evm-simulation --test executor_revm -- --nocapture
//! ```
//!
//! The three ingredients §25 names are all present in every run here: the *executor's*
//! bytecode is the committed `solc` output, read from
//! `contracts/artifacts/ArbitrageExecutor.bin-runtime`; the *pair* and *token* bytecode and
//! their reserves come from a recording the archive node served at block 37 530 593 on chain
//! 91 342; and the call is one transaction, shaped the way a real one would be. Nothing in
//! this file computes an output amount and calls that a simulation. The only Rust-side
//! arithmetic is the quote that produces the *claim* the contract is required to check, so
//! when the two disagree the run says so — and that disagreement is exactly what the
//! `*_one_wei_*` controls plant.
//!
//! The fixture is `CONTROLLED_FIXTURE` and says so in its own source string (§29): the pools
//! are the market's, the deployment and the funding are declared rows. The gate that keeps
//! that sentence true is [`fixture_adds_only_what_it_declares`], which enumerates the
//! difference between the fixture and the recording rather than trusting the builder.
//!
//! ## The two shapes of "no"
//!
//! A revert is an *answer*; an error from the harness is a *refusal to answer*. §71 keeps them
//! apart, so the boundary controls (`ChainMismatch`, `StateMismatch`, `MissingCode`) assert on
//! `SimulationError`, and everything the contract itself rejects asserts on
//! [`ExecutorOutcome::contract_error`]. The residue checks (`market_moved`, `changed_slots_in`)
//! run on both.

use alloy_primitives::{Address, B256, U256};

use evm_core::ChainId;
use evm_protocol::{ExecutorCall, ExecutorLeg, ExecutorTopics, MAX_LEGS};
use evm_simulation::error::SimulationError;
use evm_simulation::executor::{BalanceRow, ExecutorOutcome, ReserveSnapshot};
use evm_simulation::result::StepStatus;
use evm_simulation::GasCharge;

mod executor_state;
use executor_state::{
    address_word, allowance_key, allowed_word, amount_in, balance_key, exact_out,
    executor_runtime_code, mapping_slot, plain_slot, pool_balance, recorded_dump, workspace_root,
    Fixture, Knobs, BLOCK, BYSTANDER, CHAIN, EXECUTOR, EXECUTOR_LOCK_SLOT, EXECUTOR_OPERATOR_SLOT,
    EXECUTOR_PAIR_ALLOWED_SLOT, FIXTURE_LABEL, MID, OPERATOR, POOL_A, POOL_B, RECIPIENT, WETH,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The fixture on disk, rebuilt and byte-compared first, so every run below is a run on the
/// state a report can quote.
fn fixture() -> Fixture {
    Fixture::committed()
}

fn word(value: U256) -> B256 {
    B256::new(value.to_be_bytes::<32>())
}

fn addr_word(address: Address) -> B256 {
    word(address_word(address))
}

/// The contract's own named error, or a sentence saying the revert was not the contract's.
///
/// Every negative control asserts on this rather than on "reverted", because §27 forbids
/// treating a revert as sufficient evidence: the *reason* is the finding.
fn blame(outcome: &ExecutorOutcome) -> String {
    match &outcome.contract_error {
        Some(name) => name.clone(),
        None => format!(
            "not the executor: {:?} — {}",
            outcome.revert_kind,
            outcome
                .revert()
                .map(|data| data.reason())
                .unwrap_or_else(|| "no revert data".to_string())
        ),
    }
}

/// One (token, holder) pair's before/after, or a panic naming what the fixture forgot to
/// declare: the run measures exactly the holders [`evm_simulation::executor::ExecutorRun::holders`]
/// asks for, and those are the rows the fixture writes.
fn balance(outcome: &ExecutorOutcome, token: Address, holder: Address) -> &BalanceRow {
    outcome
        .balance_row(token, holder)
        .unwrap_or_else(|| panic!("no balance row for {token} at {holder}"))
}

/// §27: a reverted run has to show the market where it was, at the level of storage rather
/// than at the level of two compared numbers.
fn assert_no_residue(outcome: &ExecutorOutcome) {
    assert!(
        !outcome.market_moved(),
        "a reverted run moved a reserve or a balance: {outcome:#?}",
    );
    for watched in [POOL_A, POOL_B, WETH, MID, EXECUTOR] {
        let changed = outcome.changed_slots_in(watched);
        assert!(
            changed.is_empty(),
            "{watched} keeps {} changed word(s) after a reverted run: {changed:?}",
            changed.len(),
        );
    }
}

/// The route as the plan claims it, with leg `leg`'s ask moved by `delta` wei.
fn call_moving(fx: &Fixture, leg: usize, delta: i64) -> ExecutorCall {
    fx.execute(fx.route.legs_moved(leg, delta), fx.route.weth_out_leg2)
}

/// A pool's reserve of one token, on either side of the run, in the pair's own ordering.
fn reserve(snap: &ReserveSnapshot, token: Address) -> U256 {
    if snap.token0 == token {
        snap.reserve0
    } else {
        snap.reserve1
    }
}

/// The same list the assertion below compares against, in the order a set has.
fn sorted(mut addresses: Vec<Address>) -> Vec<Address> {
    addresses.sort();
    addresses
}

// ---------------------------------------------------------------------------
// The fixture itself
// ---------------------------------------------------------------------------

/// The one place the fixture files are written. Everything else reads them and byte-compares.
///
/// ```text
/// cargo test -p evm-simulation --test executor_revm -- --ignored --nocapture
/// ```
#[test]
#[ignore = "writes fixtures/simulation-m10/*; run it to regenerate the fixture"]
fn write_the_committed_fixture() {
    let fx = Fixture::build(Knobs::standard());
    let (path, additions) = fx.write_files();
    println!("wrote {} and {}", path.display(), additions.display());
    println!(
        "route: {} WETH in → {} MID (pool A) → {} WETH out (pool B); profit {} WETH wei",
        fx.route.amount_in,
        fx.route.mid_out_leg1,
        fx.route.weth_out_leg2,
        fx.route.weth_out_leg2 - fx.route.amount_in,
    );
    println!("declared accounts: {}", fx.accounts.len());
    for account in &fx.accounts {
        println!("  {} — {}", account.address, account.what);
    }
    println!("declared words: {}", fx.additions.len());
    for row in &fx.additions {
        println!("  {} {} = {}", row.contract, row.what, row.value);
    }
    println!("fixture bytes: {}", fx.fixture_bytes().len());
    println!("source: {}", fx.source);
}

/// §49's D3 gate: the state a run is asked about is the state on disk, byte for byte. A result
/// computed from an in-memory rebuild cannot be compared with a result a report quotes from the
/// file unless the two are the same bytes.
#[test]
fn fixture_rebuilds_the_committed_bytes() {
    let fx = fixture();
    assert_eq!(
        fx.additions.len(),
        13,
        "6 executor words, 5 balance words, 2 allowance words",
    );
    assert_eq!(
        fx.accounts.len(),
        3,
        "the executor, the recipient, the bystander"
    );
    assert!(
        fx.source.starts_with(FIXTURE_LABEL),
        "the source string has to carry the §29 label, and it reads {:?}",
        fx.source,
    );
    assert!(
        fx.source.contains("dump-37530593"),
        "and name the recording it is built on: {:?}",
        fx.source,
    );
    assert!(
        fx.source.contains("declared accounts") && fx.source.contains("declared storage rows"),
        "and say how much was added: {:?}",
        fx.source,
    );
}

/// The audit form of "the market was not touched": the difference from the recording is
/// computed by comparing two dumps, and equals the declared rows and nothing else.
#[test]
fn fixture_adds_only_what_it_declares() {
    let fx = fixture();
    assert_eq!(
        fx.difference_from_recorded(),
        fx.declared_difference(),
        "the fixture carries state it did not declare, or lost a declared row"
    );
    assert_eq!(
        fx.introduced_accounts(),
        sorted(vec![EXECUTOR, RECIPIENT, BYSTANDER]),
        "the accounts the fixture created, and nothing else"
    );

    // Stated per watched contract, since that is the sentence §29 needs: nothing was added to
    // either pool's books, and the only token words are holder balances and one approval.
    for row in &fx.additions {
        assert_ne!(row.contract, POOL_A, "a word written into pool A");
        assert_ne!(row.contract, POOL_B, "a word written into pool B");
    }
    let recorded = recorded_dump();
    for pool in [POOL_A, POOL_B] {
        assert_eq!(
            recorded.account(pool),
            fx.dump.account(pool),
            "{pool}'s account header moved",
        );
    }
}

/// A declared row has to be a word somebody asks for, or it is an invented fact with a
/// reason string attached. The witness is the provider: take the row back out of the dump and
/// the run stops at exactly that slot instead of answering zero and carrying on.
///
/// This is aimed at the least obvious row in the fixture — WETH's own
/// `allowance(executor → executor)`, which no part of the plan mentions. It is consulted
/// because the token's bytecode asks a holder what it allows itself while the executor is
/// moving WETH through it, and the check below is the executable form of that claim.
#[tokio::test]
async fn a_declared_word_the_run_does_not_ask_about_would_be_an_invention() {
    let key = allowance_key(WETH, EXECUTOR, EXECUTOR);
    let fx = Fixture::build(Knobs::standard());
    assert!(
        fx.additions
            .iter()
            .any(|row| row.contract == WETH && row.key == key),
        "the committed fixture declares the self-allowance row",
    );

    let mut thin = Fixture::build(Knobs::standard());
    let words = thin
        .dump
        .storage
        .get_mut(&WETH.to_string().to_lowercase())
        .expect("WETH has declared words");
    let slot = format!("0x{key:064x}");
    words
        .remove(&slot)
        .expect("the row is materialized in the dump");
    let error = thin
        .try_run(thin.standard_call())
        .await
        .expect_err("a run whose transfers consult an unanswered slot must not guess one");
    match &error {
        SimulationError::MissingState(reason) => {
            assert!(
                reason.contains(&format!("{WETH}")) && reason.contains(&format!("{key}")),
                "the refusal names the token and the slot it could not answer: {reason}",
            );
        }
        other => panic!("expected a state refusal at the removed slot, got {other:?}"),
    }
}

/// The gap the route trades is a fact about the recording, not a number chosen for the test:
/// the same input priced through pool A and back through pool B comes out around 8.6× larger,
/// because the two pools hold different prices for the same pair. This is §28's constructed
/// shape only in the sense that it is *found* rather than constructed — and it is exactly the
/// kind of gap §29 forbids calling a real opportunity, because a recording at one height is not
/// a market that was open at execution time.
#[test]
fn the_recorded_gap_is_what_the_route_trades() {
    let dump = recorded_dump();
    let fx = fixture();
    let a_in = pool_balance(&dump, WETH, POOL_A);
    let a_out = pool_balance(&dump, MID, POOL_A);
    let b_in = pool_balance(&dump, MID, POOL_B);
    let b_out = pool_balance(&dump, WETH, POOL_B);
    for (label, value) in [
        ("pool A WETH", a_in),
        ("pool A MID", a_out),
        ("pool B MID", b_in),
        ("pool B WETH", b_out),
    ] {
        assert!(!value.is_zero(), "{label} has no recorded balance");
    }
    assert_eq!(
        fx.route.mid_out_leg1,
        exact_out(fx.route.amount_in, a_in, a_out),
        "leg 0's ask re-derived from the token's own books",
    );
    assert_eq!(
        fx.route.weth_out_leg2,
        exact_out(fx.route.mid_out_leg1, b_in, b_out),
        "leg 1's ask re-derived from the token's own books",
    );
    assert!(
        fx.route.weth_out_leg2 > fx.route.amount_in,
        "pool A holds {a_in} WETH against {a_out} MID while pool B holds {b_out} WETH against \
         {b_in} MID — the same pair at prices the route cannot close",
    );
    assert!(
        pool_balance(&dump, WETH, POOL_B) > U256::ZERO,
        "pool B's WETH side is empty, so nothing comes back through it",
    );
}

// ---------------------------------------------------------------------------
// §55 class 1: the successful two-hop route
// ---------------------------------------------------------------------------

/// The whole point of the milestone, as one transaction: 0.0001 WETH in, about 0.00086 WETH
/// out, every number in between delivered by the pools rather than asserted by Rust.
#[tokio::test]
async fn successful_two_hop_route_profits_on_real_bytecode() {
    let fx = fixture();
    let outcome = fx.run(fx.standard_call()).await;
    println!("{}", outcome.describe());

    assert!(outcome.succeeded(), "{}", blame(&outcome));
    assert_eq!(outcome.status, StepStatus::Success);
    let delivered = outcome
        .delivered
        .expect("execute returns the delivered amount");

    // The route closes: what came back is more than what went in.
    assert!(
        delivered > outcome.amount_in,
        "the round trip returned {delivered} for {} in — that is a loss, not the fixture's \
         price gap",
        outcome.amount_in,
    );
    assert_eq!(
        delivered, fx.route.weth_out_leg2,
        "the pools paid exactly what the invariant allows, which is also what the plan claimed"
    );

    // §55's balance check, per holder: the wallet is emptied, the executor holds nothing, the
    // recipient is paid what the call returned.
    let wallet = balance(&outcome, WETH, fx.knobs.caller);
    assert_eq!(wallet.before, fx.knobs.balance, "the wallet started funded");
    assert_eq!(wallet.after, U256::ZERO, "the wallet spent its whole input");
    for token in [WETH, MID] {
        let row = balance(&outcome, token, EXECUTOR);
        assert!(
            row.after.is_zero(),
            "{token} is still sitting in the executor after a successful run: {}",
            row.after,
        );
    }
    let paid = balance(&outcome, WETH, RECIPIENT);
    assert_eq!(paid.before, U256::ZERO, "the recipient started empty");
    assert_eq!(
        paid.after, delivered,
        "the recipient is paid what the call returned"
    );

    // Both markets moved, in the direction the route needed, by exactly the amounts claimed.
    let a = outcome.reserve_row(POOL_A).expect("pool A is leg 0");
    let b = outcome.reserve_row(POOL_B).expect("pool B is leg 1");
    assert!(a.changed() && b.changed(), "neither pool moved");
    assert_eq!(
        reserve(&a.after, WETH) - reserve(&a.before, WETH),
        fx.route.amount_in,
        "pool A took in",
    );
    assert_eq!(
        reserve(&a.before, MID) - reserve(&a.after, MID),
        fx.route.mid_out_leg1,
        "pool A paid out",
    );
    assert_eq!(
        reserve(&b.after, MID) - reserve(&b.before, MID),
        fx.route.mid_out_leg1,
        "pool B took in",
    );
    assert_eq!(
        reserve(&b.before, WETH) - reserve(&b.after, WETH),
        delivered,
        "pool B paid out",
    );

    // The logs, topic0 by topic0, from the executor's own declarations.
    let topics = ExecutorTopics::default();
    let legs: Vec<&evm_simulation::result::ExecutedLog> = outcome
        .logs
        .iter()
        .filter(|log| log.topics.first() == Some(&topics.leg_executed))
        .collect();
    assert_eq!(
        legs.len(),
        2,
        "one LegExecuted per leg, and {} logs in all",
        outcome.logs.len(),
    );
    assert_eq!(legs[0].address, EXECUTOR);
    assert_eq!(legs[0].topics[1], word(U256::ZERO), "leg 0's index");
    assert_eq!(legs[0].topics[2], addr_word(POOL_A), "leg 0's pool");
    assert_eq!(legs[1].topics[1], word(U256::ONE), "leg 1's index");
    assert_eq!(legs[1].topics[2], addr_word(POOL_B), "leg 1's pool");
    let executed = outcome
        .logs
        .iter()
        .filter(|log| log.topics.first() == Some(&topics.executed))
        .collect::<Vec<_>>();
    assert_eq!(executed.len(), 1, "exactly one Executed");
    assert_eq!(executed[0].topics[1], addr_word(fx.knobs.caller));
    assert_eq!(executed[0].topics[2], addr_word(RECIPIENT));
    assert_eq!(executed[0].topics[3], addr_word(WETH));

    // Gas is measured, not assumed, and the run is nowhere near its ceiling.
    assert!(outcome.gas_used > 0, "the EVM reported no gas");
    assert!(
        outcome.gas_used < outcome.gas_limit,
        "the call used its whole allowance ({}), so this run cannot tell a route that fits \
         from one that does not",
        outcome.gas_limit,
    );
    match &outcome.charge {
        GasCharge::Priced { gas_used, wei, .. } => {
            assert_eq!(*gas_used, outcome.gas_used);
            assert!(!wei.is_zero(), "a priced run costs zero wei");
        }
        other => panic!("the run priced itself as {other:?}, but the header carries a base fee"),
    }

    // §27's mirror image, and the reason the empty residue lists in every control below are a
    // finding rather than a tautology: the machinery does see a market move when one happens.
    assert!(outcome.market_moved());
    for watched in [POOL_A, POOL_B, WETH, MID] {
        assert!(
            !outcome.changed_slots_in(watched).is_empty(),
            "the word-level diff saw no change at {watched} after a run that traded through it",
        );
    }
    // The lock is released: no executor word differs after a successful run, which is the
    // manual `_lock` ending where it began.
    assert!(
        outcome.changed_slots_in(EXECUTOR).is_empty(),
        "the deployment keeps state from a successful run: {:?}",
        outcome.changed_slots_in(EXECUTOR),
    );
}

/// The claim and the market are the same number: the run's `getReserves()` before-values are
/// what the Rust quote was computed from. If this drifts, every ask in this file is a guess.
#[tokio::test]
async fn the_route_quoted_is_the_route_the_pools_price() {
    let fx = fixture();
    let outcome = fx.run(fx.standard_call()).await;
    assert!(outcome.succeeded(), "{}", blame(&outcome));
    let dump = recorded_dump();

    let a = outcome.reserve_row(POOL_A).expect("pool A");
    for token in [WETH, MID] {
        assert_eq!(
            reserve(&a.before, token),
            pool_balance(&dump, token, POOL_A),
            "pool A's {token} reserve as the pair reports it vs as the token's books say",
        );
    }
    assert_eq!(
        reserve(&a.after, MID),
        reserve(&a.before, MID) - fx.route.mid_out_leg1,
    );

    let b = outcome.reserve_row(POOL_B).expect("pool B");
    for token in [WETH, MID] {
        assert_eq!(
            reserve(&b.before, token),
            pool_balance(&dump, token, POOL_B),
            "pool B's {token} reserve as the pair reports it vs as the token's books say",
        );
    }
    assert_eq!(
        reserve(&b.before, WETH) - reserve(&b.after, WETH),
        fx.route.weth_out_leg2,
        "the second leg paid what the quote said",
    );
}

// ---------------------------------------------------------------------------
// §55 classes 2–6 and §27: every "no" leaves nothing behind
// ---------------------------------------------------------------------------

/// §27, the test the milestone is graded on. Leg 1 has already settled when leg 2's pool
/// refuses, so the transaction reverts mid-route — and the proof is not the revert, it is that
/// Pair A's reserves, both tokens' balances, and the executor's words all read the same on both
/// sides of it.
#[tokio::test]
async fn forced_second_leg_failure_leaves_no_residue() {
    let fx = fixture();
    // Ask pool B for one wei more than its invariant can pay. It refuses, and the leg that had
    // already completed is discarded with the rest of the transaction.
    let outcome = fx.run(call_moving(&fx, 1, 1)).await;
    println!("{}", outcome.describe());

    assert!(!outcome.succeeded());
    assert!(
        outcome.contract_error.is_none(),
        "the second pool refused this one, in its own words, not the executor's: {}",
        blame(&outcome),
    );
    assert_eq!(outcome.revert_kind, Some("Error(string)"));
    let message = outcome
        .revert()
        .and_then(|data| data.message.clone())
        .expect("the pair's own error string");
    assert!(
        message.starts_with("UniswapV2"),
        "expected the pair's error string, got {message:?}"
    );
    assert_eq!(outcome.delivered, None);
    assert!(
        outcome.logs.is_empty(),
        "a reverted call emits nothing at all"
    );
    assert_no_residue(&outcome);

    // And leg 1 really did run before the refusal: the same fixture asked to fail inside the
    // first pool never reaches a second swap, so it burns strictly less gas.
    let first_leg = fx.run(call_moving(&fx, 0, 1)).await;
    assert!(!first_leg.succeeded());
    assert_eq!(first_leg.revert_kind, Some("Error(string)"));
    assert_no_residue(&first_leg);
    assert!(
        outcome.gas_used > first_leg.gas_used,
        "a run that completed leg 1 before reverting should cost more than one that reverted \
         inside it: {} vs {}",
        outcome.gas_used,
        first_leg.gas_used,
    );
}

/// §55's slippage class: the route is exactly what the market prices and the plan simply
/// demands more at the end than exists. §13's guard is a balance measured at the recipient, so
/// this is the contract's own `FinalShortfall`, reached only after both pools have paid.
#[tokio::test]
async fn final_floor_above_the_priced_output_reverts_final_shortfall() {
    let fx = fixture();
    let call = fx.execute(fx.route.legs(), fx.route.weth_out_leg2 + U256::ONE);
    let outcome = fx.run(call).await;
    println!("{}", outcome.describe());

    assert_eq!(blame(&outcome), "FinalShortfall");
    assert_eq!(outcome.delivered, None);
    assert!(outcome.logs.is_empty());
    assert_no_residue(&outcome);
}

/// A plan-level slippage claim that contradicts itself: a leg that asks for less than it
/// demands. `_checkLeg` catches it before anything moves, which is why it is a different
/// finding from the shortfall above.
#[tokio::test]
async fn a_leg_asking_below_its_own_floor_is_rejected_before_it_moves() {
    let fx = fixture();
    let mut legs = fx.route.legs();
    legs[1].amount_out = legs[1].min_amount_out - U256::ONE;
    let outcome = fx
        .run(ExecutorCall::Execute {
            legs,
            input_token: WETH,
            amount_in: fx.route.amount_in,
            min_final_amount: fx.route.weth_out_leg2,
            recipient: RECIPIENT,
        })
        .await;
    assert_eq!(blame(&outcome), "AskBelowFloor");
    assert_no_residue(&outcome);
}

/// An under-claimed *intermediate* leg, from both sides of the second pool's rounding.
///
/// A V2 pair pays exactly the amount it is asked to pay and then checks its own constant
/// product, so a leg that asks for less than the market would give does not fail — it
/// forfeits. One wei of the mid token is worth a small fraction of one wei of the WETH the
/// second pool pays, so the 1-wei probe arrives short and the route still completes at the
/// planned output: nothing reverts, nothing is left behind, and the plan simply gave up a
/// claim it could have made. That is why this shape is not a §27 residue control — a control
/// that does not fail proves nothing about residue.
///
/// Under-claim by an input the second pool can price, and the finding flips: the pair is
/// asked to pay for an input that is not there, and refuses in its own words
/// (`Error(string)`, the constant-product assert) rather than the executor's. The step is
/// derived from the recording's reserves, not guessed: it is the input that moves the pair's
/// own quote by one wei of output, and the test re-checks that against the quote itself.
#[tokio::test]
async fn a_short_intermediate_leg_is_invisible_inside_a_rounding_gap_and_visible_beyond_one() {
    let fx = fixture();

    let outcome = fx.run(call_moving(&fx, 0, -1)).await;
    assert!(
        outcome.succeeded(),
        "one wei of the middle token is worth a fraction of a wei of the output: {}",
        outcome.describe()
    );
    assert_eq!(
        outcome.delivered,
        Some(fx.route.weth_out_leg2),
        "and the shortened plan pays exactly what the untouched plan pays",
    );

    let reserve_in = pool_balance(&fx.dump, MID, POOL_B);
    let reserve_out = pool_balance(&fx.dump, WETH, POOL_B);
    // exact_out is floor(in·997·Rout / (Rin·1000 + in·997)); the quotient moves by one wei
    // when the input moves by the denominator over (997·Rout), so that ratio plus a margin
    // is the smallest shortfall the pair can see.
    let denominator = reserve_in * U256::from(1000u32) + fx.route.mid_out_leg1 * U256::from(997u32);
    let step = denominator / (U256::from(997u32) * reserve_out) + U256::from(4u32);
    assert!(
        exact_out(fx.route.mid_out_leg1 - step, reserve_in, reserve_out) < fx.route.weth_out_leg2,
        "{step} wei of the mid token has to be one the second pool can price",
    );
    let delta = -i64::try_from(step).expect("a shortfall that fits an i64");
    let outcome = fx.run(call_moving(&fx, 0, delta)).await;
    assert!(
        outcome.contract_error.is_none(),
        "expected the second pool's own refusal, not the executor's: {}",
        blame(&outcome),
    );
    assert_eq!(outcome.revert_kind, Some("Error(string)"));
    assert_no_residue(&outcome);
}

/// One wei low on the last leg: the pool keeps the difference, so what reaches the recipient is
/// one wei under the plan's floor. Nothing is lost and nothing is left behind — the route simply
/// does not happen.
#[tokio::test]
async fn second_leg_asking_low_leaves_the_floor_unmet() {
    let fx = fixture();
    let outcome = fx.run(call_moving(&fx, 1, -1)).await;
    println!("{}", outcome.describe());
    assert_eq!(blame(&outcome), "FinalShortfall");
    assert_no_residue(&outcome);
}

/// §26 NC6: the wallet does not hold the input. The contract reads `balanceOf` and `allowance`
/// itself before the transfer, so this is its refusal, in its own words, with no residue.
#[tokio::test]
async fn insufficient_input_balance_is_refused_before_any_transfer() {
    let fx = Fixture::build(Knobs {
        balance: amount_in() - U256::ONE,
        ..Knobs::standard()
    });
    let outcome = fx.run(fx.standard_call()).await;
    assert_eq!(blame(&outcome), "InsufficientBalance");
    assert_no_residue(&outcome);
    let row = balance(&outcome, WETH, fx.knobs.caller);
    assert_eq!(row.before, fx.knobs.balance, "the word the contract read");
    assert_eq!(row.after, fx.knobs.balance, "and the word it left");
}

/// §26 NC7: the same, with the approval missing instead of the money — a different error,
/// because a wallet that holds but has not approved is a different situation than one that does
/// not hold, and §50 asks the evidence to tell them apart.
#[tokio::test]
async fn insufficient_allowance_is_refused_before_any_transfer() {
    let fx = Fixture::build(Knobs {
        allowance: amount_in() - U256::ONE,
        ..Knobs::standard()
    });
    let outcome = fx.run(fx.standard_call()).await;
    assert_eq!(blame(&outcome), "InsufficientAllowance");
    assert_no_residue(&outcome);
    let key = allowance_key(WETH, fx.knobs.caller, EXECUTOR);
    assert_eq!(
        fx.dump.storage(WETH, key),
        Some(fx.knobs.allowance),
        "the refusal is about the approval the fixture declared, not about a key nobody derived",
    );
}

/// §26 NC3: the caller is not the stored operator.
#[tokio::test]
async fn an_unauthorized_caller_is_refused_by_the_access_control() {
    let fx = Fixture::build(Knobs {
        caller: BYSTANDER,
        deployed_operator: OPERATOR,
        ..Knobs::standard()
    });
    let outcome = fx.run(fx.standard_call()).await;
    assert_eq!(blame(&outcome), "NotOperator");
    assert_no_residue(&outcome);
    assert!(
        fx.dump
            .storage(WETH, balance_key(WETH, BYSTANDER))
            .is_some(),
        "the bystander's balance word is a declared row, so the run reached the contract rather \
         than failing on a hole in the fixture",
    );
}

/// The pool and token allowlists, each read from the deployment's own storage.
#[tokio::test]
async fn an_unlisted_pair_or_token_is_refused_before_anything_moves() {
    for (knobs, expected) in [
        (
            Knobs {
                pair_allowed: false,
                ..Knobs::standard()
            },
            "PairNotAllowed",
        ),
        (
            Knobs {
                token_allowed: false,
                ..Knobs::standard()
            },
            "TokenNotAllowed",
        ),
    ] {
        let fx = Fixture::build(knobs);
        let outcome = fx.run(fx.standard_call()).await;
        assert_eq!(blame(&outcome), expected, "{}", outcome.describe());
        assert_no_residue(&outcome);
    }
}

/// The storage layout is a claim, and a wrong slot silently means "not allowed". Both
/// derivations are proven by moving the *same* fact to a different address and watching the run
/// follow it: the caller that matches the stored operator succeeds even when it is an address the
/// recording never saw, and the pair-allowlist word that the run reads is the one hashed from
/// `slot 1`.
#[tokio::test]
async fn the_layout_claims_are_the_compilers() {
    // operator: slot 0, with the caller moved onto it.
    let bystander_runs = Fixture::build(Knobs {
        caller: BYSTANDER,
        deployed_operator: BYSTANDER,
        ..Knobs::standard()
    });
    let outcome = bystander_runs.run(bystander_runs.standard_call()).await;
    assert!(
        outcome.succeeded(),
        "the stored operator is read from slot 0 and it is the bystander: {}",
        blame(&outcome),
    );
    assert_eq!(
        bystander_runs
            .dump
            .storage(EXECUTOR, EXECUTOR_OPERATOR_SLOT),
        Some(address_word(BYSTANDER)),
    );

    // pairAllowed / tokenAllowed: the mapping words the fixture hashed, read by the run.
    let cleared = Fixture::build(Knobs {
        pair_allowed: false,
        ..Knobs::standard()
    });
    assert_eq!(
        cleared
            .dump
            .storage(EXECUTOR, mapping_slot(POOL_A, EXECUTOR_PAIR_ALLOWED_SLOT),),
        Some(allowed_word(false)),
    );
    assert_eq!(
        blame(&cleared.run(cleared.standard_call()).await),
        "PairNotAllowed"
    );
    let standard = fixture();
    assert_eq!(
        standard
            .dump
            .storage(EXECUTOR, mapping_slot(POOL_B, EXECUTOR_PAIR_ALLOWED_SLOT),),
        Some(allowed_word(true)),
    );

    // balances: the derivation the fixture used is the one the token's own answer agrees with,
    // which is what makes a balance row a fact about this run rather than a guess.
    assert_eq!(
        standard.dump.storage(WETH, balance_key(WETH, POOL_A)),
        Some(pool_balance(&recorded_dump(), WETH, POOL_A)),
    );
}

/// §26 NC12: a zero is not a route. Both of the call's amount fields are checked, and the
/// contract says so before reading a single balance.
#[tokio::test]
async fn zero_amounts_are_refused() {
    let fx = fixture();
    for (label, amount_in, min_final) in [
        ("amount_in", U256::ZERO, fx.route.weth_out_leg2),
        ("min_final_amount", fx.route.amount_in, U256::ZERO),
    ] {
        let outcome = fx
            .run(ExecutorCall::Execute {
                legs: fx.route.legs(),
                input_token: WETH,
                amount_in,
                min_final_amount: min_final,
                recipient: RECIPIENT,
            })
            .await;
        assert_eq!(blame(&outcome), "ZeroAmount", "{label}");
        assert_no_residue(&outcome);
    }
}

/// §40/§41: the amounts are claims, and a claim that does not chain is a planning error caught
/// before anything moves — which is why it is a different error from `HoldingMismatch`, the same
/// disagreement found against the balance the contract actually holds while running.
#[tokio::test]
async fn claims_that_do_not_chain_are_rejected() {
    let fx = fixture();
    let mut legs = fx.route.legs();
    legs[1].amount_in = legs[0].amount_out - U256::ONE;
    let outcome = fx
        .run(ExecutorCall::Execute {
            legs,
            input_token: WETH,
            amount_in: fx.route.amount_in,
            min_final_amount: fx.route.weth_out_leg2,
            recipient: RECIPIENT,
        })
        .await;
    assert_eq!(blame(&outcome), "AmountChainBroken");
    assert_no_residue(&outcome);
}

/// §26 NC4: the tokens do not join end to end; and §17's round-trip rule: a route that ends on a
/// different token than it started is an unhedged position, not an arbitrage. Plus §26 NC11's
/// route-shaped form — a call with no legs is malformed, not empty.
#[tokio::test]
async fn broken_continuity_open_routes_and_an_empty_route_are_rejected() {
    let fx = fixture();

    // Leg 1 is asked to buy the token leg 0 just sold, but the plan names WETH as its input.
    // Both tokens are allowed and the leg is not a self-loop, so this is not `_checkLeg`'s
    // finding — it is the join between neighbours, which is what NC4 is about.
    let mut legs = fx.route.legs();
    legs[1].token_in = WETH;
    legs[1].token_out = MID;
    let outcome = fx
        .run(ExecutorCall::Execute {
            legs,
            input_token: WETH,
            amount_in: fx.route.amount_in,
            min_final_amount: fx.route.weth_out_leg2,
            recipient: RECIPIENT,
        })
        .await;
    assert_eq!(blame(&outcome), "BrokenContinuity");
    assert_no_residue(&outcome);

    // One leg only: WETH in, MID out, and nothing sells the MID.
    let outcome = fx
        .run(ExecutorCall::Execute {
            legs: vec![ExecutorLeg {
                pool: POOL_A,
                token_in: WETH,
                token_out: MID,
                amount_in: fx.route.amount_in,
                amount_out: fx.route.mid_out_leg1,
                min_amount_out: fx.route.mid_out_leg1,
            }],
            input_token: WETH,
            amount_in: fx.route.amount_in,
            min_final_amount: fx.route.weth_out_leg2,
            recipient: RECIPIENT,
        })
        .await;
    assert_eq!(blame(&outcome), "NotRoundTrip");
    assert_no_residue(&outcome);

    let outcome = fx
        .run(ExecutorCall::Execute {
            legs: vec![],
            input_token: WETH,
            amount_in: fx.route.amount_in,
            min_final_amount: fx.route.weth_out_leg2,
            recipient: RECIPIENT,
        })
        .await;
    assert_eq!(blame(&outcome), "NoLegs");
    assert_no_residue(&outcome);
}

/// The exact-equality rule proven from the other side: a deployment that already holds one wei of
/// the route's mid token cannot tell whether the number it is about to spend came from this
/// transaction, so it refuses rather than trading with a balance that is partly someone else's.
/// This is also the residue case the rule exists to prevent.
#[tokio::test]
async fn leftover_dust_in_the_executor_is_a_mismatch() {
    let fx = Fixture::build(Knobs {
        dust: U256::ONE,
        ..Knobs::standard()
    });
    let outcome = fx.run(fx.standard_call()).await;
    println!("{}", outcome.describe());
    assert_eq!(blame(&outcome), "HoldingMismatch");
    assert_no_residue(&outcome);
    let row = balance(&outcome, MID, EXECUTOR);
    assert_eq!(row.before, U256::ONE, "the dust is the declared row");
    assert_eq!(
        row.after,
        U256::ONE,
        "and the run leaves it exactly where it found it"
    );
}

/// The manual lock, planted closed. A deployment whose `_lock` reads anything but 1 refuses every
/// call — the same six lines the contract would use against a reentrant token, proven here
/// against a state fact.
#[tokio::test]
async fn a_locked_deployment_refuses_the_route() {
    let fx = Fixture::build(Knobs {
        lock: plain_slot(2),
        ..Knobs::standard()
    });
    let outcome = fx.run(fx.standard_call()).await;
    assert_eq!(blame(&outcome), "ReentrancyDetected");
    assert_no_residue(&outcome);
    assert_eq!(
        fx.dump.storage(EXECUTOR, EXECUTOR_LOCK_SLOT),
        Some(plain_slot(2)),
        "the lock word the fixture wrote is the word the contract read",
    );
    // The positive half: with the same fixture and the lock at 1, the route runs, so the
    // refusal above is about the lock and not about anything else in the state.
    let open = fixture();
    assert!(open.run(open.standard_call()).await.succeeded());
}

// ---------------------------------------------------------------------------
// §54 item 12: the withdrawal door, and who may open it
// ---------------------------------------------------------------------------

/// The mid-token balance the deployment is made to already hold. A successful route leaves no
/// residue by design, so the only balance `withdraw` has to work with in this fixture is the
/// row [`Knobs::dust`] writes — scaffolding, as §29 requires a withdrawal's evidence to be.
fn dust() -> U256 {
    U256::from(7_000_000_000u64)
}

/// `withdraw` is `onlyOperator`, checked against the same stored word `execute` is, and a
/// bystander asking for the deployment's balance is refused before the token is asked for
/// anything. The two rows a successful withdrawal would move read the same on both sides.
#[tokio::test]
async fn a_withdrawal_from_anyone_but_the_operator_is_refused() {
    let fx = Fixture::build(Knobs {
        caller: BYSTANDER,
        deployed_operator: OPERATOR,
        dust: dust(),
        ..Knobs::standard()
    });
    let outcome = fx
        .run(ExecutorCall::Withdraw {
            token: MID,
            to: RECIPIENT,
            amount: dust(),
        })
        .await;
    println!("{}", outcome.describe());
    assert_eq!(blame(&outcome), "NotOperator");
    assert_no_residue(&outcome);
    assert_eq!(
        (
            balance(&outcome, MID, RECIPIENT).before,
            balance(&outcome, MID, RECIPIENT).after,
        ),
        (U256::ZERO, U256::ZERO),
        "the recipient was offered the balance and got none of it",
    );
    assert_eq!(
        balance(&outcome, MID, EXECUTOR).after,
        dust(),
        "what the bystander asked for is still where it was",
    );
}

/// The door works for the operator it is bound to, and it moves exactly what the contract
/// genuinely holds. The proof is the pair of balance rows and the two storage words behind
/// them: the event is not evidence, and neither is the call's own answer — a void function
/// answers with nothing, which is why `delivered` is `None` here rather than a zero.
#[tokio::test]
async fn the_operator_withdraws_what_the_contract_holds() {
    let fx = Fixture::build(Knobs {
        dust: dust(),
        ..Knobs::standard()
    });
    let outcome = fx
        .run(ExecutorCall::Withdraw {
            token: MID,
            to: RECIPIENT,
            amount: dust(),
        })
        .await;
    println!("{}", outcome.describe());
    assert!(outcome.succeeded(), "{}", blame(&outcome));
    assert_eq!(
        outcome.delivered, None,
        "withdraw() declares no return value, so the run reports none"
    );
    assert_eq!(
        (
            balance(&outcome, MID, EXECUTOR).before,
            balance(&outcome, MID, EXECUTOR).after,
        ),
        (dust(), U256::ZERO),
        "the deployment ends holding nothing of the token it paid out"
    );
    assert_eq!(
        (
            balance(&outcome, MID, RECIPIENT).before,
            balance(&outcome, MID, RECIPIENT).after,
        ),
        (U256::ZERO, dust()),
        "and the recipient ends holding exactly the ask"
    );

    // The same difference one level down, at the words. Exactly two of MID's slots moved, by
    // equal and opposite amounts — a transfer of what was already there, which is what makes
    // this "cannot mint or create" rather than a sentence about the contract's source.
    let changed = outcome.state_changes.storage_of(MID);
    assert_eq!(
        changed.len(),
        2,
        "expected the deployment's slot and the recipient's and nothing else: {changed:?}",
    );
    for (slot, before, after) in [
        (balance_key(MID, EXECUTOR), dust(), U256::ZERO),
        (balance_key(MID, RECIPIENT), U256::ZERO, dust()),
    ] {
        let change = changed
            .iter()
            .find(|word| word.slot == slot)
            .unwrap_or_else(|| panic!("no word changed at slot {slot}"));
        assert_eq!((change.before, change.after), (before, after));
    }
    for untouched in [EXECUTOR, POOL_A, POOL_B, WETH] {
        assert!(
            outcome.state_changes.storage_of(untouched).is_empty(),
            "{untouched} carries changed words after a withdrawal that has no business there",
        );
    }
    // The lock is written on the way in and reset on the way out, so the diff shows nothing:
    // the word is checked to be the one the fixture declared, not merely unchanged in the diff.
    assert_eq!(
        fx.dump.storage(EXECUTOR, EXECUTOR_LOCK_SLOT),
        Some(plain_slot(1)),
        "the deployment started unlocked",
    );
}

/// Two asks the door refuses from the operator it is bound to: nothing to move, and more than
/// the contract holds. Both are guards over the balances rather than over the caller, so each
/// is proven by the numbers staying where they were — `withdraw` has no route to half-finish.
#[tokio::test]
async fn a_withdrawal_of_nothing_or_of_more_than_is_held_is_refused() {
    for (label, amount, expected) in [
        ("zero", U256::ZERO, "ZeroAmount"),
        (
            "one wei over the balance",
            dust() + U256::ONE,
            "InsufficientBalance",
        ),
    ] {
        let fx = Fixture::build(Knobs {
            dust: dust(),
            ..Knobs::standard()
        });
        let outcome = fx
            .run(ExecutorCall::Withdraw {
                token: MID,
                to: RECIPIENT,
                amount,
            })
            .await;
        assert_eq!(blame(&outcome), expected, "{label}: {}", outcome.describe(),);
        assert_no_residue(&outcome);
        assert_eq!(balance(&outcome, MID, EXECUTOR).after, dust());
        assert_eq!(balance(&outcome, MID, RECIPIENT).after, U256::ZERO);
    }
}

// ---------------------------------------------------------------------------
// Refusals: the harness will not answer these questions at all
// ---------------------------------------------------------------------------

/// §26 NC1 and NC2, and §20's rule that the pin is read from the provider rather than from the
/// request. All three fire before the EVM is built, so no execution happens and no result is
/// reported.
#[tokio::test]
async fn a_run_that_lies_about_where_it_is_is_refused() {
    let fx = fixture();

    // NC1: the wrong chain.
    let mut spec = fx.spec(fx.standard_call());
    spec.chain_id = ChainId(1);
    let error = fx
        .try_run_spec(&spec)
        .await
        .expect_err("a run claiming chain 1 against a chain 91342 recording must not answer");
    match error {
        SimulationError::ChainMismatch { expected, found } => {
            assert_eq!(expected, ChainId(1));
            assert_eq!(found, CHAIN);
        }
        other => panic!("expected a chain refusal, got {other:?}"),
    }

    // NC2: a state source that is not the one the plan was priced on.
    let mut spec = fx.spec(fx.standard_call());
    spec.state_source = format!("{} (a source that does not exist)", FIXTURE_LABEL);
    let error = fx
        .try_run_spec(&spec)
        .await
        .expect_err("a plan priced on other state must be refused");
    match error {
        SimulationError::StateMismatch { pinned, reason, .. } => {
            assert_eq!(pinned.number.0, BLOCK, "the pin comes from the provider");
            assert!(
                reason.contains("was built against state source"),
                "the refusal names both sides: {reason}",
            );
        }
        other => panic!("expected a state refusal, got {other:?}"),
    }
}

/// §60: an empty `eth_getCode` is a refusal, never a bytecode to be filled in. Aimed at the
/// executor this matters more than in the plan path — a call to a codeless account would
/// otherwise "succeed" as a plain value transfer and report a route that never ran.
#[tokio::test]
async fn an_executor_with_no_bytecode_is_a_refusal_not_a_no_op() {
    let fx = fixture();
    let mut spec = fx.spec(fx.standard_call());
    spec.executor = BYSTANDER;
    let error = fx
        .try_run_spec(&spec)
        .await
        .expect_err("a call to an address with no code must not be reported as a run");
    match error {
        SimulationError::MissingCode { address, block } => {
            assert_eq!(address, BYSTANDER);
            assert_eq!(block.0, BLOCK);
        }
        other => panic!("expected a missing-code refusal, got {other:?}"),
    }

    // The mirror image: a deployment the fixture never wrote. This is a *different* refusal,
    // and §71 wants it kept different — the address is not in the state source at all, so the
    // provider declines to answer the code question rather than answering "no code". Both stop
    // the run before the EVM executes, which is the claim this test is really making.
    let undeployed = Fixture::build(Knobs {
        deploy_executor: false,
        ..Knobs::standard()
    });
    let error = undeployed
        .try_run(undeployed.standard_call())
        .await
        .expect_err("without the executor's bytecode there is nothing to run");
    match &error {
        SimulationError::MissingState(reason) => {
            assert!(
                reason.contains("code") && reason.contains(&format!("{EXECUTOR}")),
                "the refusal names the address whose code is unanswered: {reason}"
            );
        }
        other => panic!("expected a state refusal about the executor's code, got {other:?}"),
    }

    // And an operator that is a contract rather than a wallet: §58's one permitted shape.
    let mut spec = fx.spec(fx.standard_call());
    spec.operator = EXECUTOR;
    let error = fx
        .try_run_spec(&spec)
        .await
        .expect_err("the operator has to be the signing account, not a contract");
    assert!(
        matches!(&error, SimulationError::UnsupportedTransaction(reason) if reason.contains("has bytecode")),
        "{error:?}"
    );
}

/// The contract's `token == address(0)` guard is a line `withdraw` checks, and no run here
/// reaches it: the state source has never carried an account at the zero address, so §60's
/// rule — an unanswered code is a refusal, never a bytecode of length zero — stops the run one
/// step before the contract is called. This is the §54 item the harness answers with a refusal
/// rather than an outcome, and the reason is stated instead of worked around: declaring an
/// account for `address(0)` would be the fixture inventing state to make a test pass.
#[tokio::test]
async fn a_withdrawal_of_the_zero_token_never_reaches_the_contract() {
    let fx = fixture();
    let error = fx
        .try_run(ExecutorCall::Withdraw {
            token: Address::ZERO,
            to: RECIPIENT,
            amount: U256::ONE,
        })
        .await
        .expect_err("nothing in the recorded state says what code the zero address holds");
    match &error {
        SimulationError::MissingState(reason) => {
            assert!(
                reason.contains("code") && reason.contains(&format!("{}", Address::ZERO)),
                "the refusal names the address whose code is unanswered: {reason}"
            );
        }
        other => panic!("expected a state refusal about the zero token, got {other:?}"),
    }
}

/// §59's discipline in file form: the runtime code the fixture deploys is the committed artifact,
/// byte for byte, so a simulation and a deployment cannot be about different contracts.
#[test]
fn the_deployed_code_is_the_committed_artifact() {
    let code = executor_runtime_code();
    let text = std::fs::read_to_string(
        workspace_root().join("contracts/artifacts/ArbitrageExecutor.bin-runtime"),
    )
    .expect("the committed runtime bytecode");
    assert_eq!(code.len() * 2, text.trim().len(), "hex text ↔ bytes");
    assert!(code.len() < 24_576, "EIP-170: {} bytes", code.len());
    let fx = fixture();
    assert_eq!(
        fx.dump
            .account(EXECUTOR)
            .expect("the fixture deploys the executor")
            .code,
        format!("{code:?}"),
        "the account the run reads holds exactly the committed bytecode",
    );
    // The wallets the run speaks about really are codeless, which is what makes them wallets.
    for wallet in [RECIPIENT, BYSTANDER] {
        assert_eq!(
            fx.dump.account(wallet).expect("declared").code,
            "0x",
            "{wallet} was declared with bytecode",
        );
    }
}

/// §49's D1/D2 gate at the outcome layer: the same call and the same state, twice, produce
/// byte-identical JSON. A result that moves between two runs is not evidence.
#[tokio::test]
async fn two_runs_of_the_same_call_are_byte_identical() {
    let fx = fixture();
    let first = fx.run(fx.standard_call()).await;
    let second = fx.run(fx.standard_call()).await;
    let a = serde_json::to_vec_pretty(&first).expect("an outcome serializes");
    let b = serde_json::to_vec_pretty(&second).expect("an outcome serializes");
    assert_eq!(a, b, "the same run produced different bytes");
    println!(
        "identical: {} bytes, gas {}, delivered {}",
        a.len(),
        first.gas_used,
        first.delivered.unwrap_or(U256::ZERO),
    );

    // A refusal has to be as reproducible as a success, or the negative controls are anecdotes.
    let low = fx.run(call_moving(&fx, 1, -1)).await;
    let again = fx.run(call_moving(&fx, 1, -1)).await;
    assert_eq!(
        serde_json::to_vec_pretty(&low).unwrap(),
        serde_json::to_vec_pretty(&again).unwrap(),
    );
}

/// The route-length bound is the contract's own constant, and the run reaches the contract rather
/// than the EVM refusing to build the transaction: five legs is a plan shape, not an invalid
/// payload.
#[tokio::test]
async fn a_route_longer_than_the_contracts_own_bound_is_rejected() {
    let fx = fixture();
    let base = fx.route.legs();
    let mut legs = Vec::new();
    while legs.len() <= MAX_LEGS as usize {
        legs.extend(base.iter().cloned());
    }
    let outcome = fx
        .run(ExecutorCall::Execute {
            legs,
            input_token: WETH,
            amount_in: fx.route.amount_in,
            min_final_amount: fx.route.weth_out_leg2,
            recipient: RECIPIENT,
        })
        .await;
    assert_eq!(blame(&outcome), "TooManyLegs");
    assert_no_residue(&outcome);
}

#[tokio::test]
async fn a_limit_built_from_the_burn_starves_the_second_pair_and_the_proved_limit_does_not() {
    // §13's gas policy resolves the send limit out of what the simulation measured, and this
    // control says which measurement it has to be. EIP-150 gives a nested call 63/64 of the gas
    // its caller has left, so a limit equal to the gas the route *burned* is not a limit the
    // route can meet: the deepest frame — here the second pair paying out WETH — is handed less
    // than the transfer costs and its `_safeTransfer` fails, which the pair reports as
    // `UniswapV2: TRANSFER_FAILED`. This is the shape §57's first real attempt came back with
    // (229,302 gas burned against a 249,302 limit), and the run below reproduces it without a
    // network: REVM and the node agree on both the burn and the failure, so the milestone's
    // reconciliation has no simulation-versus-execution divergence to explain.
    let fx = fixture();
    let wide = fx.run(fx.standard_call()).await;
    assert!(wide.succeeded(), "{}", blame(&wide));
    assert!(
        wide.gas_used < wide.gas_limit,
        "the proving run needs headroom of its own to prove anything: used {}, limit {}",
        wide.gas_used,
        wide.gas_limit,
    );

    // The margin §13's default policy carries, spelled out here so the control names the two
    // numbers that produced the failing limit rather than a magic value.
    const MARGIN: u64 = 20_000;
    let mut tight = fx.spec(fx.standard_call());
    tight.gas_limit = wide.gas_used + MARGIN;
    let starved = fx.run_spec(&tight).await;
    assert!(
        !starved.succeeded(),
        "a limit of burn+{MARGIN} ({}) completed — the premise of this control is gone, and §13 \
         would have to be re-measured rather than quietly kept",
        tight.gas_limit,
    );
    assert_eq!(
        starved.delivered, None,
        "the starved run still paid someone, so the shortfall was not what stopped it"
    );

    // The same call at the limit the wide run was actually proved under, plus that same margin,
    // completes: monotone in the limit, which is exactly why the proved limit is the number to
    // carry forward and the burn is not.
    let mut roomy = fx.spec(fx.standard_call());
    roomy.gas_limit = wide.gas_limit + MARGIN;
    let funded = fx.run_spec(&roomy).await;
    assert!(
        funded.succeeded(),
        "more gas than a run that already succeeded cannot be what breaks it: {}",
        blame(&funded)
    );
    assert_eq!(
        funded.gas_used, wide.gas_used,
        "the extra allowance is returned, not spent — a larger limit costs the route nothing"
    );
    assert_eq!(
        funded.delivered, wide.delivered,
        "and it changes nothing about what the route pays out"
    );
}
