//! §19–§25 on the real machine: the M11 multi-hop candidate asked of M10's executor,
//! against deployed bytecode, at a pinned header.
//!
//! ```text
//! cargo test -p evm-simulation --test multihop_revm -- --test-threads=1
//! ```
//!
//! §19 forbids a second simulator, and this file is where that ban is checked rather than
//! merely stated. `tests/multihop_adapter.rs` proves what the adapter *puts in the request*
//! and *reads back* by handing it outcomes it describes; that is the only honest way to ask
//! about an out-of-gas or halted run. Here nothing is handed over: the request comes from
//! [`evm_simulation::executor_run`], the run comes from
//! [`evm_simulation::executor::run`] — the same function M10's suite calls — and the state
//! comes from the M7 recording plus the M10 fixture's declared rows. So the strongest claim
//! this file can make is also its first test: the bytes the M11 adapter produces for the
//! recorded route are the bytes M10's own fixture produces for it, and the two runs agree in
//! every measured field. If the adapter were a second opinion, that equality is exactly what
//! would fail.
//!
//! ## Where each number comes from
//!
//! The table is in `mod multihop_recorded`, since all three files that run this market read
//! their numbers from the same places. What this file adds to that is the comparison itself.
//!
//! The §4 distinction is the point of the whole file: `priced_output` and `final_amount` are
//! read from different places and compared, never derived from each other. The header, the
//! chain, the pools, the tokens and their reserves are GIWA's; the executor deployment, the
//! operator's balance and the approvals are declared scaffolding — which is why the state
//! source the run carries is asserted to start with `CONTROLLED_FIXTURE` and this file
//! publishes no real-market verdict (§41).
//!
//! ## One record, three files
//!
//! The market, the route, the candidate, the run spec and §26–§30's policy all live in
//! `mod multihop_recorded`, which this file, `tests/multihop_negative_controls.rs` and
//! `tests/multihop_determinism.rs` each include. That is not tidiness: §46 asks a planted
//! failure of *this* market and §47 asks the same market twice, and an answer only means
//! something if the untouched record behind it is one object rather than three copies that
//! could drift. What differs between the files is which field a test moves, never how the
//! record is assembled.
//!
//! Nothing here signs, broadcasts, or holds a key.

use alloy_primitives::{keccak256, U256};
use evm_core::BlockNumber;

use evm_opportunity::Gross;
use evm_protocol::ExecutorCall;
use evm_simulation::{legs, MultiHopBuildError, SimulatedOpportunity, SimulationStatus};

mod executor_state;
mod multihop_market;
mod multihop_recorded;

use evm_execution::{executable_plan, plan_from_simulation};
use evm_risk::RiskCheck;
use executor_state::{
    amount_in, pool_balance, BLOCK, CHAIN, ENDOWMENT, EXECUTOR, FIXTURE_LABEL, GAS_LIMIT, MID,
    POOL_A, POOL_B, RECIPIENT, WETH,
};
use multihop_market::with_quote;
use multihop_recorded::{
    ask, assert_no_residue, blame, candidate, config, file, fixture, fresh_recorded,
    recorded_acceptance, recorded_binding, recorded_context, recorded_facts, recorded_policy,
    request, run_ok, simulate,
};

// ---------------------------------------------------------------------------
// §19/§20: the recorded market prices the recorded route, and the legs agree
// ---------------------------------------------------------------------------

