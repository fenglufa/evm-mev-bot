//! The market, the candidate and the outcome shape that §19–§25 are tested against.
//!
//! Three jobs, all of them so a test can say one sentence instead of forty lines:
//!
//! - [`market`] builds a [`GraphSnapshot`] the way the real pipeline does — an
//!   `InMemoryStateStore` fed register/sync updates, projected by the graph builder —
//!   so no test gets to invent a market shape the system never produces. It is the same
//!   stage `crates/opportunity/tests/support` runs, at the same scale, because §7–§18
//!   proved that shape produces priceable routes. [`market_on`] is the same builder with the
//!   chain and the reserves left open, which is how `tests/multihop_revm.rs` projects the
//!   recorded GIWA pair out of the M7 dump without a second graph code path.
//! - [`at_amount`] prices a route at exactly one input. §20's claims are about the legs
//!   a candidate carries, so the search must not be free to move the amount underneath a
//!   test: a one-point domain is walked (it is one input), the winner is that input, and
//!   `best_output` is the quote's own answer.
//! - [`outcome_of`] writes an [`ExecutorOutcome`] by hand. The EVM is not running in this
//!   module's tests: §25 is about what the adapter *says* about five different endings,
//!   and the only way to ask about an out-of-gas run or a halted one without burning a
//!   real execution each time is to hand the reader the answer it would have got. The
//!   real executions are in `tests/multihop_revm.rs`, where nothing is invented.
//!
//! Nothing here signs, broadcasts, or holds a key.
#![allow(dead_code)]

use alloy_primitives::{address, keccak256, Address, Bytes, U256};

use evm_core::{
    BlockNumber, ChainId, Fee, LogIndex, PoolId, PoolMeta, PoolType, ProtocolId, TokenId,
};
use evm_graph::{EdgeId, GraphSnapshot, MarketGraphBuilder};
use evm_opportunity::{
    optimize, Gross, MultiHopQuote, MultiHopRoute, OptimizationPolicy, OptimizedCandidate,
};
use evm_protocol::{decode_revert, ExecutorCall, ExecutorLeg};
use evm_simulation::executor::{ExecutorOutcome, ExecutorRun};
use evm_simulation::result::{RevertData, StateChanges, StepStatus};
use evm_simulation::state::BlockPin;
use evm_simulation::{legs, EvmRules, GasCharge, GasPricing, MultiHopBuildError, RunConfig};
use evm_state::{InMemoryStateStore, StateStore, StateUpdate, UpdatePosition};

pub const CHAIN: ChainId = ChainId(7);
pub const BLOCK: u64 = 100;

/// The header the hand-written outcomes are pinned to. A real run takes its hash from the
/// provider; a synthetic pin is a stated choice, spelled out here rather than hidden in a
/// zero so an identity hash that moves is traceable to the field that moved it.
pub fn pin() -> BlockPin {
    BlockPin::new(BlockNumber(BLOCK), keccak256(b"m11-synthetic-header"))
}

pub const FEE: Fee = Fee {
    numerator: 997,
    denominator: 1000,
};

pub const A: Address = address!("0x000000000000000000000000000000000000000a");
pub const B: Address = address!("0x000000000000000000000000000000000000000b");
pub const C: Address = address!("0x000000000000000000000000000000000000000c");
pub const D: Address = address!("0x000000000000000000000000000000000000000d");
pub const E: Address = address!("0x000000000000000000000000000000000000000e");
pub const P1: Address = address!("0x00000000000000000000000000000000000000f1");
pub const P2: Address = address!("0x00000000000000000000000000000000000000f2");
pub const P3: Address = address!("0x00000000000000000000000000000000000000f3");
pub const P4: Address = address!("0x00000000000000000000000000000000000000f4");
pub const P5: Address = address!("0x00000000000000000000000000000000000000f5");

