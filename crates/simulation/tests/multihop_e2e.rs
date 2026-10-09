//! §37–§39: the multi-hop candidate walked end to end, on the machine that would spend it.
//!
//! ```text
//! cargo test -p evm-simulation --test multihop_e2e -- --test-threads=1
//! ```
//!
//! M11-P1..P5 each proved one stage against its own neighbours. This file asks the single
//! question the milestone exists to answer: does a closed route, found as a shape in a graph,
//! survive every hand-off all the way to bytes a real executor runs — and when one leg refuses,
//! does the whole thing leave nothing behind?
//!
//! ```text
//! CycleCandidate → Pricing → Optimizer → REVM → Risk → ExecutablePlan → M10's executor
//!                                                                        → receipt → balance
//! ```
//!
//! ## The two markets, and what each one is allowed to claim
//!
//! The two-hop case runs on the market M7 froze: both pools, both tokens and every reserve are
//! the recording's, and the executor is M10's own deployment scaffolding replayed over it — the
//! same state [`multihop_revm`] judges, walked from further upstream.
//!
//! The three-hop case needed a triangle, and the measurement that led here is in
//! `triangle_probe`'s instruments 7–9: the one real three-pool cycle in the recording lives in
//! pools whose code carries no `swap` dispatcher arm, so it can be priced but never driven. Since
//! §38 forbids shipping a 3-hop that executes as a 2-hop, the fixture keeps what the recording
//! actually has and declares only what it lacks:
//!
//! ```text
//! real     the executor bytecode, the pair bytecode (copied byte for byte from POOL_A),
//!          leg A's pool and both its recorded reserves, the header, the chain, the gas price
//! declared leg B's and leg C's pools (that same pair code at new addresses), the third token,
//!          the three pools' depths as balance rows, and the executor's allow-list entries
//! ```
//!
//! which is why the two-hop plan is bound with [`MarketKind::RealMarket`] and the three-hop plan
//! with [`MarketKind::ControlledFixture`], and why no number in this file is published as a
//! real-market verdict (§41). `tests/multihop_state` spells the declaration row by row with a
//! reason per row; [`the_declared_cycle_is_a_replay_of_the_recorded_pair`] is this file's check
//! that the declaration did not quietly become a second, simpler implementation.
//!
//! Nothing here signs, broadcasts, or holds a key. The receipt a test attaches is built from the
//! simulation's own measurements, and the one field no simulation can produce — the hash of a
//! transaction that was never signed — is stated as a script rather than invented.

use alloy_primitives::{Address, B256, U256};
use evm_core::{BlockNumber, PoolId, TokenId};
use evm_execution::{
    bind, executable_plan, Claim, ExecutablePlan, ExecutionBinding, ExecutionStatus,
    ExpectedTransaction, Ledger, MarketKind, MultihopBinding, MultihopPlanContext, Receipt,
    SenderFunding,
};
use evm_graph::GraphSnapshot;
use evm_opportunity::{MultiHopRoute, OptimizedCandidate};
use evm_pathfinder::{find_cycles, CycleCandidate, FeeStatus, PathFinderConfig};
use evm_protocol::{decode_calldata, ExecutorCall, ExecutorLeg};
use evm_risk::{MarketFacts, MultihopAcceptance, MultihopRiskDecision, MultihopRiskPolicy};
use evm_simulation::{
    executor::{run, ExecutorOutcome, ExecutorRun},
    executor_run,
    state::StateDump,
    EvmRules, GasCharge, GasPricing, Movement, MultiHopBuildError, RunConfig, SimulatedOpportunity,
    SimulationStatus, StepStatus,
};

mod executor_state;
mod multihop_market;
mod multihop_state;

use executor_state::{
    amount_in, pool_balance, Fixture, BLOCK, CHAIN, ENDOWMENT, EXECUTOR, FIXTURE_LABEL, GAS_LIMIT,
    MID, POOL_A, POOL_B, RECIPIENT, WETH,
};
use multihop_market::{at_amount, market_on, reserves};
use multihop_state::{
    cycle_amount_in, pools_in_order, recorded_code, Cycle, POOL_BC, POOL_CA, TOKEN_C,
};

/// The clock the scripted lifecycle stamps its rungs with. Fixed rather than `now()` because §47
/// asks two runs to produce the same record.
const AT_MS: u64 = 37_530_593_000;

/// The words one leg occupies in the `execute` calldata: `pool`, `token_in`, `token_out`,
/// `amount_in`, `amount_out`, `min_amount_out` — the six ABI fields of one leg, each on its own
/// 32-byte word. Spelled as a number rather than derived from `size_of`, because the Rust struct's
/// padding and the ABI's word layout are different things and only the second decides the bytes.
const LEG_WORDS: usize = 6;

// ---------------------------------------------------------------------------
// The two markets, each found as a shape before anything prices it
// ---------------------------------------------------------------------------

/// The recorded pair's market: the two pools M7 froze, with reserves read out of the tokens' own
/// balance words.
fn recorded_graph(dump: &StateDump) -> GraphSnapshot {
    market_on(
        CHAIN,
        BLOCK,
        &[
            reserves(
                POOL_A,
                MID,
                WETH,
                pool_balance(dump, MID, POOL_A),
                pool_balance(dump, WETH, POOL_A),
                Some(multihop_market::FEE),
            ),
            reserves(
                POOL_B,
                MID,
                WETH,
                pool_balance(dump, MID, POOL_B),
                pool_balance(dump, WETH, POOL_B),
                Some(multihop_market::FEE),
            ),
        ],
    )
}

/// The declared triangle's market: leg A's real pool with its recorded reserves, then the two
/// pools this fixture introduced, each stated as its own pair words and its own balance rows.
fn cycle_graph(cycle: &Cycle) -> GraphSnapshot {
    market_on(CHAIN, BLOCK, &cycle.reserves())
}