/// The pricing stage reads the same recording M10 read, by a different road: the reserves
/// come out of the tokens' balance words, go through the graph builder and `price()`'s
/// per-hop integer arithmetic, and have to land on the two numbers `Route::from_recorded`
/// derived directly from those same words. Two independent derivations of one route, and no
/// hand-typed amount anywhere in the comparison.
#[test]
fn the_recorded_graph_prices_the_route_the_recording_carries() {
    let fx = fixture();
    let priced = candidate(&fx.dump);
    let quote = &priced.search.quote;

    assert_eq!(
        quote.hops.len(),
        2,
        "the recorded route is a two-hop round trip"
    );
    assert_eq!(
        quote.hops[0].amount_out, fx.route.mid_out_leg1,
        "the first hop's MID output as the graph prices it vs as the recorded reserves say"
    );
    assert_eq!(
        quote.hops[1].amount_out, fx.route.weth_out_leg2,
        "and the second hop's WETH output"
    );
    assert_eq!(priced.search.best_input, amount_in());
    assert_eq!(
        priced.search.best_output, fx.route.weth_out_leg2,
        "the search's winner is the quote's own answer at the staked amount"
    );
    assert_eq!(
        priced.search.gross,
        Gross::between(amount_in(), fx.route.weth_out_leg2),
        "the recorded pair is a gain at this amount, which is why the fixture stakes it"
    );
    assert!(
        priced.search.gross.is_gain(),
        "and it is stated as a gain, not as a number that happens to be positive"
    );

    // §23: the block and the chain are the route's, because the snapshot's are the
    // recording's. A candidate that lost this pairing would let a GIWA route be asked
    // against another chain's state.
    assert_eq!(priced.chain_id, CHAIN);
    assert_eq!(priced.target_block, BlockNumber(BLOCK));
    assert_eq!(priced.route.chain_id(), CHAIN);
    assert_eq!(priced.route.target_block(), BlockNumber(BLOCK));
}

/// §20/§21 on the real market: the legs the adapter builds are structurally the legs M10's
/// fixture built by hand from the same reserves — same pools, same direction, same amounts,
/// same floors. This is the equality §19 asks for before any EVM runs: one route, one leg
/// list.
#[test]
fn the_adapter_builds_the_fixtures_own_legs() {
    let fx = fixture();
    let priced = candidate(&fx.dump);
    let built = legs(&priced).expect("the recorded candidate builds");
    assert_eq!(
        built,
        fx.route.legs(),
        "the legs from the quote are the legs from the recording"
    );
    for leg in &built {
        assert_eq!(
            leg.min_amount_out, leg.amount_out,
            "{leg:?} — each floor is its own quoted output"
        );
    }
    assert_eq!(built[1].amount_in, built[0].amount_out, "the chain chains");
}

// ---------------------------------------------------------------------------
// §19: the request is M10's request
// ---------------------------------------------------------------------------

/// The strongest form of "reuse M10, do not write another simulator": for the recorded route
/// the adapter's call encodes to the same bytes M10's fixture call encodes to, and decoding
/// them back through the protocol crate returns the same structured call. One codec, one
/// request shape, no parallel implementation of either.
#[test]
fn the_request_is_byte_for_byte_the_m10_request() {
    let fx = fixture();
    let priced = candidate(&fx.dump);
    let spec = request(&fx, &priced);

    let mine = spec.call.encode();
    let theirs = fx.standard_call().encode();
    assert_eq!(
        mine, theirs,
        "the M11 adapter and the M10 fixture ask the contract for different bytes"
    );
    assert_eq!(spec.chain_id, priced.chain_id);
    assert_eq!(spec.priced_at, priced.target_block);
    assert_eq!(spec.state_source, fx.source);
    assert_eq!(spec.gas_limit, GAS_LIMIT);
    assert_eq!(spec.endowment, Some(ENDOWMENT));

    let decoded = evm_protocol::decode_calldata(&mine).expect("the protocol decoder reads it");
    assert_eq!(decoded, spec.call, "and reads it back as the same call");
}

// ---------------------------------------------------------------------------
// §23/§24/§25: a real run
// ---------------------------------------------------------------------------

