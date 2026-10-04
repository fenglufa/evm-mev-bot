//! §16–§24/§54: the route as six transactions, and the four audits it has to survive.
//!
//! [`stage_matrix`](crate::stage_matrix) proves one transaction can be priced, built, signed,
//! sent and accounted for. This file proves the *sequence* claim, which is a different claim:
//! that the plan the simulation produced is what gets sent, in that order, one nonce after
//! another; and that afterwards the run can state — from the chain's own logs and balances,
//! not from its own intentions — what the wallet actually gained, what it actually paid, and
//! whether either matches what the simulation said.
//!
//! §40's permission is the same here as in M6 and no wider: the endpoint's answers are scripted,
//! because what is under test is what this crate does with an answer. The builder, the signer
//! (on the synthetic scalar-1 key), the gate, the ledger, the lane and the receipt tracker are
//! the crate's own, run for real. The scripted receipts keep the shape measured in
//! `data/evidence/m6/probe-submission-surface.txt` — L1 fee fields included — and the simulated
//! run they are compared against has the 10-step / 6-transaction shape of the real M7 candidate
//! priced in `data/evidence/m7/candidate-fee-measurement.json`, wrap and unwrap included.
//! Nothing in this file is evidence that a broadcast worked.
//!
//! Two of the tests below are about numbers *not* being equal, which is only a test because the
//! fixture arithmetic is written as arithmetic: the expected side of every comparison is
//! computed from the same constants the scripted chain answers with, so a fixture that stopped
//! adding up would fail rather than quietly pass.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_primitives::{Address, Bytes, I256, U256};
use async_trait::async_trait;
use evm_chain::{RpcTraceSink, RpcTraceSource};
use evm_core::{BlockNumber, ChainId, LogIndex, TxHash, TxIndex};
use evm_execution::{
    audit_route, broadcastable, reconcile_flows, signing_hash_for_chain, swap_observations,
    transfer_flows, wrap_moves, Abilities, AssetReader, AssetReading, BlockContextScope,
    BlockIdentity, BuildPolicy, ChainReader, ContextRefusal, DeltaAudit, EndpointKind,
    ExecutionError, ExecutionKey, ExecutionMode, ExecutionSetup, ExecutionStatus, FeePolicy,
    FeeReading, FeeSource, GasPolicy, GateAttempt, LaneRelease, MarketKind, NonceReading,
    NonceSource, PreflightCheck, PreflightFinding, PreflightReport, ProducerOutcome,
    ProfitVerificationStatus, Receipt, ReceiptPolicy, ReceiptStatus, SenderFunding, SequencePlan,
    SequenceStage, SignedTransaction, Signer, SnapshotPin, SubmissionOutcome, TokenFlow, Tolerance,
    TransactionIntent, TransactionSubmitter, TransactionType, UnsignedTransaction,
    VerifiedBlockContext, WrapMove,
};
use evm_metrics::{Clock, Metrics};
use evm_protocol::signatures::{V2Topics, Weth9Topics};
use evm_risk::RiskDecision;
use evm_simulation::{
    Binding, BlockPin, Denomination, ExecutedLog, ExecutedStep, ExecutionStatus as SimStatus,
    GasCharge, GasPricing, MeasuredValue, Movement, NetProfit, OutputComparison, PlanSummary,
    SimulatedOutcome, SimulationResult, SlippagePolicy, SlippageRecord, StateChanges, StepStatus,
};

/// §40's synthetic key: the scalar one, never the operator's wallet.
const TEST_SCALAR: [u8; 32] = {
    let mut bytes = [0u8; 32];
    bytes[31] = 1;
    bytes
};

const CHAIN: u64 = 91_342;
/// The block the M7 candidate was priced against (`candidate-fee-measurement.json`).
const PIN: u64 = 37_530_593;
/// M8.4.4 §16: the subtree of the evidence tree that holds this file's raw arm rows. Named here
/// and in `tests/block_context_evidence.rs` — the two test binaries cannot share a module — so
/// the writer's own comment below states what the second copy would have to drift into.
const FIXED_BLOCK_DIR: &str = "fixed-block";
/// The base fee measured at that block, in wei per gas.
const BASE_FEE: u64 = 362;
const TIP: u64 = 0;
/// What each scripted receipt charges per gas. Equal to [`BASE_FEE`] on purpose: the point of
/// §18's gas line is the *units*, and a fixture that also drifted the price would test two
/// things at once and name neither.
const CHARGED_PRICE: u64 = BASE_FEE;
/// The `l1Fee` a GIWA receipt carries for a call of this size, measured in M6.
const L1_FEE: u64 = 7_400_000_000;

const PURSE: u128 = 20_000_000_000_000_000;
const INPUT: u128 = 1_000_000_000_000_000;
const PROFIT: u128 = 2_000_000_000_000;
const OUTPUT: u128 = INPUT + PROFIT;
const MID: u128 = 1_000_000_000_000_000_000;

/// The gas the six transactions measured, in send order.
const STEP_GAS: [u64; 6] = [100_000, 60_000, 150_000, 60_000, 150_000, 80_000];
/// The gas the two `balanceOf` measurement steps consumed inside the simulation. Never paid on
/// chain — that is §18's whole point about `SequencePlan::expected_gas_used`.
const READ_GAS: u64 = 3_000;
/// The builder's headroom over each measured step.
const MARGIN: u64 = 20_000;

fn weth() -> Address {
    "0x4200000000000000000000000000000000000006"
        .parse()
        .expect("the chain's wrapped gas asset")
}

fn mid() -> Address {
    "0x07D4af6E2bc8DD82beb06b4FD279DF4c9028F26f"
        .parse()
        .expect("the candidate route's mid token")
}

fn pool_a() -> Address {
    "0x2a3ceafbA30f6626170CBB0CD67392eFb94BD9A4"
        .parse()
        .expect("the buying venue")
}

fn pool_b() -> Address {
    "0x5b3C1E3Fb6A97c0130aE015ff10f53A1A30C353e"
        .parse()
        .expect("the selling venue")
}

/// The pinned block's own hash, computed rather than typed.
///
/// `crates/cli/tests/no_execution.rs` scans this crate's test files for a 32-byte
/// hex literal because that shape is indistinguishable from a private key (§17,
/// §40), and a real chain hash would trip it for the wrong reason.
fn pinned_hash() -> alloy_primitives::B256 {
    alloy_primitives::keccak256("giwa block 37530593")
}

fn block_hash(number: u64) -> alloy_primitives::B256 {
    alloy_primitives::B256::left_padding_from(&[(number % 253) as u8 + 1, (number / 253) as u8])
}

fn test_signer(mode: ExecutionMode) -> Signer {
    let key = ExecutionKey::from_secret_bytes(&TEST_SCALAR).expect("the synthetic key is in range");
    Signer::from_key(mode, key)
}

/// The key's own account. The route spends from it, so every log names it.
fn sender() -> Address {
    test_signer(ExecutionMode::SignOnly)
        .address()
        .expect("a signer built from a key knows its address")
}

fn u(value: u128) -> U256 {
    U256::from(value)
}

fn topic(address: Address) -> alloy_primitives::B256 {
    alloy_primitives::B256::left_padding_from(address.as_slice())
}

fn words(values: &[U256]) -> Vec<u8> {
    values.iter().flat_map(|w| w.to_be_bytes::<32>()).collect()
}

/// One event as both sides of the fixture need it: the simulated run records it as an
/// [`ExecutedLog`], the scripted chain answers it as a [`ChainLog`](evm_chain::ChainLog).
struct Event {
    address: Address,
    topics: Vec<alloy_primitives::B256>,
    data: Vec<u8>,
}

fn transfer(from: Address, to: Address, token: Address, amount: U256) -> Event {
    let topics = V2Topics::default();
    Event {
        address: token,
        topics: vec![topics.transfer, topic(from), topic(to)],
        data: words(&[amount]),
    }
}

fn wrap(token: Address, account: Address, amount: U256, mints: bool) -> Event {
    let topics = Weth9Topics::default();
    Event {
        address: token,
        topics: vec![
            if mints {
                topics.deposit
            } else {
                topics.withdrawal
            },
            topic(account),
        ],
        data: words(&[amount]),
    }
}

/// A pool's own `Swap`. `amount0_*`/`amount1_*` are left in the pair's own slot order — the
/// fixture does not claim which slot is WETH here, and §23's audit does not ask.
fn swap(pool: Address, sender: Address, in_a: U256, out_b: U256) -> Event {
    let topics = V2Topics::default();
    Event {
        address: pool,
        topics: vec![topics.swap, topic(sender), topic(sender)],
        data: words(&[in_a, U256::ZERO, U256::ZERO, out_b]),
    }
}

/// The six transactions' events, in send order: wrap, pay the buying venue, its swap, pay the
/// selling venue, its swap, unwrap.
fn step_events(position: usize, account: Address) -> Vec<Event> {
    match position {
        0 => vec![wrap(weth(), account, u(INPUT), true)],
        1 => vec![transfer(account, pool_a(), weth(), u(INPUT))],
        2 => vec![
            swap(pool_a(), account, u(INPUT), u(MID)),
            transfer(pool_a(), account, mid(), u(MID)),
        ],
        3 => vec![transfer(account, pool_b(), mid(), u(MID))],
        4 => vec![
            swap(pool_b(), account, u(MID), u(OUTPUT)),
            transfer(pool_b(), account, weth(), u(OUTPUT)),
        ],
        _ => vec![wrap(weth(), account, u(OUTPUT), false)],
    }
}

/// The bill for the gas the chain actually charges: the six transactions, at the price the
/// scripted receipts report.
fn l2_total() -> U256 {
    STEP_GAS.iter().fold(U256::ZERO, |total, gas| {
        total + u(*gas as u128) * u(CHARGED_PRICE as u128)
    })
}

fn l1_total() -> U256 {
    u(L1_FEE as u128) * u(STEP_GAS.len() as u128)
}

/// The wallet's native balance after the route: the purse, minus what the wrap took, plus what
/// the unwrap handed back, minus both halves of the bill. §39's equation is the same sum read
/// term by term, which is why the happy-path test can require it to close exactly.
fn settled_native() -> U256 {
    u(PURSE) - u(INPUT) + u(OUTPUT) - l2_total() - l1_total()
}

/// What the simulation said the native balance would end at. Two known differences from
/// [`settled_native`] separate it from the chain: the measurement steps' gas was charged inside
/// the EVM and never on chain, and the simulated bill carries no L1 data fee at all.
fn simulated_native_end() -> U256 {
    u(PURSE) - u(INPUT) + u(OUTPUT)
        - u((STEP_GAS.iter().sum::<u64>() + 2 * READ_GAS) as u128 * CHARGED_PRICE as u128)
}

/// A call the engine ran, as the fixture states it: which step and which nonce, which contract
/// and which calldata, how much gas it measured, and which of the route's transactions it is.
struct Call {
    index: usize,
    nonce: u64,
    position: usize,
    to: Address,
    value: U256,
    signature: &'static str,
    selector: [u8; 4],
    args: Vec<U256>,
    gas_used: u64,
}