/// The cycle M9.3's search finds in `graph` that starts on `start` and walks exactly `pools`, in
/// that order.
///
/// Selection is by pool set and start token, never by "the first candidate the search returned":
/// the search also reports the same cycle walked in the opposite direction, and a test that picked
/// by position would change meaning when the graph grew.
fn candidate_over(graph: &GraphSnapshot, pools: &[Address], start: Address) -> CycleCandidate {
    let wanted: Vec<PoolId> = pools.iter().map(|pool| PoolId::new(CHAIN, *pool)).collect();
    let found = find_cycles(graph, &PathFinderConfig::new(pools.len()))
        .unwrap_or_else(|error| panic!("the search refused {pools:?}: {error}"));
    let reported = found.len();
    let matches: Vec<CycleCandidate> = found
        .into_iter()
        .filter(|candidate| {
            candidate.hop_count == wanted.len()
                && candidate.pools() == wanted
                && candidate.start_token.address == start
                && candidate.fee_status == FeeStatus::Complete
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "exactly one candidate walks {pools:?} entered at {start}; the search reported \
         {reported} candidate(s) for this graph and {} of them matched",
        matches.len()
    );
    matches.into_iter().next().expect("the one match")
}

/// The recorded pair, as the search reports it: `WETH → pool A → MID → pool B → WETH`.
fn two_hop_candidate(dump: &StateDump) -> CycleCandidate {
    candidate_over(&recorded_graph(dump), &[POOL_A, POOL_B], WETH)
}

// ---------------------------------------------------------------------------
// The stages below the search, at the numbers this fixture holds
// ---------------------------------------------------------------------------

/// The run spec M11 asks the executor with: M10's addresses, gas limit, rules, pricing and
/// endowment, and this fixture's own state label.
fn config(fx: &Fixture) -> RunConfig {
    RunConfig {
        state_source: fx.source.clone(),
        executor: EXECUTOR,
        operator: fx.knobs.caller,
        recipient: fx.knobs.recipient,
        gas_limit: GAS_LIMIT,
        rules: EvmRules::Prague,
        pricing: GasPricing::Eip1559 {
            priority_fee_per_gas: 0,
            provenance: format!(
                "block {BLOCK}'s own base fee as the recorded header reports it, with no tip: a \
                 hypothetical transaction on a historical block is not competing to be included \
                 in it"
            ),
        },
        endowment: Some(ENDOWMENT),
    }
}

/// The ceiling is the run's own measured burn and the floor is 0, so §27's checks answer the
/// questions this file asks rather than refusing on a number nobody chose.
fn policy(gas_used: u64) -> MultihopRiskPolicy {
    MultihopRiskPolicy {
        minimum_gross_profit: U256::ZERO,
        maximum_gas: gas_used,
        maximum_simulation_age: 3,
        maximum_state_age: 3,
        executor: EXECUTOR,
        chain_id: CHAIN,
        provenance: "M11's end-to-end thresholds: the ceiling is this run's measured burn, the \
                     floor is 0 because the question asked above it is `output > input`, and both \
                     block facts come from the pinned header rather than from a node"
            .to_string(),
    }
}

/// Both block facts at the header the state was loaded from, so age is zero and a freshness
/// rejection can only be caused by a test moving one of them.
fn facts() -> MarketFacts {
    MarketFacts {
        head: Some(BlockNumber(BLOCK)),
        state_version: Some(BlockNumber(BLOCK)),
        provenance: format!(
            "block {BLOCK} as the recording's own header reports it; no node was asked, which is \
             what §28 requires of this layer"
        ),
    }
}

fn deployment() -> ExecutionBinding {
    ExecutionBinding {
        chain_id: CHAIN.0,
        executor: EXECUTOR,
    }
}

/// The four facts a simulation cannot know, with the market label the caller has evidence for.
fn context(fx: &Fixture, market: MarketKind, correlation_id: &str) -> MultihopPlanContext {
    MultihopPlanContext {
        sender: fx.knobs.caller,
        correlation_id: correlation_id.to_string(),
        max_block_age: 3,
        validity_provenance: "§8's window as this fixture declares it".to_string(),
        funding: SenderFunding::RealState {
            source: "the operator's balance and allowance are rows in the pinned fixture, read by \
                     the run rather than written into it"
                .to_string(),
        },
        market,
        state_fingerprint: format!("{BLOCK}:0"),
        floor_provenance: "§12's floor, mirrored from the guard the call carries".to_string(),
    }
}

/// §41's label for the recorded pair, with the evidence in it.
fn real_market() -> MarketKind {
    MarketKind::RealMarket {
        attested_by: format!(
            "chain {}, block {BLOCK}: both pools, both tokens and every reserve in this route \
             come from the recording M7 froze out of the archive node",
            CHAIN.0
        ),
    }
}

/// §41's label for the declared triangle: what the run is entitled to claim, and which three
/// venues it claims them on.
fn declared_market(pools: &[Address]) -> MarketKind {
    MarketKind::ControlledFixture {
        proves: format!(
            "three legs ({}), each against a pool whose runtime code is the recorded pair's byte \
             for byte; the topology — one third token and pools at addresses the recording has \
             never seen — is declared row by row in tests/multihop_state",
            pools
                .iter()
                .map(|pool| format!("{pool:#x}"))
                .collect::<Vec<_>>()
                .join(" → ")
        ),
    }
}

/// The stake a candidate is priced at, read from where its own fixture declares it: the recorded
/// pair at M10's size, the declared triangle at this module's.
fn stake_for(candidate: &CycleCandidate) -> U256 {
    match candidate.hop_count {
        2 => amount_in(),
        3 => cycle_amount_in(),
        hops => panic!(
            "this file walks the recorded pair and the declared triangle; {hops} hops \
                       is neither"
        ),
    }
}

// ---------------------------------------------------------------------------
// The whole chain, once per case
// ---------------------------------------------------------------------------

/// One route, walked through every stage, holding what each stage answered.
struct Chain {
    candidate: CycleCandidate,
    priced: OptimizedCandidate,
    sim: SimulatedOpportunity,
    figures: MultihopAcceptance,
    plan: ExecutablePlan,
    binding: MultihopBinding,
}

impl Chain {
    /// Search → route → price → optimise → run → file → judge → plan, in that order, with no
    /// stage allowed to restate another's numbers. `market` is the caller's evidence about the
    /// venues, which nothing downstream can derive: the EVM answers about state, not about where
    /// the state came from (§41).
    async fn walk(
        fx: &Fixture,
        graph: &GraphSnapshot,
        candidate: CycleCandidate,
        market: MarketKind,
    ) -> Self {
        let stake = stake_for(&candidate);
        let route = MultiHopRoute::new(graph, candidate.edges.as_slice())
            .unwrap_or_else(|error| panic!("the candidate is not a route: {error}"));
        let priced = at_amount(&route, stake);
        let spec: ExecutorRun = executor_run(&priced, &config(fx), priced.search.best_output)
            .unwrap_or_else(|error| panic!("the candidate refused to build a request: {error}"));
        let outcome = run(fx.provider(), &spec).await.expect("the harness ran");
        let sim = SimulatedOpportunity::new(&priced, &spec, outcome)
            .expect("the run is this candidate's run");
        let figures = match policy(sim.gas_used).evaluate(&sim, &facts()) {
            MultihopRiskDecision::Accept(figures) => figures,
            other => panic!("the risk layer refused: {other}"),
        };
        let (plan, binding) = executable_plan(
            &sim,
            &figures,
            &context(fx, market, &format!("m11-e2e-{}hop", candidate.hop_count)),
            &deployment(),
        )
        .unwrap_or_else(|refusal| panic!("the accepted run did not bind to a plan: {refusal}"));
        Self {
            candidate,
            priced,
            sim,
            figures,
            plan,
            binding,
        }
    }

    /// The legs as the contract's own codec sees them: decoded out of the plan's bytes rather
    /// than read off a Rust field, so §38's claim is about the calldata that would be sent.
    fn call_legs(&self) -> Vec<ExecutorLeg> {
        match self.plan.plan().to_call() {
            ExecutorCall::Execute { legs, .. } => legs,
            other => panic!("the plan is not an execute call: {}", other.signature()),
        }
    }

    /// The contract's own named error, or a sentence saying the revert was not the contract's.
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
}

// ---------------------------------------------------------------------------
// §37: the controlled 2-hop, all the way to a bound plan
// ---------------------------------------------------------------------------

/// §37's first case. The route is not handed to the pricing here — M9.3's search finds it in the
/// graph the recording projects, and every stage below takes the stage above's answer: the pool
/// set and start token select the candidate, the candidate's edges build the route, the route's
/// quote builds the call, the call's run builds the record, and the record builds the plan.
#[tokio::test]
async fn the_controlled_two_hop_walks_from_a_found_cycle_to_a_bound_plan() {
    let fx = Fixture::committed();
    let candidate = two_hop_candidate(&fx.dump);
    let chain = Chain::walk(&fx, &recorded_graph(&fx.dump), candidate, real_market()).await;

    assert_eq!(chain.candidate.hop_count, 2);
    assert_eq!(
        chain.candidate.pools(),
        vec![PoolId::new(CHAIN, POOL_A), PoolId::new(CHAIN, POOL_B)],
        "both venues are the recorded ones"
    );
    assert_eq!(chain.candidate.start_token.address, WETH);
    assert_eq!(chain.candidate.target_block, BlockNumber(BLOCK));
    assert_eq!(chain.candidate.chain_id, CHAIN);
    assert!(chain.candidate.is_connected());
    assert!(chain.candidate.belongs_to(&recorded_graph(&fx.dump)));

    // The stake came from the fixture, and the search was asked at exactly that point, so the
    // winner is the stake and the quote is two hops long.
    assert_eq!(chain.priced.search.quote.hops.len(), 2);
    assert_eq!(chain.priced.search.best_input, amount_in());
    assert_eq!(chain.sim.input_amount, amount_in());
    assert!(
        matches!(chain.sim.status, SimulationStatus::Delivered { .. }),
        "{}",
        Chain::blame(&chain.sim.outcome)
    );
    assert_eq!(
        chain.sim.matches_pricing(),
        Some(true),
        "the contract delivered exactly what the pricing claimed on the pre-trade reserves"
    );

    assert!(chain.binding.bound(), "{:?}", chain.binding);
    assert_eq!(
        chain.binding.simulation_calldata_hash, chain.binding.execution_calldata_hash,
        "§30's headline, on the recorded pair"
    );
    assert_eq!(chain.call_legs().len(), 2);
    assert!(
        chain.plan.plan().validate(&deployment()).is_empty(),
        "M10's gate refuses what M11 handed it"
    );
    assert_eq!(
        chain.plan.plan().simulation.market.name(),
        "REAL_MARKET",
        "and the label is on the plan, not only in this file's prose"
    );

    // M10's own entry point, asked of the plan's own bytes: the plan is not a description of a
    // run, it re-runs as one.
    let from_plan = fx.run(chain.plan.plan().to_call()).await;
    assert_eq!(from_plan.status, chain.sim.outcome.status);
    assert_eq!(from_plan.delivered, chain.sim.outcome.delivered);
    assert_eq!(from_plan.gas_used, chain.sim.outcome.gas_used);
    assert_eq!(from_plan.calldata, chain.plan.calldata().clone());
}

// ---------------------------------------------------------------------------
// §37's receipt and balance rungs
// ---------------------------------------------------------------------------

/// §37's `receipt` and `balance` rungs. No node was asked and no transaction was signed: the
/// receipt carries the simulation's measured gas and the recorded header's price for it, and the
/// one field a simulation cannot produce — the hash of a transaction that never existed — is
/// labelled a script. What is being exercised is M6's three binding reads and the ledger's
/// carry-forward over M11's plan, reusing both frameworks unchanged (§2 forbids new ones).
#[tokio::test]
async fn the_two_hop_plan_reaches_the_receipt_framework_and_the_balances_reconcile() {
    let fx = Fixture::committed();
    let candidate = two_hop_candidate(&fx.dump);
    let chain = Chain::walk(&fx, &recorded_graph(&fx.dump), candidate, real_market()).await;
    let intent = chain.plan.to_intent();
    let legs = chain.call_legs();

    let GasCharge::Priced {
        gas_used,
        effective_gas_price,
        wei,
        ..
    } = chain.sim.gas_charge.clone()
    else {
        panic!("the recorded header prices its own gas, so this run has a bill");
    };
    assert_eq!(gas_used, chain.sim.gas_used);

    // The script hash: the plan's calldata hash, standing in for the identity a signed
    // transaction would carry. It is not a transaction hash and nothing below claims it is.
    let script_hash: B256 = chain.plan.calldata_hash();
    let receipt = Receipt {
        transaction_hash: script_hash,
        block_number: BLOCK,
        block_hash: chain.sim.simulation_block.hash,
        transaction_index: 0,
        success: true,
        gas_used,
        effective_gas_price: U256::from(effective_gas_price),
        cumulative_gas_used: None,
        from: fx.knobs.caller,
        to: Some(EXECUTOR),
        contract_address: None,
        tx_type: intent.tx_type.type_byte().map(u64::from),
        logs: Vec::new(),
        l1_fee: None,
        l1_gas_price: None,
        l1_gas_used: None,
        l1_base_fee_scalar: None,
        l1_blob_base_fee: None,
        l1_blob_base_fee_scalar: None,
        provenance: format!(
            "script, not a chain read: no bytes were signed and no node was asked. Every field \
             but `transaction_hash` is this run's own measurement — {gas_used} gas the EVM \
             burned at block {BLOCK}'s base fee, the executor the call names, the sender the \
             state funded — and the hash is the plan's calldata hash standing in for a signature \
             hash so that what is under test, {FIXTURE_LABEL}'s three binding reads, is exercised \
             over values this run produced"
        ),
    };
    let expected = ExpectedTransaction {
        transaction_hash: script_hash,
        sender: fx.knobs.caller,
        target: Some(EXECUTOR),
        nonce: intent.nonce,
        chain_id: CHAIN.0,
    };
    bind(&receipt, &expected).expect("the receipt binds to the transaction the plan describes");

    let mut ledger = Ledger::new();
    let Claim::New(handle) = ledger.claim(&intent, ExecutionStatus::Submitted, AT_MS) else {
        panic!("the plan's intent claims a fresh rung");
    };
    ledger
        .attach_route_id(&handle.execution_id, chain.plan.route_id())
        .expect("the record takes the route identity");
    ledger
        .attach_transaction_hash(&handle.execution_id, script_hash)
        .expect("the record takes its hash");
    let attached = ledger
        .attach_receipt(&receipt, AT_MS + 1)
        .expect("the receipt attaches");
    assert_eq!(
        attached.as_deref(),
        Some(handle.execution_id.as_str()),
        "and it attached to the record this plan claimed, joined on the hash"
    );
    let record = ledger
        .get(&handle.execution_id)
        .expect("the ledger holds the record");
    assert_eq!(record.status, ExecutionStatus::Included);
    assert_eq!(
        record.route_id.as_deref(),
        Some(chain.plan.route_id()),
        "the route id the ledger carries is the one the plan derives from its own legs"
    );
    assert_eq!(record.gas_used, Some(gas_used));
    assert_eq!(
        record.l2_fee,
        receipt.l2_cost_wei(),
        "the bill the record carries is the bill the receipt computes"
    );
    assert_eq!(
        record.l2_fee,
        Some(wei),
        "and it is the bill the simulator charged, not a recomputation of it"
    );
    assert_eq!(record.execution_block, Some(BLOCK));
    assert_eq!(
        record.execution_block_hash,
        Some(chain.sim.simulation_block.hash)
    );
    assert_eq!(record.route_transactions, Some(1));
    assert_eq!(record.target, EXECUTOR);
    assert_eq!(record.sender, fx.knobs.caller);
    assert_eq!(record.chain_id, CHAIN.0);

    // §30's dedup, over M11's plan rather than M6's hand-written intent.
    assert!(
        matches!(
            ledger.claim(&intent, ExecutionStatus::Submitted, AT_MS + 2),
            Claim::Existing(_)
        ),
        "one plan claims one transaction"
    );

    // The three binding reads, each refused for the reason it exists.
    let wrong_target = Receipt {
        to: Some(POOL_A),
        ..receipt.clone()
    };
    let reason = bind(&wrong_target, &expected).expect_err("a receipt for somebody else's call");
    assert!(reason.contains("target"), "{reason}");
    let wrong_sender = Receipt {
        from: RECIPIENT,
        ..receipt.clone()
    };
    let reason = bind(&wrong_sender, &expected).expect_err("a receipt from somebody else's wallet");
    assert!(reason.contains("sender"), "{reason}");
    let wrong_hash = Receipt {
        transaction_hash: chain.plan.plan_hash(),
        ..receipt
    };
    let reason = bind(&wrong_hash, &expected).expect_err("a receipt for another transaction");
    assert!(reason.contains("not the"), "{reason}");

    // The balance sheet the run claims: the wallet paid in, the recipient took out, the executor
    // holds nothing between legs, and each pool moved by exactly the leg that traded through it.
    let outcome = &chain.sim.outcome;
    assert_eq!(
        movement(outcome, WETH, fx.knobs.caller),
        Movement::Decreased { by: amount_in() },
        "the stake left the wallet"
    );
    assert_eq!(
        movement(outcome, WETH, RECIPIENT),
        Movement::Increased {
            by: outcome.delivered.expect("a delivery")
        },
        "the delivery landed with the recipient"
    );
    for (token, holder) in [(WETH, EXECUTOR), (MID, EXECUTOR), (MID, fx.knobs.caller)] {
        assert_eq!(
            movement(outcome, token, holder),
            Movement::Unchanged,
            "{holder} holds {token} {} → {} after a round trip that should have drained it",
            before(outcome, token, holder),
            after(outcome, token, holder),
        );
    }
    for (pool, leg) in [(POOL_A, &legs[0]), (POOL_B, &legs[1])] {
        assert_eq!(
            reserve_movement(outcome, pool, leg.token_in),
            (Direction::In, leg.amount_in),
            "{pool:#x} should have taken {} of {} in",
            leg.amount_in,
            leg.token_in
        );
        assert_eq!(
            reserve_movement(outcome, pool, leg.token_out),
            (Direction::Out, leg.amount_out),
            "{pool:#x} should have paid {} of {} out",
            leg.amount_out,
            leg.token_out
        );
    }
}

// ---------------------------------------------------------------------------
// §37 case 2 + §38: the controlled 3-hop, three legs, real calldata, real executor
// ---------------------------------------------------------------------------

/// The declaration is a replay, not a reimplementation: each declared pool carries the recorded
/// pair's runtime code byte for byte, and the only thing this fixture adds is where those bytes
/// sit and what numbers they read. Checked from the fixture's own account rows, so the claim is
/// about the state the run loaded rather than about the code that built it.
#[test]
fn the_declared_cycle_is_a_replay_of_the_recorded_pair() {
    let cycle = Cycle::build();
    let pair_code = cycle
        .fx
        .dump
        .account(POOL_A)
        .expect("the recorded pair")
        .code
        .clone();
    let [pool_a, pool_bc, pool_ca] = pools_in_order();
    assert_eq!(
        pool_a, POOL_A,
        "leg A's venue is the recorded pool, so the declaration starts from what the market has"
    );

    let mut replayed = 0usize;
    for row in &cycle.added_accounts {
        if row.address == TOKEN_C {
            assert_eq!(
                row.code,
                format!("{:?}", recorded_code(&cycle.fx.dump, WETH)),
                "the third token is WETH's code"
            );
            continue;
        }
        assert!(
            [pool_bc, pool_ca].contains(&row.address),
            "an account this module introduced is neither the third token nor a declared pool: \
             {:?}",
            row.address
        );
        let at_address = cycle
            .fx
            .dump
            .account(row.address)
            .expect("a declared pool")
            .code
            .clone();
        assert_eq!(
            at_address, pair_code,
            "{} is not carrying the recorded pair's code",
            row.address
        );
        assert!(
            row.reason.contains("byte for byte"),
            "and the row says so: {}",
            row.reason
        );
        replayed += 1;
    }
    assert_eq!(replayed, 2, "both declared pools replay the recorded pair");

    // The pair words each declared pool was re-pointed at, read back out of the state the run
    // loads rather than restated from this module's constants.
    let depths = cycle.declared_depths();
    assert_eq!(depths.len(), 2, "the two declared pools");
    for (pool, token0, token1, reserve0, reserve1) in depths {
        assert_eq!(
            (token0, token1),
            if pool == pool_bc {
                (MID, TOKEN_C)
            } else {
                assert_eq!(pool, pool_ca, "{pool:#x} is neither declared pool");
                (WETH, TOKEN_C)
            },
            "the pair order is the address order this fixture declares"
        );
        assert!(
            !reserve0.is_zero() && !reserve1.is_zero(),
            "{pool:#x} carries zero depth, so a leg through it prices against nothing"
        );
    }
}

/// §37's headline case and §38's reason it exists: the three-hop entered as a
/// [`CycleCandidate`], priced, optimised, run through REVM over the real executor bytecode,
/// judged by the risk layer, and bound to a plan — with the contract's own answer agreeing with
/// the pricing's at every leg.
#[tokio::test]
async fn the_declared_three_hop_walks_from_a_found_cycle_to_a_bound_plan() {
    let cycle = Cycle::build();
    let graph = cycle_graph(&cycle);
    let candidate = candidate_over(&graph, &pools_in_order(), WETH);
    let market = declared_market(&pools_in_order());
    let chain = Chain::walk(&cycle.fx, &graph, candidate, market.clone()).await;

    assert_eq!(chain.candidate.hop_count, 3, "A → B → C → A");
    assert_eq!(chain.candidate.tokens().len(), 4);
    assert_eq!(
        chain.candidate.tokens(),
        vec![
            TokenId::new(CHAIN, WETH),
            TokenId::new(CHAIN, MID),
            TokenId::new(CHAIN, TOKEN_C),
            TokenId::new(CHAIN, WETH),
        ],
        "the route steps WETH → MID → the declared token → WETH, in that order"
    );
    assert_eq!(
        chain
            .candidate
            .edges
            .iter()
            .map(|edge| edge.pool.address)
            .collect::<Vec<_>>(),
        pools_in_order(),
        "and its pools are in trade order, entered at the venue the fixture calls leg A"
    );
    assert_eq!(chain.priced.search.quote.hops.len(), 3);
    assert_eq!(chain.sim.input_amount, cycle_amount_in());
    assert!(
        matches!(chain.sim.status, SimulationStatus::Delivered { .. }),
        "{}",
        Chain::blame(&chain.sim.outcome)
    );

    // Pricing against execution, leg by leg: the three asks the optimizer built the call from
    // are the three amounts the pools actually paid. Two different measurements — the quote is
    // integer math on pre-trade reserves, the outflow is what the EVM left in the token's books.
    let quoted: Vec<U256> = chain
        .priced
        .search
        .quote
        .hops
        .iter()
        .map(|hop| hop.amount_out)
        .collect();
    let paid: Vec<U256> = chain
        .call_legs()
        .iter()
        .map(|leg| reserve_movement(&chain.sim.outcome, leg.pool, leg.token_out).1)
        .collect();
    assert_eq!(
        quoted, paid,
        "the pricing's three leg outputs and the EVM's three pool outflows"
    );
    assert_eq!(chain.sim.matches_pricing(), Some(true));
    assert!(chain.sim.market_moved(), "three pools traded");

    assert!(chain.binding.bound(), "{:?}", chain.binding);
    assert_eq!(chain.call_legs().len(), 3);
    assert_eq!(
        chain.binding.simulation_calldata_hash, chain.binding.execution_calldata_hash,
        "§30's claim, held for a three-leg route"
    );
    assert_eq!(
        chain.plan.route_id(),
        format!(
            "m10-{}-{POOL_A:#x}>{POOL_BC:#x}>{POOL_CA:#x}-{WETH:#x}>{MID:#x}>{TOKEN_C:#x}>{WETH:#x}",
            CHAIN.0
        ),
        "M10's own spelling of the route: pools in trade order, then every token it steps \
         through, closing back on the input"
    );
    assert!(
        chain.plan.plan().validate(&deployment()).is_empty(),
        "M10's gate accepts a three-leg plan (its leg cap is four)"
    );
    assert_eq!(
        chain.plan.plan().simulation.market,
        market,
        "and the label the plan carries is the one this file has evidence for"
    );
    assert!(
        !chain
            .plan
            .plan()
            .simulation
            .market
            .counts_as_real_arbitrage(),
        "§41: a controlled delivery is not a real arbitrage, whatever it delivered"
    );
    assert_eq!(chain.plan.plan().to_call(), chain.sim.run.call);
    assert_eq!(chain.figures.gas_used, chain.sim.gas_used);

    // §37's last rung, over three legs: the wallet paid once, the recipient was paid once, and
    // the executor holds nothing of any of the three tokens when the call returns.
    for token in [WETH, MID, TOKEN_C] {
        assert_eq!(
            movement(&chain.sim.outcome, token, EXECUTOR),
            Movement::Unchanged,
            "the executor still holds {} of {token}",
            after(&chain.sim.outcome, token, EXECUTOR)
        );
    }
    assert_eq!(
        movement(&chain.sim.outcome, WETH, cycle.fx.knobs.caller),
        Movement::Decreased {
            by: cycle_amount_in()
        }
    );
    assert_eq!(
        movement(&chain.sim.outcome, WETH, RECIPIENT),
        Movement::Increased {
            by: chain.sim.outcome.delivered.expect("a delivery")
        }
    );
    assert!(
        chain
            .sim
            .gross
            .expect("a delivery carries a gross")
            .gain()
            .expect("the declared triangle gains at this stake")
            > U256::ZERO,
        "a fixture that lost money would prove §38's mechanics and nothing about §37's chain"
    );
    // The same gross from the other side of the chain: the fixture's own constant-product
    // arithmetic over the balances it declares, against what the EVM handed back. Agreement
    // here is §37's claim that the pricing and the run are about one route.
    assert_eq!(
        cycle.route.priced_gross(),
        chain.sim.gross.and_then(|gross| gross.gain()),
        "the fixture's arithmetic and the EVM's answer disagree about this cycle's gross"
    );
}

/// §38's forbidden shape: 3-hop pricing and 3-hop simulation, but a 2-hop call. The guard is the
/// record's own: the bytes that reach the contract decode back into three legs over three
/// distinct pools, and a run filed under a three-hop candidate with two legs is refused before it
/// can become a record.
#[tokio::test]
async fn three_legs_reach_the_contract_as_three_real_legs() {
    let cycle = Cycle::build();
    let graph = cycle_graph(&cycle);
    let candidate = candidate_over(&graph, &pools_in_order(), WETH);
    let market = declared_market(&pools_in_order());
    let chain = Chain::walk(&cycle.fx, &graph, candidate, market).await;

    let calldata = chain.plan.calldata().clone();
    let decoded = decode_calldata(calldata.as_ref()).expect("the plan's bytes decode");
    let ExecutorCall::Execute {
        legs,
        input_token,
        amount_in: call_amount_in,
        min_final_amount,
        recipient,
    } = decoded
    else {
        panic!("the plan's bytes are not an execute call: {decoded:?}");
    };

    assert_eq!(legs.len(), 3, "Leg A / Leg B / Leg C, in the call itself");
    assert_eq!(
        legs.iter().map(|leg| leg.pool).collect::<Vec<_>>(),
        pools_in_order(),
        "three distinct venues, in trade order"
    );
    assert_eq!(
        legs.iter()
            .flat_map(|leg| [leg.token_in, leg.token_out])
            .collect::<Vec<_>>(),
        [WETH, MID, MID, TOKEN_C, TOKEN_C, WETH],
        "and the tokens they trade, in trade order"
    );
    // The chain rule the contract enforces: every leg's input is the leg before it's output.
    for pair in legs.windows(2) {
        assert_eq!(pair[0].amount_out, pair[1].amount_in);
        assert_eq!(pair[0].token_out, pair[1].token_in);
    }
    assert_eq!(input_token, WETH);
    assert_eq!(call_amount_in, cycle_amount_in());
    assert_eq!(recipient, cycle.fx.knobs.recipient);
    assert_eq!(min_final_amount, chain.priced.search.best_output);

    // Three pools moved, each by its own leg — the state-level statement that no leg was skipped.
    // A 2-hop execution of a 3-hop route leaves one pool's words untouched.
    for (index, leg) in legs.iter().enumerate() {
        assert!(
            !chain.sim.outcome.changed_slots_in(leg.pool).is_empty(),
            "{} carries no changed word, so leg {} ({leg:?}) never traded through it",
            leg.pool,
            index,
        );
        assert!(
            chain
                .sim
                .outcome
                .reserve_row(leg.pool)
                .unwrap_or_else(|| panic!("the run reports no reserve row for {}", leg.pool))
                .changed(),
            "leg {index} ran against a reserve that did not move"
        );
        assert_eq!(
            reserve_movement(&chain.sim.outcome, leg.pool, leg.token_in).1,
            leg.amount_in,
            "leg {index}'s pool took in {} of {} and nothing else",
            leg.amount_in,
            leg.token_in
        );
    }
    assert_eq!(
        chain
            .sim
            .outcome
            .reserves
            .iter()
            .filter(|row| row.changed())
            .count(),
        legs.len(),
        "one moved pool per leg"
    );

    // The same call at one leg fewer is a measurably different call: one array element's worth of
    // words, with no other field to account for the difference.
    let two_hop_graph = recorded_graph(&cycle.fx.dump);
    let two = Chain::walk(
        &cycle.fx,
        &two_hop_graph,
        two_hop_candidate(&cycle.fx.dump),
        real_market(),
    )
    .await;
    let delta = calldata.len() - two.plan.calldata().len();
    assert_eq!(
        delta,
        LEG_WORDS * 32,
        "the three-leg call carries exactly one leg more than the two-leg call, in a struct with \
         no other difference between them"
    );

    // …and the record refuses the shortfall rather than filing it under the triangle.
    let mut shorter = chain.sim.clone();
    let ExecutorCall::Execute {
        legs: three,
        input_token,
        amount_in,
        min_final_amount,
        recipient,
    } = chain.sim.run.call.clone()
    else {
        panic!("the run's call is an execute call");
    };
    assert_eq!(three.len(), 3);
    shorter.run.call = ExecutorCall::Execute {
        legs: three[..2].to_vec(),
        input_token,
        amount_in,
        min_final_amount,
        recipient,
    };
    let error =
        SimulatedOpportunity::new(&shorter.candidate, &shorter.run, shorter.outcome.clone())
            .expect_err("two legs are not three hops");
    assert_eq!(
        error,
        MultiHopBuildError::RunLegMismatch { index: 3 },
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// §39: the atomic rollback, three legs
// ---------------------------------------------------------------------------

/// §39: leg A succeeds, leg B succeeds, leg C fails — and nothing is left behind. The failure is
/// caused by one field of one leg, so the residue (or the lack of it) is attributable to leg C
/// rather than to a call that never started.
#[tokio::test]
async fn a_failed_third_leg_leaves_every_balance_and_reserve_untouched() {
    let cycle = Cycle::build();
    let legs = cycle.route.legs_inflated(2, 2);
    let outcome = cycle
        .fx
        .run(cycle.call(legs, cycle.route.weth_out_leg_c))
        .await;

    assert!(
        matches!(outcome.status, StepStatus::Reverted(_)),
        "a pool asked to pay double its priced output must refuse: {}",
        outcome.describe()
    );
    // Which guard fired is part of §39's claim: the pair's own constant-product check, not one
    // of the executor's named errors. A plan that failed at the contract's entry validation would
    // be a different experiment — legs A and B would not have traded at all, and the residue
    // check below would prove nothing.
    assert_eq!(
        outcome.revert().map(|data| data.reason().to_string()),
        Some("UniswapV2: K".to_string()),
        "the run is refused by the third pool's invariant: {}",
        Chain::blame(&outcome)
    );
    assert_eq!(
        outcome.contract_error, None,
        "and nothing on the executor's own error list fired: {outcome:#?}"
    );
    assert_eq!(outcome.delivered, None, "and nothing reaches the recipient");

    // The three lines §39 lists, each answered from the run's own before/after rows rather than
    // from the diff: pair reserves, token balances, executor balances.
    assert_eq!(outcome.reserves.len(), 3, "one row per leg's pool");
    for row in &outcome.reserves {
        assert_eq!(
            (row.before.reserve0, row.before.reserve1),
            (row.after.reserve0, row.after.reserve1),
            "{} kept its reserves across a run that traded through it: {} → {}",
            row.pool,
            row.before.reserve0,
            row.after.reserve0
        );
        assert!(!row.changed(), "{pool} moved", pool = row.pool);
    }
    assert_eq!(outcome.balances.len(), 9, "three tokens × three holders");
    for row in &outcome.balances {
        assert_eq!(
            row.movement(),
            Movement::Unchanged,
            "{} moved {} of {}: {} → {}",
            row.holder,
            row.before,
            row.token,
            row.before,
            row.after
        );
    }
    assert!(
        !outcome.market_moved(),
        "a reverted run moved a reserve or a balance: {outcome:#?}"
    );

    // The same claim at the level of storage words, over every contract the cycle touches.
    for watched in [
        POOL_A, POOL_B, POOL_BC, POOL_CA, WETH, MID, TOKEN_C, EXECUTOR,
    ] {
        let changed = outcome.changed_slots_in(watched);
        assert!(
            changed.is_empty(),
            "{watched:#x} keeps {} changed word(s) after a reverted three-leg run: {changed:?}",
            changed.len(),
        );
    }
}

/// §39's other half: that the legs ran *in order*, so the failure at leg C is a failure at the
/// third swap rather than a refusal before the first. Each variant inflates one leg's ask; the
/// legs before it are untouched and trade for real, so the burn each pays is the measurement of
/// how far the call got.
#[tokio::test]
async fn the_gas_ladder_shows_the_three_legs_ran_in_order() {
    let cycle = Cycle::build();
    let mut burned: Vec<(usize, u64)> = Vec::new();
    for index in 0..3 {
        let legs = cycle.route.legs_inflated(index, 2);
        let outcome = cycle
            .fx
            .run(cycle.call(legs, cycle.route.weth_out_leg_c))
            .await;
        assert!(
            matches!(outcome.status, StepStatus::Reverted(_)),
            "leg {index} inflated to a double ask must revert: {}",
            outcome.describe()
        );
        assert!(!outcome.market_moved(), "leg {index}'s revert left residue");
        assert!(
            outcome.gas_used > 0,
            "a run that burned nothing never started: {outcome:#?}"
        );
        burned.push((index, outcome.gas_used));
    }
    let successful = cycle.fx.run(cycle.standard_call()).await;
    assert!(
        matches!(successful.status, StepStatus::Success),
        "the same fixture at its own asks delivers: {}",
        successful.describe()
    );

    for pair in burned.windows(2) {
        assert!(
            pair[1].1 > pair[0].1,
            "failing at leg {} burned {} gas while failing at leg {} burned {} — the run would \
             have to reach the later swap to cost more getting there",
            pair[0].0,
            pair[0].1,
            pair[1].0,
            pair[1].1,
        );
    }
    let at_b = burned[1].1;
    let at_c = burned[2].1;
    let one_leg = at_c - at_b;
    // The two increments are near each other, which is what lets the ordering carry its claim:
    // a later rung costing more is evidence of *reaching* a further swap, not of the third leg
    // being intrinsically dearer than the second.
    let first_leg = burned[1].1 - burned[0].1;
    assert!(
        first_leg.saturating_sub(one_leg) < first_leg / 4,
        "leg B's work cost {first_leg} gas and leg C's cost {one_leg}; if they were far apart \
         the ladder would be measuring leg difficulty rather than leg order",
    );
    // Failing at leg C is not a shorter run than delivering: it is billed within one leg of the
    // run that settles. Which of the two is larger is the EVM's own accounting of a run that
    // finished against one that stopped inside a call, and this test does not claim to know how
    // that splits; the size of the gap is what says both runs reached the third swap.
    let gap = successful.gas_used.abs_diff(at_c);
    assert!(
        gap < one_leg,
        "delivered {} gas, stopped inside leg C {at_c} gas, one leg's work {one_leg} gas: the \
         gap of {gap} has to sit inside a leg or this test cannot say both runs reached the third \
         swap",
        successful.gas_used,
    );
}

// ---------------------------------------------------------------------------
// §47 over the whole chain
// ---------------------------------------------------------------------------

/// §47 over the declared fixture: the whole chain, twice, from a fixture rebuilt from scratch.
/// Same candidate, same quote, same optimizer answer, same identity, same bytes, same plan hash.
#[tokio::test]
async fn two_runs_of_the_declared_cycle_are_the_same_run() {
    let mut spells: Vec<(String, String, B256, B256, u64, String)> = Vec::new();
    for _ in 0..2 {
        let cycle = Cycle::build();
        let graph = cycle_graph(&cycle);
        let candidate = candidate_over(&graph, &pools_in_order(), WETH);
        let quoted = candidate.tokens().len();
        let chain = Chain::walk(
            &cycle.fx,
            &graph,
            candidate,
            declared_market(&pools_in_order()),
        )
        .await;
        spells.push((
            chain.sim.canonical_text(),
            chain.sim.identity(),
            chain.plan.calldata_hash(),
            chain.plan.plan_hash(),
            chain.sim.gas_used,
            format!("{quoted} {}", chain.plan.route_id()),
        ));
    }
    assert_eq!(spells[0], spells[1], "two runs of the same fixture");
    assert!(
        spells[0]
            .1
            .starts_with(&format!("m11-sim-{}-{BLOCK}-", CHAIN.0)),
        "the identity names the chain and the header it was walked at: {}",
        spells[0].1
    );
    assert!(
        spells[0].5.contains(&format!("{POOL_CA:#x}")),
        "and the route it was filed under names all three venues: {}",
        spells[0].5
    );
}

// ---------------------------------------------------------------------------
// Development instruments
// ---------------------------------------------------------------------------

/// One look at the declared cycle, printed rather than asserted: what the EVM answered, how much
/// it cost, how many rows it reported. Kept because when the cycle stops working the first
/// question is which of those numbers moved.
#[tokio::test]
async fn a_first_look_at_the_declared_cycle() {
    let cycle = Cycle::build();
    let outcome = cycle.fx.run(cycle.standard_call()).await;
    println!(
        "status: {:?}\ngas used: {}\ndelivered: {:?}\nlogs: {}\nreserve rows: {}\nbalance rows: \
         {}\nchanged slots: {}\nwords declared by this module: {}\naccounts declared by this \
         module: {}\nroute as the fixture prices it: in {} → {} MID → {} {} → {} WETH\nsource: {}",
        outcome.status,
        outcome.gas_used,
        outcome.delivered,
        outcome.logs.len(),
        outcome.reserves.len(),
        outcome.balances.len(),
        outcome.state_changes.slots.len(),
        cycle.added_words.len(),
        cycle.added_accounts.len(),
        cycle.route.amount_in,
        cycle.route.mid_out_leg_a,
        cycle.route.tc_out_leg_b,
        TOKEN_C,
        cycle.route.weth_out_leg_c,
        cycle.fx.source,
    );

    let failing = cycle
        .fx
        .run(cycle.call(cycle.route.legs_inflated(2, 2), cycle.route.weth_out_leg_c))
        .await;
    println!(
        "\nthe same cycle with leg C asking double: status {:?}, gas {}, blame {}",
        failing.status,
        failing.gas_used,
        Chain::blame(&failing),
    );
}

/// The crack loop, `#[ignore]`d and run by hand whenever the declared fixture starts refusing:
/// each round builds the fixture with one more declared word per refusal the provider reported,
/// until the cycle runs. A word enters the fixture because a run named it, never because this
/// file guessed it.
#[tokio::test]
#[ignore = "development: walks the refusals so the fixture's declared rows are learned, not typed"]
async fn the_cycle_walks_its_own_refusals() {
    let mut rows = Vec::new();
    for round in 0..24 {
        let cycle = Cycle::build_at(cycle_amount_in(), &rows);
        match cycle.fx.try_run(cycle.standard_call()).await {
            Ok(ran) => {
                println!(
                    "round {round}: the cycle ran — {:?}, {} gas; {} extra word(s) learned",
                    ran.status,
                    ran.gas_used,
                    rows.len(),
                );
                return;
            }
            Err(error) => {
                let text = error.to_string();
                let (contract, key) = parse_refusal(&text).unwrap_or_else(|| {
                    panic!("round {round}: a refusal this walk cannot read: {text}")
                });
                println!("round {round}: the run asked for {key:#x} in {contract:#x}");
                rows.push(multihop_state::learned_row(
                    contract,
                    key,
                    "read by the three-leg cycle",
                ));
            }
        }
    }
    panic!("24 rounds of refusals and the cycle still does not run");
}

/// `missing state: {source}: storage {addr} slot {dec}` → the contract and the slot.
fn parse_refusal(text: &str) -> Option<(Address, U256)> {
    let rest = text.split("storage ").nth(1)?;
    let (address, slot) = rest.split_once(" slot ")?;
    let word = slot.split_whitespace().next()?;
    // Both readings tried: the refusal is a string and a crack loop must not give up on a format.
    let value = match word.parse::<U256>() {
        Ok(decimal) => decimal,
        Err(_) => U256::from_str_radix(word, 16).ok()?,
    };
    Some((address.parse().ok()?, value))
}

// ---------------------------------------------------------------------------
// Balance and reserve readings, taken from the run's rows rather than restated
// ---------------------------------------------------------------------------

/// Which way one side of one pool moved. Named rather than returned as a bool because a test that
/// reads `(true, amount)` has to go and look up what `true` means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    In,
    Out,
}

fn movement(outcome: &ExecutorOutcome, token: Address, holder: Address) -> Movement {
    outcome
        .balance_row(token, holder)
        .unwrap_or_else(|| panic!("the run reports no {token:#x} row for {holder:#x}"))
        .movement()
}

fn before(outcome: &ExecutorOutcome, token: Address, holder: Address) -> U256 {
    outcome
        .balance_row(token, holder)
        .expect("a balance row")
        .before
}

fn after(outcome: &ExecutorOutcome, token: Address, holder: Address) -> U256 {
    outcome
        .balance_row(token, holder)
        .expect("a balance row")
        .after
}

/// Which way, and by how much, one pool's side of one token moved — read out of the pool's own
/// reserve row, which is why the test asks the pool which side it is rather than assuming
/// `reserve0`.
fn reserve_movement(outcome: &ExecutorOutcome, pool: Address, token: Address) -> (Direction, U256) {
    let row = outcome
        .reserve_row(pool)
        .unwrap_or_else(|| panic!("the run reports no reserve row for {pool:#x}"));
    let (start, end) = if row.before.token0 == token {
        (row.before.reserve0, row.after.reserve0)
    } else {
        assert_eq!(
            row.before.token1, token,
            "{token:#x} is not in {pool:#x}'s pair"
        );
        (row.before.reserve1, row.after.reserve1)
    };
    if end > start {
        (Direction::In, end - start)
    } else {
        assert!(start > end, "{token:#x} did not move in {pool:#x}");
        (Direction::Out, start - end)
    }
}