/// The recorded route, priced by `price()`, asked of the contract, executed by REVM on the
/// pair and token bytecode that is deployed on GIWA. §24's four numbers are read from four
/// different places and all four are asserted.
#[tokio::test]
async fn a_real_run_delivers_what_the_pricing_claimed() {
    let fx = fixture();
    let sim = simulate(&fx).await;
    println!("{}", sim.outcome.describe());

    let delivered = fx.route.weth_out_leg2;
    assert_eq!(
        sim.status,
        SimulationStatus::Delivered {
            final_amount: delivered,
        },
        "the EVM's ending, in §25's words"
    );
    assert_eq!(sim.final_amount, Some(delivered));
    assert_eq!(
        sim.gross,
        Some(Gross::Gain(delivered - amount_in())),
        "profit in the input token's units, never netted against gas (§24)"
    );
    assert_eq!(sim.matches_pricing(), Some(true));
    assert_eq!(
        sim.priced_against_delivered(),
        Some(Gross::Even),
        "the quote and the delivery differ by nothing at this amount"
    );
    assert_eq!(sim.input_amount, amount_in());
    assert_eq!(sim.min_final_output, delivered);

    // §23's header: the run pins the recorded block, and the pin is the provider's answer,
    // not a field the caller could set.
    assert_eq!(sim.chain_id, CHAIN);
    assert_eq!(sim.simulation_block.number, BlockNumber(BLOCK));

    // The money actually moved, at the level of the token's books.
    let row = sim
        .outcome
        .balance_row(WETH, RECIPIENT)
        .expect("the recipient is watched");
    assert_eq!(
        row.after - row.before,
        delivered,
        "the recipient received what the call returned"
    );
    assert!(
        sim.market_moved(),
        "a run that traded has to leave the reserves different — the check is two-sided"
    );

    // §29/§41: the state this verdict is about is labelled, and only labelled, scaffolding.
    assert!(
        sim.outcome.state_source.starts_with(FIXTURE_LABEL),
        "a fixture run must not read as a market observation: {}",
        sim.outcome.state_source
    );
}

/// §19's proof by measurement rather than by prose: the same state asked through M10's own
/// fixture call and through the M11 adapter returns the same delivery, the same gas, and the
/// same state diff. A second simulator would drift in one of these three.
#[tokio::test]
async fn the_two_paths_to_the_evm_agree_on_every_measured_number() {
    let fx = fixture();
    let sim = simulate(&fx).await;
    let direct = fx.run(fx.standard_call()).await;

    assert_eq!(sim.outcome.status, direct.status);
    assert_eq!(sim.outcome.delivered, direct.delivered);
    assert_eq!(
        sim.outcome.gas_used, direct.gas_used,
        "the same bytes cost the same gas"
    );
    assert_eq!(sim.outcome.charge, direct.charge);
    assert_eq!(
        sim.outcome.reserves, direct.reserves,
        "and left the same reserves behind"
    );
    assert_eq!(sim.outcome.balances, direct.balances);
    assert_eq!(sim.calldata(), direct.calldata);
}

/// §23's identity: chain, block and a hash over the canonical text, with the header hash and
/// the route's own edges inside that text. Quoted from the run, so an execution claim in §30
/// has a number to point at.
#[tokio::test]
async fn the_identity_of_a_real_run_names_the_chain_the_header_and_the_route() {
    let fx = fixture();
    let sim = simulate(&fx).await;

    let identity = sim.identity();
    assert!(
        identity.starts_with(&format!("m11-sim-{}-{BLOCK}-", CHAIN.0)),
        "{identity}"
    );
    assert_eq!(
        identity,
        format!("m11-sim-{}-{BLOCK}-{}", sim.chain_id.0, sim.identity_hash()),
        "the spelled identity is the hash over the spelled text"
    );

    let text = sim.canonical_text();
    assert!(text.contains(&format!("block_hash={}", sim.simulation_block.hash)));
    assert!(text.contains(&format!("block={BLOCK}")));
    assert!(text.contains(&format!("chain_id={}", CHAIN.0)));
    assert!(text.contains("status=delivered"));
    assert!(
        text.contains(&format!("{POOL_A:#x}/{WETH:#x}>{MID:#x}")),
        "the route is named by its recorded edges: {text}"
    );
    assert!(
        !text.contains(&format!("{POOL_B:#x}/{WETH:#x}>{MID:#x}")),
        "and in the direction it was walked, not the other one: {text}"
    );
    assert_eq!(
        sim.calldata_hash(),
        keccak256(sim.calldata().as_ref()),
        "and the hash the text carries is a hash of the bytes that were asked"
    );
}