/// The deployment, the operator and the payee for the synthetic chain. These are addresses
/// with no story: the tests here never reach an EVM, they check what the adapter puts in
/// the request and what it reads back.
pub const EXECUTOR: Address = address!("0x1000000000000000000000000000000000000010");
pub const OPERATOR: Address = address!("0x953e7e98562714c23bc22c7d186cdf516f9dfa6f");
pub const RECIPIENT: Address = address!("0x2000000000000000000000000000000000000020");

pub fn token(a: Address) -> TokenId {
    token_on(CHAIN, a)
}

pub fn pool(a: Address) -> PoolId {
    PoolId::new(CHAIN, a)
}

pub fn edge(p: Address, from: Address, to: Address) -> EdgeId {
    edge_on(CHAIN, p, from, to)
}

/// The same two identities on an explicit chain, for a market that is not this module's
/// synthetic one — `tests/multihop_revm.rs` projects the recorded GIWA pair this way.
pub fn token_on(chain: ChainId, a: Address) -> TokenId {
    TokenId::new(chain, a)
}

pub fn edge_on(chain: ChainId, p: Address, from: Address, to: Address) -> EdgeId {
    EdgeId::new(
        PoolId::new(chain, p),
        token_on(chain, from),
        token_on(chain, to),
    )
}

pub fn u(x: u128) -> U256 {
    U256::from(x)
}

/// A pool as an attestation states it: which two tokens, whose order decides which reserve
/// is which, and whatever fee has been proved for it.
pub struct Spec {
    pub pool: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: u128,
    pub reserve1: u128,
    pub fee: Option<Fee>,
}

pub fn spec(
    pool: Address,
    token0: Address,
    token1: Address,
    reserve0: u128,
    reserve1: u128,
    fee: Option<Fee>,
) -> Spec {
    Spec {
        pool,
        token0,
        token1,
        reserve0,
        reserve1,
        fee,
    }
}

/// The same pool stated with the reserves a source gives them — 256 bits, never narrowed.
/// A hand-typed market fits in [`Spec`]; a recorded one arrives here, because the whole
/// point of reading reserves out of a dump is that nobody chose the numbers.
pub struct Reserves {
    pub pool: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    pub fee: Option<Fee>,
}

pub fn reserves(
    pool: Address,
    token0: Address,
    token1: Address,
    reserve0: U256,
    reserve1: U256,
    fee: Option<Fee>,
) -> Reserves {
    Reserves {
        pool,
        token0,
        token1,
        reserve0,
        reserve1,
        fee,
    }
}

/// Register every pool, then sync it, and project the result — the one graph builder M11
/// uses, so a synthetic market and the recorded GIWA pair are built by the same code and
/// can differ only in where their numbers came from.
pub fn market_on(chain: ChainId, block: u64, pools: &[Reserves]) -> GraphSnapshot {
    let mut store = InMemoryStateStore::new(chain);
    for entry in pools {
        let meta = PoolMeta {
            id: PoolId::new(chain, entry.pool),
            protocol: ProtocolId::new("test-v2"),
            token0: token_on(chain, entry.token0),
            token1: token_on(chain, entry.token1),
            fee: entry.fee,
            pool_type: PoolType::ConstantProduct,
        };
        store
            .apply(StateUpdate::PoolRegistered(meta))
            .expect("register a fixture pool");
    }
    for (index, entry) in pools.iter().enumerate() {
        store
            .apply(StateUpdate::PoolSynced {
                pool: PoolId::new(chain, entry.pool),
                reserve0: entry.reserve0,
                reserve1: entry.reserve1,
                position: UpdatePosition::new(BlockNumber(block), LogIndex(index as u64 + 1)),
            })
            .expect("sync a fixture pool");
    }
    MarketGraphBuilder::new()
        .build(&store.snapshot())
        .expect("project the fixture graph")
}