fn call(step: Call, account: Address) -> ExecutedStep {
    let mut calldata = step.selector.to_vec();
    calldata.extend(words(&step.args));
    ExecutedStep {
        index: step.index,
        nonce: step.nonce,
        from: account,
        to: step.to,
        value: step.value,
        signature: step.signature.to_string(),
        selector: format!("0x{}", hex(&step.selector)),
        calldata: Bytes::from(calldata),
        gas_limit: step.gas_used + MARGIN,
        gas_used: step.gas_used,
        status: StepStatus::Success,
        logs: step_events(step.position, account)
            .into_iter()
            .map(|event| ExecutedLog {
                address: event.address,
                topics: event.topics,
                data: Bytes::from(event.data),
            })
            .collect(),
        measured: None,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The measurement the engine recorded next to the call that produced it.
fn balance_read(
    index: usize,
    nonce: u64,
    token: Address,
    binding: Binding,
    value: U256,
    account: Address,
) -> ExecutedStep {
    ExecutedStep {
        index,
        nonce,
        from: account,
        to: token,
        value: U256::ZERO,
        signature: "balanceOf(address)".to_string(),
        selector: "0x70a08231".to_string(),
        calldata: Bytes::from({
            let mut data = vec![0x70u8, 0xa0, 0x82, 0x31];
            data.extend(words(&[U256::from_be_slice(account.as_slice())]));
            data
        }),
        gas_limit: READ_GAS + MARGIN,
        gas_used: READ_GAS,
        status: StepStatus::Success,
        logs: Vec::new(),
        measured: Some(MeasuredValue {
            binding: binding.name(),
            value,
        }),
    }
}

fn native_read(index: usize, binding: Binding, value: U256, account: Address) -> ExecutedStep {
    ExecutedStep::native_read(index, account, 0, binding, value)
}

/// The route, as the simulation left it: 10 plan steps, 6 of them transactions, in the shape
/// the M7 candidate's priced run has — wrap, two legs each with a transfer ahead of it, two
/// `balanceOf` reads and a native read between them, unwrap last.
fn simulated_run() -> SimulationResult {
    let account = sender();
    let steps = vec![
        native_read(0, Binding::SenderNativeStart, u(PURSE), account),
        call(
            Call {
                index: 1,
                nonce: 0,
                position: 0,
                to: weth(),
                value: u(INPUT),
                signature: "deposit()",
                selector: [0xd0, 0xe3, 0x0d, 0xb0],
                args: Vec::new(),
                gas_used: STEP_GAS[0],
            },
            account,
        ),
        call(
            Call {
                index: 2,
                nonce: 1,
                position: 1,
                to: weth(),
                value: U256::ZERO,
                signature: "transfer(address,uint256)",
                selector: [0xa9, 0x05, 0x9c, 0xbb],
                args: vec![U256::from_be_slice(pool_a().as_slice()), u(INPUT)],
                gas_used: STEP_GAS[1],
            },
            account,
        ),
        call(
            Call {
                index: 3,
                nonce: 2,
                position: 2,
                to: pool_a(),
                value: U256::ZERO,
                signature: "swap(uint256,uint256,address,bytes)",
                selector: [0x02, 0x2c, 0x04, 0x00],
                args: vec![u(MID), U256::ZERO, U256::from_be_slice(account.as_slice())],
                gas_used: STEP_GAS[2],
            },
            account,
        ),
        balance_read(4, 3, mid(), Binding::SenderMidReceived, u(MID), account),
        call(
            Call {
                index: 5,
                nonce: 4,
                position: 3,
                to: mid(),
                value: U256::ZERO,
                signature: "transfer(address,uint256)",
                selector: [0xa9, 0x05, 0x9c, 0xbb],
                args: vec![U256::from_be_slice(pool_b().as_slice()), u(MID)],
                gas_used: STEP_GAS[3],
            },
            account,
        ),
        call(
            Call {
                index: 6,
                nonce: 5,
                position: 4,
                to: pool_b(),
                value: U256::ZERO,
                signature: "swap(uint256,uint256,address,bytes)",
                selector: [0x02, 0x2c, 0x04, 0x00],
                args: vec![
                    U256::ZERO,
                    u(OUTPUT),
                    U256::from_be_slice(account.as_slice()),
                ],
                gas_used: STEP_GAS[4],
            },
            account,
        ),
        balance_read(7, 6, weth(), Binding::SenderInputEnd, u(OUTPUT), account),
        native_read(8, Binding::SenderNativeEnd, simulated_native_end(), account),
        call(
            Call {
                index: 9,
                nonce: 7,
                position: 5,
                to: weth(),
                value: U256::ZERO,
                signature: "withdraw(uint256)",
                selector: [0x2e, 0x1a, 0x7d, 0x4d],
                args: vec![u(OUTPUT)],
                gas_used: STEP_GAS[5],
            },
            account,
        ),
    ];
    let gas_used = STEP_GAS.iter().sum::<u64>() + 2 * READ_GAS;
    let gas_cost = u(gas_used as u128 * CHARGED_PRICE as u128);
    SimulationResult {
        chain_id: ChainId(CHAIN),
        block: BlockPin::new(BlockNumber(PIN), pinned_hash()),
        state_source: "the pinned dump fixtures/simulation-m7/dump-37530593-07D4af6E.json"
            .to_string(),
        sender: account,
        status: SimStatus::Completed,
        steps,
        gas_charge: GasCharge::Priced {
            gas_used,
            effective_gas_price: CHARGED_PRICE as u128,
            base_fee_per_gas: Some(BASE_FEE as u128),
            wei: gas_cost,
            pricing: GasPricing::Eip1559 {
                priority_fee_per_gas: 0,
                provenance: "the base fee measured at block 37,530,593".to_string(),
            },
        },
        measurements: vec![
            MeasuredValue {
                binding: Binding::SenderNativeStart.name(),
                value: u(PURSE),
            },
            MeasuredValue {
                binding: Binding::SenderMidReceived.name(),
                value: u(MID),
            },
            MeasuredValue {
                binding: Binding::SenderInputEnd.name(),
                value: u(OUTPUT),
            },
            MeasuredValue {
                binding: Binding::SenderNativeEnd.name(),
                value: simulated_native_end(),
            },
        ],
        compared: OutputComparison {
            analytical: u(INPUT) + u(PROFIT),
            simulated: Some(u(OUTPUT)),
        },
        outcome: SimulatedOutcome::Executed {
            output: u(OUTPUT),
            movement: Movement::Increased { by: u(PROFIT) },
        },
        gross_profit: Some(u(PROFIT)),
        gross_loss: None,
        net_profit: NetProfit::Gain {
            amount: u(PROFIT) - gas_cost,
            gross: u(PROFIT),
            gas_cost,
            denomination: Denomination {
                token: evm_core::TokenId::new(ChainId(CHAIN), weth()),
                converted: u(OUTPUT),
                native_start: u(PURSE),
                native_end: simulated_native_end(),
                native_spent: u(INPUT),
                gas_paid: gas_cost,
                proved_by: "withdraw(…) executed on the wrapped gas asset".to_string(),
            },
        },
        state_changes: StateChanges::default(),
        slippage: SlippageRecord {
            expected_output: u(OUTPUT),
            policy: SlippagePolicy::Exact,
            minimum_output: u(OUTPUT),
        },
        plan_summary: PlanSummary {
            steps: vec![
                "deposit()".to_string(),
                "transfer WETH to the buying venue".to_string(),
                "swap on the buying venue".to_string(),
                "transfer the mid token to the selling venue".to_string(),
                "swap on the selling venue".to_string(),
                "withdraw()".to_string(),
            ],
            evm_rules: "prague".to_string(),
            pools: vec![pool_a(), pool_b()],
            input_token: weth(),
            input_amount: u(INPUT),
            analytical_mid_amount: u(MID),
            analytical_output: u(OUTPUT),
            priced_by: "the candidate's measured 3000 ppm fee ceiling".to_string(),
        },
    }
}

/// The accept §2's hand-off requires. Its numbers are the run's own, so an intent built from
/// this decision carries a floor the route then has to clear.
fn accepted() -> RiskDecision {
    RiskDecision::Accept {
        net_profit_wei: u(PROFIT) - u((STEP_GAS.iter().sum::<u64>() + 2 * READ_GAS) as u128 * 362),
        gross_profit_wei: u(PROFIT),
        gas_cost_wei: u((STEP_GAS.iter().sum::<u64>() + 2 * READ_GAS) as u128 * 362),
        gas_used: STEP_GAS.iter().sum::<u64>() + 2 * READ_GAS,
        minimum_net_profit_wei: u(1),
        maximum_gas: 1_000_000,
        reason: "the run completed, its net figure is a gain, and it is under the ceiling"
            .to_string(),
    }
}

fn real_funding() -> SenderFunding {
    SenderFunding::RealState {
        source: "eth_getBalance of the test wallet at the pinned block, plus the deposit() the \
                 plan itself executes"
            .to_string(),
    }
}

/// §51, stated rather than defaulted: every route in this file comes out of a run the file
/// scripted, so its market is a controlled test market. That is what lets the sequence lane be
/// tested at all, and it is also why no number in this file may be read as a real arbitrage —
/// [`evm_execution::SequenceReport::counts_as_successful_real_arbitrage`] is false here by
/// construction, and one of the tests below asserts exactly that.
fn fixture_market() -> MarketKind {
    MarketKind::ControlledFixture {
        proves: "the sequence lane's pricing, ordering, audits and accounting, over a simulated \
                 run this file scripted"
            .to_string(),
    }
}

fn plan() -> SequencePlan {
    SequencePlan::from_run(
        &simulated_run(),
        &accepted(),
        "sequence-under-test",
        &format!("{PIN}/0"),
        real_funding(),
        fixture_market(),
    )
    .expect("the fixture run is a completed, accepted two-venue route")
}

/// A `Transfer` log as a chain log, for the decode tests that work on logs directly.
fn chain_log(
    address: Address,
    topics: Vec<alloy_primitives::B256>,
    data: Vec<u8>,
    block_number: u64,
    log_index: u64,
) -> evm_chain::ChainLog {
    evm_chain::ChainLog {
        chain_id: ChainId(CHAIN),
        block_number: BlockNumber(block_number),
        tx_hash: TxHash(alloy_primitives::B256::left_padding_from(&[
            block_number as u8,
            log_index as u8,
        ])),
        tx_index: TxIndex(0),
        log_index: LogIndex(log_index),
        address,
        topics,
        data: Bytes::from(data),
    }
}

// ---------------------------------------------------------------------------
// The plan: six transactions, positions that are not step indices, and the gas the chain pays
// ---------------------------------------------------------------------------

#[test]
fn the_plan_is_the_run_s_transactions_in_order_and_nothing_else() {
    let plan = plan();
    assert_eq!(plan.len(), 6, "wrap, two legs, unwrap");
    assert!(!plan.is_empty());
    // The run has 10 steps and 6 transactions, so the two index spaces differ from step 1 on.
    // Nothing downstream can confuse them: `position` is what the sequence sends, and
    // `simulated_index` is what the run recorded.
    let simulated: Vec<usize> = plan.steps.iter().map(|step| step.simulated_index).collect();
    assert_eq!(simulated, vec![1, 2, 3, 5, 6, 9]);
    let positions: Vec<usize> = plan.steps.iter().map(|step| step.position).collect();
    assert_eq!(positions, vec![0, 1, 2, 3, 4, 5]);
    let signatures: Vec<&str> = plan
        .steps
        .iter()
        .map(|step| step.signature.as_str())
        .collect();
    assert_eq!(
        signatures,
        vec![
            "deposit()",
            "transfer(address,uint256)",
            "swap(uint256,uint256,address,bytes)",
            "transfer(address,uint256)",
            "swap(uint256,uint256,address,bytes)",
            "withdraw(uint256)",
        ]
    );
    for step in &plan.steps {
        let intent = &step.intent;
        assert_eq!(intent.chain_id, CHAIN);
        assert_eq!(intent.block_number, BlockNumber(PIN));
        assert_eq!(intent.block_hash, pinned_hash());
        assert_eq!(intent.sender, sender());
        assert_eq!(intent.tx_type, TransactionType::DynamicFee);
        assert_eq!(intent.simulated_steps, 10);
        let position = intent.sequence.expect("a sequence step names its position");
        assert_eq!(position.index, step.position);
        assert_eq!(position.count, 6);
        // The fee is a fact about the moment of sending, not about the plan.
        assert_eq!(intent.max_fee_per_gas, None);
    }
    // The nonce the plan carries is the simulation's own account numbering, gaps included: two
    // of the ten steps are `balanceOf` calls that consumed a nonce inside the EVM and never go
    // on chain. What goes on chain is the lane's — 0 through 5, contiguous — and the driven
    // test below is where that substitution is pinned.
    let simulated_nonces: Vec<u64> = plan.steps.iter().map(|step| step.intent.nonce).collect();
    assert_eq!(simulated_nonces, vec![0, 1, 2, 4, 5, 7]);
    // Only the wrap carries value; every other step pays from a balance the step before made.
    assert_eq!(plan.input_native_wei(), u(INPUT));
    assert_eq!(broadcastable(&simulated_run()).len(), 6);
}

#[test]
fn a_position_that_is_not_the_run_s_nth_transaction_is_refused() {
    let run = simulated_run();
    let executable = broadcastable(&run);
    // Step 1 is transaction 0. Claiming it is transaction 1 of 6 would let one intent jump
    // ahead of the route, and §4's permission to build a multi-step intent is exactly the
    // agreement between the two index spaces.
    let error = TransactionIntent::from_simulated_step(
        &run,
        &accepted(),
        "wrong-position",
        &format!("{PIN}/0"),
        real_funding(),
        evm_execution::intent::SequencePosition { index: 1, count: 6 },
        executable[0],
    )
    .expect_err("a step cannot claim a position it does not hold");
    assert!(
        error.to_string().contains("which is not that transaction"),
        "{error}"
    );

    // And a count that is not this run's transaction count is refused too, whatever step is
    // offered with it — this is what stops a caller relabelling a 6-step route as 2 of 2.
    let error = TransactionIntent::from_simulated_step(
        &run,
        &accepted(),
        "wrong-count",
        &format!("{PIN}/0"),
        real_funding(),
        evm_execution::intent::SequencePosition { index: 0, count: 2 },
        executable[0],
    )
    .expect_err("the count has to be the run's own");
    assert!(
        error.to_string().contains("this run has 6 steps"),
        "{error}"
    );
}

#[test]
fn the_expected_gas_is_what_the_chain_will_be_charged_for() {
    let run = simulated_run();
    let plan = SequencePlan::from_run(
        &run,
        &accepted(),
        "gas-expectation",
        &format!("{PIN}/0"),
        real_funding(),
        fixture_market(),
    )
    .expect("the fixture run is acceptable");
    let whole_plan: u64 = run.steps.iter().map(|step| step.gas_used).sum();
    assert_eq!(plan.expected_gas_used, STEP_GAS.iter().sum::<u64>());
    assert_eq!(whole_plan, 606_000);
    assert_ne!(
        plan.expected_gas_used,
        run.gas_used(),
        "the run's bill includes two balance reads the chain is never asked to make, so the \
         §18 gas line would compare a chain total against a simulation total and call the \
         difference a mismatch"
    );
}

#[test]
fn the_snapshots_read_the_input_token_even_when_it_never_transferred() {
    // A route that only ever wrapped and unwrapped would, without this, be audited against
    // snapshots that never touch the asset the profit is in.
    let mut run = simulated_run();
    for step in &mut run.steps {
        step.logs = step
            .logs
            .iter()
            .filter(|log| log.topics.first() != Some(&V2Topics::default().transfer))
            .cloned()
            .collect();
    }
    let bare = SequencePlan::from_run(
        &run,
        &accepted(),
        "no-transfers",
        &format!("{PIN}/0"),
        real_funding(),
        fixture_market(),
    )
    .expect("a plan with no Transfer logs is still a plan");
    assert_eq!(bare.tokens, vec![weth()]);
    // And with the real logs, both the mid token and the wrapped asset are read.
    let with_flows = plan();
    assert_eq!(with_flows.tokens, vec![mid(), weth()]);
}

#[test]
fn a_run_the_risk_layer_did_not_accept_never_becomes_a_plan() {
    let decision = RiskDecision::Reject {
        rule: evm_risk::RiskRule::MinimumNetProfit,
        reason: "the net figure was under the floor".to_string(),
    };
    let error = SequencePlan::from_run(
        &simulated_run(),
        &decision,
        "rejected",
        &format!("{PIN}/0"),
        real_funding(),
        fixture_market(),
    )
    .expect_err("§2's boundary is a hard one");
    assert!(
        error.to_string().contains("RiskDecision::Accept"),
        "{error}"
    );
}

#[test]
fn a_reverted_run_becomes_no_plan_at_all() {
    let mut run = simulated_run();
    run.status = SimStatus::Reverted {
        step: 3,
        call: "swap(uint256,uint256,address,bytes)".to_string(),
        revert: evm_simulation::RevertData::new(Bytes::new()),
    };
    let error = SequencePlan::from_run(
        &run,
        &accepted(),
        "reverted",
        &format!("{PIN}/0"),
        real_funding(),
        fixture_market(),
    )
    .expect_err("a run that reverted has nothing to send");
    assert!(error.to_string().contains("did not complete"), "{error}");
}

// ---------------------------------------------------------------------------
// §22: the logs, including the two the ERC-20 interface does not have
// ---------------------------------------------------------------------------

#[test]
fn transfer_logs_decode_and_a_short_log_is_counted_not_dropped() {
    let account = sender();
    let good = transfer(account, pool_a(), weth(), u(INPUT));
    let topics = V2Topics::default();
    let logs = vec![
        chain_log(good.address, good.topics, good.data, PIN + 1, 0),
        // Same topic0, no `to` topic: an audit that silently lost this flow would report a
        // route that moved less than the chain says it did.
        chain_log(
            weth(),
            vec![topics.transfer, topic(account)],
            words(&[u(INPUT)]),
            PIN + 2,
            0,
        ),
        // Not a Transfer at all, so it is neither decoded nor counted.
        chain_log(weth(), vec![topics.swap], Vec::new(), PIN + 3, 0),
    ];
    let (flows, undecodable) = transfer_flows(&logs);
    assert_eq!(flows.len(), 1);
    assert_eq!(undecodable, 1);
    let flow = &flows[0];
    assert_eq!(flow.token, weth());
    assert_eq!(flow.from, account);
    assert_eq!(flow.to, pool_a());
    assert_eq!(flow.amount, u(INPUT));
    assert_eq!(flow.block_number, PIN + 1);
    assert_eq!(flow.log_index, 0);
}

#[test]
fn the_wrap_and_the_unwrap_are_read_because_no_transfer_reports_them() {
    let account = sender();
    let events = vec![
        (0, wrap(weth(), account, u(INPUT), true)),
        (5, wrap(weth(), account, u(OUTPUT), false)),
    ];
    let mut logs = Vec::new();
    for (position, event) in events {
        logs.push(chain_log(
            event.address,
            event.topics,
            event.data,
            PIN + position as u64 + 1,
            0,
        ));
    }
    // A Deposit on a different contract is not this route's wrap, and is not decoded.
    let stranger = wrap(mid(), account, u(MID), true);
    logs.push(chain_log(
        stranger.address,
        stranger.topics,
        stranger.data,
        PIN + 7,
        0,
    ));

    let (moves, undecodable) = wrap_moves(&logs, weth());
    assert_eq!(undecodable, 0);
    assert_eq!(moves.len(), 2);
    assert_eq!(
        moves,
        vec![
            WrapMove {
                token: weth(),
                account,
                amount: u(INPUT),
                mints: true,
                log_index: 0,
                block_number: PIN + 1,
                transaction_hash: moves[0].transaction_hash,
            },
            WrapMove {
                token: weth(),
                account,
                amount: u(OUTPUT),
                mints: false,
                log_index: 0,
                block_number: PIN + 6,
                transaction_hash: moves[1].transaction_hash,
            },
        ]
    );
    // The native the unwrap handed back is the burn amount, and the deposit is not part of it.
    let settled = moves
        .iter()
        .filter(|move_| !move_.mints)
        .fold(U256::ZERO, |total, move_| total + move_.amount);
    assert_eq!(settled, u(OUTPUT));
}

#[test]
fn swap_logs_are_attributed_to_the_pool_that_emitted_them() {
    let account = sender();
    let first = swap(pool_a(), account, u(INPUT), u(MID));
    let second = swap(pool_b(), account, u(MID), u(OUTPUT));
    let topics = V2Topics::default();
    let logs = vec![
        chain_log(first.address, first.topics, first.data, PIN + 3, 1),
        chain_log(second.address, second.topics, second.data, PIN + 7, 1),
        // Four amount words are what the declaration says; a truncated one is counted.
        chain_log(
            pool_a(),
            vec![topics.swap, topic(account), topic(account)],
            words(&[u(INPUT), U256::ZERO]),
            PIN + 9,
            0,
        ),
    ];
    let (observed, undecodable) = swap_observations(&logs);
    assert_eq!(observed.len(), 2);
    assert_eq!(undecodable, 1);
    assert_eq!(observed[0].pool, pool_a());
    assert_eq!(observed[1].pool, pool_b());
    assert_eq!(observed[0].amount0_in, u(INPUT));
    assert_eq!(observed[1].amount1_out, u(OUTPUT));
    assert_eq!(observed[0].recipient, account);
}

#[test]
fn both_legs_pass_only_when_each_venue_emitted_its_own_swap_in_route_order() {
    let account = sender();
    let pools = vec![pool_a(), pool_b()];
    let logs: Vec<evm_chain::ChainLog> = [2usize, 4]
        .into_iter()
        .flat_map(|position| step_events(position, account))
        .enumerate()
        .map(|(index, event)| {
            chain_log(
                event.address,
                event.topics,
                event.data,
                PIN + index as u64 / 2 + 1,
                (index % 2) as u64,
            )
        })
        .collect();
    let (swaps, _) = swap_observations(&logs);
    let audit = audit_route(&pools, &swaps, account);
    assert!(audit.passed, "{}", audit.detail);
    assert_eq!(audit.legs.len(), 2);
    assert_eq!(audit.legs[0].leg, 0);
    assert_eq!(audit.legs[1].pool, pool_b());
    assert_eq!(audit.legs[0].swaps_found, 1);
    assert!(audit.legs[0].first_swap.is_some());

    // The second venue silent: one leg ran, which is the half-trade §23 refuses to call an
    // arbitrage even though every transaction returned `status = 1`.
    let one_leg = audit_route(&pools, &swaps[..1], account);
    assert!(!one_leg.passed);
    assert!(
        one_leg.detail.contains("emitted no Swap log"),
        "{}",
        one_leg.detail
    );

    // Both venues active and both silent about nothing — but the selling venue ran first. The
    // two logs carry the same amounts in the opposite chain order, which is a different trade,
    // and leg order is the only thing that tells them apart. `settle` hands this function the
    // receipts' logs sorted by chain position, so the fixture does too.
    let sell = swap(pool_b(), account, u(MID), u(OUTPUT));
    let buy = swap(pool_a(), account, u(INPUT), u(MID));
    let sell_then_buy = vec![
        chain_log(sell.address, sell.topics, sell.data, PIN + 1, 0),
        chain_log(buy.address, buy.topics, buy.data, PIN + 2, 0),
    ];
    let (reversed, _) = swap_observations(&sell_then_buy);
    assert_eq!(reversed[0].pool, pool_b());
    let out_of_order = audit_route(&pools, &reversed, account);
    assert!(!out_of_order.passed);
    assert!(
        out_of_order.detail.contains("not after"),
        "{}",
        out_of_order.detail
    );
}

// ---------------------------------------------------------------------------
// §21: logs against balances, with the wrap counted on the log side
// ---------------------------------------------------------------------------

fn snapshot(
    block_number: u64,
    native: U256,
    tokens: Vec<(Address, U256)>,
) -> evm_execution::AssetSnapshot {
    evm_execution::AssetSnapshot {
        block_number,
        block_hash: block_hash(block_number),
        account: sender(),
        native_wei: native,
        token_balances: tokens.into_iter().collect::<BTreeMap<_, _>>(),
        provenance: format!("scripted reads at block {block_number}"),
    }
}

fn flows_and_moves(account: Address) -> (Vec<TokenFlow>, Vec<WrapMove>, usize, usize) {
    let mut logs = Vec::new();
    for (position, event) in (0..6).flat_map(|position| {
        step_events(position, account)
            .into_iter()
            .map(move |event| (position, event))
    }) {
        let count = logs.len() as u64;
        logs.push(chain_log(
            event.address,
            event.topics,
            event.data,
            PIN + position as u64 + 1,
            count % 3,
        ));
    }
    let (flows, flow_undecodable) = transfer_flows(&logs);
    let (moves, move_undecodable) = wrap_moves(&logs, weth());
    (flows, moves, flow_undecodable, move_undecodable)
}

#[test]
fn a_weth_balance_that_ended_where_it_started_agrees_once_the_wrap_is_counted() {
    let account = sender();
    let (flows, moves, undecodable, _) = flows_and_moves(account);
    assert_eq!(undecodable, 0);
    // The whole route: 6 transfers (two wraps emit none of them), and the wrapped asset moved
    // INPUT out and OUTPUT in — a `Transfer` net of the profit — while its balance ended where
    // it started because the unwrap burned all of it.
    assert_eq!(flows.len(), 4);
    assert_eq!(moves.len(), 2);
    let delta = evm_execution::BalanceDelta::new(
        snapshot(
            PIN,
            u(PURSE),
            vec![(weth(), U256::ZERO), (mid(), U256::ZERO)],
        ),
        snapshot(
            PIN + 6,
            settled_native(),
            vec![(weth(), U256::ZERO), (mid(), U256::ZERO)],
        ),
    )
    .expect("a before block older than the after block, for one account");
    let checks = reconcile_flows(&flows, Some(&delta), account, &moves);
    assert_eq!(checks.len(), 2);
    for check in &checks {
        assert!(check.agrees, "{}: {:?} {check:?}", check.token, check);
    }
    let wrapped = checks
        .iter()
        .find(|check| check.token == weth())
        .expect("the wrapped asset is audited");
    assert_eq!(wrapped.minted, u(INPUT));
    assert_eq!(wrapped.burned, u(OUTPUT));
    assert_eq!(wrapped.log_net, I256::from_raw(u(PROFIT)));
    assert_eq!(wrapped.balance_delta, I256::ZERO);

    // Without the wrap side the same evidence reads as a disagreement, which is the mistake
    // this module must not make: the asset that funds the route would look stolen from.
    let no_moves = reconcile_flows(&flows, Some(&delta), account, &[]);
    let unwrapped = no_moves
        .iter()
        .find(|check| check.token == weth())
        .expect("the wrapped asset is still audited");
    assert!(!unwrapped.agrees);
}

#[test]
fn a_mid_token_that_ended_with_more_than_the_logs_explain_is_reported_not_absorbed() {
    let account = sender();
    let (flows, moves, _, _) = flows_and_moves(account);
    // The wallet holds 1 000 MID more than its Transfers account for: a tax the simulation did
    // not see, a second swap nobody asked for, or an unlogged mint — this module cannot tell,
    // and §21's answer is to say the two measurements disagree.
    let delta = evm_execution::BalanceDelta::new(
        snapshot(
            PIN,
            u(PURSE),
            vec![(weth(), U256::ZERO), (mid(), U256::ZERO)],
        ),
        snapshot(
            PIN + 6,
            settled_native(),
            vec![(weth(), U256::ZERO), (mid(), u(1_000))],
        ),
    )
    .expect("the two pins are one account, forwards in time");
    let checks = reconcile_flows(&flows, Some(&delta), account, &moves);
    let mid_check = checks
        .iter()
        .find(|check| check.token == mid())
        .expect("the mid token is audited");
    assert_eq!(mid_check.log_net, I256::ZERO);
    assert!(mid_check.minted.is_zero() && mid_check.burned.is_zero());
    assert!(!mid_check.agrees);
    // Every token both sides touched gets a line, and one read but never moved gets one too.
    let tokens: Vec<Address> = checks.iter().map(|check| check.token).collect();
    assert_eq!(tokens, vec![mid(), weth()]);
}

#[test]
fn a_token_the_snapshots_never_read_still_gets_a_line() {
    let account = sender();
    let (flows, moves, _, _) = flows_and_moves(account);
    let delta = evm_execution::BalanceDelta::new(
        snapshot(PIN, u(PURSE), vec![]),
        snapshot(PIN + 6, settled_native(), vec![]),
    )
    .expect("one account, forwards in time");
    let checks = reconcile_flows(&flows, Some(&delta), account, &moves);
    assert_eq!(checks.len(), 2, "both tokens the receipts moved");
    assert!(
        checks.iter().all(|check| !check.agrees),
        "an unread balance is a disagreement, not a pass: {checks:?}"
    );
}

// ---------------------------------------------------------------------------
// §18: the tolerance and the audit it decides
// ---------------------------------------------------------------------------

#[test]
fn a_tolerance_of_zero_absorbs_nothing_but_an_exact_match() {
    let exact = Tolerance::new(0, 1);
    assert!(exact.within(u(100), u(100)));
    assert!(!exact.within(u(100), u(101)));
    assert!(!exact.within(u(100), u(99)));
    // A ratio of 1/N admits up to expected/N either way, and the boundary is inclusive.
    let hundredth = Tolerance::new(1, 100);
    assert!(hundredth.within(u(10_000), u(10_100)));
    assert!(hundredth.within(u(10_000), u(9_900)));
    assert!(!hundredth.within(u(10_000), u(10_101)));
    assert_eq!(hundredth.describe(), "1/100");
    // A zero expectation demands an exact match whatever the ratio: "100% of nothing" is not a
    // licence, which is the line §18's tolerance would otherwise wave through.
    assert!(Tolerance::new(1, 1).within(U256::ZERO, U256::ZERO));
    assert!(!Tolerance::new(1, 1).within(U256::ZERO, u(7)));
}

#[test]
fn a_missing_expected_is_recorded_as_uncompared_and_never_as_a_pass() {
    let mut audit = DeltaAudit::new(Tolerance::new(1, 100));
    audit.push("measured", None, u(5), "nothing produced this number");
    assert!(audit.lines.is_empty());
    assert_eq!(audit.uncompared.len(), 1);
    assert!(audit.uncompared[0].starts_with("measured:"));
    assert!(
        audit.outcome().is_ok(),
        "an uncompared quantity is not a mismatch"
    );
    assert!(audit.mismatched().is_empty());
}

#[test]
fn the_first_line_outside_tolerance_names_itself_and_becomes_an_execution_mismatch() {
    let mut audit = DeltaAudit::new(Tolerance::new(1, 1000));
    audit.push(
        "gas used across the sequence",
        Some(u(600_000)),
        u(900_000),
        "simulated total against bound receipts",
    );
    audit.push(
        "route's input token received back",
        Some(u(OUTPUT)),
        u(INPUT),
        "the selling venue paid less than the plan said",
    );
    assert_eq!(audit.mismatched().len(), 2);
    let error = audit.outcome().expect_err("both lines are out");
    let ExecutionError::ExecutionMismatch(why) = error else {
        panic!("a delta outside the tolerance is §47's mismatch, not a failure: {error:?}");
    };
    assert!(why.contains("expected 600000 actual 900000"), "{why}");
    assert!(why.contains("tolerance 1/1000"), "{why}");
    assert!(why.contains("simulated total"), "{why}");
}

// ---------------------------------------------------------------------------
// The scripted endpoint: the four traits plus the ERC-20 half of the snapshot
// ---------------------------------------------------------------------------

/// One endpoint answering every surface the sequence reads, recording everything it was handed.
///
/// Two behaviours are this file's own code written to a documented contract rather than the
/// crate's: it refuses to send when `may_submit()` says no, and it stamps the requested
/// transaction hash onto the receipt *and onto that receipt's logs*, because a flow audit whose
/// log provenance disagreed with its receipt would be testing the fixture.
struct Scripted {
    allowed: bool,
    /// Native balance by block. The gate asks at the pinned block; the two snapshots ask at
    /// their own pins.
    balances: HashMap<u64, U256>,
    /// `balanceOf` by (block, token); an unlisted pair reads zero.
    tokens: HashMap<(u64, Address), U256>,
    /// The chain's blocks: height → hash. Includes the pinned block and every mined one.
    blocks: HashMap<u64, alloy_primitives::B256>,
    /// What the lane sees per step. Advanced by the endpoint when it accepts a transaction, so
    /// the second step cannot be handed the first step's nonce.
    next_nonce: AtomicU64,
    answers: Mutex<VecDeque<SubmissionOutcome>>,
    receipts: Mutex<VecDeque<Option<Receipt>>>,
    sent: Mutex<Vec<Vec<u8>>>,
    /// M8.4.1 §12's account of the lane: every read this endpoint answered, with the label the
    /// lane had stamped on the run's trace at the instant it asked.
    reads: Mutex<Vec<LaneRead>>,
    /// The same sink handle the stage was given, so the endpoint can report what it was told
    /// rather than the test having to guess. `None` is the uninstrumented arm.
    trace: Option<RpcTraceSink>,
}

/// One read the lane asked for, as its endpoint saw it.
struct LaneRead {
    /// Which surface of the four this came in on — the endpoint's own name for the question.
    site: &'static str,
    /// The stage stamped at the moment of the ask, and the leg that asked, or `None` when
    /// nothing was watching.
    stage: Option<String>,
    caller: Option<String>,
}

impl Scripted {
    fn new(mode: ExecutionMode) -> Self {
        let mut blocks = HashMap::from([(PIN, pinned_hash()), (PIN - 1, block_hash(PIN - 1))]);
        let mut balances = HashMap::from([(PIN, u(PURSE)), (PIN - 1, u(PURSE))]);
        let mut receipts = VecDeque::new();
        for (position, gas) in STEP_GAS.iter().enumerate() {
            let block = PIN + position as u64 + 1;
            blocks.insert(block, block_hash(block));
            let account = sender();
            receipts.push_back(Some(Scripted::receipt(position, block, *gas, account)));
        }
        // The after-snapshot's balances: everything the route spent and settled is in
        // `settled_native`, and both tokens ended where they started.
        balances.insert(PIN + STEP_GAS.len() as u64, settled_native());
        Self {
            allowed: mode.may_submit(),
            balances,
            tokens: HashMap::new(),
            blocks,
            next_nonce: AtomicU64::new(0),
            answers: Mutex::new(VecDeque::new()),
            receipts: Mutex::new(receipts),
            sent: Mutex::new(Vec::new()),
            reads: Mutex::new(Vec::new()),
            trace: None,
        }
    }

    /// Answer reads while reporting the label the lane stamped on `trace` for each one.
    fn traced(mut self, trace: RpcTraceSink) -> Self {
        self.trace = Some(trace);
        self
    }

    /// Tally one read and the label it went out under.
    ///
    /// The label is read here, at the endpoint, because that is the moment the lane's ask has
    /// happened and its answer has not: a stamp the lane put after the read would be recorded
    /// as what followed it, and §9's question is what asked.
    fn read(&self, site: &'static str) {
        let label = self
            .trace
            .as_ref()
            .and_then(|sink| sink.context())
            .map(|context| (Some(context.stage), Some(context.caller)));
        let (stage, caller) = label.unwrap_or((None, None));
        self.reads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(LaneRead {
                site,
                stage,
                caller,
            });
    }

    /// The reads this endpoint answered, in arrival order, each with its label.
    fn lane_reads(&self) -> Vec<LaneRead> {
        // A clone of the whole tally: `LaneRead` owns its strings, and the test that compares
        // two arms holds two of these endpoints, so this borrows nothing.
        self.reads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|read| LaneRead {
                site: read.site,
                stage: read.stage.clone(),
                caller: read.caller.clone(),
            })
            .collect()
    }

    /// The receipt for transaction `position`, in the shape the M6 probe measured on this
    /// chain, L1 fields included.
    fn receipt(position: usize, block_number: u64, gas_used: u64, account: Address) -> Receipt {
        let target = match position {
            0 | 1 | 5 => weth(),
            2 => pool_a(),
            3 => mid(),
            _ => pool_b(),
        };
        Receipt {
            transaction_hash: alloy_primitives::B256::ZERO,
            block_number,
            block_hash: block_hash(block_number),
            transaction_index: 0,
            success: true,
            gas_used,
            effective_gas_price: u(CHARGED_PRICE as u128),
            cumulative_gas_used: Some(u(gas_used as u128)),
            from: account,
            to: Some(target),
            contract_address: None,
            tx_type: Some(2),
            logs: step_events(position, account)
                .into_iter()
                .enumerate()
                .map(|(index, event)| {
                    chain_log(
                        event.address,
                        event.topics,
                        event.data,
                        block_number,
                        index as u64,
                    )
                })
                .collect(),
            l1_fee: Some(u(L1_FEE as u128)),
            l1_gas_price: Some(u(1_000)),
            l1_gas_used: Some(u(L1_FEE as u128)),
            l1_base_fee_scalar: Some(u(1_000)),
            l1_blob_base_fee: Some(U256::ZERO),
            l1_blob_base_fee_scalar: Some(U256::ZERO),
            provenance: "scripted eth_getTransactionReceipt in §54's ladder".to_string(),
        }
    }

    fn answer(self, outcome: SubmissionOutcome) -> Self {
        self.answers
            .lock()
            .expect("an unlocked queue")
            .push_back(outcome);
        self
    }

    /// Mark one transaction as reverted. Its bill stays, because the chain charged it; its
    /// logs do not, because a revert discards the state changes *and* the events that would
    /// have reported them — and an audit that read a reverted receipt as a moved asset would
    /// credit the route with a leg the chain never ran.
    fn revert_at(mut self, position: usize) -> Self {
        let queue = self.receipts.get_mut().unwrap_or_else(|e| e.into_inner());
        if let Some(receipt) = queue[position].as_mut() {
            receipt.success = false;
            receipt.logs = Vec::new();
        }
        self
    }

    /// The route's first `count` transactions land; the rest have no receipt yet.
    fn receipts_for(mut self, count: usize, extra_native_spent: U256) -> Self {
        let queue = self.receipts.get_mut().unwrap_or_else(|e| e.into_inner());
        for (position, slot) in queue.iter_mut().enumerate() {
            if position >= count {
                *slot = None;
            }
        }
        let mut balances = self.balances.clone();
        let last = PIN + count as u64;
        let l2: U256 = STEP_GAS[..count].iter().fold(U256::ZERO, |total, gas| {
            total + u(*gas as u128) * u(CHARGED_PRICE as u128)
        });
        let l1 = u(L1_FEE as u128 * count as u128);
        balances.insert(last, u(PURSE) - u(INPUT) - l2 - l1 + extra_native_spent);
        balances.insert(PIN + STEP_GAS.len() as u64, settled_native());
        self.balances = balances;
        let mut tokens = self.tokens.clone();
        tokens.insert((last, mid()), u(MID));
        self.tokens = tokens;
        let mut blocks = self.blocks.clone();
        blocks.insert(last, block_hash(last));
        self.blocks = blocks;
        self
    }

    /// An extra `balanceOf` answer at one block, for the test that needs the wallet's balance to
    /// move in a way its Transfer logs do not account for.
    fn token_at(mut self, block_number: u64, token: Address, amount: U256) -> Self {
        self.tokens.insert((block_number, token), amount);
        self
    }

    fn sent_count(&self) -> usize {
        self.sent.lock().expect("an unlocked counter").len()
    }
}

#[async_trait]
impl FeeSource for Scripted {
    async fn fee_reading(
        &self,
        block_number: u64,
        block_hash: alloy_primitives::B256,
        tx_type: TransactionType,
        policy: &FeePolicy,
    ) -> evm_execution::Result<FeeReading> {
        self.read("fee_reading");
        policy.apply(
            CHAIN,
            block_number,
            block_hash,
            Some(u(BASE_FEE as u128)),
            Some(u(TIP as u128)),
            tx_type,
        )
    }

    async fn suggested_tip(&self) -> evm_execution::Result<Option<U256>> {
        self.read("suggested_tip");
        Ok(Some(u(TIP as u128)))
    }

    async fn balance(&self, address: Address, block_number: u64) -> evm_execution::Result<U256> {
        self.read("balance");
        Ok(self
            .balances
            .get(&block_number)
            .copied()
            .unwrap_or_else(|| u(PURSE) - u(INPUT) * u(address.is_zero() as u128)))
    }
}

#[async_trait]
impl AssetReader for Scripted {
    async fn token_balance(
        &self,
        token: Address,
        account: Address,
        block_number: u64,
    ) -> evm_execution::Result<AssetReading> {
        self.read("token_balance");
        Ok(AssetReading {
            amount: self
                .tokens
                .get(&(block_number, token))
                .copied()
                .unwrap_or(U256::ZERO),
            source: format!(
                "scripted eth_call balanceOf({account}, {token}) at block {block_number}"
            ),
        })
    }
}

#[async_trait]
impl NonceSource for Scripted {
    async fn nonce(&self, address: Address) -> evm_execution::Result<NonceReading> {
        self.read("nonce");
        let seen = self.next_nonce.load(Ordering::SeqCst);
        Ok(NonceReading {
            address,
            confirmed: seen,
            pending: seen,
            at_block: PIN,
            source: format!(
                "scripted eth_getTransactionCount (confirmed and pending) at block {PIN}"
            ),
        })
    }
}

#[async_trait]
impl ChainReader for Scripted {
    async fn block_hash_at(
        &self,
        number: BlockNumber,
    ) -> evm_execution::Result<Option<alloy_primitives::B256>> {
        self.read("block_hash_at");
        Ok(self.blocks.get(&number.0).copied())
    }

    async fn endpoint_chain_id(&self) -> evm_execution::Result<u64> {
        self.read("endpoint_chain_id");
        Ok(CHAIN)
    }
}

#[async_trait]
impl TransactionSubmitter for Scripted {
    fn endpoint(&self) -> EndpointKind {
        EndpointKind::PublicHttpRpc
    }

    fn may_submit(&self) -> bool {
        self.allowed
    }

    async fn submit(
        &self,
        transaction: &SignedTransaction,
    ) -> evm_execution::Result<SubmissionOutcome> {
        if !self.may_submit() {
            return Err(ExecutionError::ModeGate(format!(
                "submission was asked of an endpoint that may not broadcast ({})",
                self.endpoint().name()
            )));
        }
        self.sent
            .lock()
            .expect("an unlocked counter")
            .push(transaction.raw().to_vec());
        let outcome = self
            .answers
            .lock()
            .expect("an unlocked queue")
            .pop_front()
            .unwrap_or(SubmissionOutcome::Accepted {
                transaction_hash: None,
                hash_matches_local: true,
                endpoint: EndpointKind::PublicHttpRpc,
                detail: "scripted eth_sendRawTransaction acknowledgement".to_string(),
            });
        if matches!(outcome, SubmissionOutcome::Accepted { .. }) {
            self.next_nonce.fetch_add(1, Ordering::SeqCst);
        }
        Ok(outcome)
    }

    async fn receipt(
        &self,
        transaction_hash: alloy_primitives::B256,
    ) -> evm_execution::Result<Option<Receipt>> {
        let next = self
            .receipts
            .lock()
            .expect("an unlocked queue")
            .pop_front()
            .flatten();
        Ok(next.map(|mut receipt| {
            receipt.transaction_hash = transaction_hash;
            for log in &mut receipt.logs {
                log.tx_hash = TxHash(transaction_hash);
            }
            receipt
        }))
    }
}

/// The stage over a scripted endpoint, with the receipt budget cut to two fast reads.
///
/// Returns the endpoint back as well: [`SequenceStage`] holds the only other handles on it, and
/// the count that proves "nothing was sent" has to be read from outside.
fn assemble(
    endpoint: Scripted,
    mode: ExecutionMode,
    tolerance: Tolerance,
) -> (SequenceStage, Arc<Scripted>) {
    let scripted = Arc::new(endpoint);
    let abilities = Abilities {
        submitter: scripted.clone(),
        fees: scripted.clone(),
        nonces: scripted.clone(),
        chain: scripted.clone(),
    };
    let setup = ExecutionSetup {
        mode,
        fee: FeePolicy::BaseFeeHeadroom { headroom_blocks: 2 },
        build: BuildPolicy {
            expected_chain_id: CHAIN,
            gas: GasPolicy::SimulationGasPlus { margin: MARGIN },
            ..Default::default()
        },
        receipts: ReceiptPolicy {
            attempts: 2,
            between_attempts: Duration::from_millis(1),
        },
    };
    // §19: `BuildOnly` runs with no key present at all, which is what makes its stop a ceiling
    // rather than a near miss.
    let signer = match mode {
        ExecutionMode::BuildOnly => Signer::without_key(mode),
        other => test_signer(other),
    };
    let stage = SequenceStage::new(
        abilities,
        scripted.clone(),
        signer,
        setup,
        CHAIN,
        Clock::new(),
        tolerance,
    )
    .expect("a sequence stage over a scripted endpoint");
    (stage, scripted)
}

fn arbitrage_attempt() -> GateAttempt {
    GateAttempt::Arbitrage {
        simulation_success: true,
        risk_accepted: true,
        freshness: evm_execution::Freshness::Active,
    }
}

/// The head the caller read immediately before sending: one block behind the first inclusion.
fn before_head() -> SnapshotPin {
    SnapshotPin {
        block_number: PIN - 1,
        block_hash: block_hash(PIN - 1),
    }
}

/// §26's verdict, in the shape the stage asks for.
///
/// The thirteen checks themselves are `preflight`'s own tests; what the sequence stage
/// decides on is only whether a gate cleared *this* attempt, so a hand-built report is the
/// honest fixture here — the stage must not be trusted to re-check the arithmetic behind it
/// (§27), and it must not be trusted to accept one that failed or that names another
/// opportunity, which is what the three refusal tests below pin.
fn preflight_ok(plan: &SequencePlan) -> PreflightReport {
    let ceiling = STEP_GAS.iter().sum::<u64>() as u128 * CHARGED_PRICE as u128 + 6 * L1_FEE as u128;
    PreflightReport {
        attempt_id: plan.opportunity_id.clone(),
        findings: Vec::new(),
        passed: true,
        repriced_output_wei: Some(u(OUTPUT)),
        expected_net_after_costs_wei: Some(I256::from_raw(u(PROFIT))),
        rejected_because: None,
        sequence_ceiling_wei: u(ceiling),
        // The hand-built verdict of §54's tests proves the *gate*, not M8.4.4's producer leg, so
        // it propagates nothing: the sequence stage then records `no_context` per step and still
        // reads the block at its own pin, which is the §30 shape — a stage that was handed
        // nothing says so, and does not pretend the producer answered.
        block_context: ProducerOutcome::Refused(ContextRefusal::UnverifiableBlockContext(
            "this fixture hand-builds a verdict and gathers no read of the block".to_string(),
        )),
    }
}

async fn drive(stage: &mut SequenceStage, metrics: &mut Metrics) -> evm_execution::SequenceReport {
    let plan = plan();
    let verdict = preflight_ok(&plan);
    stage
        .run(
            &plan,
            &arbitrage_attempt(),
            Some(&verdict),
            &before_head(),
            metrics,
        )
        .await
}

/// M8.4.4 §5's producer leg, expressed over this fixture's own pin.
///
/// The identity comes from the plan rather than from the `PIN` constant on purpose: the
/// consumer's expected side is built by production code off `step.intent`, so an assertion
/// that hardcoded a height here would pass even if the stage started checking a different
/// block than the one it sends against.
fn pinned_identity() -> BlockIdentity {
    let intent = &plan().steps[0].intent;
    BlockIdentity {
        chain_id: ChainId(intent.chain_id),
        number: intent.block_number,
        hash: intent.block_hash,
    }
}

/// A verdict that carries a producer-verified context over `identity`.
///
/// `verify` is handed the same block on both sides, which is what a real producer leg looks
/// like when the endpoint answers the question it was asked: the two reads agreeing *is* the
/// proof, and nothing here is trusted past it. The scope is passed in because §8 makes it a
/// separate question from identity, and the tests below keep it separate too.
fn verdict_carrying(identity: &BlockIdentity, scope: BlockContextScope) -> PreflightReport {
    let context = VerifiedBlockContext::verify(
        identity,
        identity,
        scope,
        "scripted eth_getBlockByNumber at the pin",
        "the §5 producer leg of this test",
        1_700_000_000_000,
    )
    .expect("a block read that answers the question it was asked verifies against itself");
    let plan = plan();
    let mut verdict = preflight_ok(&plan);
    verdict.block_context = ProducerOutcome::Verified(context);
    verdict
}

/// [`drive`] with the verdict chosen by the caller, because M8.4.4's question is what the
/// build stage does with a context handed to it, and `preflight_ok` propagates none.
async fn drive_with_verdict(
    stage: &mut SequenceStage,
    metrics: &mut Metrics,
    verdict: &PreflightReport,
) -> evm_execution::SequenceReport {
    let plan = plan();
    stage
        .run(
            &plan,
            &arbitrage_attempt(),
            Some(verdict),
            &before_head(),
            metrics,
        )
        .await
}

/// The one line M8.4.4's consumer leg writes per step it drives, or `None` when it wrote none.
fn context_line(report: &evm_execution::SequenceReport) -> String {
    let lines: Vec<&String> = report
        .sources
        .iter()
        .filter(|source| source.contains("block context"))
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "one step driven, so exactly one consumer answer: {lines:?}"
    );
    lines[0].clone()
}