/// §47's determinism on the real machine: two runs of the same candidate over the same
/// recording produce byte-identical canonical text and one and the same identity. §46's
/// double-run gate depends on this being true of a *recorded* market, not only of a
/// hand-written outcome.
#[tokio::test]
async fn two_runs_of_the_recorded_route_are_the_same_run() {
    let fx = fixture();
    let first = simulate(&fx).await;
    let second = simulate(&fx).await;

    assert_eq!(first.canonical_text(), second.canonical_text());
    assert_eq!(first.identity(), second.identity());
    assert_eq!(first.identity_hash(), second.identity_hash());
    assert_eq!(first.outcome.state_changes, second.outcome.state_changes);
    assert_eq!(first.gas_used, second.gas_used);
}

// ---------------------------------------------------------------------------
// §25/§45: the endings that pay nothing
// ---------------------------------------------------------------------------

/// A guard above what the route can deliver is a bug at the call site, not a fact about the
/// market, so it is refused while building — no bytes, no EVM, no revert to explain.
#[tokio::test]
async fn an_unreachable_guard_is_refused_before_the_evm_is_asked() {
    let fx = fixture();
    let priced = candidate(&fx.dump);
    let error =
        evm_simulation::executor_run(&priced, &config(&fx), priced.search.best_output + U256::ONE)
            .expect_err("a floor nobody can meet is refused");
    assert_eq!(
        error,
        MultiHopBuildError::UnreachableGuard {
            min_final_output: priced.search.best_output + U256::ONE,
            quoted: priced.search.best_output,
        }
    );
    assert!(
        !error.to_string().is_empty(),
        "and the refusal says which numbers disagreed"
    );
}

/// The contract does accept a guard one wei under its priced output — it is the plan's own
/// ask, and §20's floor rule keeps it exactly at the quote. This run is here to show the
/// refusal above is about *reachability*, not about floors being ignored: the tight guard is
/// met on real bytecode.
#[tokio::test]
async fn the_guard_at_the_priced_output_is_met_on_real_bytecode() {
    let fx = fixture();
    let priced = candidate(&fx.dump);
    let spec = request(&fx, &priced);
    let guard = match &spec.call {
        ExecutorCall::Execute {
            min_final_amount, ..
        } => *min_final_amount,
        other => panic!("{} is not an execute call", other.signature()),
    };
    assert_eq!(guard, priced.search.best_output);
    let outcome = run_ok(&fx, &spec).await;
    assert_eq!(outcome.delivered, Some(guard), "{}", blame(&outcome));
}

/// §45's "the plan is one wei off the market", run for real: the pool is asked to pay less
/// than its own quote and does, so what reaches the recipient falls short of the guard the
/// same call carries. The contract names the invariant that closed the transaction, and §25
/// keeps the run out of the profit columns entirely.
#[tokio::test]
async fn a_one_wei_under_plan_reverts_and_reports_no_profit() {
    let fx = fixture();
    let mut priced = candidate(&fx.dump);
    priced.search.quote.hops[1].amount_out -= U256::ONE;
    let spec = request(&fx, &priced);
    let sim = file(&priced, &spec, run_ok(&fx, &spec).await);
    println!("{}", sim.outcome.describe());

    let name = match &sim.status {
        SimulationStatus::Reverted {
            contract_error,
            reason,
            ..
        } => {
            assert_eq!(
                contract_error.as_deref(),
                Some("FinalShortfall"),
                "the contract's own words, not a bare revert: {reason}"
            );
            contract_error.clone().expect("named")
        }
        other => panic!("expected a revert, got {other:?}"),
    };
    assert_eq!(sim.status.name(), "reverted");
    assert_eq!(sim.outcome.revert_kind, Some(name.as_str()));
    assert_eq!(
        sim.final_amount, None,
        "a run that paid nothing has no amount"
    );
    assert_eq!(sim.gross, None, "and no profit, not even a zero one");
    assert_eq!(
        sim.matches_pricing(),
        None,
        "there is no delivery to compare with the pricing"
    );
    let text = sim.canonical_text();
    assert!(text.contains("final_amount=none"), "{text}");
    assert!(text.contains("gross=none"), "{text}");
    assert_no_residue(&sim.outcome);
}