/// The hand-typed pools, widened to the shared builder at this module's own chain.
pub fn market_at(block: u64, pools: &[Spec]) -> GraphSnapshot {
    let widened: Vec<Reserves> = pools
        .iter()
        .map(|entry| Reserves {
            pool: entry.pool,
            token0: entry.token0,
            token1: entry.token1,
            reserve0: U256::from(entry.reserve0),
            reserve1: U256::from(entry.reserve1),
            fee: entry.fee,
        })
        .collect();
    market_on(CHAIN, block, &widened)
}

pub fn market(pools: &[Spec]) -> GraphSnapshot {
    market_at(BLOCK, pools)
}

// ---------------------------------------------------------------------------
// The markets, at the scale §7–§18 proved priceable: deep reserves, small inputs.
// ---------------------------------------------------------------------------

/// `A -> B -> C -> A` across three pools, the first paying 1 100 000 of B per 1 000 000 of
/// A: a price product of 1.1 against a fee product of `0.997³ ≈ 0.991`, so it gains.
pub fn triangle_snapshot() -> GraphSnapshot {
    market(&[
        spec(P1, A, B, 1_000_000, 1_100_000, Some(FEE)),
        spec(P2, B, C, 1_000_000, 1_000_000, Some(FEE)),
        spec(P3, C, A, 1_000_000, 1_000_000, Some(FEE)),
    ])
}

pub fn triangle_route() -> MultiHopRoute {
    MultiHopRoute::new(
        &triangle_snapshot(),
        &[edge(P1, A, B), edge(P2, B, C), edge(P3, C, A)],
    )
    .expect("the triangle closes")
}

/// `A -> B -> C -> D -> A`, four pools: the longest route the contract will carry.
pub fn square_route() -> MultiHopRoute {
    let snapshot = market(&[
        spec(P1, A, B, 1_000_000, 1_100_000, Some(FEE)),
        spec(P2, B, C, 1_000_000, 1_000_000, Some(FEE)),
        spec(P3, C, D, 1_000_000, 1_000_000, Some(FEE)),
        spec(P4, D, A, 1_000_000, 1_000_000, Some(FEE)),
    ]);
    MultiHopRoute::new(
        &snapshot,
        &[
            edge(P1, A, B),
            edge(P2, B, C),
            edge(P3, C, D),
            edge(P4, D, A),
        ],
    )
    .expect("the square closes")
}

/// `A -> B -> C -> D -> E -> A`, five pools: one leg past `MAX_LEGS`, so a quote exists and
/// a call does not.
pub fn pentagon_route() -> MultiHopRoute {
    let snapshot = market(&[
        spec(P1, A, B, 1_000_000, 1_100_000, Some(FEE)),
        spec(P2, B, C, 1_000_000, 1_000_000, Some(FEE)),
        spec(P3, C, D, 1_000_000, 1_000_000, Some(FEE)),
        spec(P4, D, E, 1_000_000, 1_000_000, Some(FEE)),
        spec(P5, E, A, 1_000_000, 1_000_000, Some(FEE)),
    ]);
    MultiHopRoute::new(
        &snapshot,
        &[
            edge(P1, A, B),
            edge(P2, B, C),
            edge(P3, C, D),
            edge(P4, D, E),
            edge(P5, E, A),
        ],
    )
    .expect("the pentagon closes")
}

/// `A -> B -> A` across two pools, the second paying 1 250 000 of A per 500 000 of B: the
/// shape M3 already knows, and the shortest thing §20 can call a multi-hop route.
pub fn pair_route() -> MultiHopRoute {
    let snapshot = market(&[
        spec(P1, A, B, 1_000_000, 500_000, Some(FEE)),
        spec(P2, B, A, 500_000, 1_250_000, Some(FEE)),
    ]);
    MultiHopRoute::new(&snapshot, &[edge(P1, A, B), edge(P2, B, A)]).expect("the pair closes")
}

// ---------------------------------------------------------------------------
// Candidates
// ---------------------------------------------------------------------------