// ---------------------------------------------------------------------------
// §54: the whole route driven
// ---------------------------------------------------------------------------

/// The claim M7 accepts on: six transactions went out in the plan's order, and the run can
/// state — from receipts, balances and logs — that the wallet ended with more than it started
/// with, after both halves of the bill.
#[tokio::test]
async fn the_full_route_settles_verified_positive_from_six_transactions() {
    let (mut stage, scripted) = assemble(
        Scripted::new(ExecutionMode::Submit),
        ExecutionMode::Submit,
        Tolerance::new(1, 100),
    );
    let mut metrics = Metrics::default();
    let report = drive(&mut stage, &mut metrics).await;

    assert!(report.completed, "{}", report.detail);
    assert_eq!(report.steps_planned, 6);
    assert_eq!(scripted.sent_count(), 6);
    assert!(report.mismatch.is_none(), "{}", report.detail);
    // §54's full climb: the route is on chain, settled, and its profit is a verified verdict.
    assert_eq!(
        report.reached,
        Some(ExecutionStatus::ProfitVerified),
        "{}",
        report.detail
    );
    assert!(matches!(report.lane, LaneRelease::Released));

    // One lane, one nonce per transaction, in the plan's order, and six different hashes.
    let positions: Vec<usize> = report
        .transactions
        .iter()
        .map(|step| step.position)
        .collect();
    assert_eq!(positions, vec![0, 1, 2, 3, 4, 5]);
    let nonces: Vec<u64> = report.transactions.iter().map(|step| step.nonce).collect();
    assert_eq!(nonces, vec![0, 1, 2, 3, 4, 5]);
    let blocks: Vec<u64> = report
        .transactions
        .iter()
        .map(|step| step.block_number)
        .collect();
    assert_eq!(
        blocks,
        (1..=6).map(|offset| PIN + offset).collect::<Vec<_>>()
    );
    let hashes = report
        .transactions
        .iter()
        .map(|step| step.transaction_hash)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(hashes.len(), 6, "six transactions, six hashes");
    for (step, gas) in report.transactions.iter().zip(STEP_GAS) {
        assert_eq!(step.gas_limit, gas + MARGIN);
        assert_eq!(step.receipt_status, ReceiptStatus::Included);
        assert_eq!(step.logs.len(), step_events(step.position, sender()).len());
    }

    // §22/§23: both venues produced a Swap, in route order, and the flows close.
    let route = report.route.as_ref().expect("a completed route is audited");
    assert!(route.passed, "{}", route.detail);
    assert_eq!(route.legs.len(), 2);
    assert_eq!(route.legs[0].pool, pool_a());
    assert_eq!(route.legs[1].pool, pool_b());
    assert_eq!(report.wraps.len(), 2, "the wrap and the unwrap");
    assert_eq!(report.flows.len(), 4, "two legs in and two legs out");
    let checks = &report.flow_checks;
    assert_eq!(checks.len(), 2);
    for check in checks {
        assert!(check.agrees, "{check:?}");
    }

    // §18: every line the simulation measured has a chain number beside it, and all four fit.
    assert!(report.deltas.uncompared.is_empty());
    assert_eq!(report.deltas.lines.len(), 4);
    assert!(report.deltas.mismatched().is_empty());
    assert!(report.deltas.outcome().is_ok());

    // §37: the bill is per transaction, both halves, with the L1 source named.
    let cost = report
        .cost
        .clone()
        .expect("six receipts are six cost lines");
    assert_eq!(cost.transaction_count, 6);
    assert_eq!(cost.lines.len(), 6);
    assert_eq!(cost.l2_fee_total, l2_total());
    assert_eq!(cost.l1_fee_total, l1_total());
    assert_eq!(cost.total_execution_cost, l2_total() + l1_total());
    for line in &cost.lines {
        assert_eq!(line.l1_fee, u(L1_FEE as u128));
        assert!(line.is_fully_measured());
    }
    assert!(
        report
            .sources
            .iter()
            .any(|source| source.contains("preflight passed")),
        "§26's verdict belongs in the evidence next to the numbers it cleared: {:?}",
        report.sources
    );
    assert!(
        report
            .sources
            .iter()
            .any(|source| source.contains("receipt field `l1Fee`")),
        "§37 asks the report to state where the L1 fee came from"
    );

    // §24/§39: the equation, and the status only a closed equation may claim.
    let profit = report
        .profit
        .clone()
        .expect("a settled route is accounted for");
    assert_eq!(profit.block_before, PIN - 1);
    assert_eq!(profit.block_after, PIN + 6);
    assert_eq!(
        profit.realized.gross_profit,
        I256::from_raw(u(PROFIT)),
        "the round trip's own difference, before any cost"
    );
    let costs = l2_total() + l1_total();
    assert_eq!(
        profit.realized.net_profit,
        Some(I256::from_raw(u(PROFIT)) - I256::from_raw(costs)),
        "§12's net figure is the wallet's own native difference"
    );
    assert!(
        profit.realized.net_profit.unwrap() > I256::ZERO,
        "and it is positive: {} wei of profit against {} wei of gas and {} wei of L1 fee",
        u(PROFIT),
        l2_total(),
        l1_total()
    );
    let equation = profit
        .equation
        .as_ref()
        .expect("a one-unit claim is recomputable");
    assert!(equation.checks_out);
    assert_eq!(
        equation.observed_final_balance,
        I256::from_raw(settled_native())
    );
    assert_eq!(equation.terms.len(), 5);
    assert_eq!(profit.status, ProfitVerificationStatus::VerifiedPositive);
    assert!(profit.status.counts_as_successful_real_arbitrage());

    // §55: the same numbers are on the ledger's record, as route totals rather than the
    // first receipt's, so the evidence file a report reads has one place to look.
    let record = stage
        .ledger()
        .get(report.execution_id.as_deref().expect("a claimed record"))
        .expect("the attempt has a record");
    assert_eq!(record.status, ExecutionStatus::ProfitVerified);
    assert_eq!(record.route_transactions, Some(6));
    assert_eq!(record.gas_used, Some(STEP_GAS.iter().sum()));
    assert_eq!(record.input_amount, Some(u(INPUT)));
    assert_eq!(record.gross_output, Some(u(OUTPUT)));
    assert_eq!(record.gross_profit, Some(I256::from_raw(u(PROFIT))));
    assert_eq!(record.l2_fee, Some(l2_total()));
    assert_eq!(record.l1_fee, Some(l1_total()));
    assert_eq!(record.total_fee, Some(costs));
    assert_eq!(record.realized_profit, profit.realized.net_profit);
    assert_eq!(record.profit_status, Some(profit.status));
    // The record's own hash and block belong to the first transaction of six — the line
    // above that says how many there were is what keeps that from reading as a mistake.
    assert_eq!(
        record.transaction_hash,
        Some(report.transactions[0].transaction_hash)
    );
    assert_eq!(record.execution_block, Some(PIN + 1));
    assert!(record.settled_at_ms.is_some());
    assert!(record.profit_verified_at_ms.is_some());
    assert!(record.was_sent());

    assert_eq!(metrics.get("execution_sequence_attempt"), 1);
    assert_eq!(metrics.get("execution_sequence_mismatch"), 0);
    assert_eq!(metrics.get("execution_sequence_route_incomplete"), 0);
    assert_eq!(
        metrics.get("execution_sequence_profit_verified_positive"),
        1
    );
    // §54's three new rungs each earned their counter exactly once, in ladder order.
    assert_eq!(metrics.get("execution_preflight_success"), 1);
    assert_eq!(metrics.get("execution_settle_success"), 1);
    assert_eq!(metrics.get("execution_profit_verified"), 1);
    assert_eq!(metrics.get("execution_receipt_success"), 1);
}