/// A run that is not this candidate's run cannot be filed under it — §30 turns a simulation
/// into an execution claim, so the check has to be positional and on real legs, not on a
/// hand-written comparison. The mutated plan is executed, so the refusal is about a run that
/// genuinely happened and still does not belong.
#[tokio::test]
async fn a_real_run_that_is_not_the_candidates_plan_is_refused() {
    let fx = fixture();
    let priced = candidate(&fx.dump);
    let mut spec = request(&fx, &priced);
    let moved = match &spec.call {
        ExecutorCall::Execute {
            legs: _,
            input_token,
            amount_in,
            min_final_amount,
            recipient,
        } => ExecutorCall::Execute {
            legs: fx.route.legs_moved(1, -1),
            input_token: *input_token,
            amount_in: *amount_in,
            min_final_amount: *min_final_amount,
            recipient: *recipient,
        },
        other => panic!("{} is not an execute call", other.signature()),
    };
    spec.call = moved;
    let outcome = run_ok(&fx, &spec).await;
    let error = SimulatedOpportunity::new(&priced, &spec, outcome)
        .expect_err("a different plan is a different run");
    assert_eq!(error, MultiHopBuildError::RunLegMismatch { index: 1 });
    assert!(
        error.to_string().contains("leg 1"),
        "and it names the leg: {error}"
    );
}

/// §23: the header a simulation is filed under is the one the state was actually loaded
/// from, and it comes back through the provider rather than through the request. The plan's
/// own claim (`run.priced_at`) and the pin (`simulation_block`) are two different fields that
/// happen to agree here — which is what makes a stale plan detectable instead of merely
/// unstated.
#[tokio::test]
async fn the_pin_is_the_providers_and_agrees_with_the_block_the_plan_claimed() {
    let fx = fixture();
    let sim = simulate(&fx).await;

    let recorded = fx
        .dump
        .pin()
        .expect("the recording names the header its state was read from");
    assert_eq!(
        sim.simulation_block, recorded,
        "the identity's header is the recorded one, read from the dump's own field"
    );
    assert_eq!(sim.simulation_block.number, BlockNumber(BLOCK));
    assert_eq!(sim.run.priced_at, sim.candidate.target_block);
    assert_eq!(sim.run.priced_at, sim.simulation_block.number);
}

/// §71's other half, on the real path: a request that names state the provider does not serve
/// is refused by the harness before the EVM starts. That is a different fact from the
/// contract rejecting a route, and it produces no outcome at all — so there is nothing for
/// `SimulatedOpportunity` to file, which is why the refusal is asserted as an error and not
/// as a status.
#[tokio::test]
async fn a_request_naming_state_the_provider_does_not_serve_is_refused() {
    let fx = fixture();
    let priced = candidate(&fx.dump);
    let mut spec = request(&fx, &priced);
    spec.state_source =
        "M11_WRONG_SOURCE: a label the provider this request is asked of does not carry"
            .to_string();
    let error = ask(&fx, &spec)
        .await
        .expect_err("a request about state the provider is not serving must not answer");
    assert!(
        error.contains("state mismatch"),
        "the harness refused in its own words: {error}"
    );
}

/// §19's prohibition stated as a measurement: the multi-hop stage brings no state of its own
/// into the run. Every reserve the graph was built from is a word the recording already
/// answers — `pool_balance` panics rather than defaulting, so reaching this line is the proof
/// — and none of the fixture's declared rows names one of the two pools. The market the
/// pricing read is the market the archive node served; only the deployment and the funding
/// are ours (§29).
#[tokio::test]
async fn the_pricing_reads_the_recording_and_adds_no_state() {
    let fx = fixture();
    let priced = candidate(&fx.dump);
    for (pool, token) in [(POOL_A, MID), (POOL_A, WETH), (POOL_B, MID), (POOL_B, WETH)] {
        let word = pool_balance(&fx.dump, token, pool);
        assert!(
            !word.is_zero(),
            "{pool} holds no recorded {token}, so the graph would have been built on an \
             invented reserve"
        );
    }
    for row in &fx.additions {
        assert_ne!(
            row.contract, POOL_A,
            "a declared row edits pool A's state: {row:?}"
        );
        assert_ne!(
            row.contract, POOL_B,
            "a declared row edits pool B's state: {row:?}"
        );
    }

    let sim = simulate(&fx).await;
    assert_eq!(sim.outcome.state_source, fx.source);
    assert_eq!(
        priced.search.quote.fees(),
        vec![multihop_market::FEE; 2],
        "both hops were priced at the fee the recording's pools pay — and the runs above are \
         what prove that fee, since the contract asks the pair for exactly these numbers"
    );
    assert!(
        !sim.outcome.state_changes.slots.is_empty(),
        "a delivered run leaves words different, and that diff is the EVM's, not the \
         pricing's"
    );
}