/// The candidate whose legs every §20 test reads: `route` priced at exactly `amount`, by a
/// domain of one input. The search is exhaustive over that window, so `best_input == amount`
/// and the quote attached is the quote at that amount — nothing downstream gets to disagree
/// about which input is being discussed.
pub fn at_amount(route: &MultiHopRoute, amount: U256) -> OptimizedCandidate {
    optimize(route, amount, amount, OptimizationPolicy::default_search())
        .unwrap_or_else(|error| panic!("the route cannot be priced at {amount}: {error}"))
}

/// [`triangle_route`] at the amount the suite mostly uses: 1 000 of A against pools of a
/// million. Small against the depth is what makes it a gain — the same route at a tenth of
/// the pool loses to its own slippage, which §45's all-negative case is built from.
pub fn candidate() -> OptimizedCandidate {
    at_amount(&triangle_route(), u(1_000))
}

/// [`square_route`] at the same input, for the tests that want `MAX_LEGS` reached rather
/// than passed.
pub fn four_hop_candidate() -> OptimizedCandidate {
    at_amount(&square_route(), u(1_000))
}

// ---------------------------------------------------------------------------
// Requests and outcomes
// ---------------------------------------------------------------------------

/// The gas model the hand-written runs carry. `Unresolved` on purpose: none of these tests
/// asks a cost question, and a charge that says "no price could be attached" is closer to
/// the truth than a base fee invented for a chain that does not exist.
pub fn pricing() -> GasPricing {
    GasPricing::Unresolved {
        reason: "no header is pinned for a hand-written request, so no gas price is claimed"
            .to_string(),
    }
}

/// Everything but the route and the block, at the addresses above.
pub fn config() -> RunConfig {
    RunConfig {
        state_source: "M11_SYNTHETIC: an offline request, no EVM behind it".to_string(),
        executor: EXECUTOR,
        operator: OPERATOR,
        recipient: RECIPIENT,
        gas_limit: 3_000_000,
        rules: EvmRules::Prague,
        pricing: pricing(),
        endowment: None,
    }
}

/// The request for a candidate, with the final guard at its priced output — the tightest
/// ask the route can claim, which is what §20's floor rule is tested against.
pub fn request(candidate: &OptimizedCandidate) -> ExecutorRun {
    let guard = candidate.search.best_output;
    evm_simulation::executor_run(candidate, &config(), guard)
        .unwrap_or_else(|error| panic!("the candidate refused to build a request: {error}"))
}

/// The same request with a caller-chosen guard.
pub fn request_guarded(candidate: &OptimizedCandidate, guard: U256) -> ExecutorRun {
    evm_simulation::executor_run(candidate, &config(), guard)
        .unwrap_or_else(|error| panic!("the candidate refused to build a request: {error}"))
}

/// An [`ExecutorOutcome`] that answers for `run` with `status` and, when the run returned
/// an amount, that amount.
///
/// Every field is taken from the request it belongs to, because §23's identity is a claim
/// about *this* run: an outcome that disagreed with its own request about the input amount
/// would be a hand-written fact rather than a hand-written answer. The two fields no request
/// contains are the block hash (the provider's, here [`pin`]) and the delivery (the EVM's).
pub fn outcome_of(
    run: &ExecutorRun,
    status: StepStatus,
    delivered: Option<U256>,
    gas_used: u64,
) -> ExecutorOutcome {
    let (input_token, amount_in, min_final_amount, recipient) = match &run.call {
        ExecutorCall::Execute {
            input_token,
            amount_in,
            min_final_amount,
            recipient,
            ..
        } => (*input_token, *amount_in, *min_final_amount, *recipient),
        other => panic!("{} is not an execute call", other.signature()),
    };
    let calldata = run.call.encode();
    // The same derivation the real run uses (crates/simulation/src/executor.rs), so a
    // hand-written revert cannot name itself something the classifier would not call it.
    let (contract_error, revert_kind) = match &status {
        StepStatus::Reverted(data) => {
            let payload = decode_revert(&data.raw);
            (
                payload.executor().map(|error| error.name().to_string()),
                Some(payload.kind()),
            )
        }
        _ => (None, None),
    };
    let charge = match &run.pricing {
        GasPricing::Unresolved { reason } => GasCharge::Unpriced {
            gas_used,
            reason: reason.clone(),
        },
        other => GasCharge::Priced {
            gas_used,
            effective_gas_price: 1,
            base_fee_per_gas: Some(1),
            wei: U256::from(gas_used),
            pricing: other.clone(),
        },
    };
    ExecutorOutcome {
        chain_id: run.chain_id,
        block: pin(),
        state_source: run.state_source.clone(),
        executor: run.executor,
        operator: run.operator,
        recipient,
        input_token,
        amount_in,
        min_final_amount,
        signature: run.call.signature(),
        selector: format!("0x{}", hex::encode(run.call.selector())),
        calldata_len: calldata.len(),
        calldata,
        gas_limit: run.gas_limit,
        gas_used,
        charge,
        status,
        contract_error,
        revert_kind,
        delivered,
        logs: Vec::new(),
        reserves: Vec::new(),
        balances: Vec::new(),
        state_changes: StateChanges {
            accounts: Vec::new(),
            slots: Vec::new(),
        },
    }
}