/// §51: the label decides the sentence, not the arithmetic.
///
/// Two runs with identical numbers — same fixture route, same receipts, same verified-positive
/// profit — differing only in whether the market produced the price gap or the run arranged it.
/// The controlled one must not be countable as a real arbitrage and the real-market one must
/// be, because that single bit is the difference between "the execution system works" and
/// "this bot found money on a market", and §51 says only the second may be reported as
/// M7's `Real Arbitrage = COMPLETE`.
#[tokio::test]
async fn a_verified_profit_on_a_controlled_market_is_not_a_real_arbitrage() {
    let (mut stage, _) = assemble(
        Scripted::new(ExecutionMode::Submit),
        ExecutionMode::Submit,
        Tolerance::new(1, 100),
    );
    let mut metrics = Metrics::default();
    let report = drive(&mut stage, &mut metrics).await;

    // The economics are the passing ones from the test above; only the label is under test.
    assert!(report.completed, "{}", report.detail);
    let profit = report
        .profit
        .clone()
        .expect("a settled route is accounted for");
    assert_eq!(profit.status, ProfitVerificationStatus::VerifiedPositive);
    assert!(profit.status.counts_as_successful_real_arbitrage());
    assert_eq!(report.market, fixture_market());

    // And yet the run counts as zero real arbitrages.
    assert!(
        !report.counts_as_successful_real_arbitrage(),
        "a fixture's profit proves the lane works, not that a market offered the route: {}",
        report.line()
    );
    assert_eq!(
        report.to_json()["market"]["counts_as_real_arbitrage"],
        serde_json::json!(false)
    );
    assert!(report.line().contains("CONTROLLED_FIXTURE"));

    // The same route, labelled as a market that actually offered it.
    let (mut stage, _) = assemble(
        Scripted::new(ExecutionMode::Submit),
        ExecutionMode::Submit,
        Tolerance::new(1, 100),
    );
    let mut plan = plan();
    plan.market = MarketKind::RealMarket {
        attested_by: "reserves read from the endpoint at the pinned block, fee measured by \
                      `data/evidence/m7/candidate-fee-measurement.json`"
            .to_string(),
    };
    let verdict = preflight_ok(&plan);
    let mut metrics = Metrics::default();
    let report = stage
        .run(
            &plan,
            &arbitrage_attempt(),
            Some(&verdict),
            &before_head(),
            &mut metrics,
        )
        .await;
    assert!(report.completed, "{}", report.detail);
    assert_eq!(
        report.profit.as_ref().expect("settled").status,
        ProfitVerificationStatus::VerifiedPositive
    );
    assert!(report.counts_as_successful_real_arbitrage());
    assert_eq!(
        report.to_json()["market"]["counts_as_real_arbitrage"],
        serde_json::json!(true)
    );
}