// ---------------------------------------------------------------------------
// §26–§30: the recorded run judged, and bound to a plan
// ---------------------------------------------------------------------------

/// §26/§27 over the real recording: the run M10's own executor made on the recorded state is
/// accepted, and the figures an `Accept` carries are the EVM's measurements rather than the
/// pricing's claims. This is the first place in M11 where the gross figure and the gas bill are
/// both real numbers: the recorded header prices its own gas, so unlike the hand-written fixture
/// the bill is `Some`, and it is the same `Some` the record already held.
#[tokio::test]
async fn the_risk_policy_accepts_the_recorded_run() {
    let fx = fixture();
    let simulated = simulate(&fx).await;
    let figures = recorded_acceptance(&simulated);

    assert_eq!(figures.input_amount, simulated.input_amount);
    assert_eq!(
        figures.input_amount,
        amount_in(),
        "the stake the fixture sets"
    );
    assert_eq!(
        figures.delivered,
        simulated.final_amount.expect("a delivery")
    );
    assert_eq!(
        figures.gross_profit,
        simulated
            .gross
            .expect("a delivery carries a gross")
            .gain()
            .expect("the recorded pair gains"),
    );
    assert_eq!(figures.gas_used, simulated.gas_used);
    assert_eq!(
        figures.gas_charge_wei,
        simulated.gas_charge.wei(),
        "the bill is quoted, not recomputed"
    );
    assert!(
        figures.gas_charge_wei.is_some(),
        "the recorded header carries a base fee, so this run has a price for its gas — the \
         opposite case is the hand-written fixture, where it is spelled `unpriced`"
    );
    assert_eq!(figures.simulation_block, BlockNumber(BLOCK));
    // Two questions one record keeps answerable, with opposite answers for different reasons: the
    // reserves moved because the route actually traded, and the delivery still matched the figure
    // the pricing claimed on the pre-trade reserves. The pair is why an `Accept` is allowed to
    // quote the delivered number rather than restate the pricing's claim.
    assert!(
        simulated.market_moved(),
        "a delivered multi-hop run changes the reserves and balances of both pools it swaps \
         through; a run that moved nothing is the reverted case, which this file's other tests \
         hold at zero residue"
    );
    assert_eq!(
        simulated.matches_pricing(),
        Some(true),
        "the contract delivered exactly the amount the quote had promised"
    );
}

/// §27's freshness line, asked of a record that is otherwise accepted: 40 blocks of head is a
/// rejection, and it is the same record the test above accepted — so the rejection is caused by
/// the caller's reference moving, which is exactly the fact a pipeline has to be able to state.
#[tokio::test]
async fn a_recorded_run_behind_the_head_is_rejected() {
    let fx = fixture();
    let simulated = simulate(&fx).await;
    let policy = recorded_policy(simulated.gas_used);
    assert!(
        policy
            .evaluate(&simulated, &fresh_recorded())
            .detail()
            .contains("no broadcast"),
        "the passing answer is the one §26 requires, so the failing answer below is the same \
         policy's answer to one moved field"
    );
    let stale = policy.evaluate(
        &simulated,
        &recorded_facts(BlockNumber(BLOCK + 40), BlockNumber(BLOCK)),
    );
    assert_eq!(stale.name(), "reject");
    assert_eq!(
        stale.check().map(RiskCheck::label),
        Some("simulation_freshness"),
        "{stale}"
    );
    assert!(stale.detail().contains("40 blocks"), "{stale}");
}