/// A pool's `revert("...")` / `require(cond, "...")` answer: an `Error(string)` payload,
/// which the protocol crate classifies as `Error(string)` and *not* as one of the
/// executor's 22 named errors. That distinction is §25's, so the payload is built by the
/// same encoding a real pair uses rather than by a string in a test.
pub fn pool_revert(message: &str) -> RevertData {
    RevertData::new(Bytes::from(error_string_bytes(message)))
}

/// `Error(string)` over `message`, encoded field by field: selector, offset word, length
/// word, then the bytes padded to a whole word.
pub fn error_string_bytes(message: &str) -> Vec<u8> {
    let selector = &keccak256(b"Error(string)")[..4];
    let body = message.as_bytes();
    let mut raw = selector.to_vec();
    raw.extend_from_slice(&word(&[32u8]));
    raw.extend_from_slice(&word(&(body.len() as u64).to_be_bytes()));
    raw.extend_from_slice(body);
    raw.extend(std::iter::repeat_n(0u8, (32 - body.len() % 32) % 32));
    raw
}

/// Left-pad a value to a 32-byte word.
fn word(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[32 - bytes.len()..].copy_from_slice(bytes);
    out
}

/// A delivered run at the candidate's priced output.
pub fn delivered(candidate: &OptimizedCandidate) -> ExecutorOutcome {
    let run = request(candidate);
    let amount = candidate.search.best_output;
    outcome_of(&run, StepStatus::Success, Some(amount), 180_000)
}

/// File-private alias so the refusal tests can name the error type without importing the
/// crate's whole error surface.
pub type Refusal = MultiHopBuildError;

/// The legs, for a test that only wants to look at them.
pub fn leg_list(candidate: &OptimizedCandidate) -> Result<Vec<ExecutorLeg>, Refusal> {
    legs(candidate)
}

/// The quote, so a planted defect can be written into a clone of it.
pub fn quote_of(candidate: &OptimizedCandidate) -> MultiHopQuote {
    candidate.search.quote.clone()
}

/// The three-way profit answer, re-derived here so a test compares against a second
/// implementation rather than against the crate's own word.
pub fn gross_between(input: U256, output: U256) -> Gross {
    Gross::between(input, output)
}

/// Replace a candidate's quote with a broken one, leaving the route and the block alone.
/// Only reachable from a test that intends to be refused.
pub fn with_quote(
    candidate: &OptimizedCandidate,
    mutate: impl FnOnce(&mut MultiHopQuote),
) -> OptimizedCandidate {
    let mut broken = candidate.clone();
    mutate(&mut broken.search.quote);
    broken
}

/// A candidate whose *claimed* best input is not the first hop's input.
pub fn with_best_input(candidate: &OptimizedCandidate, amount: U256) -> OptimizedCandidate {
    let mut broken = candidate.clone();
    broken.search.best_input = amount;
    broken
}