/// A route that stopped halfway is a fact about the wallet, not a gap in the record (§34): the
/// reverted transaction keeps its bill, the audits run over what actually landed, and the loss
/// is stated and verified rather than left inconclusive.
#[tokio::test]
async fn a_revert_in_the_middle_keeps_its_bill_and_becomes_a_proven_loss() {
    let endpoint = Scripted::new(ExecutionMode::Submit)
        .receipts_for(4, U256::ZERO)
        .revert_at(3);
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit, Tolerance::new(1, 100));
    let mut metrics = Metrics::default();
    let report = drive(&mut stage, &mut metrics).await;

    // Four transactions went out; the fourth was included and failed.
    assert_eq!(scripted.sent_count(), 4);
    assert!(!report.completed);
    assert_eq!(report.stopped_at, Some(3));
    assert_eq!(report.transactions.len(), 4);
    assert_eq!(
        report.transactions[3].receipt_status,
        ReceiptStatus::Reverted
    );
    assert!(
        report.transactions[3].logs.is_empty(),
        "a revert discards its events, so the flow audit cannot see the leg that did not happen"
    );
    // The shared record is stamped by the route's first transaction, and that one really did
    // land — but M7 no longer calls inclusion final: the attempt stopped halfway, so its
    // record ends `Failed` with the revert named, while the *money* question is answered
    // separately by the profit lines §55 put on the same record.
    assert_eq!(report.reached, Some(ExecutionStatus::Failed));
    // The receipt named the failure, so the nonce question is settled: the lane lets go.
    assert!(matches!(report.lane, LaneRelease::Released));

    let costs = STEP_GAS[..4].iter().fold(U256::ZERO, |total, gas| {
        total + u(*gas as u128) * u(CHARGED_PRICE as u128)
    }) + u(L1_FEE as u128 * 4);
    let cost = report
        .cost
        .clone()
        .expect("four transactions are four bills");
    assert_eq!(cost.transaction_count, 4);
    assert_eq!(cost.total_execution_cost, costs);

    // §18: the plan said two legs and the chain ran one, so all four lines disagree.
    assert_eq!(report.deltas.mismatched().len(), 4);
    let error = report
        .mismatch
        .clone()
        .expect("the disagreement is the finding");
    let ExecutionError::ExecutionMismatch(why) = error else {
        panic!("a half route is §47's mismatch, not a failure: {error:?}");
    };
    assert!(why.contains("mid token"), "{why}");
    assert!(report.detail.contains("reverted"), "{}", report.detail);

    // The route audit sees one venue's Swap: the second leg never happened, and saying so is
    // the difference between a partial record and a false one.
    let route = report
        .route
        .as_ref()
        .expect("the run still audits its route");
    assert!(!route.passed);
    assert_eq!(route.legs[1].swaps_found, 0);

    // Yet the flows still close: the wallet minted the input, spent it, took the mid token, and
    // still holds it. Nothing is absorbed; nothing is called a discrepancy that is not one.
    for check in &report.flow_checks {
        assert!(check.agrees, "{check:?}");
    }
    let held = report
        .flow_checks
        .iter()
        .find(|check| check.token == mid())
        .expect("the mid token is audited");
    assert_eq!(held.log_net, I256::from_raw(u(MID)));

    let profit = report
        .profit
        .clone()
        .expect("a proven loss is still a profit statement");
    assert_eq!(profit.status, ProfitVerificationStatus::VerifiedNegative);
    assert!(!profit.status.counts_as_successful_real_arbitrage());
    let equation = profit.equation.as_ref().expect("the loss is recomputable");
    assert!(equation.checks_out);
    assert_eq!(profit.realized.final_balance, settled_after_revert());
    // The wallet paid for the wrap and for four transactions' bill and got no native back: the
    // mid token it still holds is an asset, not a settlement, and §13 refuses to net the two.
    assert_eq!(
        profit.realized.net_profit,
        Some(I256::ZERO - I256::from_raw(u(INPUT)) - I256::from_raw(costs))
    );
    assert!(profit.realized.net_profit.unwrap() < I256::ZERO);

    // §34 and §55 on one record: the attempt stopped, so its ladder rung is `Failed` and its
    // `failure` names what the chain did — and the money question is still answered in full,
    // because a proven loss is a finding rather than a missing number.
    let record = stage
        .ledger()
        .get(report.execution_id.as_deref().expect("a claimed record"))
        .expect("the attempt has a record");
    assert_eq!(record.status, ExecutionStatus::Failed);
    assert!(record
        .failure
        .as_deref()
        .is_some_and(|why| why.contains("reverted")));
    assert_eq!(record.route_transactions, Some(4));
    assert_eq!(record.total_fee, Some(costs));
    assert_eq!(record.realized_profit, profit.realized.net_profit);
    assert_eq!(
        record.profit_status,
        Some(ProfitVerificationStatus::VerifiedNegative)
    );
    // The two rungs above inclusion are not climbed, because `Failed` is terminal: the ladder
    // says the attempt stopped even though the settlement reached a final verdict. That is why
    // §56's count reads `profit_status`, not the status word.
    assert_eq!(record.settled_at_ms, None);
    assert_eq!(record.profit_verified_at_ms, None);

    assert_eq!(metrics.get("execution_sequence_mismatch"), 1);
    assert_eq!(
        metrics.get("execution_sequence_profit_verified_negative"),
        1
    );
    assert_eq!(metrics.get("execution_sequence_route_incomplete"), 1);
    assert_eq!(metrics.get("execution_settle_success"), 0);
    assert_eq!(metrics.get("execution_profit_verified"), 0);
}

/// The wallet's native balance at the block the half route ended in: the purse, minus the wrap,
/// minus four transactions' two-part bill. Nothing came back — the unwrap was never sent.
fn settled_after_revert() -> U256 {
    u(PURSE)
        - u(INPUT)
        - STEP_GAS[..4].iter().fold(U256::ZERO, |total, gas| {
            total + u(*gas as u128) * u(CHARGED_PRICE as u128)
        })
        - u(L1_FEE as u128 * 4)
}

/// §20's binding is about time, not about reading: a "before" snapshot taken at or after the
/// route's own blocks is a picture of the result. The run still sends — the gate cannot see
/// ahead — but it refuses to state a delta afterwards.
#[tokio::test]
async fn a_before_read_at_the_route_s_last_block_claims_no_delta() {
    let (mut stage, scripted) = assemble(
        Scripted::new(ExecutionMode::Submit),
        ExecutionMode::Submit,
        Tolerance::new(1, 100),
    );
    let mut metrics = Metrics::default();
    let plan = plan();
    let late = SnapshotPin {
        block_number: PIN + 6,
        block_hash: block_hash(PIN + 6),
    };
    let report = stage
        .run(
            &plan,
            &arbitrage_attempt(),
            Some(&preflight_ok(&plan)),
            &late,
            &mut metrics,
        )
        .await;

    assert_eq!(scripted.sent_count(), 6);
    assert!(
        report.completed,
        "the transactions themselves landed: {}",
        report.detail
    );
    assert!(report.after.is_none());
    assert!(report.profit.is_none());
    assert!(
        report.cost.is_none(),
        "no delta, so no bill totals to compare"
    );
    assert!(report.deltas.lines.is_empty());
    assert!(
        report.detail.contains("§20's binding is unusable"),
        "{}",
        report.detail
    );
    assert_eq!(metrics.get("execution_sequence_unsettled"), 1);
    // §54: `Settled` is a claim about the audits, and this run formed none of them, so the
    // record stops at the rung the chain itself earned.
    assert_eq!(report.reached, Some(ExecutionStatus::Included));
    let record = stage
        .ledger()
        .get(report.execution_id.as_deref().expect("a claimed record"))
        .expect("the attempt has a record");
    assert_eq!(record.realized_profit, None);
    assert_eq!(record.profit_status, None);
    assert_eq!(record.settled_at_ms, None);
    assert_eq!(
        record.route_transactions,
        Some(1),
        "unsettled, so the only numbers on the record are still the one receipt's"
    );
}

/// §19/§20: `BuildOnly` has no key to read, so the route stops at the end of the build step and
/// nothing is handed to a node. The stop is a ceiling, not a near miss.
#[tokio::test]
async fn build_only_stops_at_built_and_sends_nothing() {
    let (mut stage, scripted) = assemble(
        Scripted::new(ExecutionMode::BuildOnly),
        ExecutionMode::BuildOnly,
        Tolerance::new(1, 100),
    );
    let mut metrics = Metrics::default();
    let report = drive(&mut stage, &mut metrics).await;

    assert_eq!(scripted.sent_count(), 0);
    assert!(report.transactions.is_empty());
    assert_eq!(report.stopped_at, Some(0));
    assert_eq!(report.reached, Some(ExecutionStatus::Built));
    assert!(
        report.detail.contains("stops at Built"),
        "{}",
        report.detail
    );
    assert!(matches!(report.lane, LaneRelease::Released));
    // §20's snapshot was taken — the run needs it to say what it cannot claim.
    assert!(report.before.is_some());
    assert!(report.after.is_none());
    assert!(report.profit.is_none());
    assert!(report.cost.is_none());
    assert_eq!(metrics.get("execution_sequence_blocked"), 1);
    assert_eq!(metrics.get("execution_sequence_submission_submitted"), 0);
}

/// The other half of the mode ladder: bytes exist, a node exists, and the mode says no. The
/// endpoint's own `may_submit` is what stops the call, and the reason names the endpoint.
#[tokio::test]
async fn sign_only_never_hands_bytes_to_the_node() {
    let (mut stage, scripted) = assemble(
        Scripted::new(ExecutionMode::SignOnly),
        ExecutionMode::SignOnly,
        Tolerance::new(1, 100),
    );
    let mut metrics = Metrics::default();
    let report = drive(&mut stage, &mut metrics).await;

    assert_eq!(scripted.sent_count(), 0);
    assert!(report.transactions.is_empty());
    assert_eq!(report.stopped_at, Some(0));
    assert_eq!(report.reached, Some(ExecutionStatus::Signed));
    assert!(
        report
            .detail
            .contains("signed and was never handed to a node"),
        "{}",
        report.detail
    );
    assert!(report
        .sources
        .iter()
        .any(|source| source.contains("signed bytes exist and were never sent")));
    assert!(matches!(report.lane, LaneRelease::Released));
}

/// §25: the node's answer left a transaction possibly in flight. Nothing is resent, the lane
/// keeps the nonce, and the run does not pretend to know what the chain did.
#[tokio::test]
async fn an_unknown_submission_answer_holds_the_lane_and_resends_nothing() {
    let endpoint = Scripted::new(ExecutionMode::Submit).answer(SubmissionOutcome::Unknown {
        reason: "connection reset before the node answered".to_string(),
        endpoint: EndpointKind::PublicHttpRpc,
    });
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit, Tolerance::new(1, 100));
    let mut metrics = Metrics::default();
    let report = drive(&mut stage, &mut metrics).await;

    assert_eq!(scripted.sent_count(), 1, "one attempt, and no second one");
    assert_eq!(report.stopped_at, Some(0));
    assert!(!report.completed);
    let LaneRelease::Held { reason } = &report.lane else {
        panic!(
            "an unknown answer must hold the lane, got {:?}",
            report.lane
        );
    };
    assert!(reason.contains("§25"), "{reason}");
    assert!(reason.contains("stays reserved"), "{reason}");
    assert!(
        report.detail.contains("nothing is resent"),
        "{}",
        report.detail
    );
    // The record keeps the rung it honestly reached: the bytes were signed and handed over,
    // and nothing after that is known. Writing it as `Submitted` would claim an acceptance the
    // node never gave, and `Failed` would claim nothing is in flight.
    assert_eq!(report.reached, Some(ExecutionStatus::Signed));
    assert_eq!(metrics.get("execution_sequence_submission_unknown"), 1);
    assert_eq!(metrics.get("execution_sequence_blocked"), 1);
    assert_eq!(metrics.get("execution_sequence_stopped"), 0);
    assert!(report.transactions.is_empty());
    assert!(report.profit.is_none());
}