/// §29/§30 end to end on real state: the recorded run, accepted, becomes M10's own plan — and the
/// plan re-encodes to the exact bytes that ran. The route identity is computed three times over
/// (from the priced hops, from the call, and from the plan's own field set) and is one string; the
/// calldata hash is computed in two crates and is one hash.
#[tokio::test]
async fn the_recorded_run_binds_to_a_plan_that_reproduces_its_own_calldata() {
    let fx = fixture();
    let simulated = simulate(&fx).await;
    let granted = recorded_acceptance(&simulated);
    let context = recorded_context(&fx);

    let (executable, binding) =
        executable_plan(&simulated, &granted, &context, &recorded_binding())
            .expect("the recorded run should bind to an executable plan");
    let plan = executable.plan();

    assert!(binding.bound(), "{binding:?}");
    assert_eq!(binding.simulation_id, simulated.identity());
    assert_eq!(binding.simulation_calldata_hash, simulated.calldata_hash());
    assert_eq!(binding.execution_calldata_hash, plan.calldata_hash());
    assert_eq!(
        binding.simulation_calldata_hash, binding.execution_calldata_hash,
        "§30's headline claim, on the bytes a real executor ran"
    );
    assert_eq!(binding.priced_route_id, binding.simulated_route_id);
    assert_eq!(binding.execution_route_id, plan.route_id());
    assert_eq!(
        binding.execution_route_id,
        format!(
            "m10-{}-{POOL_A:#x}>{POOL_B:#x}-{WETH:#x}>{MID:#x}>{WETH:#x}",
            CHAIN.0
        ),
        "the recorded route spelled M10's way: pools in trade order, then every token it steps \
         through, closing back on WETH"
    );

    // §29's reuse, at the level of the bytes: the plan the risk layer would hand onward is the
    // same call, same token, same amount, same two recorded pools.
    assert_eq!(plan.to_call(), simulated.run.call);
    assert_eq!(plan.legs.len(), 2);
    assert_eq!(plan.chain_id, CHAIN.0);
    assert_eq!(plan.executor, EXECUTOR);
    assert_eq!(plan.sender, fx.knobs.caller);
    assert_eq!(plan.recipient, RECIPIENT);
    assert_eq!(plan.input_token, WETH);
    assert_eq!(plan.input_amount, amount_in());
    assert_eq!(plan.min_final_output, granted.min_final_output);
    assert_eq!(plan.simulation.block_hash, simulated.simulation_block.hash);
    assert_eq!(plan.simulation.simulation_id, simulated.identity_hash());
    assert!(
        plan.canonical_text()
            .starts_with("m10-arbitrage-execution-plan\n"),
        "§29: M11 generates M10's plan type, and the plan's own canonical spelling says so"
    );
    assert!(
        plan.validate(&recorded_binding()).is_empty(),
        "M10's gate accepts what M11 handed it"
    );
}

/// §30 at the boundary it exists for: a plan built against a record whose route no longer matches
/// its own pricing is refused, and the refusal is the record's — not a warning a caller can ignore
/// on the way to a signature. The edit is made on the real record because the real record is the
/// one that would ever reach an executor.
#[tokio::test]
async fn a_recorded_run_with_an_edited_route_never_becomes_a_plan() {
    let fx = fixture();
    let simulated = simulate(&fx).await;
    let granted = recorded_acceptance(&simulated);

    let mut edited = simulated.clone();
    edited.candidate = with_quote(&simulated.candidate, |quote| {
        // The recorded chain, not the synthetic one: the pricing now claims pool A ran twice,
        // while the call that produced this record still names pool B at hop 1.
        quote.hops[1].pool = evm_core::PoolId::new(CHAIN, POOL_A);
    });

    let refused = plan_from_simulation(&edited, &granted, &recorded_context(&fx))
        .expect_err("a record whose legs no longer match its pricing must not be planned");
    assert_eq!(refused.code(), "route_validity");
    assert!(
        refused.to_string().contains("leg 1"),
        "the refusal should say which hop moved: {refused}"
    );

    // The same question, asked one layer earlier, answers the same way — so the two layers cannot
    // disagree about whether a record is its candidate's run.
    let judged = recorded_policy(simulated.gas_used).evaluate(&edited, &fresh_recorded());
    assert_eq!(
        judged.check().map(RiskCheck::label),
        Some("route_validity"),
        "{judged}"
    );
}