/// §30/§33: one attempt per state binding. A second run over the same plan is refused by the
/// ledger before it reads, builds, signs or sends anything.
#[tokio::test]
async fn a_second_attempt_on_the_same_binding_sends_nothing_new() {
    let (mut stage, scripted) = assemble(
        Scripted::new(ExecutionMode::Submit),
        ExecutionMode::Submit,
        Tolerance::new(1, 100),
    );
    let mut metrics = Metrics::default();
    let first = drive(&mut stage, &mut metrics).await;
    let second = drive(&mut stage, &mut metrics).await;

    assert_eq!(
        scripted.sent_count(),
        6,
        "the first run's six and nothing else"
    );
    assert_eq!(second.execution_id, first.execution_id);
    assert!(!second.completed);
    assert!(second.transactions.is_empty());
    assert!(second.detail.starts_with("§30"), "{}", second.detail);
    assert_eq!(second.reached, first.reached);
    assert_eq!(metrics.get("execution_duplicate"), 1);
    assert!(stage.lane_is_idle());
}

/// §26/§54: an arbitrage that never went through the gate cannot reach `Built`, and the
/// refusal costs nothing — no head read for the snapshot, no nonce, no fee read, no bytes.
/// The rung order is the assertion: `execution_preflight_success` stays 0 and so does
/// `execution_build_success`, because the ladder was stopped at the first rung §54 added.
#[tokio::test]
async fn an_arbitrage_with_no_preflight_verdict_never_gets_built() {
    let (mut stage, scripted) = assemble(
        Scripted::new(ExecutionMode::Submit),
        ExecutionMode::Submit,
        Tolerance::new(1, 100),
    );
    let mut metrics = Metrics::default();
    let plan = plan();
    let report = stage
        .run(
            &plan,
            &arbitrage_attempt(),
            None,
            &before_head(),
            &mut metrics,
        )
        .await;

    assert_eq!(scripted.sent_count(), 0);
    assert!(report.transactions.is_empty());
    assert_eq!(report.stopped_at, Some(0));
    assert!(report.detail.contains("§26"), "{}", report.detail);
    assert_eq!(report.reached, Some(ExecutionStatus::Failed));
    let record = stage
        .ledger()
        .get(report.execution_id.as_deref().expect("a claimed record"))
        .expect("the attempt has a record");
    assert!(record
        .failure
        .as_deref()
        .is_some_and(|why| why.contains("§26")));
    assert!(record.included_at_ms.is_none());
    assert_eq!(metrics.get("execution_preflight_blocked"), 1);
    assert_eq!(metrics.get("execution_preflight_success"), 0);
    assert_eq!(metrics.get("execution_build_success"), 0);
    assert_eq!(metrics.get("execution_sequence_stopped"), 1);
    assert!(
        stage.lane_is_idle(),
        "nothing was spent, so nothing is held"
    );
}

/// §26 with a verdict that says no: the report's own §39 mapping becomes the stop reason, so
/// a stale reserve reads as a stale opportunity here rather than as a generic refusal.
#[tokio::test]
async fn a_rejected_preflight_verdict_stops_the_route_with_its_own_reason() {
    let (mut stage, scripted) = assemble(
        Scripted::new(ExecutionMode::Submit),
        ExecutionMode::Submit,
        Tolerance::new(1, 100),
    );
    let mut metrics = Metrics::default();
    let plan = plan();
    let mut verdict = preflight_ok(&plan);
    verdict.passed = false;
    verdict.findings = vec![PreflightFinding {
        check: PreflightCheck::PoolReserves,
        passed: false,
        detail: "pool A's reserves no longer clear the quote".to_string(),
    }];
    let report = stage
        .run(
            &plan,
            &arbitrage_attempt(),
            Some(&verdict),
            &before_head(),
            &mut metrics,
        )
        .await;

    assert_eq!(scripted.sent_count(), 0);
    assert_eq!(report.stopped_at, Some(0));
    assert!(
        report.detail.contains("stale opportunity"),
        "{}",
        report.detail
    );
    assert_eq!(report.reached, Some(ExecutionStatus::Failed));
    assert_eq!(metrics.get("execution_preflight_blocked"), 1);
    assert_eq!(metrics.get("execution_preflight_success"), 0);
}

/// §27's rule made operational: a PASS belonging to another candidate is not this candidate's
/// gate. Accepting it would be the candidate switch the task book forbids, wearing evidence.
#[tokio::test]
async fn a_preflight_verdict_for_another_attempt_is_refused() {
    let (mut stage, scripted) = assemble(
        Scripted::new(ExecutionMode::Submit),
        ExecutionMode::Submit,
        Tolerance::new(1, 100),
    );
    let mut metrics = Metrics::default();
    let plan = plan();
    let mut verdict = preflight_ok(&plan);
    verdict.attempt_id = "the candidate that was not chosen".to_string();
    let report = stage
        .run(
            &plan,
            &arbitrage_attempt(),
            Some(&verdict),
            &before_head(),
            &mut metrics,
        )
        .await;

    assert_eq!(scripted.sent_count(), 0);
    assert!(report.detail.contains("§27"), "{}", report.detail);
    assert!(
        report.detail.contains(&plan.opportunity_id),
        "the refusal names the attempt that actually needed the gate: {}",
        report.detail
    );
    assert_eq!(metrics.get("execution_preflight_blocked"), 1);
    assert_eq!(metrics.get("execution_preflight_success"), 0);
}

/// §8/§31: a stale finding does not become a transaction. The gate refuses it at step 0, before
/// any bytes exist, and the run does not go looking for a fresher candidate (§27).
#[tokio::test]
async fn a_stale_attempt_stops_at_the_gate() {
    let (mut stage, scripted) = assemble(
        Scripted::new(ExecutionMode::Submit),
        ExecutionMode::Submit,
        Tolerance::new(1, 100),
    );
    let mut metrics = Metrics::default();
    let attempt = GateAttempt::Arbitrage {
        simulation_success: true,
        risk_accepted: true,
        freshness: evm_execution::Freshness::Stale {
            reason: "the quote behind this finding lost its lifecycle lease".to_string(),
        },
    };
    let plan = plan();
    let report = stage
        .run(
            &plan,
            &attempt,
            Some(&preflight_ok(&plan)),
            &before_head(),
            &mut metrics,
        )
        .await;

    assert_eq!(scripted.sent_count(), 0);
    assert_eq!(report.stopped_at, Some(0));
    assert!(report.detail.contains("stale"), "{}", report.detail);
    assert_eq!(report.reached, Some(ExecutionStatus::Failed));
    assert_eq!(metrics.get("execution_gate_blocked"), 1);
    assert!(matches!(report.lane, LaneRelease::Released));
    assert!(report.transactions.is_empty());
}

/// §21: a balance the logs do not explain. The route can still settle and still be profitable —
/// the money moved the way the plan said — and the audit has to say, in the report's own words,
/// that one token's balance moved by more than its `Transfer` logs cover rather than letting the
/// difference disappear into a pass.
#[tokio::test]
async fn a_balance_the_logs_do_not_explain_is_stated_as_a_flow_gap() {
    let endpoint = Scripted::new(ExecutionMode::Submit).token_at(PIN + 6, weth(), u(500));
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit, Tolerance::new(1, 100));
    let mut metrics = Metrics::default();
    let report = drive(&mut stage, &mut metrics).await;

    assert_eq!(scripted.sent_count(), 6);
    let check = report
        .flow_checks
        .iter()
        .find(|check| check.token == weth())
        .expect("the wrapped asset is audited");
    assert!(check.balance_read, "both snapshots read it");
    assert!(!check.agrees, "{check:?}");
    assert_eq!(check.balance_delta, I256::from_raw(u(500)));
    assert!(
        report
            .sources
            .iter()
            .any(|source| source.contains("flow audit:") && source.contains(&format!("{}", weth()))),
        "the gap is stated in words: {:?}",
        report.sources
    );
    assert_eq!(metrics.get("execution_sequence_flow_unreconciled"), 1);
}

// ---------------------------------------------------------------------------
// M8.4.1 §9, §11, §12: the execution lane under the same trace as the simulation
// ---------------------------------------------------------------------------

/// A sink with no simulation behind it — the lane's reads are the only ones it sees.
fn lane_sink() -> RpcTraceSink {
    RpcTraceSink::new(
        std::time::Instant::now(),
        "lane-under-test",
        RpcTraceSource::Live,
        Some(CHAIN),
    )
}

/// §9, §11: which leg of the lane asked for each read.
///
/// M8.3.2's `outside-simulation-rpc.json` could only say *that* the execution lane read the
/// node; it could not say which of the lane's own legs a given `eth_getBalance` belonged to,
/// because the lane opened a socket no sink was watching. The lane now answers through the
/// run's own adapter, and this table is what that buys: ten named questions, each carrying the
/// stage and the caller the code stamped before asking it.
///
/// The order matters as much as the labels. The three before-snapshot rows come first because
/// §20's snapshot is taken before any transaction is priced; the five step-1 rows then follow
/// [`SequenceStage::drive_step`]'s recipe — price, nonce, then §26's gate — and the run ends
/// there because `BuildOnly` refuses to read a key. Nothing in the fixture decides that order:
/// it is the crate's control flow, and the assertion is written against what the endpoint saw.
#[tokio::test]
async fn every_lane_read_arrives_labelled_with_the_leg_that_asked() {
    let sink = lane_sink();
    let endpoint = Scripted::new(ExecutionMode::BuildOnly).traced(sink.clone());
    let (stage, scripted) = assemble(endpoint, ExecutionMode::BuildOnly, Tolerance::new(1, 100));
    let mut stage = stage.with_rpc_trace(sink);
    let mut metrics = Metrics::default();
    let report = drive(&mut stage, &mut metrics).await;
    assert_eq!(scripted.sent_count(), 0);
    let reads = scripted.lane_reads();
    let table: Vec<(&str, Option<&str>, Option<&str>)> = reads
        .iter()
        .map(|read| (read.site, read.stage.as_deref(), read.caller.as_deref()))
        .collect();
    assert_eq!(
        table,
        vec![
            (
                "balance",
                Some("build"),
                Some("before-snapshot: native and input-token balances")
            ),
            (
                "token_balance",
                Some("build"),
                Some("before-snapshot: native and input-token balances")
            ),
            (
                "token_balance",
                Some("build"),
                Some("before-snapshot: native and input-token balances")
            ),
            (
                "fee_reading",
                Some("build"),
                Some("step 1: fee at pinned block")
            ),
            (
                "nonce",
                Some("build"),
                Some("step 1: pending and latest nonces")
            ),
            (
                "endpoint_chain_id",
                Some("build"),
                Some("step 1: gate — endpoint chain id")
            ),
            (
                "block_hash_at",
                Some("build"),
                Some("step 1: gate — block binding at pin")
            ),
            (
                "balance",
                Some("build"),
                Some("step 1: gate — native balance")
            ),
        ],
        "{table:#?}"
    );
    // §20's ceiling, stated as a number rather than as prose: `BuildOnly` stops at the first
    // step's sign, so the after-snapshot and every later step's reads never happen.
    assert_eq!(report.stopped_at, Some(0));
    assert_eq!(report.reached, Some(ExecutionStatus::Built));
}

/// §11, §17: a run that does send the route labels *every* read, and none of them lands
/// outside the four stages the lane's own work belongs to.
///
/// The count is the interesting part: 3 + 6×6 + 3. Two snapshots of three reads each (native
/// plus the route's two tokens), and per step five reads under `build` — price, nonce, and
/// §26's three gate facts — plus one `block_hash_at` under `receipt`, where
/// [`crate::sequence::ReceiptTracker`] re-binds the block the receipt arrived in. A read that
/// arrived unlabelled would mean a leg of the lane the trace cannot name, which is exactly the
/// gap §11 forbids leaving silently.
#[tokio::test]
async fn a_route_that_sends_labels_every_read_it_asked_for() {
    let sink = lane_sink();
    let endpoint = Scripted::new(ExecutionMode::Submit).traced(sink.clone());
    let (stage, scripted) = assemble(endpoint, ExecutionMode::Submit, Tolerance::new(1, 100));
    let mut stage = stage.with_rpc_trace(sink);
    let mut metrics = Metrics::default();
    let report = drive(&mut stage, &mut metrics).await;
    assert!(report.completed, "{}", report.detail);

    let reads = scripted.lane_reads();
    assert_eq!(reads.len(), 42, "3 + 6 steps × 6 + 3");
    let unlabelled: Vec<&str> = reads
        .iter()
        .filter(|read| read.stage.is_none() || read.caller.is_none())
        .map(|read| read.site)
        .collect();
    assert!(unlabelled.is_empty(), "{unlabelled:?}");

    let stages: std::collections::BTreeSet<&str> = reads
        .iter()
        .map(|read| read.stage.as_deref().unwrap_or_default())
        .collect();
    assert_eq!(
        stages,
        std::collections::BTreeSet::from(["build", "receipt", "settlement"])
    );
    // The last snapshot is the settlement one, and it is the only place `settlement` appears —
    // a label that had leaked out of `settle` would show up here as a second run of rows.
    let settlement = reads
        .iter()
        .filter(|read| read.stage.as_deref() == Some("settlement"))
        .count();
    assert_eq!(settlement, 3);
    assert!(report.after.is_some(), "{}", report.detail);
}

/// §12: the instrument did not add, drop or reorder a call, and did not change what the run
/// concluded.
///
/// Two arms, same fixture, same mode — one with the sink on both the stage and its endpoint,
/// one with neither. The comparison is on the endpoint's own tally (what the lane asked for,
/// in the order it asked) and on the report's non-timestamp fields, because those are the
/// fields a run's *semantics* live in: a stamp that had also inserted a read, or re-ordered
/// one, or changed a label the code then read back, would show up as a differing sequence, a
/// differing count, or a differing `detail`.
#[tokio::test]
async fn labelling_the_lane_changes_neither_its_calls_nor_its_verdict() {
    let sink = lane_sink();
    let (mut traced_stage, traced_endpoint) = assemble(
        Scripted::new(ExecutionMode::BuildOnly).traced(sink.clone()),
        ExecutionMode::BuildOnly,
        Tolerance::new(1, 100),
    );
    traced_stage = traced_stage.with_rpc_trace(sink);
    let mut traced_metrics = Metrics::default();
    let traced = drive(&mut traced_stage, &mut traced_metrics).await;

    let (mut plain_stage, plain_endpoint) = assemble(
        Scripted::new(ExecutionMode::BuildOnly),
        ExecutionMode::BuildOnly,
        Tolerance::new(1, 100),
    );
    let mut plain_metrics = Metrics::default();
    let plain = drive(&mut plain_stage, &mut plain_metrics).await;

    let traced_sites: Vec<&str> = traced_endpoint
        .lane_reads()
        .iter()
        .map(|read| read.site)
        .collect();
    let plain_sites: Vec<&str> = plain_endpoint
        .lane_reads()
        .iter()
        .map(|read| read.site)
        .collect();
    assert_eq!(traced_sites, plain_sites);
    assert_eq!(traced_sites.len(), plain_sites.len());
    // The instrumented arm is the one that has labels to lose, so a bare count match is only
    // meaningful if that arm really was watched.
    assert!(traced_endpoint
        .lane_reads()
        .iter()
        .all(|read| read.stage.is_some()));
    assert!(plain_endpoint
        .lane_reads()
        .iter()
        .all(|read| read.stage.is_none()));

    assert_eq!(traced.completed, plain.completed);
    assert_eq!(traced.steps_planned, plain.steps_planned);
    assert_eq!(traced.reached, plain.reached);
    assert_eq!(traced.stopped_at, plain.stopped_at);
    assert_eq!(traced.detail, plain.detail);
    assert_eq!(traced.sources, plain.sources);
    assert_eq!(traced.transactions.len(), plain.transactions.len());
    assert_eq!(
        traced.before.map(|snapshot| snapshot.provenance),
        plain.before.map(|snapshot| snapshot.provenance)
    );
    assert_eq!(
        traced_metrics.counters.entries(),
        plain_metrics.counters.entries()
    );
}

// ---------------------------------------------------------------------------
// M8.4.4 §5/§6/§14: the consumer leg, run by the stage that has to check for itself
// ---------------------------------------------------------------------------

/// §6's pass case at the only scale that proves the wiring — the real stage, the real gate, the
/// real halt: the build stage is handed a context naming the block it reads for itself, and it
/// records `accepted` with the consumer's own read still in hand. That second half is the whole
/// difference between checking and believing, and §2 forbids the belief.
#[tokio::test]
async fn the_build_stage_accepts_a_context_that_names_the_block_it_reads_for_itself() {
    let (mut stage, scripted) = assemble(
        Scripted::new(ExecutionMode::BuildOnly),
        ExecutionMode::BuildOnly,
        Tolerance::new(1, 100),
    );
    let mut metrics = Metrics::default();
    let identity = pinned_identity();
    let report = drive_with_verdict(
        &mut stage,
        &mut metrics,
        &verdict_carrying(&identity, BlockContextScope::FixedHistorical),
    )
    .await;

    let line = context_line(&report);
    assert!(
        line.starts_with("step 1: block context accepted for "),
        "{line}"
    );
    // the identity the consumer checked against is the step's own pin, printed on the line so
    // the evidence says which block was agreed about rather than only that someone agreed
    assert!(line.contains(&identity.describe()), "{line}");
    assert!(line.contains("consumer read made: true"), "{line}");
    assert_eq!(metrics.get("execution_block_context_accepted"), 1);
    assert_eq!(metrics.get("execution_block_context_rejected"), 0);
    assert_eq!(metrics.get("execution_block_context_no_context"), 0);

    // Accepting a context changes no decision the mode had already made: the step is built and
    // stops there, with no key read, no signature and nothing handed to a node.
    assert_eq!(report.reached, Some(ExecutionStatus::Built));
    assert!(
        report.detail.contains("stops at Built"),
        "{}",
        report.detail
    );
    assert_eq!(scripted.sent_count(), 0);
}

/// §7's invalidation and §19's negative control, run end to end. The producer's answer is a real
/// verification of a real block — just not the block this step pins, at the same height. Before
/// this milestone nothing in the crate compared the two, so a disagreement between two stages
/// about one block was not a fact anybody could count; now it is a named rejection that lands
/// before any signature is asked for.
#[tokio::test]
async fn a_context_about_another_block_at_the_same_height_is_refused_by_name() {
    let (mut stage, scripted) = assemble(
        Scripted::new(ExecutionMode::BuildOnly),
        ExecutionMode::BuildOnly,
        Tolerance::new(1, 100),
    );
    let mut metrics = Metrics::default();
    let tampered = BlockIdentity {
        chain_id: ChainId(CHAIN),
        number: BlockNumber(PIN),
        hash: alloy_primitives::B256::repeat_byte(0x55),
    };
    assert_ne!(tampered.hash, pinned_hash());
    let report = drive_with_verdict(
        &mut stage,
        &mut metrics,
        &verdict_carrying(&tampered, BlockContextScope::FixedHistorical),
    )
    .await;

    let line = context_line(&report);
    assert!(
        line.starts_with("step 1: block context rejected for "),
        "{line}"
    );
    // both hashes are on the line: what this step pins, and what the context claims. A refusal
    // that named neither would be a sentence nobody could re-check (§17).
    assert!(line.contains(&format!("{:#x}", pinned_hash())), "{line}");
    assert!(line.contains(&format!("{:#x}", tampered.hash)), "{line}");
    assert_eq!(metrics.get("execution_block_context_rejected"), 1);
    assert_eq!(metrics.get("execution_block_context_accepted"), 0);

    // The refusal judges the propagated context, not this step's own pin: the gate's binding
    // read still answered confirmed, the build still happened, and the halt is still the mode's
    // own. §2's line — a consumer that cannot check must reject — buys a detector, not a new
    // way to fail a route that was sound.
    assert_eq!(report.reached, Some(ExecutionStatus::Built));
    assert!(
        report.detail.contains("stops at Built"),
        "{}",
        report.detail
    );
    assert_eq!(metrics.get("execution_gate_blocked"), 0);
    assert_eq!(scripted.sent_count(), 0);
}

/// §7's NC2 run end to end: the context is a true statement about a real block, and it is one
/// block off the pin this step sends against. Height is the first thing the consumer compares
/// after chain, so a context about the neighbouring block never reaches a hash comparison — and
/// the row says which two heights disagreed.
#[tokio::test]
async fn a_context_about_the_neighbouring_height_is_refused_before_the_hash_is_compared() {
    let (mut stage, scripted) = assemble(
        Scripted::new(ExecutionMode::BuildOnly),
        ExecutionMode::BuildOnly,
        Tolerance::new(1, 100),
    );
    let mut metrics = Metrics::default();
    let elsewhere = BlockIdentity {
        chain_id: ChainId(CHAIN),
        number: BlockNumber(PIN + 1),
        hash: pinned_hash(),
    };
    let report = drive_with_verdict(
        &mut stage,
        &mut metrics,
        &verdict_carrying(&elsewhere, BlockContextScope::FixedHistorical),
    )
    .await;

    let line = context_line(&report);
    assert!(
        line.starts_with("step 1: block context rejected for "),
        "{line}"
    );
    assert!(
        line.contains(&format!(
            "the two answers name blocks {PIN} and {}",
            PIN + 1
        )),
        "{line}"
    );
    assert_eq!(metrics.get("execution_block_context_rejected"), 1);
    assert_eq!(report.reached, Some(ExecutionStatus::Built));
    assert_eq!(scripted.sent_count(), 0);
}

/// §7's NC3 run end to end. A chain id is part of the identity (§25) precisely because a testnet
/// height and a mainnet height can be the same integer over the same-looking hash: the consumer
/// refuses on the chain alone, before it has any opinion about the block.
#[tokio::test]
async fn a_context_from_another_chain_is_refused_and_names_both_chains() {
    let (mut stage, scripted) = assemble(
        Scripted::new(ExecutionMode::BuildOnly),
        ExecutionMode::BuildOnly,
        Tolerance::new(1, 100),
    );
    let mut metrics = Metrics::default();
    let foreign = BlockIdentity {
        chain_id: ChainId(CHAIN + 1),
        number: BlockNumber(PIN),
        hash: pinned_hash(),
    };
    let report = drive_with_verdict(
        &mut stage,
        &mut metrics,
        &verdict_carrying(&foreign, BlockContextScope::FixedHistorical),
    )
    .await;

    let line = context_line(&report);
    assert!(
        line.starts_with("step 1: block context rejected for "),
        "{line}"
    );
    assert!(
        line.contains(&format!(
            "the two answers name chains {CHAIN} and {}",
            CHAIN + 1
        )),
        "{line}"
    );
    assert_eq!(metrics.get("execution_block_context_rejected"), 1);
    assert_eq!(report.reached, Some(ExecutionStatus::Built));
    assert_eq!(scripted.sent_count(), 0);
}

/// §14's gate and §2's, at the same time and at unit scale: the consumer leg must not add a read
/// to the step it judges, and must not retire the read that judges it. Three arms, three
/// different propagated contexts, one identical tally of lane sites — because the only thing that
/// differs between them is a judgement made on an answer the lane had already given.
#[tokio::test]
async fn all_three_consumer_answers_ask_the_lane_for_exactly_the_same_reads() {
    async fn consumer_arm(verdict: &PreflightReport) -> Vec<&'static str> {
        let (mut stage, scripted) = assemble(
            Scripted::new(ExecutionMode::BuildOnly),
            ExecutionMode::BuildOnly,
            Tolerance::new(1, 100),
        );
        let mut metrics = Metrics::default();
        let plan = plan();
        stage
            .run(
                &plan,
                &arbitrage_attempt(),
                Some(verdict),
                &before_head(),
                &mut metrics,
            )
            .await;
        let mut sites: Vec<&'static str> =
            scripted.lane_reads().iter().map(|read| read.site).collect();
        sites.sort_unstable();
        sites
    }

    let identity = pinned_identity();
    let tampered = BlockIdentity {
        chain_id: ChainId(CHAIN),
        number: BlockNumber(PIN),
        hash: alloy_primitives::B256::repeat_byte(0x55),
    };
    let accepted = consumer_arm(&verdict_carrying(
        &identity,
        BlockContextScope::FixedHistorical,
    ))
    .await;
    let refused = consumer_arm(&verdict_carrying(
        &tampered,
        BlockContextScope::FixedHistorical,
    ))
    .await;
    let plan = plan();
    let none = consumer_arm(&preflight_ok(&plan)).await;

    assert_eq!(
        accepted, refused,
        "a judgement must not cost an RPC or save one"
    );
    assert_eq!(accepted, none);
    // the positive control: the arm really did read the block the consumer compares against,
    // once, which is what makes §2's ban on deleting that read visible as a number.
    assert_eq!(
        accepted
            .iter()
            .filter(|site| **site == "block_hash_at")
            .count(),
        1,
        "{accepted:?}"
    );
    assert!(accepted.contains(&"endpoint_chain_id"), "{accepted:?}");
}

/// §21's controlled freshness test, on the path that actually runs.
///
/// Case A is the arm the evidence tree already publishes: a context about the pinned block, the
/// step reads that block, the two agree, accepted. Case B is this test. §21 forbids deciding the
/// answer in the test, so the answer is read off the two facts the production lane states:
///
/// * [`crate::sequence`]'s `Build` leg calls `consumer_check(.., None)` — §20 bans `latest` on
///   this path, so the stage that consumes a block context never asks what the head is;
/// * `consumer_check`'s own rule for a `LiveHead` context with no head answer is a named refusal,
///   because "verified while it was the head" says nothing about now (§8).
///
/// Those two together are the current freshness contract, and what they give here is a REJECT —
/// not because a head moved, but because this boundary has no honest way to say one hasn't. The
/// same verdict on a `FixedHistorical` context is accepted two lines lower, which is the whole
/// content of §8's distinction and the reason the refusal cannot be read as "propagation is
/// unsafe": the block identity leg passed first, and the step went on to build the identical
/// transaction from its own read either way (§30).
#[tokio::test]
async fn a_live_head_context_is_refused_at_the_build_boundary_that_reads_no_head() {
    let identity = pinned_identity();
    let live_head = verdict_carrying(&identity, BlockContextScope::LiveHead);
    let fixed_pin = verdict_carrying(&identity, BlockContextScope::FixedHistorical);

    async fn drive_with(verdict: &PreflightReport) -> (evm_execution::SequenceReport, Metrics) {
        let (mut stage, _) = assemble(
            Scripted::new(ExecutionMode::BuildOnly),
            ExecutionMode::BuildOnly,
            Tolerance::new(1, 100),
        );
        let mut metrics = Metrics::default();
        let report = drive_with_verdict(&mut stage, &mut metrics, verdict).await;
        (report, metrics)
    }

    let (case_b, metrics_b) = drive_with(&live_head).await;
    let refused = case_b
        .context_checks
        .first()
        .unwrap_or_else(|| panic!("the consumer leg recorded no row: {:?}", case_b.to_json()));
    assert_eq!(refused.outcome.name(), "rejected", "{refused:?}");
    let refused_row = refused.outcome.to_row();
    assert_eq!(
        refused_row["reason"].as_str(),
        Some("unverifiable_block_context"),
        "the refusal must name why the freshness question has no answer here, not that a hash \
         disagreed: {refused_row}"
    );
    assert_eq!(
        refused_row["consumer_read"].as_bool(),
        Some(true),
        "the identity leg did run: this stage read its own block and it agreed: {refused_row}"
    );
    assert_eq!(metrics_b.get("execution_block_context_rejected"), 1);
    assert!(
        context_line(&case_b).contains("unverifiable_block_context"),
        "{}",
        context_line(&case_b)
    );

    // Case A, one scope away: the same read, the same height, the same hash.
    let (case_a, metrics_a) = drive_with(&fixed_pin).await;
    let accepted = case_a
        .context_checks
        .first()
        .unwrap_or_else(|| panic!("the consumer leg recorded no row: {:?}", case_a.to_json()));
    assert_eq!(accepted.outcome.name(), "accepted", "{accepted:?}");
    assert_eq!(metrics_a.get("execution_block_context_accepted"), 1);

    // §30 on the refused side: the step builds from its own pin, at the same block the
    // refused context named, and the refusal changes nothing that the build leg decided —
    // the two arms' build rows are the same row.
    for report in [&case_b, &case_a] {
        assert_eq!(report.builds.len(), 1, "{:?}", report.builds);
        assert_eq!(report.builds[0].identity.number, BlockNumber(PIN));
        assert_eq!(report.builds[0].identity.hash, pinned_hash());
        assert_eq!(report.reached, Some(ExecutionStatus::Built));
    }
    assert_eq!(
        case_b.builds[0].fingerprint, case_a.builds[0].fingerprint,
        "a freshness judgement that refused must not reach the transaction that was built"
    );
}

// ---------------------------------------------------------------------------
// M8.4.4 §11/§12/§19/§35: the two arms, their negative control, and the rows they publish
// ---------------------------------------------------------------------------

/// §11's three arms: one driven `BuildOnly` step each, on this fixture's own pin. The plan, the
/// lane's answers, the mode and the tolerance are shared constants, so the only thing a reader is
/// comparing between arms is the verdict the build stage was handed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arm {
    /// Arm A — the path as it stood before this milestone. [`preflight_ok`] hand-builds a §26
    /// verdict that verified nothing about the block, which is exactly what a producer with no
    /// context to propagate hands down: the consumer records `no_context` and reads the block
    /// for itself.
    Baseline,
    /// Arm B — §5's producer leg verified this step's pin, and the verdict carries that context.
    VerifiedContext,
    /// §19's negative control, and §7's NC1 and NC5 in one shape: a producer-verified statement
    /// about a *different block at the height this step pins*.
    NegativeControl,
}

impl Arm {
    fn name(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::VerifiedContext => "verified_context",
            Self::NegativeControl => "negative_control",
        }
    }

    fn verdict(self) -> PreflightReport {
        match self {
            Self::Baseline => preflight_ok(&plan()),
            Self::VerifiedContext => {
                verdict_carrying(&pinned_identity(), BlockContextScope::FixedHistorical)
            }
            Self::NegativeControl => {
                verdict_carrying(&tampered_identity(), BlockContextScope::FixedHistorical)
            }
        }
    }
}

/// The pin this fixture drives, with a hash the lane never answers for it — the identity §7's
/// NC1 and NC5 demand, built off [`pinned_identity`] so the height and chain stay the real ones.
fn tampered_identity() -> BlockIdentity {
    let mut identity = pinned_identity();
    identity.hash = alloy_primitives::B256::repeat_byte(0x55);
    identity
}

/// One arm driven, and everything it left behind written the way a reader gets it: the
/// [`evm_execution::SequenceReport::to_json`] row is the same object the CLI appends to a live
/// run's `executions.jsonl`, and the lane's tally is its own account of the reads it answered.
///
/// §17 is why the row is the report's serialization rather than a summary typed out here: every
/// figure the tables below publish has to be recomputable from the record it describes, and a
/// hand-written one could only ever agree with itself.
async fn drive_arm(arm: Arm) -> serde_json::Value {
    let (mut stage, scripted) = assemble(
        Scripted::new(ExecutionMode::BuildOnly),
        ExecutionMode::BuildOnly,
        Tolerance::new(1, 100),
    );
    let mut metrics = Metrics::default();
    let verdict = arm.verdict();
    let producer = verdict.block_context.to_row();
    let report = drive_with_verdict(&mut stage, &mut metrics, &verdict).await;
    let reads: Vec<serde_json::Value> = scripted
        .lane_reads()
        .iter()
        .enumerate()
        .map(|(index, read)| {
            serde_json::json!({
                "seq": index + 1,
                "site": read.site,
                "stage": read.stage,
                "caller": read.caller,
            })
        })
        .collect();
    serde_json::json!({
        "arm": arm.name(),
        "producer_verdict": producer,
        "lane_reads": reads,
        "counters": metrics.counters.to_json(),
        "record": report.to_json(),
    })
}

/// The one build row an arm leaves, or a failure naming the arm that left none. A BuildOnly step
/// builds exactly one transaction, so a row count other than one is the fixture disagreeing with
/// the mode it was run in.
fn build_row(arm: Arm, row: &serde_json::Value) -> serde_json::Value {
    let builds = row["record"]["builds"]
        .as_array()
        .unwrap_or_else(|| panic!("{}: the record carries no `builds` array", arm.name()));
    assert_eq!(
        builds.len(),
        1,
        "{}: one step driven, so one build row",
        arm.name()
    );
    builds[0].clone()
}

/// The consumer leg's answer for one arm, read off the record's own line rather than from a
/// counter, because the line is what names the two identities that disagreed.
fn context_line_of(arm: Arm, row: &serde_json::Value) -> String {
    let lines: Vec<&str> = row["record"]["sources"]
        .as_array()
        .expect("the record carries its sources")
        .iter()
        .filter_map(|line| line.as_str())
        .filter(|line| line.contains("block context"))
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "{}: one step driven, so one consumer answer: {lines:?}",
        arm.name()
    );
    lines[0].to_string()
}

/// §35's experiment claim, first half: the arm that was handed a verified context and the arm
/// that was handed nothing act on the *same block*. Read off the build rows rather than the
/// fixture's constants, because §13 forbids inferring this from the final transaction alone —
/// the identity has to be a stated field of the record before the result can be compared to it.
#[tokio::test]
async fn baseline_and_reuse_same_block_identity() {
    let baseline = drive_arm(Arm::Baseline).await;
    let reuse = drive_arm(Arm::VerifiedContext).await;
    let left = build_row(Arm::Baseline, &baseline);
    let right = build_row(Arm::VerifiedContext, &reuse);
    for field in ["chain_id", "block_number", "block_hash"] {
        assert_eq!(
            left[field], right[field],
            "the two arms acted on different {field}: {left} vs {right}"
        );
    }
    // and the identity they agree on is this step's own pin, not a value the test brought along:
    // the height is the one the plan pins, and the hash is the one the scripted lane answers at it.
    assert_eq!(
        right["block_hash"],
        serde_json::json!(format!("{:#x}", pinned_hash())),
        "{right}"
    );
    assert_eq!(right["block_number"], serde_json::json!(PIN), "{right}");
    // the consumer said *which* answer it checked against, on the line, in both arms — an arm
    // with no context still names the block it read for itself (§2's line: it cannot be silent).
    assert!(
        context_line_of(Arm::Baseline, &baseline).contains(&pinned_identity().describe()),
        "{:?}",
        context_line_of(Arm::Baseline, &baseline)
    );
    assert!(
        context_line_of(Arm::VerifiedContext, &reuse).contains(&pinned_identity().describe()),
        "{:?}",
        context_line_of(Arm::VerifiedContext, &reuse)
    );
}

/// §12's field list, compared as the whole `unsigned` object instead of a hand-picked subset: a
/// field no test names is exactly the field that could differ quietly. `target`, `value`,
/// `calldata`, `nonce`, `gas_limit`, both fee fields, `chain_id` and `access_list` are all inside
/// it, and `sender_expected` is the builder's own statement of who may sign these bytes.
#[tokio::test]
async fn baseline_and_reuse_same_build_result() {
    let baseline = drive_arm(Arm::Baseline).await;
    let reuse = drive_arm(Arm::VerifiedContext).await;
    let left = build_row(Arm::Baseline, &baseline);
    let right = build_row(Arm::VerifiedContext, &reuse);
    assert_eq!(left["unsigned"], right["unsigned"], "{left} vs {right}");
    assert_eq!(left["sender_expected"], right["sender_expected"]);
    // the rung and the reason the run stopped there belong to the mode, not to the arm: §19's
    // equality is about the transaction, and a difference here would mean the context changed
    // what the route decided — which is the one thing this milestone must not do quietly.
    assert_eq!(
        baseline["record"]["status"], reuse["record"]["status"],
        "the arms reached different rungs"
    );
    assert_eq!(baseline["record"]["builds"], reuse["record"]["builds"]);
    // positive control on the fixture itself: the build row really does name the §12 fields, so
    // an equality over `unsigned` is a comparison of eight values and not of one empty object.
    let unsigned = right["unsigned"].as_object().expect("a built transaction");
    for field in [
        "tx_type",
        "chain_id",
        "nonce",
        "to",
        "value",
        "gas_limit",
        "input",
        "access_list",
        "max_fee_per_gas",
        "max_priority_fee_per_gas",
    ] {
        assert!(
            unsigned.contains_key(field),
            "{field} missing from {unsigned:?}"
        );
    }
}

/// §35's third experiment claim and §12's fingerprint: the two arms produced the same bytes to
/// sign. The second half of this test is the control that makes the first half mean anything —
/// the fingerprint is recomputed from the row's own fields with the crate's own hash leg, so it
/// cannot be a constant the fixture carried from one arm to the other.
#[tokio::test]
async fn baseline_and_reuse_same_fingerprint() {
    let baseline = drive_arm(Arm::Baseline).await;
    let reuse = drive_arm(Arm::VerifiedContext).await;
    let left = build_row(Arm::Baseline, &baseline);
    let right = build_row(Arm::VerifiedContext, &reuse);
    assert_eq!(
        left["fingerprint"], right["fingerprint"],
        "the arms signed different bytes"
    );
    assert_eq!(
        left["serialization_bytes"], right["serialization_bytes"],
        "the arms serialized to different lengths"
    );
    let unsigned: UnsignedTransaction =
        serde_json::from_value(right["unsigned"].clone()).expect("a record's own build fields");
    let recomputed = signing_hash_for_chain(&unsigned, unsigned.chain_id)
        .expect("the recorded fields form a transaction this crate can hash");
    assert_eq!(
        right["fingerprint"],
        serde_json::json!(format!("{:#x}", recomputed)),
        "the published fingerprint is not what the published fields hash to"
    );
    // and it moves when a field it covers moves: the same row with one fee wei more is a
    // different fingerprint, so equality above is a statement about bytes and not about a stub.
    let mut shifted = unsigned.clone();
    shifted.max_fee_per_gas = shifted.max_fee_per_gas.map(|fee| fee + U256::from(1u64));
    let shifted_hash = signing_hash_for_chain(&shifted, shifted.chain_id)
        .expect("the shifted fields still form a transaction");
    assert_ne!(recomputed, shifted_hash, "a fee change left the hash alone");
}

/// §14's gate, as a number rather than a sentence: the only difference an arm may introduce is
/// the header reuse the milestone declares — and because §2 forbids deleting the read that makes
/// the check, the declared difference is *nothing*. Compared as an ordered list, so an arm that
/// added one read at the end and removed one at the start cannot pass on a multiset match, and
/// compared as counters too, because the stage's own tally is what a reader audits the line
/// against.
#[tokio::test]
async fn no_extra_rpc_outside_declared_header_reuse() {
    let mut by_arm = Vec::new();
    for arm in [Arm::Baseline, Arm::VerifiedContext, Arm::NegativeControl] {
        by_arm.push((arm, drive_arm(arm).await));
    }
    let reference = &by_arm[0].1["lane_reads"];
    for (arm, row) in &by_arm[1..] {
        assert_eq!(
            &row["lane_reads"],
            reference,
            "{} asked the lane for a different sequence of reads than {}",
            arm.name(),
            by_arm[0].0.name()
        );
        // every counter the run bumped, except the three this milestone's consumer leg writes:
        // those are the judgement's own output, and they are compared by name further down.
        let mut left = row["counters"].as_object().expect("counters").clone();
        let mut right = by_arm[0].1["counters"]
            .as_object()
            .expect("counters")
            .clone();
        for name in [
            "execution_block_context_accepted",
            "execution_block_context_rejected",
            "execution_block_context_no_context",
        ] {
            left.remove(name);
            right.remove(name);
        }
        assert_eq!(
            left,
            right,
            "{} and {} bumped different counters while asking the same reads",
            arm.name(),
            by_arm[0].0.name()
        );
    }
    // the positive control on the comparison above: the sequence is not empty, and the block read
    // the consumer judges itself against is in it exactly once per step.
    let sites: Vec<&str> = reference
        .as_array()
        .expect("lane reads")
        .iter()
        .map(|read| read["site"].as_str().expect("a named site"))
        .collect();
    assert!(!sites.is_empty(), "the arm answered no reads at all");
    assert_eq!(
        sites
            .iter()
            .filter(|site| **site == "block_hash_at")
            .count(),
        1,
        "{sites:?}"
    );
    // §32's counts, and §14's honest arithmetic: the consumer leg's judgement is the only thing
    // that moved, no arm added or retired a read, and `saved` therefore is 0 rather than a
    // number the milestone would rather had.
    for (arm, row) in &by_arm {
        let counters = row["counters"].as_object().expect("counters");
        let value = |name: &str| {
            counters
                .get(name)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        let expected_accepted = usize::from(*arm == Arm::VerifiedContext);
        let expected_rejected = usize::from(*arm == Arm::NegativeControl);
        let expected_none = usize::from(*arm == Arm::Baseline);
        assert_eq!(
            value("execution_block_context_accepted"),
            u64::try_from(expected_accepted).expect("one arm"),
            "{}: {row}",
            arm.name()
        );
        assert_eq!(
            value("execution_block_context_rejected"),
            u64::try_from(expected_rejected).expect("one arm"),
            "{}: {row}",
            arm.name()
        );
        assert_eq!(
            value("execution_block_context_no_context"),
            u64::try_from(expected_none).expect("one arm"),
            "{}: {row}",
            arm.name()
        );
        // the read that judges the propagated context is asked once, in every arm — which is
        // what §2's ban on deleting it looks like as a number, and why `saved = 0`.
        assert_eq!(
            reference
                .as_array()
                .expect("lane reads")
                .iter()
                .filter(|read| read["site"].as_str() == Some("block_hash_at"))
                .count(),
            1,
            "{}",
            arm.name()
        );
    }
}

/// E1 and §16: the arms' raw rows, published where a reader can recompute the tables from them.
///
/// Three runs of one deterministic drive, and the file says what that is: `repetitions_of_one_drive`
/// is 3 while `independent_runs` is 0. Two runs agreeing here is evidence the fixture is
/// reproducible, not evidence about a market — the independent samples this milestone reasons
/// about are the live runs under `live/`, where a node answers at three different moments.
#[tokio::test]
async fn the_fixed_block_arms_publish_their_raw_rows() {
    let dir = fixture_evidence_root();
    let mut published: Vec<(String, serde_json::Value)> = Vec::new();
    for index in 1..=3 {
        let name = format!("run-{index:02}");
        let mut arms = Vec::new();
        for arm in [Arm::Baseline, Arm::VerifiedContext, Arm::NegativeControl] {
            arms.push(drive_arm(arm).await);
        }
        let row = serde_json::json!({
            "schema": 1,
            "milestone": "M8.4.4",
            "run": name,
            "source": "fixture",
            "what_source_means": "a deterministic in-process stand-in answers the lane: these \
                                  numbers show the contract working, they are not market \
                                  behaviour and are never added to a live run's",
            "repetitions_of_one_drive": 3,
            "independent_runs": 0,
            "execution_mode": "build-only",
            "venue": "the scripted lane of crates/execution/tests/sequence.rs",
            "arms": arms,
        });
        write_json(&dir.join(format!("{name}.json")), &row);
        published.push((name, row));
    }

    // the three runs differ in their own label and in nothing else, measured rather than
    // asserted in prose: strip the label out of run 01 and compare it to the others.
    let mut first = published[0].1.clone();
    first["run"] = serde_json::json!("");
    for (name, row) in &published[1..] {
        let mut other = row.clone();
        other["run"] = serde_json::json!("");
        assert_eq!(
            first, other,
            "{name}: two runs of one deterministic drive disagreed"
        );
    }

    // E8's raw material, in every run: the negative control is refused by name, and the two
    // hashes that disagree are both on its line.
    for (name, row) in &published {
        let arms = row["arms"].as_array().expect("three arms");
        assert_eq!(arms.len(), 3, "{name}");
        let control = &arms[2];
        let line = context_line_of(Arm::NegativeControl, control);
        assert!(line.contains("rejected"), "{name}: {line}");
        assert!(line.contains("block_hash_mismatch"), "{name}: {line}");
        assert!(
            line.contains(&format!("{:#x}", pinned_hash())),
            "{name}: {line}"
        );
        assert!(
            line.contains(&format!("{:#x}", tampered_identity().hash)),
            "{name}: {line}"
        );
        assert_eq!(
            control["producer_verdict"]["outcome"],
            serde_json::json!("verified"),
            "{name}: the control's producer did verify its own (wrong) block, which is the \
             point — a refusal here would be testing that garbage is caught by nobody checking \
             it, not testing the consumer"
        );
    }
}

/// §29's named error and §31's `reused` flag, checked against the sentence the same verdict wrote
/// one line away. Two representations of one fact are only safe if a test compares them: the gate
/// in `tests/block_context_evidence.rs` reads the fields, a human reading `executions.jsonl` reads
/// the line, and a drift between the two would mean the evidence says one thing and the report
/// another.
#[tokio::test]
async fn the_structured_consumer_row_and_the_prose_line_agree() {
    for arm in [Arm::Baseline, Arm::VerifiedContext, Arm::NegativeControl] {
        let row = drive_arm(arm).await;
        let checks = row["record"]["context_checks"]
            .as_array()
            .expect("one step driven, one consumer row");
        assert_eq!(checks.len(), 1, "{}", arm.name());
        let check = &checks[0];
        let line = context_line_of(arm, &row);

        assert_eq!(check["position"], serde_json::json!(0), "{}", arm.name());
        // §25: the row names the whole identity it checked, never a bare height.
        assert_eq!(
            check["expected_chain_id"],
            serde_json::json!(CHAIN),
            "{line}"
        );
        assert_eq!(
            check["expected_block_number"],
            serde_json::json!(PIN),
            "{line}"
        );
        assert_eq!(
            check["expected_block_hash"],
            serde_json::json!(format!("{:#x}", pinned_hash())),
            "{line}"
        );

        // the outcome word is the same token in both places, and it is the token the counter
        // was keyed by — three call sites, one name.
        let outcome = check["outcome"].as_str().expect("an outcome word");
        assert!(
            line.contains(&format!("block context {outcome} for ")),
            "{line}"
        );
        assert_eq!(
            row["counters"][format!("execution_block_context_{outcome}")]
                .as_u64()
                .unwrap_or(0),
            1,
            "{line}"
        );

        match outcome {
            "no_context" => {
                assert_eq!(check["reason"], serde_json::Value::Null, "{line}");
                assert_eq!(check["consumer_read"], serde_json::Value::Null, "{line}");
            }
            "accepted" => {
                assert_eq!(check["reason"], serde_json::Value::Null, "{line}");
                // §31: the consumer read for itself, so `reused` is the flag that says no value
                // was taken on trust — which is what this production path always does.
                assert_eq!(check["consumer_read"], serde_json::json!(true), "{line}");
                assert_eq!(check["reused"], serde_json::json!(false), "{line}");
            }
            "rejected" => {
                assert_eq!(
                    check["reason"],
                    serde_json::json!("block_hash_mismatch"),
                    "{line}: §29 asks the refusal to have a name; this arm tampers with the hash \
                     and nothing else"
                );
                assert!(line.contains("block_hash_mismatch"), "{line}");
                assert_eq!(check["consumer_read"], serde_json::json!(true), "{line}");
                assert_eq!(check["reused"], serde_json::json!(false), "{line}");
                // §30: the refusal is recorded as a fallback performed, and the step's own read is
                // what the row shows it fell back to.
                assert_eq!(
                    row["record"]["builds"][0]["block_hash"],
                    serde_json::json!(format!("{:#x}", pinned_hash())),
                    "{line}: the rejected context was never the value the step sent against"
                );
            }
            other => panic!("{}: {other} is not one of §7's three outcomes", arm.name()),
        }
    }
}

// ---------------------------------------------------------------------------
// the publishing helpers the experiment test above uses
// ---------------------------------------------------------------------------
/// The workspace root, as this crate's own manifest sees it.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The evidence tree: `M844_FIXTURE_EVIDENCE` names it — the same shape as
/// `M842_FIXTURE_EVIDENCE`, so a regeneration is one environment variable and not an edit. Its
/// absence is a scratch directory under `target/`, where a plain `cargo test` cannot overwrite
/// committed evidence.
///
/// Only the subtree named at the call site is created and written: these tests run in one process,
/// and a root-level wipe would let a second test delete the first one's output.
fn evidence_tree() -> PathBuf {
    match std::env::var("M844_FIXTURE_EVIDENCE") {
        Ok(dir) if !dir.is_empty() => {
            let path = PathBuf::from(dir);
            if path.is_relative() {
                workspace_root().join(path)
            } else {
                path
            }
        }
        _ => workspace_root().join("target/execution-tests/m8.4.4"),
    }
}

/// Where §16's raw arm rows go: `<tree>/fixed-block/run-NN.json`.
fn fixture_evidence_root() -> PathBuf {
    let dir = evidence_tree().join(FIXED_BLOCK_DIR);
    std::fs::create_dir_all(&dir).unwrap_or_else(|error| {
        panic!(
            "{}: the fixed-block subtree could not be created: {error}",
            dir.display()
        )
    });
    dir
}

/// One file, one row, in the repository's byte shape: pretty JSON, one trailing newline.
fn write_json(path: &Path, value: &serde_json::Value) {
    let text = serde_json::to_string_pretty(value).expect("a row built here is serializable");
    std::fs::write(path, format!("{text}\n")).unwrap_or_else(|error| {
        panic!("{}: the row could not be written: {error}", path.display())
    });
}
