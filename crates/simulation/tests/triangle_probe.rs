//! Which storage words does a three-leg round trip actually read, on the recorded triangle?
//!
//! §37's 3-hop fixture has to run on real market state, and `multihop_capture.rs` recorded
//! that state: three real pools, three real tokens, the real header, and every scalar slot of
//! each contract. What a recording cannot contain is a *layout claim* — the fixture still has
//! to say which of a token's mappings is `balances` and which is `allowances`, because funding
//! a route means writing a word at a derived key, and a key derived from the wrong mapping is
//! a word nobody will ever read.
//!
//! So this file measures the claims instead of asserting them. It starts from the recording
//! with nothing declared but the executor's own configuration, asks the EVM to run a three-leg
//! round trip, and lets the provider's refusals do the talking:
//! [`evm_simulation::state::DumpStateProvider`] answers an unrecorded word with
//! `Missing { what: "storage {address} slot {slot}" }` rather than a guessed zero, so the
//! first run names the first key the contract consulted. Writing that key as zero and running
//! again names the next one. The loop stops at the contract's own answer — a delivery, a
//! revert, or a halt — and every key it passed through is cracked back into the derivation
//! that produces it, which is where a balance slot and an allowance slot stop being guesses.
//!
//! Nine instruments live here. Each one only works once the previous has made the state
//! readable, and each answers the question the last one left open — the tables are the record
//! of that order:
//!
//! 1. [`measure_which_slots_a_three_leg_run_reads`] — which words a three-leg run reads, and
//!    where it stops → `probe-{BLOCK}-three-leg-slots.json`;
//! 2. [`measure_what_each_recorded_pool_honours`] — what each pool actually pays, read out of
//!    the contract's own `DeliveryMismatch` arguments → `probe-{BLOCK}-pool-claims.json`. The
//!    first instrument's route asks for one wei on every leg, which the contract's
//!    exact-equality checks reject before the market gets a say, so the fee question needs a
//!    chained claim rather than a permissive one — see [`honour_chains`];
//! 3. [`measure_which_recorded_tokens_move`] — whether `transfer` and `withdraw` move these
//!    three tokens at all → `probe-{BLOCK}-token-transfers.json`;
//! 4. [`measure_whether_native_ether_is_the_gate`] — whether the empty answer is a failed value
//!    transfer → `probe-{BLOCK}-native-side.json`;
//! 5. [`measure_whether_a_recorded_pool_trades`] — whether one recorded pool answers a two-leg
//!    round trip on its own → `probe-{BLOCK}-self-cycle.json`;
//! 6. [`measure_whether_the_vat_row_is_the_gate`] — the same trip with the fee-voucher predeploy
//!    given its row → `probe-{BLOCK}-vat-row.json`;
//! 7. [`measure_whether_the_cached_reserves_match_the_balances`] — the same trip priced from
//!    `getReserves()` and from the pool's recorded token balances →
//!    `probe-{BLOCK}-reserves-vs-balances.json`;
//! 8. [`measure_whether_the_tokens_pay_a_pool`] — whether the tokens pay a declared holding to
//!    the pool itself → `probe-{BLOCK}-transfers-into-pools.json`;
//! 9. [`measure_which_entry_points_each_recorded_contract_exposes`] — where each contract's
//!    runtime code writes `PUSH4` for the six named signatures →
//!    `probe-{BLOCK}-entry-points.json`. This is the instrument that turns the answer of 1–8
//!    from a coincidence into a property of the bytecode: the pools of this triangle carry no
//!    dispatcher arm for the call M10's executor issues, and the pools of M10's own recording
//!    do.
//!
//! [`the_committed_entry_point_table_is_the_measurement_replayed`] replays instrument 9 from the
//! two committed dumps without a node and without a REVM run, so §40's verdict — this triangle
//! cannot be driven by M10's executor — is checkable by anyone who opens the file, not only by
//! whoever ran an ignored instrument.
//!
//! What none of them does is judge the market. A payout here is a fact about one trade at one
//! size on fixture-declared funding rows, not a claim that a trader could have made this profit,
//! so `REAL_PROFITABLE_ARBITRAGE` stays `UNKNOWN` (§41). `tests/multihop_e2e.rs` does deliver a
//! three-leg cycle, and says of itself that the venues it delivers through are declared
//! (`CONTROLLED_FIXTURE`) rather than this triangle — which is the separation §41 asks for, not
//! a step toward closing it.
//!
//! Nothing here signs, broadcasts, or holds a key. The operator is an address.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use alloy_primitives::{address, keccak256, Address, Bytes, U256};
use serde::{Deserialize, Serialize};

use evm_core::{BlockNumber, ChainId};
use evm_protocol::{decode_revert, ExecutorCall, ExecutorLeg, ExecutorRevert, RevertPayload};
use evm_simulation::error::SimulationError;
use evm_simulation::executor::{run, ExecutorOutcome, ExecutorRun};
use evm_simulation::state::{DumpStateProvider, StateDump, StateProvider};
use evm_simulation::{EvmRules, GasPricing};

mod executor_state;

use executor_state::{
    address_word, allowed_word, exact_out, executor_runtime_code, mapping_slot, plain_slot,
    workspace_root,
};

// ---------------------------------------------------------------------------
// The recorded world, and the scaffolding this probe puts on top of it
// ---------------------------------------------------------------------------

const CHAIN: ChainId = ChainId(91_342);
const BLOCK: u64 = 37_224_031;

/// The triangle, row for row from the capture: pool 1 holds tokens 1 and 2, pool 2 holds 2
/// and 3, pool 3 holds 3 and 1 — so the round trip starts and ends on token 1.
const TOKEN_1: Address = address!("0x03a884af9fa7af6557a21496f67e781fc8d00f95");
const TOKEN_2: Address = address!("0xc5bf73bddef871bafb49d12f5d05a5b300422cdf");
const TOKEN_3: Address = address!("0x7c20cb4ab7c6d731c6a167433986ffd17c16ddc6");
const POOL_1: Address = address!("0x0e70f2af6bff7c9810ee613814039d95a939e3b5");
const POOL_2: Address = address!("0xe1a9db8507570806ef64ff8d3583787c43f9eff0");
const POOL_3: Address = address!("0x354f408aa2f45fd8d955070e9ef1e47c368bfc42");

const TOKENS: [Address; 3] = [TOKEN_1, TOKEN_2, TOKEN_3];
const POOLS: [Address; 3] = [POOL_1, POOL_2, POOL_3];

/// The deterministic test sender every fixture of this repository funds (§58), recorded as
/// the chain knows it: an account holding none of these three tokens.
const OPERATOR: Address = address!("0x953e7e98562714c23bc22c7d186cdf516f9dfa6f");

/// M10's synthetic deployment and payee addresses — the same constants `executor_state`
/// uses, so a fixture over the triangle and a fixture over the recorded pair can differ in
/// their market and nowhere else.
const EXECUTOR: Address = address!("0x1000000000000000000000000000000000000010");
const RECIPIENT: Address = address!("0x2000000000000000000000000000000000000020");
const BYSTANDER: Address = address!("0x3000000000000000000000000000000000000030");

/// The factory all three recorded pools name in their own slot 0, and the address tokens 1
/// and 3 agree on in their slot 4. Both are in the candidate list because a token's
/// `transfer` can consult a mapping keyed by either, and a key the run reads is only
/// crackable if the holder it names is one of the holders being tried.
const FACTORY: Address = address!("0x760aa9d41cb1db0a262e44d0f1fc309644d079f3");
const TOKEN_OWNER: Address = address!("0x8bfef56efb629a6eb22d04c9edb69fa799223644");

/// The burn address and the zero address — both plausible keys in an exclusion map.
const DEAD: Address = address!("0x000000000000000000000000000000000000dead");
const ZERO: Address = address!("0x0000000000000000000000000000000000000000");

/// The recording under probe. `multihop_capture.rs` writes it; nothing here edits it.
const RECORDING: &str = "fixtures/simulation-m11/dump-37224031-triangle-03a884af.json";

/// Where the measured table goes, so the fixture can pin itself to it.
const PROBE_TABLE: &str = "fixtures/simulation-m11/probe-37224031-three-leg-slots.json";

/// `ArbitrageExecutor`: `mapping(address => bool) public pairAllowed` (slot 1),
/// `mapping(address => bool) public tokenAllowed` (slot 2), `uint256 private _lock` (slot 3).
/// These three are M10's layout claims, already checked by execution over the recorded pair;
/// reusing them here is what keeps the triangle fixture from resting on a fourth guess.
const PAIR_ALLOWED_SLOT: u64 = 1;
const TOKEN_ALLOWED_SLOT: u64 = 2;
const LOCK_SLOT: U256 = U256::from_limbs([3, 0, 0, 0]);

/// The stake the probe puts up: ten units of an 18-decimal token, far below every reserve
/// involved, so the price impact of the probe's own transfers is a rounding error and the run
/// is about plumbing rather than about the market.
const PROBE_AMOUNT: U256 = U256::from_limbs([10_000_000_000_000_000_000u64, 0, 0, 0]);

/// The gas ceiling and the native endowment M10's fixture uses, so a triangle run and a pair run
/// differ in their market and nowhere else.
const GAS_LIMIT: u64 = 3_000_000;
const ENDOWMENT: U256 = U256::from_limbs([10_000_000_000_000_000_000u64, 0, 0, 0]);

/// The ask every leg of the *first* instrument demands: one wei. A V2 pool honours a demand it can satisfy
/// and reverts with its own `K` when it cannot, so asking for a wei puts every leg on the
/// passing side of the fee boundary and leaves the executor's exact-equality checks as the
/// only thing that can fail — which is what makes a taxed token say so out loud.
const PERMISSIVE_ASK: U256 = U256::ONE;

/// The most refusals the loop will turn into state before it gives up. The route needs on the
/// order of fifteen words; a run still naming holes after a hundred has a missing *contract*,
/// not a missing row, and the honest answer is to say so.
const MAX_REFUSALS: usize = 120;

/// How far a cracked key's mapping index is searched. The capture enumerated scalar slots
/// `0..=0x14` for a pool and `0..=0x0a` for a token; a mapping index beyond that is a layout
/// claim this probe cannot make, and the table then says `unexplained` instead of guessing.
const CRACK_SLOTS: u64 = 24;

fn recording_path() -> PathBuf {
    workspace_root().join(RECORDING)
}

fn table_path() -> PathBuf {
    workspace_root().join(PROBE_TABLE)
}

// ---------------------------------------------------------------------------
// The derivations, and the cracking that turns a key back into a claim
// ---------------------------------------------------------------------------

fn pad(address: Address) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(address.as_slice());
    word
}

/// `keccak256(pad(key) ‖ pad(slot))` — one mapping's word for a flat key.
///
/// Re-derived here rather than only imported so the probe is a *second* implementation of the
/// derivation, and [`probe_agrees_with_the_fixtures_derivation`] can make the two check each
/// other instead of the fixture grading its own homework.
fn flat_key(holder: Address, slot: u64) -> U256 {
    let mut preimage = [0u8; 64];
    preimage[..32].copy_from_slice(&pad(holder));
    preimage[32..].copy_from_slice(&plain_slot(slot).to_be_bytes::<32>());
    keccak256(preimage).into()
}

/// `keccak256(pad(spender) ‖ inner)` — the outer word of a nested mapping, where `inner` is
/// itself the hash of the owner's row.
fn nested_key(spender: Address, inner: U256) -> U256 {
    let mut preimage = [0u8; 64];
    preimage[..32].copy_from_slice(&pad(spender));
    preimage[32..].copy_from_slice(&inner.to_be_bytes::<32>());
    keccak256(preimage).into()
}

/// Every holder whose keys are worth trying.
fn candidates() -> Vec<Address> {
    vec![
        TOKEN_1,
        TOKEN_2,
        TOKEN_3,
        POOL_1,
        POOL_2,
        POOL_3,
        OPERATOR,
        EXECUTOR,
        RECIPIENT,
        BYSTANDER,
        FACTORY,
        TOKEN_OWNER,
        DEAD,
        ZERO,
    ]
}

/// What a key the run read turns out to be, in the derivation that produces it — or the
/// honest `unexplained`, which means the fixture would have to invent a mapping this probe
/// cannot name.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Shape {
    /// A scalar slot, named by its index.
    Plain(u64),
    /// `mapping(index => value)` with this holder as the key.
    Flat { slot: u64, holder: Address },
    /// `mapping(outer => mapping(inner => value))`, spelled with the owner's row hashed
    /// first — which is what an `allowances` map is.
    Nested {
        slot: u64,
        owner: Address,
        spender: Address,
    },
    /// Nothing this probe can name.
    Unexplained,
}

impl Shape {
    fn describe(&self, contract: Address) -> String {
        match self {
            Shape::Plain(slot) => format!("plain slot {slot} of {contract:#x}"),
            Shape::Flat { slot, holder } => {
                format!("mapping at slot {slot} of {contract:#x}, key {holder:#x}")
            }
            Shape::Nested {
                slot,
                owner,
                spender,
            } => format!(
                "nested mapping at slot {slot} of {contract:#x}, owner {owner:#x}, spender \
                 {spender:#x}"
            ),
            Shape::Unexplained => "unexplained".to_string(),
        }
    }
}

/// Which derivation produces this key? The contract is not part of the answer — a key is
/// cracked from its own bytes, and the caller pairs it with the contract it was read from when
/// the row gets spelled out.
fn crack(slot: U256) -> Shape {
    if slot < U256::from(CRACK_SLOTS) {
        return Shape::Plain(slot.to::<u64>());
    }
    for holder in candidates() {
        for index in 0..CRACK_SLOTS {
            if flat_key(holder, index) == slot {
                return Shape::Flat {
                    slot: index,
                    holder,
                };
            }
        }
    }
    for owner in candidates() {
        for inner_slot in 0..CRACK_SLOTS {
            let inner = flat_key(owner, inner_slot);
            for spender in candidates() {
                if nested_key(spender, inner) == slot {
                    return Shape::Nested {
                        slot: inner_slot,
                        owner,
                        spender,
                    };
                }
            }
        }
    }
    Shape::Unexplained
}

// ---------------------------------------------------------------------------
// The probe
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
struct Step {
    /// `hole` — the provider refused to answer, and the probe wrote the word to get past it;
    /// `gate` — the contract refused, and the probe moved a declared row to get past it;
    /// `account` — the recording has no account row at all; `dead end` — nothing in this
    /// vocabulary fits, and the probe says so rather than inventing a way around it.
    kind: String,
    address: Address,
    /// The refused word, or the refusal's own text when the refusal is about an account.
    slot: U256,
    /// What was written in response.
    wrote: String,
    /// The derivation the refused key turns out to be, for a `hole`.
    crack: String,
    /// The refusal this step answers, in the provider's or the contract's own words.
    refusal: String,
}

#[derive(Clone, Debug, Serialize)]
struct KeyRow {
    address: Address,
    slot: U256,
    crack: String,
}

#[derive(Clone, Debug, Serialize)]
struct LegRow {
    index: usize,
    pool: Address,
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    amount_out: U256,
}

#[derive(Clone, Debug, Serialize)]
struct BalanceRow {
    token: Address,
    holder: Address,
    before: U256,
    after: U256,
    changed: bool,
}

#[derive(Clone, Debug, Serialize)]
struct ReserveRow {
    pool: Address,
    token0: Address,
    token1: Address,
    reserve0_before: U256,
    reserve0_after: U256,
    reserve1_before: U256,
    reserve1_after: U256,
    changed: bool,
}

#[derive(Clone, Debug, Serialize)]
struct Table {
    _provenance: String,
    recording: String,
    chain_id: u64,
    block_number: u64,
    probe_amount: U256,
    legs: Vec<LegRow>,
    steps: Vec<Step>,
    /// Every key the run read, in read order, with the claim each one is.
    keys: Vec<KeyRow>,
    /// `describe()` of the last run — the answer the loop stopped on.
    final_answer: String,
    /// Whether `final_answer` came from a run that reached the EVM. A provider refusal or a
    /// stopped loop is *not* a run, and the run-only fields below are then null rather than
    /// empty or false — an unmeasured "the market did not move" would read as a result.
    run_completed: bool,
    /// The named contract error, when the last run reverted with one of the executor's own.
    final_contract_error: Option<String>,
    /// Whether the last run moved the market at all.
    market_moved: Option<bool>,
    gas_used: Option<u64>,
    /// What the call answered with, when it answered at all.
    final_delivered: Option<U256>,
    /// The revert payload's own classification, in the protocol crate's words — `Empty` is
    /// the finding that started this file's second instrument.
    final_revert_kind: Option<String>,
    final_revert_bytes: Option<String>,
    /// The rows the run reported. A successful permissive run proves here that each token
    /// moved whole units — the tax question, answered by state rather than by a comment.
    balances: Option<Vec<BalanceRow>>,
    reserves: Option<Vec<ReserveRow>>,
    not_measured: Vec<String>,
}

/// The three legs, chained at the permissive ask.
fn probe_legs() -> Vec<ExecutorLeg> {
    [
        (POOL_1, TOKEN_1, TOKEN_2, PROBE_AMOUNT),
        (POOL_2, TOKEN_2, TOKEN_3, PERMISSIVE_ASK),
        (POOL_3, TOKEN_3, TOKEN_1, PERMISSIVE_ASK),
    ]
    .iter()
    .map(|(pool, token_in, token_out, amount_in)| ExecutorLeg {
        pool: *pool,
        token_in: *token_in,
        token_out: *token_out,
        amount_in: *amount_in,
        amount_out: PERMISSIVE_ASK,
        min_amount_out: PERMISSIVE_ASK,
    })
    .collect()
}

/// The probe's scaffolding: M10's contract at an address the recording has never seen,
/// configured the way a deployment would have configured it, and nothing else. Funding and
/// approvals are deliberately absent — a refusal to read them *is* the measurement.
fn scaffold(dump: &mut StateDump) {
    dump.insert_account(EXECUTOR, U256::ZERO, 0, &executor_runtime_code());
    for address in [RECIPIENT, BYSTANDER] {
        if dump.account(address).is_none() {
            dump.insert_account(address, U256::ZERO, 0, &Bytes::new());
        }
    }
    dump.insert_storage(EXECUTOR, U256::ZERO, address_word(OPERATOR));
    for pool in POOLS {
        dump.insert_storage(
            EXECUTOR,
            mapping_slot(pool, PAIR_ALLOWED_SLOT),
            allowed_word(true),
        );
    }
    for token in TOKENS {
        dump.insert_storage(
            EXECUTOR,
            mapping_slot(token, TOKEN_ALLOWED_SLOT),
            allowed_word(true),
        );
    }
    dump.insert_storage(EXECUTOR, LOCK_SLOT, U256::ONE);
}

fn spec(source: &str, legs: &[ExecutorLeg], input_token: Address) -> ExecutorRun {
    ExecutorRun {
        chain_id: CHAIN,
        priced_at: BlockNumber(BLOCK),
        state_source: source.to_string(),
        executor: EXECUTOR,
        operator: OPERATOR,
        call: ExecutorCall::Execute {
            legs: legs.to_vec(),
            amount_in: first_amount(legs),
            input_token,
            min_final_amount: legs
                .last()
                .expect("a route asks for something")
                .min_amount_out,
            recipient: RECIPIENT,
        },
        gas_limit: GAS_LIMIT,
        rules: EvmRules::Prague,
        pricing: gas_pricing(),
        endowment: Some(ENDOWMENT),
    }
}

/// The call's `amountIn`, taken from the route rather than restated beside it — `_checkRoute`
/// requires the two to agree, so a second source of the number would be a second way to be wrong.
fn first_amount(legs: &[ExecutorLeg]) -> U256 {
    legs.first()
        .expect("a route has at least one leg")
        .amount_in
}

/// The same transaction shape with the route replaced by the contract's own rescue path: one
/// token out of the executor to the route's payee. `withdraw` measures the payout as a delta at
/// the payee (M10 §19), which is exactly the question a three-leg fixture cannot answer for
/// itself from inside a multi-hop run.
fn withdrawal_spec(source: &str, token: Address, payee: Address, amount: U256) -> ExecutorRun {
    ExecutorRun {
        chain_id: CHAIN,
        priced_at: BlockNumber(BLOCK),
        state_source: source.to_string(),
        executor: EXECUTOR,
        operator: OPERATOR,
        call: ExecutorCall::Withdraw {
            token,
            to: payee,
            amount,
        },
        gas_limit: GAS_LIMIT,
        rules: EvmRules::Prague,
        pricing: gas_pricing(),
        endowment: Some(ENDOWMENT),
    }
}

fn gas_pricing() -> GasPricing {
    GasPricing::Eip1559 {
        priority_fee_per_gas: 0,
        provenance: format!(
            "block {BLOCK}'s own base fee as the recording's header reports it, with no tip — a \
             hypothetical transaction on a historical block is not competing to be included in it"
        ),
    }
}

/// One answer the loop can act on.
enum Move {
    /// `storage {address} slot {slot}` — the provider could not answer about one word.
    Word(Address, U256),
    /// `account {address}` or `code {address}` — the provider could not answer about an
    /// account. The probe declares it empty, because a route's payee is a wallet and the next
    /// refusal will say whether that was enough.
    Account(Address),
    /// A refusal this vocabulary has no answer for, kept as the provider's own sentence.
    DeadEnd(String),
}

/// Read a provider refusal in the shape the engine actually spells it.
///
/// `evm_simulation::engine` renders a `ProviderError::Missing` as
/// `missing state: {source}: {what}`, so the `what` — the part that names the missing row — is
/// only reachable by stripping the probe's own source sentence first. Doing it the other way
/// round (searching the whole string for the word `account`) would let a prose prefix be parsed
/// as a refusal, and the probe's source string deliberately contains that word.
fn classify(error: &SimulationError, source: &str) -> Move {
    let SimulationError::MissingState(text) = error else {
        return Move::DeadEnd(error.to_string());
    };
    let Some(what) = text.strip_prefix(&format!("{source}: ")) else {
        return Move::DeadEnd(text.clone());
    };
    if let Some(rest) = what.strip_prefix("storage ") {
        let (address, tail) = match rest.split_once(" slot ") {
            Some(pair) => pair,
            None => return Move::DeadEnd(text.clone()),
        };
        let Ok(contract) = address.parse::<Address>() else {
            return Move::DeadEnd(text.clone());
        };
        // The provider spells a slot the way `U256` displays one: decimal.
        let Ok(slot) = tail.trim().parse::<U256>() else {
            return Move::DeadEnd(text.clone());
        };
        return Move::Word(contract, slot);
    }
    for prefix in ["account ", "code "] {
        if let Some(address) = what.strip_prefix(prefix) {
            if let Ok(contract) = address.trim().parse::<Address>() {
                return Move::Account(contract);
            }
        }
    }
    Move::DeadEnd(text.clone())
}

/// The two refusals the loop is willing to answer by *moving* state rather than by adding a
/// word: the contract's own balance and allowance gates. Anything else is an answer, not an
/// obstacle.
fn gate(outcome: &ExecutorOutcome) -> Option<&'static str> {
    if outcome.succeeded() {
        return None;
    }
    match outcome.contract_error.as_deref() {
        Some("InsufficientBalance") => Some("InsufficientBalance"),
        Some("InsufficientAllowance") => Some("InsufficientAllowance"),
        _ => None,
    }
}

/// A row the contract's own gate says the run needs, and how the probe arrived at its key.
struct Funding {
    contract: Address,
    slot: U256,
    /// The derivation in terms of the holes the run itself named, so the table states which
    /// measurement justifies writing this word — not a slot typed in by the author.
    derivation: String,
}

/// The slot of `token`'s `mapping(address => uint256) balances`, as the run's own holes revealed
/// it. The executor and the recipient own none of these tokens on the recorded chain, so the
/// first row the run wanted for each of them was refused; the refused key cracks back into
/// `mapping at slot N`, which is the only reason this probe gets to name a balance slot.
fn learned_balance_slot(read: &[(Address, U256, Shape)], token: Address) -> Option<u64> {
    read.iter().rev().find_map(|(contract, _, shape)| {
        if *contract != token {
            return None;
        }
        match shape {
            Shape::Flat { slot, holder } if *holder == EXECUTOR || *holder == RECIPIENT => {
                Some(*slot)
            }
            _ => None,
        }
    })
}

/// The slot of `token`'s `allowances` mapping, from a nested hole the run named.
fn learned_allowance_slot(read: &[(Address, U256, Shape)], token: Address) -> Option<u64> {
    read.iter().rev().find_map(|(contract, _, shape)| {
        if *contract != token {
            return None;
        }
        match shape {
            Shape::Nested {
                slot,
                owner,
                spender,
            } if *owner == OPERATOR && *spender == EXECUTOR => Some(*slot),
            _ => None,
        }
    })
}

/// The key a gate's refusal is about.
///
/// `execute` pulls the input by reading the operator's balance and then the operator's allowance
/// for the executor, so when either error fires the row it needs is the operator's row on the
/// input token — at the mapping this probe learned from a hole, never at a slot it assumed. A
/// balance row can already exist in the recording at zero, which is why the key has to be derived
/// rather than taken from the list of holes: the recording answering "what is the operator's
/// balance" with 0 is a fact about the market, and the fixture's whole point is to say out loud
/// that it overrides it.
fn fundable(read: &[(Address, U256, Shape)], name: &str, token: Address) -> Option<Funding> {
    match name {
        "InsufficientBalance" => learned_balance_slot(read, token).map(|slot| Funding {
            contract: token,
            slot: flat_key(OPERATOR, slot),
            derivation: format!(
                "balances slot {slot} of {token:#x}, learned from the run's own refused row for \
                 the executor — the operator's balance at that key is a recorded zero, so this \
                 word is a declared fixture funding row rather than a hole being filled"
            ),
        }),
        "InsufficientAllowance" => learned_allowance_slot(read, token).map(|slot| Funding {
            contract: token,
            slot: nested_key(EXECUTOR, flat_key(OPERATOR, slot)),
            derivation: format!(
                "allowances slot {slot} of {token:#x}, owner {OPERATOR:#x}, spender \
                 {EXECUTOR:#x} — the slot learned from the run's own refused nested row"
            ),
        }),
        _ => None,
    }
}

/// What the table reports as its answer. The distinction that matters here is between a run
/// that reached the EVM and a probe that never got one: a provider refusal or a stopped loop is
/// not evidence that the market did not move, so the run-only fields stay null instead of being
/// filled with the shape of an absence.
struct Record {
    completed: bool,
    answer: String,
    contract_error: Option<String>,
    market_moved: Option<bool>,
    gas_used: Option<u64>,
    delivered: Option<U256>,
    revert_kind: Option<String>,
    revert_bytes: Option<String>,
    balances: Option<Vec<BalanceRow>>,
    reserves: Option<Vec<ReserveRow>>,
}

impl Record {
    fn from_outcome(outcome: &ExecutorOutcome) -> Record {
        Record {
            completed: true,
            answer: outcome.describe(),
            contract_error: outcome.contract_error.clone(),
            market_moved: Some(outcome.market_moved()),
            gas_used: Some(outcome.gas_used),
            delivered: outcome.delivered,
            revert_kind: outcome.revert_kind.map(String::from),
            revert_bytes: outcome
                .revert()
                .map(|data| format!("0x{}", hex::encode(&data.raw))),
            balances: Some(
                outcome
                    .balances
                    .iter()
                    .map(|row| BalanceRow {
                        token: row.token,
                        holder: row.holder,
                        before: row.before,
                        after: row.after,
                        changed: row.changed(),
                    })
                    .collect(),
            ),
            reserves: Some(
                outcome
                    .reserves
                    .iter()
                    .map(|row| ReserveRow {
                        pool: row.pool,
                        token0: row.before.token0,
                        token1: row.before.token1,
                        reserve0_before: row.before.reserve0,
                        reserve0_after: row.after.reserve0,
                        reserve1_before: row.before.reserve1,
                        reserve1_after: row.after.reserve1,
                        changed: row.changed(),
                    })
                    .collect(),
            ),
        }
    }

    /// The run never happened — the provider could not answer, and no word this probe may write
    /// would change that.
    fn refused(error: &SimulationError) -> Record {
        Record {
            completed: false,
            answer: error.to_string(),
            contract_error: None,
            market_moved: None,
            gas_used: None,
            delivered: None,
            revert_kind: None,
            revert_bytes: None,
            balances: None,
            reserves: None,
        }
    }

    /// The loop spent its whole budget on refusals it could answer without the run ever
    /// settling. How many words it wrote is itself the finding.
    fn stopped(steps: usize) -> Record {
        Record {
            completed: false,
            answer: format!(
                "the probe wrote {steps} words and still received no answer from the run"
            ),
            contract_error: None,
            market_moved: None,
            gas_used: None,
            delivered: None,
            revert_kind: None,
            revert_bytes: None,
            balances: None,
            reserves: None,
        }
    }
}

/// Walk the run's refusals until it answers, and write down every step.
///
/// This is an instrument, so it is allowed to be more adaptive than a test: each step is both
/// recorded and minimal — a hole gets exactly the word the provider named, and a gate gets
/// the one key the run just read funded. A step that is neither is reported and the loop ends.
///
/// Alongside the table it hands back the rows it passed through, still typed: [`census`] needs to
/// know which slot each token's `balances` mapping turned out to be, and reading that out of the
/// table's own prose would be a second place to get it wrong.
async fn walk(
    dump: &mut StateDump,
    source: &str,
    legs: &[ExecutorLeg],
    input_token: Address,
) -> (Table, Vec<(Address, U256, Shape)>) {
    let mut steps: Vec<Step> = Vec::new();
    let mut read: Vec<(Address, U256, Shape)> = Vec::new();
    let mut record: Option<Record> = None;

    for _ in 0..MAX_REFUSALS {
        let provider: Arc<dyn StateProvider> =
            Arc::new(DumpStateProvider::new(dump.clone(), source.to_string()));
        match run(provider, &spec(source, legs, input_token)).await {
            Ok(outcome) => {
                let Some(name) = gate(&outcome) else {
                    record = Some(Record::from_outcome(&outcome));
                    break;
                };
                let refusal = outcome.describe();
                let Some(row) = fundable(&read, name, input_token) else {
                    steps.push(Step {
                        kind: "dead end".to_string(),
                        address: ZERO,
                        slot: U256::ZERO,
                        wrote: format!(
                            "nothing: no hole this run named reveals a {name} mapping on {input_token:#x}, \
                             so the probe would have to invent a slot to answer it"
                        ),
                        crack: String::new(),
                        refusal,
                    });
                    record = Some(Record::from_outcome(&outcome));
                    break;
                };
                dump.insert_storage(row.contract, row.slot, PROBE_AMOUNT);
                steps.push(Step {
                    kind: "gate".to_string(),
                    address: row.contract,
                    slot: row.slot,
                    wrote: PROBE_AMOUNT.to_string(),
                    crack: row.derivation,
                    refusal: format!("{name}: {refusal}"),
                });
            }
            Err(error) => match classify(&error, source) {
                Move::Word(contract, slot) => {
                    let shape = crack(slot);
                    if shape == Shape::Unexplained {
                        steps.push(Step {
                            kind: "dead end".to_string(),
                            address: contract,
                            slot,
                            wrote: "nothing: the key is not derivable from a mapping this probe \
                                   can name"
                                .to_string(),
                            crack: shape.describe(contract),
                            refusal: error.to_string(),
                        });
                        record = Some(Record::refused(&error));
                        break;
                    }
                    dump.insert_storage(contract, slot, U256::ZERO);
                    steps.push(Step {
                        kind: "hole".to_string(),
                        address: contract,
                        slot,
                        wrote: "0".to_string(),
                        crack: shape.describe(contract),
                        refusal: error.to_string(),
                    });
                    read.push((contract, slot, shape));
                }
                Move::Account(contract) => {
                    dump.insert_account(contract, U256::ZERO, 0, &Bytes::new());
                    steps.push(Step {
                        kind: "account".to_string(),
                        address: contract,
                        slot: U256::ZERO,
                        wrote: "an account with no balance, no nonce and no code".to_string(),
                        crack: "the recording holds no account row for it".to_string(),
                        refusal: error.to_string(),
                    });
                }
                Move::DeadEnd(text) => {
                    steps.push(Step {
                        kind: "dead end".to_string(),
                        address: ZERO,
                        slot: U256::ZERO,
                        wrote: String::new(),
                        crack: String::new(),
                        refusal: text,
                    });
                    record = Some(Record::refused(&error));
                    break;
                }
            },
        }
    }

    let record = record.unwrap_or_else(|| Record::stopped(MAX_REFUSALS));

    let table = Table {
        _provenance: format!(
            "crates/simulation/tests/triangle_probe.rs — a measurement of which storage words \
             a three-leg round trip reads, taken by running M10's executor against {RECORDING}. \
             It measures no fee and judges no market"
        ),
        recording: RECORDING.to_string(),
        chain_id: CHAIN.0,
        block_number: BLOCK,
        probe_amount: PROBE_AMOUNT,
        legs: legs
            .iter()
            .enumerate()
            .map(|(index, leg)| LegRow {
                index,
                pool: leg.pool,
                token_in: leg.token_in,
                token_out: leg.token_out,
                amount_in: leg.amount_in,
                amount_out: leg.amount_out,
            })
            .collect(),
        steps,
        keys: read
            .iter()
            .map(|(contract, slot, shape)| KeyRow {
                address: *contract,
                slot: *slot,
                crack: shape.describe(*contract),
            })
            .collect(),
        final_answer: record.answer,
        run_completed: record.completed,
        final_contract_error: record.contract_error,
        market_moved: record.market_moved,
        gas_used: record.gas_used,
        final_delivered: record.delivered,
        final_revert_kind: record.revert_kind,
        final_revert_bytes: record.revert_bytes,
        balances: record.balances,
        reserves: record.reserves,
        not_measured: vec![
            "the three pools' effective fees — measured by the second instrument in this file, \
             `measure_what_each_recorded_pool_honours`, which chains the claims the exact-equality \
             checks demand and reads each pool's answer out of the contract's own \
             `DeliveryMismatch`"
                .to_string(),
            "whether any direction of this cycle is profitable — nothing in this repository trades \
             these pools, so the question is not asked here and §41 keeps \
             `REAL_PROFITABLE_ARBITRAGE` at `UNKNOWN`"
                .to_string(),
        ],
    };
    (table, read)
}

// ---------------------------------------------------------------------------
// The second instrument: what each recorded pool actually pays
// ---------------------------------------------------------------------------

/// The first instrument answers *which words a run reads*. It cannot answer what the pools pay,
/// because its route asks for one wei on every leg, and `ArbitrageExecutor` compares each leg's
/// claim against the pool's payout with exact equality (`DeliveryMismatch`) and against the
/// previous leg's claim with exact equality (`AmountChainBroken`, checked up front). A permissive
/// ask therefore dies in leg 0 as a fact about the contract's checks rather than about the market.
///
/// The second instrument asks the market directly: it chains the claims at what a 0.3 % pool
/// would pay on the recorded balances, runs the cycle, and reads each pool's true payout out of
/// the contract's own error arguments — `DeliveryMismatch: leg {i} was paid {received}` *is* the
/// measurement — then repairs that leg's claim and runs again. A leg whose claim the pool honoured
/// needed no repair, so its payout is the claim; a leg that was repaired carries the number the
/// pool paid. Either way the fee falls out of `amount_in`, the payout and the two sides, and every
/// number in the table was said by the EVM rather than typed in beside it.
const CHAIN_TABLE: &str = "fixtures/simulation-m11/probe-37224031-pool-claims.json";

/// The most repairs the loop makes before it reports the answer it already has. Three legs need
/// at most three, so a fourth pass means the contract is not converging on a payout.
const MAX_CHAIN_RUNS: usize = 12;

fn chain_table_path() -> PathBuf {
    workspace_root().join(CHAIN_TABLE)
}

/// The pool's recorded balance of one of its two tokens, at the `balances` slot the first
/// instrument measured. For a synced V2 pair this *is* the reserve the pair trades against, and
/// the table keeps the pool's own `getReserves()` answer beside it so a reader can see whether
/// that identity held here rather than being told it did.
fn pool_side(
    read: &[(Address, U256, Shape)],
    dump: &StateDump,
    token: Address,
    pool: Address,
) -> Result<U256, String> {
    let slot = learned_balance_slot(read, token).ok_or_else(|| {
        format!("no row the run refused named reveals a balances slot for {token:#x}")
    })?;
    dump.storage(token, flat_key(pool, slot)).ok_or_else(|| {
        format!("the recording holds no {token:#x} balance of {pool:#x} at balances slot {slot}")
    })
}

/// A priced cycle and the market it was priced against: the legs to issue, and, in the same
/// order, the `(reserve_in, reserve_out)` pair each leg's payout came from. The second half is
/// what lets the instrument re-price a tail from the recorded sides without reading the dump
/// again, so the two vectors are always the same length and always correspond by position.
type ChainedRoute = (Vec<ExecutorLeg>, Vec<(U256, U256)>);

/// The three legs of the cycle, with amounts chained at the 0.3 % prediction on the recorded
/// balances. The pools are the claim's counterparty, so this is the one place the fee is
/// *assumed* — and the loop below replaces every assumption with the pool's own answer.
fn chained(
    read: &[(Address, U256, Shape)],
    dump: &StateDump,
    amount_in: U256,
) -> Result<ChainedRoute, String> {
    let mut legs: Vec<ExecutorLeg> = Vec::new();
    let mut sides: Vec<(U256, U256)> = Vec::new();
    let mut carried = amount_in;
    for (index, template) in probe_legs().into_iter().enumerate() {
        let reserve_in = pool_side(read, dump, template.token_in, template.pool)?;
        let reserve_out = pool_side(read, dump, template.token_out, template.pool)?;
        let paid = exact_out(carried, reserve_in, reserve_out);
        if paid.is_zero() {
            return Err(format!(
                "leg {index} predicts a zero payout for {carried} in against {reserve_in}/\
                 {reserve_out} — the recorded side is too thin for this stake"
            ));
        }
        legs.push(ExecutorLeg {
            amount_in: carried,
            amount_out: paid,
            min_amount_out: paid,
            ..template
        });
        sides.push((reserve_in, reserve_out));
        carried = paid;
    }
    Ok((legs, sides))
}

/// Replace every claim from `from` onwards with what a 0.3 % pool would pay on `carried`, which
/// is what the run just proved the contract holds. Returns `None` when a recorded side cannot
/// price the tail, and the loop then reports the answer it has.
fn restail(
    legs: &mut [ExecutorLeg],
    sides: &[(U256, U256)],
    from: usize,
    carried: U256,
) -> Option<U256> {
    let mut amount = carried;
    for (index, leg) in legs.iter_mut().enumerate().skip(from) {
        let (reserve_in, reserve_out) = sides[index];
        let paid = exact_out(amount, reserve_in, reserve_out);
        if paid.is_zero() {
            return None;
        }
        leg.amount_in = amount;
        leg.amount_out = paid;
        leg.min_amount_out = paid;
        amount = paid;
    }
    Some(amount)
}

/// One pass of the chain loop: the claims it carried, the answer it got, and what it changed
/// because of that answer.
#[derive(Clone, Debug, Serialize)]
struct ChainRow {
    run: usize,
    /// Each leg as `claimed in -> claimed out`, in order.
    claims: Vec<String>,
    answer: String,
    contract_error: Option<String>,
    revert_kind: Option<String>,
    delivered: Option<U256>,
    gas_used: Option<u64>,
    market_moved: Option<bool>,
    /// The repair the next run carries, or why there is no next run.
    repair: String,
}

/// One pool, its recorded sides, and what it paid.
#[derive(Clone, Debug, Serialize)]
struct PoolRow {
    index: usize,
    pool: Address,
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    reserve_in: U256,
    reserve_out: U256,
    /// What the call asked the pool for.
    asked: Option<U256>,
    /// What the pool paid, from its own `DeliveryMismatch` argument or from the claim it
    /// accepted without complaint.
    paid: Option<U256>,
    /// `1e6 · (1 − paid·reserve_in / (amount_in·(reserve_out − paid)))` — the fee the pool kept,
    /// solved out of the three numbers the run reported rather than read off a configuration.
    fee_ppm: Option<u128>,
    /// How this row's `paid` was obtained, including the honest `never reached`.
    how_measured: String,
}

/// The table the second instrument writes.
#[derive(Clone, Debug, Serialize)]
struct ChainTable {
    _provenance: String,
    recording: String,
    chain_id: u64,
    block_number: u64,
    stake: U256,
    /// The keys the first instrument measured, which this one's funding and reserves depend on.
    keys: Vec<KeyRow>,
    /// The state this instrument declared on top of the recording, in the words of the refusals
    /// that made it declare them.
    funding: Vec<Step>,
    legs: Vec<LegRow>,
    runs: Vec<ChainRow>,
    pools: Vec<PoolRow>,
    /// Whether the cycle delivered end to end.
    settled: bool,
    delivered: Option<U256>,
    /// Whether the last run moved the market, and the reserves the last run reported — the
    /// pair's own answer, kept next to the balances this instrument priced against.
    market_moved: Option<bool>,
    gas_used: Option<u64>,
    final_reserves: Option<Vec<ReserveRow>>,
    not_measured: Vec<String>,
}

/// The ppm fee a pool kept, solved from the three numbers the run reported. `None` is not a zero
/// fee: it means the payout does not sit on the near-side of its own reserve, so no fee fraction
/// between 0 and 1 explains it and the table says so instead of dividing anyway.
fn implied_fee_ppm(
    amount_in: U256,
    reserve_in: U256,
    reserve_out: U256,
    paid: U256,
) -> Option<u128> {
    if reserve_out <= paid || amount_in.is_zero() || reserve_in.is_zero() {
        return None;
    }
    // paid · reserve_in = amount_in · (1 − f) · (reserve_out − paid)
    let kept = paid * reserve_in;
    let den = amount_in * (reserve_out - paid);
    if kept > den {
        return None;
    }
    ((den - kept) * U256::from(1_000_000u32) / den)
        .to::<u128>()
        .into()
}

/// Run the cycle until the pools stop correcting its claims.
async fn honour_chains(
    dump: &StateDump,
    source: &str,
    read: &[(Address, U256, Shape)],
    key_rows: &[KeyRow],
    funding: &[Step],
) -> ChainTable {
    let mut runs: Vec<ChainRow> = Vec::new();
    let mut settled = false;
    let mut last: Option<Record> = None;
    let mut repeated: Option<(usize, U256)> = None;

    let (mut legs, sides) = match chained(read, dump, PROBE_AMOUNT) {
        Ok(pair) => pair,
        Err(reason) => {
            return ChainTable {
                _provenance: "crates/simulation/tests/triangle_probe.rs — the chain instrument \
                              never ran: the recorded triangle could not price the claims from \
                              its own balances"
                    .to_string(),
                recording: RECORDING.to_string(),
                chain_id: CHAIN.0,
                block_number: BLOCK,
                stake: PROBE_AMOUNT,
                keys: key_rows.to_vec(),
                funding: funding.to_vec(),
                legs: Vec::new(),
                runs: Vec::new(),
                pools: Vec::new(),
                settled: false,
                delivered: None,
                market_moved: None,
                gas_used: None,
                final_reserves: None,
                not_measured: vec![format!("every pool's payout — {reason}")],
            };
        }
    };
    let mut pools: Vec<PoolRow> = legs
        .iter()
        .zip(sides.iter())
        .enumerate()
        .map(|(index, (leg, (reserve_in, reserve_out)))| PoolRow {
            index,
            pool: leg.pool,
            token_in: leg.token_in,
            token_out: leg.token_out,
            amount_in: leg.amount_in,
            reserve_in: *reserve_in,
            reserve_out: *reserve_out,
            asked: None,
            paid: None,
            fee_ppm: None,
            how_measured: "never reached".to_string(),
        })
        .collect();

    for run_number in 1..=MAX_CHAIN_RUNS {
        let provider: Arc<dyn StateProvider> =
            Arc::new(DumpStateProvider::new(dump.clone(), source.to_string()));
        let claims: Vec<String> = legs
            .iter()
            .map(|leg| format!("{} in -> {} out", leg.amount_in, leg.amount_out))
            .collect();
        let outcome = match run(provider, &spec(source, &legs, TOKEN_1)).await {
            Ok(outcome) => outcome,
            Err(error) => {
                let record = Record::refused(&error);
                runs.push(ChainRow {
                    run: run_number,
                    claims,
                    answer: record.answer.clone(),
                    contract_error: record.contract_error.clone(),
                    revert_kind: record.revert_kind.clone(),
                    delivered: record.delivered,
                    gas_used: record.gas_used,
                    market_moved: record.market_moved,
                    repair: "the provider refused a word, and no claim change answers that"
                        .to_string(),
                });
                last = Some(record);
                break;
            }
        };
        let record = Record::from_outcome(&outcome);
        let payload = outcome
            .revert()
            .map(|data| decode_revert(&data.raw))
            .filter(|payload| !matches!(payload, RevertPayload::Empty));
        let mut repair = String::from("nothing — the loop ends on this answer");

        if outcome.succeeded() {
            settled = true;
            for (leg, pool) in legs.iter().zip(pools.iter_mut()) {
                pool.asked = Some(leg.amount_out);
                pool.paid = Some(leg.amount_out);
                pool.how_measured = "the pool paid the claim exactly — `DeliveryMismatch` is an \
                                     equality check, so a leg that never raised it was paid in \
                                     full"
                    .to_string();
            }
            repair = "the cycle delivered; no claim was left to repair".to_string();
            refresh_fees(&mut pools, &legs);
            runs.push(ChainRow {
                run: run_number,
                claims,
                answer: record.answer.clone(),
                contract_error: record.contract_error.clone(),
                revert_kind: record.revert_kind.clone(),
                delivered: record.delivered,
                gas_used: record.gas_used,
                market_moved: record.market_moved,
                repair,
            });
            last = Some(record);
            break;
        }

        let index_and_payout = match payload {
            Some(RevertPayload::Executor(ExecutorRevert::DeliveryMismatch {
                index,
                asked,
                received,
            })) => {
                let index = index.to::<usize>();
                if index < pools.len() {
                    pools[index].asked = Some(asked);
                    pools[index].paid = Some(received);
                    pools[index].how_measured = "read out of this run's own `DeliveryMismatch` \
                                                 arguments"
                        .to_string();
                }
                Some((index, received))
            }
            Some(RevertPayload::Executor(ExecutorRevert::HoldingMismatch {
                index,
                held,
                claimed,
                ..
            })) => {
                let index = index.to::<usize>();
                if index > 0 && index <= pools.len() {
                    pools[index - 1].paid = Some(held);
                    pools[index - 1].how_measured = format!(
                        "the pool paid {held} and the contract proved it before asking — \
                         `HoldingMismatch` on leg {index} compares the previous leg's delivery \
                         against a claim of {claimed}"
                    )
                }
                Some((index, held))
            }
            other => {
                repair = match other {
                    Some(RevertPayload::Executor(ExecutorRevert::InputDeliveryMismatch {
                        claimed,
                        received,
                    })) => format!(
                        "{claimed} of {TOKEN_1:#x} was sent and {received} arrived: the input \
                         token takes a fee on transfer, so no chain of claims can route it \
                         through this contract"
                    ),
                    Some(RevertPayload::Executor(revert)) => format!(
                        "the contract answered {} — a check a claim repair cannot pass",
                        revert.name()
                    ),
                    Some(RevertPayload::StandardString(message)) => format!(
                        "an external contract said {message:?} — its own check, not this \
                         contract's"
                    ),
                    Some(payload) => format!(
                        "the payload is a {} rather than one of this contract's rejections",
                        payload.kind()
                    ),
                    _ => "the payload is not the contract's own, so the run's answer is about an \
                          external call rather than about a claim"
                        .to_string(),
                };
                None
            }
        };

        if let Some((index, payout)) = index_and_payout {
            if payout.is_zero() {
                repair =
                    format!("leg {index} was paid nothing, so the cycle cannot be chained past it");
            } else if repeated == Some((index, payout)) {
                repair = format!(
                    "leg {index} was paid {payout} twice — the loop is not converging on a \
                     payout, so it stops and reports what it has"
                );
            } else {
                repeated = Some((index, payout));
                if index < legs.len() {
                    legs[index].amount_out = payout;
                    legs[index].min_amount_out = payout;
                }
                if index + 1 < legs.len() {
                    match restail(&mut legs, &sides, index + 1, payout) {
                        Some(_) => {
                            repair = format!(
                            "leg {index}'s claim is now the {payout} the pool paid, and the tail \
                             was re-priced from it"
                        )
                        }
                        None => {
                            repair = format!(
                            "leg {index}'s claim is now {payout}, and the recorded sides cannot \
                             price the tail on it"
                        )
                        }
                    }
                } else {
                    repair = format!("the closing leg's claim is now {payout}");
                }
            }
            refresh_fees(&mut pools, &legs);
        }

        runs.push(ChainRow {
            run: run_number,
            claims,
            answer: record.answer.clone(),
            contract_error: record.contract_error.clone(),
            revert_kind: record.revert_kind.clone(),
            delivered: record.delivered,
            gas_used: record.gas_used,
            market_moved: record.market_moved,
            repair,
        });
        last = Some(record);
        if index_and_payout.is_none() {
            break;
        }
    }

    let record = last.unwrap_or_else(|| Record::stopped(MAX_CHAIN_RUNS));
    refresh_fees(&mut pools, &legs);
    ChainTable {
        _provenance: format!(
            "crates/simulation/tests/triangle_probe.rs — a measurement of what the three recorded \
             pools pay a chained three-leg cycle, taken by running M10's executor against \
             {RECORDING} and reading each payout out of the contract's own error arguments. Every \
             fee here is solved from a payout the EVM reported, not read off a configuration"
        ),
        recording: RECORDING.to_string(),
        chain_id: CHAIN.0,
        block_number: BLOCK,
        stake: PROBE_AMOUNT,
        keys: key_rows.to_vec(),
        funding: funding.to_vec(),
        legs: legs
            .iter()
            .enumerate()
            .map(|(index, leg)| LegRow {
                index,
                pool: leg.pool,
                token_in: leg.token_in,
                token_out: leg.token_out,
                amount_in: leg.amount_in,
                amount_out: leg.amount_out,
            })
            .collect(),
        runs,
        pools,
        settled,
        delivered: record.delivered,
        market_moved: record.market_moved,
        gas_used: record.gas_used,
        final_reserves: record.reserves,
        not_measured: vec![
            "whether any direction of this cycle is profitable for a real trader — this instrument \
             stakes a fixture-funded amount on fixture-declared rows over pools no run of this \
             repository trades on, so §41 keeps `REAL_PROFITABLE_ARBITRAGE` at `UNKNOWN`"
                .to_string(),
            "the pools' fee *configuration* — the ppm column is the fee kept on one specific \
             trade at one specific size, which is the only fee claim a simulation can make"
                .to_string(),
        ],
    }
}

/// Recompute the ppm column from whichever payouts this run knows about.
fn refresh_fees(pools: &mut [PoolRow], legs: &[ExecutorLeg]) {
    for pool in pools.iter_mut() {
        let Some(paid) = pool.paid else { continue };
        let Some(leg) = legs.get(pool.index) else {
            continue;
        };
        pool.amount_in = leg.amount_in;
        pool.fee_ppm = implied_fee_ppm(leg.amount_in, pool.reserve_in, pool.reserve_out, paid);
    }
}

// ---------------------------------------------------------------------------
// The measurements
// ---------------------------------------------------------------------------

/// The provider-refusal reader is itself a claim, so it gets a negative control: a sentence
/// that *mentions* an account without being a refusal about one must not be read as one.
/// Otherwise the probe declares a row the run never asked for, and the table looks like a
/// measurement.
#[test]
fn classify_reads_the_refusal_only_after_its_own_source_prefix() {
    let source = "probe source";
    let stray = "0x03a884af9fa7af6557a21496f67e781fc8d00f95";

    let cases: Vec<(SimulationError, bool)> = vec![
        (
            SimulationError::MissingState(format!("{source}: account {stray}")),
            true,
        ),
        (
            SimulationError::MissingState(format!("{source}: code {stray}")),
            true,
        ),
        (
            SimulationError::MissingState(format!("{source}: storage {stray} slot 42")),
            true,
        ),
        // The same words behind a different source sentence are not this probe's refusal, so
        // they are not actionable. A substring reader would take both of the first two for
        // account holes and the third for a missing slot.
        (
            SimulationError::MissingState(format!("other source: account {stray}")),
            false,
        ),
        (
            SimulationError::MissingState(format!(
                "the fixture records no account row, and mentions {stray} anyway"
            )),
            false,
        ),
        (
            SimulationError::MissingState(format!("{source}: header {stray}")),
            false,
        ),
        (
            SimulationError::MissingState(format!("{source}: storage {stray} slot not-a-number")),
            false,
        ),
    ];

    for (error, actionable) in cases {
        let acted_on = match classify(&error, source) {
            Move::Account(_) | Move::Word(_, _) => true,
            Move::DeadEnd(_) => false,
        };
        assert_eq!(
            acted_on, actionable,
            "wrong actionability, expected actionable={actionable}: {error}"
        );
    }

    // A refused slot is read the way the provider spells it, so a decimal answer must survive
    // the round trip — otherwise a hole would be filled at the wrong key and the "fix" would be
    // in vain while the loop kept re-reading the same refusal.
    let error = SimulationError::MissingState(format!("{source}: storage {stray} slot 42"));
    let Move::Word(contract, slot) = classify(&error, source) else {
        panic!("a decimal slot must be readable: {error}")
    };
    assert_eq!(
        contract,
        stray.parse::<Address>().expect("the address parses")
    );
    assert_eq!(slot, U256::from(42));
}

/// The derivation the probe uses is the derivation the fixture uses — checked, not assumed.
#[test]
fn probe_agrees_with_the_fixtures_derivation() {
    for holder in candidates() {
        for slot in [0u64, 1, 3, 5, 8, 0x14] {
            assert_eq!(
                flat_key(holder, slot),
                mapping_slot(holder, slot),
                "two implementations of `keccak(pad(key) ‖ pad(slot))` disagree at {holder:#x} \
                 slot {slot}"
            );
        }
    }
}

/// The recording, scaffolded, with the source sentence every run of this file carries.
fn probed() -> (StateDump, String) {
    let mut dump = StateDump::from_file(&recording_path())
        .unwrap_or_else(|error| panic!("{}: {error}", recording_path().display()));
    scaffold(&mut dump);
    let source = format!(
        "CONTROLLED_FIXTURE probe: {RECORDING} (three recorded pools, three recorded tokens, \
         recorded header) with only M10's deployment words declared"
    );
    (dump, source)
}

/// Walk the three-leg run's refusals and write the table.
///
/// Ignored because it is an instrument rather than a gate: it changes no committed file except
/// its own table, and the tests that need its answers read what was committed.
#[tokio::test]
#[ignore = "measurement: runs M10's executor over the recorded triangle to discover which \
             storage words it reads, then writes \
             fixtures/simulation-m11/probe-37224031-three-leg-slots.json — invoke with `cargo \
             test -p evm-simulation --test triangle_probe -- --ignored --nocapture`"]
async fn measure_which_slots_a_three_leg_run_reads() {
    let (mut dump, source) = probed();
    let (table, _learned) = walk(&mut dump, &source, &probe_legs(), TOKEN_1).await;
    write_json(&table_path(), &table);

    println!(
        "{} steps, {} keys read",
        table.steps.len(),
        table.keys.len()
    );
    for step in &table.steps {
        println!(
            "  [{}] {:#x} {} wrote {} as {} — because {}",
            step.kind, step.address, step.slot, step.wrote, step.crack, step.refusal
        );
    }
    println!("final: {}", table.final_answer);
    for row in table.balances.as_deref().unwrap_or(&[]) {
        println!(
            "  balance {:#x} of {:#x}: {} -> {}{}",
            row.holder,
            row.token,
            row.before,
            row.after,
            if row.changed { " (moved)" } else { "" }
        );
    }
    for row in table.reserves.as_deref().unwrap_or(&[]) {
        println!(
            "  reserve {:#x}: ({}, {}) -> ({}, {}){}",
            row.pool,
            row.reserve0_before,
            row.reserve1_before,
            row.reserve0_after,
            row.reserve1_after,
            if row.changed { " (moved)" } else { "" }
        );
    }
    println!("wrote {}", table_path().display());
}

/// An instrument's table is an artifact, so it is written the same way both times: pretty, one
/// trailing newline, and no field that only exists in the printed form.
fn write_json(path: &std::path::Path, value: &impl Serialize) {
    let mut text = serde_json::to_string_pretty(value)
        .expect("the table serializes")
        .into_bytes();
    text.push(b'\n');
    std::fs::create_dir_all(path.parent().expect("the fixture directory has a parent"))
        .expect("the probe directory exists");
    std::fs::write(path, &text).expect("the probe table writes");
}

/// Run the cycle until the pools stop correcting its claims, and write what they paid.
///
/// Ignored for the same reason the first instrument is: it is how the committed table was made,
/// not a gate that runs on every `cargo test`.
#[tokio::test]
#[ignore = "measurement: chains a three-leg cycle's claims at the 0.3 % prediction, runs it \
            against the recorded triangle, and reads each pool's real payout out of the \
            contract's own `DeliveryMismatch` arguments, then writes \
            fixtures/simulation-m11/probe-37224031-pool-claims.json — invoke with `cargo test -p \
            evm-simulation --test triangle_probe -- --ignored --nocapture`"]
async fn measure_what_each_recorded_pool_honours() {
    let (mut dump, source) = probed();
    let (slots, learned) = walk(&mut dump, &source, &probe_legs(), TOKEN_1).await;
    let table = honour_chains(&dump, &source, &learned, &slots.keys, &slots.steps).await;
    write_json(&chain_table_path(), &table);

    println!(
        "{} runs, settled: {}, delivered: {}, market moved: {}",
        table.runs.len(),
        table.settled,
        table
            .delivered
            .map(|amount| amount.to_string())
            .unwrap_or_else(|| "null".to_string()),
        table
            .market_moved
            .map(|moved| moved.to_string())
            .unwrap_or_else(|| "null".to_string())
    );
    for row in &table.runs {
        println!(
            "  run {}: [{}] -> {} | {}",
            row.run,
            row.claims.join(" | "),
            row.contract_error
                .as_deref()
                .or(row.revert_kind.as_deref())
                .unwrap_or("settled"),
            row.repair
        );
    }
    for pool in &table.pools {
        println!(
            "  leg {} {:#x} -> {:#x} at {:#x}: in {} reserve {}/{} asked {} paid {} fee {} \
             ppm — {}",
            pool.index,
            pool.token_in,
            pool.token_out,
            pool.pool,
            pool.amount_in,
            pool.reserve_in,
            pool.reserve_out,
            pool.asked.map(|v| v.to_string()).unwrap_or_default(),
            pool.paid.map(|v| v.to_string()).unwrap_or_default(),
            pool.fee_ppm
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".to_string()),
            pool.how_measured
        );
    }
    for row in table.final_reserves.as_deref().unwrap_or(&[]) {
        println!(
            "  getReserves {:#x}: ({}, {}) -> ({}, {}){}",
            row.pool,
            row.reserve0_before,
            row.reserve1_before,
            row.reserve0_after,
            row.reserve1_after,
            if row.changed { " (moved)" } else { "" }
        );
    }
    println!("wrote {}", chain_table_path().display());
}

// ---------------------------------------------------------------------------
// Instrument 3 — which recorded token moves at all
// ---------------------------------------------------------------------------

/// The third table: the same three tokens, paid out through the contract's own rescue door
/// instead of through a route.
///
/// Both previous instruments end at a revert that carries no payload, and the executor cannot
/// produce one — every one of its rejections names itself. So the answer came from an external
/// call, and there are two authors it could have come from: a token's `transfer` or a pool's
/// `swap`. `withdraw` tells them apart. It is `onlyOperator`, its whole effect is one
/// `token.transfer(to, amount)`, and M10 proved the door works over recorded state
/// (`executor_revm.rs::the_operator_withdraws_what_the_contract_holds`). A token that pays here
/// is not where the three-leg run stops; a token that reverts here with the same empty payload
/// is exactly where it stops, and the pools have nothing to do with it.
const TRANSFER_TABLE: &str = "fixtures/simulation-m11/probe-37224031-token-transfers.json";

fn transfer_table_path() -> PathBuf {
    workspace_root().join(TRANSFER_TABLE)
}

#[derive(Clone, Debug, Serialize)]
struct TransferRow {
    index: usize,
    token: Address,
    /// The `balances` slot instrument 1 measured for this token, which is what makes the three
    /// declared rows below the right rows rather than three guesses.
    balance_slot: Option<u64>,
    /// What the fixture wrote into the executor's row before the call.
    declared: U256,
    answer: String,
    contract_error: Option<String>,
    revert_kind: Option<String>,
    revert_bytes: Option<String>,
    gas_used: Option<u64>,
    /// The payee's delta, which is the token's own answer about what it let through.
    paid: Option<U256>,
    verdict: String,
}

#[derive(Clone, Debug, Serialize)]
struct TransferTable {
    _provenance: String,
    recording: String,
    chain_id: u64,
    block_number: u64,
    declared: U256,
    rows: Vec<TransferRow>,
    /// One sentence in the table's own numbers: the tokens that paid, and the tokens that did
    /// not. A reader who disagrees with the reading can check it against the rows.
    reading: String,
    not_measured: Vec<String>,
}

/// What the withdrawal's numbers mean, in the vocabulary the other two instruments use.
fn transfer_verdict(paid: Option<U256>, declared: U256, record: &Record) -> String {
    match (
        paid,
        record.contract_error.as_deref(),
        record.revert_kind.as_deref(),
    ) {
        (Some(paid), _, _) if paid == declared => {
            "the token paid the full declared amount through `withdraw`, so its `transfer` \
             answers for a holding this fixture declared — the three-leg run does not stop \
             inside this token"
                .to_string()
        }
        (Some(paid), _, _) => {
            format!(
                "the token paid {paid} of the {declared} it was handed — it keeps part of what \
                 moves through it, and `withdraw` measures that as a delta rather than as an \
                 error"
            )
        }
        (None, Some(name), _) => format!(
            "the contract's own {name} answered before the payee's row changed, so the money \
             moved in one direction and not the other"
        ),
        (None, None, Some("Empty")) => "the token's transfer reverted with no payload and no \
         message — this is the gate the three-leg run stops at, and it is inside this token \
         rather than inside a pool"
            .to_string(),
        (None, None, Some(kind)) => format!(
            "the token reverted with a payload of kind {kind} that the executor did not raise, \
             so the answer is the token's own"
        ),
        (None, None, None) => if record.completed {
            "the run settled and the payee's row still reports no delta"
        } else {
            "the provider refused a word, so the token was never asked"
        }
        .to_string(),
    }
}

/// Move one recorded token through the rescue door, on rows this function declares itself.
async fn moves(
    dump: &StateDump,
    source: &str,
    read: &[(Address, U256, Shape)],
    index: usize,
    token: Address,
) -> TransferRow {
    let slot = learned_balance_slot(read, token);
    let mut local = dump.clone();
    if let Some(slot) = slot {
        for holder in [OPERATOR, EXECUTOR, RECIPIENT] {
            let key = mapping_slot(holder, slot);
            if local.storage(token, key).is_none() {
                local.insert_storage(token, key, U256::ZERO);
            }
        }
        local.insert_storage(token, mapping_slot(EXECUTOR, slot), PROBE_AMOUNT);
    }
    let provider: Arc<dyn StateProvider> =
        Arc::new(DumpStateProvider::new(local, source.to_string()));
    let record = match run(
        provider,
        &withdrawal_spec(source, token, RECIPIENT, PROBE_AMOUNT),
    )
    .await
    {
        Ok(outcome) => {
            let paid = outcome
                .balance_row(token, RECIPIENT)
                .and_then(|row| row.after.checked_sub(row.before));
            let record = Record::from_outcome(&outcome);
            return TransferRow {
                index,
                token,
                balance_slot: slot,
                declared: if slot.is_some() {
                    PROBE_AMOUNT
                } else {
                    U256::ZERO
                },
                paid,
                verdict: transfer_verdict(paid, PROBE_AMOUNT, &record),
                answer: record.answer,
                contract_error: record.contract_error,
                revert_kind: record.revert_kind,
                revert_bytes: record.revert_bytes,
                gas_used: record.gas_used,
            };
        }
        Err(error) => Record::refused(&error),
    };
    TransferRow {
        index,
        token,
        balance_slot: slot,
        declared: U256::ZERO,
        paid: None,
        verdict: transfer_verdict(None, PROBE_AMOUNT, &record),
        answer: record.answer,
        contract_error: record.contract_error,
        revert_kind: record.revert_kind,
        revert_bytes: record.revert_bytes,
        gas_used: record.gas_used,
    }
}

/// Walk the refusals, then ask each recorded token to move by itself, and write the table.
#[tokio::test]
#[ignore = "measurement: pays each of the triangle's three tokens out through the contract's \
            `withdraw` over rows this instrument declares, to find which side of the \
            three-leg run's empty revert the token/pair boundary is on, then writes \
            fixtures/simulation-m11/probe-37224031-token-transfers.json — invoke with `cargo \
            test -p evm-simulation --test triangle_probe -- --ignored --nocapture`"]
async fn measure_which_recorded_tokens_move() {
    let (mut dump, source) = probed();
    let (slots, learned) = walk(&mut dump, &source, &probe_legs(), TOKEN_1).await;
    let mut rows = Vec::new();
    for (index, token) in TOKENS.iter().enumerate() {
        rows.push(moves(&dump, &source, &learned, index, *token).await);
    }
    let movers: Vec<String> = rows
        .iter()
        .filter(|row| row.paid.is_some())
        .map(|row| format!("{:#x}", row.token))
        .collect();
    let reading = format!(
        "{} of {} recorded tokens pay out through `withdraw`: [{}]. The three-leg run spent {} \
         gas and answered {} — a token that pays here is cleared of being where that run stops, \
         and one that reverts with the same kind of payload is where it stops.",
        movers.len(),
        rows.len(),
        movers.join(", "),
        slots
            .gas_used
            .map(|gas| format!("{gas}"))
            .unwrap_or_else(|| "no measured".to_string()),
        slots
            .final_revert_kind
            .clone()
            .or(slots.final_contract_error.clone())
            .unwrap_or_else(|| "no answer".to_string()),
    );
    let table = TransferTable {
        _provenance: format!(
            "crates/simulation/tests/triangle_probe.rs — a measurement of what each recorded \
             token of {RECORDING} moves when M10's executor is asked to withdraw it, with only \
             the balance rows this instrument declares. It measures no fee and judges no market"
        ),
        recording: RECORDING.to_string(),
        chain_id: CHAIN.0,
        block_number: BLOCK,
        declared: PROBE_AMOUNT,
        rows,
        reading,
        not_measured: vec![
            "whether a pool's `swap` answers when its own tokens do — a withdrawal never calls a \
             pool, so a table in which all three tokens pay leaves the three-leg run's empty \
             revert attributable to a pool and measures nothing about which one"
                .to_string(),
            "whether any direction of this cycle is profitable — nothing in this repository trades \
             these pools, so the question is not asked here and §41 keeps \
             `REAL_PROFITABLE_ARBITRAGE` at `UNKNOWN`"
                .to_string(),
        ],
    };
    write_json(&transfer_table_path(), &table);

    println!("{}", table.reading);
    for row in &table.rows {
        println!(
            "  token {} {:#x} balances slot {}: declared {} paid {} in {} gas — {} ({})",
            row.index,
            row.token,
            row.balance_slot
                .map(|s| s.to_string())
                .unwrap_or_else(|| "null".to_string()),
            row.declared,
            row.paid
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".to_string()),
            row.gas_used
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".to_string()),
            row.verdict,
            row.answer,
        );
    }
    println!("wrote {}", transfer_table_path().display());
}

// ---------------------------------------------------------------------------
// Instrument 4 — does the empty revert have anything to do with native ether?
// ---------------------------------------------------------------------------

/// The fourth table: the same chained cycle, run against dumps that differ only in which
/// accounts hold native ether.
///
/// Instrument 3 cleared all three tokens: each one pays a whole declared amount through
/// `withdraw`, in ~65k gas, with no fee taken. The recorded pools are the only other
/// contracts the run touches, and the executor cannot answer with an empty payload. An empty
/// revert is what a failed `address.transfer` looks like — Solidity compiles the value
/// transfer to a low-level call whose failure branch reverts with no data — so the next
/// question is whether some account on the path is trying to send ether it does not hold.
/// The recording says none of them holds any: the three pools and tokens 1 and 3 have a
/// native balance of zero, and token 2 has 8 ether.
const NATIVE_TABLE: &str = "fixtures/simulation-m11/probe-37224031-native-side.json";

/// 1 ether, in wei, as a decimal string — enough that any fee-sized transfer clears, and
/// small enough that nobody should mistake it for a fact about the chain.
const NATIVE_WEI: &str = "1000000000000000000";

fn native_table_path() -> PathBuf {
    workspace_root().join(NATIVE_TABLE)
}

/// Copy one recorded account row with a different native balance, keeping its nonce and its
/// bytecode. An account the recording does not have is inserted empty but endowed, which the
/// row's own `declared` column says out loud.
fn endow(dump: &mut StateDump, address: Address) {
    let (nonce, code) = match dump.account(address) {
        Some(row) => (
            row.nonce,
            row.code
                .parse::<Bytes>()
                .unwrap_or_else(|_| panic!("{}'s code is not hex", row.code)),
        ),
        None => (0, Bytes::new()),
    };
    let wei = U256::from_str_radix(NATIVE_WEI, 10).expect("the endowment is decimal wei");
    dump.insert_account(address, wei, nonce, &code);
}

#[derive(Clone, Debug, Serialize)]
struct NativeRow {
    variant: &'static str,
    /// The addresses this variant holds ether for, as the table's own words.
    endowed: Vec<String>,
    answer: String,
    contract_error: Option<String>,
    revert_kind: Option<String>,
    gas_used: Option<u64>,
    market_moved: Option<bool>,
    /// How many of the three pools answered `getReserves()` differently after the call, which
    /// is the difference between a run that reached a pool and one that never got there.
    reserves_moved: usize,
    balances_moved: usize,
    verdict: String,
}

#[derive(Clone, Debug, Serialize)]
struct NativeTable {
    _provenance: String,
    recording: String,
    chain_id: u64,
    block_number: u64,
    wei: String,
    claims: Vec<String>,
    rows: Vec<NativeRow>,
    not_measured: Vec<String>,
}

fn native_verdict(row: &NativeRow) -> String {
    match (
        row.contract_error.as_deref(),
        row.revert_kind.as_deref(),
        row.market_moved,
    ) {
        (Some(name), _, _) => format!(
            "the answer moved from an empty payload to the contract's own {name}, so the side \
             this variant endowed was the one refusing to pay"
        ),
        (None, Some("Empty"), _) => "the same empty payload — native ether on this side did not \
         change the answer"
            .to_string(),
        (None, Some(kind), _) => format!(
            "the answer moved to a payload of kind {kind} that the executor did not raise, so \
             the run got further than instrument 2's single run did"
        ),
        (None, None, Some(true)) => "the run delivered and the market moved".to_string(),
        (None, None, _) => "the run settled without moving the market".to_string(),
    }
}

/// Run the chained cycle once per endowment variant and write what each one answered.
#[tokio::test]
#[ignore = "measurement: re-runs the chained three-leg cycle over dumps that differ only in \
            which accounts hold 1 ether, to test whether the empty revert is a failed value \
            transfer, then writes \
            fixtures/simulation-m11/probe-37224031-native-side.json — invoke with `cargo test \
            -p evm-simulation --test triangle_probe -- --ignored --nocapture`"]
async fn measure_whether_native_ether_is_the_gate() {
    let (mut dump, source) = probed();
    let (_slots, learned) = walk(&mut dump, &source, &probe_legs(), TOKEN_1).await;
    let (legs, _sides) = chained(&learned, &dump, PROBE_AMOUNT)
        .expect("the recorded triangle prices the chained claims");
    let claims: Vec<String> = legs
        .iter()
        .map(|leg| format!("{} in -> {} out", leg.amount_in, leg.amount_out))
        .collect();

    let variants: [(&str, Vec<Address>); 4] = [
        ("no ether declared anywhere new", Vec::new()),
        ("the three tokens hold 1 ether each", TOKENS.to_vec()),
        ("the three pools hold 1 ether each", POOLS.to_vec()),
        (
            "every contract the run names holds 1 ether",
            TOKENS
                .iter()
                .chain(POOLS.iter())
                .copied()
                .collect::<Vec<_>>(),
        ),
    ];

    let mut rows = Vec::new();
    for (variant, addresses) in variants {
        let mut local = dump.clone();
        for address in &addresses {
            endow(&mut local, *address);
        }
        let provider: Arc<dyn StateProvider> =
            Arc::new(DumpStateProvider::new(local, source.to_string()));
        let outcome = run(provider, &spec(&source, &legs, TOKEN_1))
            .await
            .expect("the walk already proved this dump can answer every word the run reads");
        let record = Record::from_outcome(&outcome);
        let mut row = NativeRow {
            variant,
            endowed: addresses
                .iter()
                .map(|address| format!("{address:#x}"))
                .collect(),
            answer: record.answer,
            contract_error: record.contract_error,
            revert_kind: record.revert_kind,
            gas_used: record.gas_used,
            market_moved: record.market_moved,
            reserves_moved: record
                .reserves
                .as_ref()
                .map(|rows| rows.iter().filter(|r| r.changed).count())
                .unwrap_or_default(),
            balances_moved: record
                .balances
                .as_ref()
                .map(|rows| rows.iter().filter(|r| r.changed).count())
                .unwrap_or_default(),
            verdict: String::new(),
        };
        row.verdict = native_verdict(&row);
        println!("  {variant}: {} — {}", row.answer, row.verdict);
        rows.push(row);
    }

    let table = NativeTable {
        _provenance: format!(
            "crates/simulation/tests/triangle_probe.rs — a measurement of what M10's executor \
             answers for one chained three-leg cycle when the recorded contracts of \
             {RECORDING} are given native ether, over {NATIVE_TABLE}. It measures no fee and \
             judges no market"
        ),
        recording: RECORDING.to_string(),
        chain_id: CHAIN.0,
        block_number: BLOCK,
        wei: NATIVE_WEI.to_string(),
        claims,
        rows,
        not_measured: vec![
            "which contract the empty payload came from, if no variant changes the answer — the \
             variants here differ only about native ether, so a finding that survives all four \
             points at something other than a value transfer"
                .to_string(),
            "whether any direction of this cycle is profitable — nothing in this repository trades \
             these pools, so the question is not asked here and §41 keeps \
             `REAL_PROFITABLE_ARBITRAGE` at `UNKNOWN`"
                .to_string(),
        ],
    };
    write_json(&native_table_path(), &table);
    println!("wrote {}", native_table_path().display());
    assert_eq!(
        table.rows.first().map(|row| row.revert_kind.as_deref()),
        Some(Some("Empty")),
        "the control variant is the run instruments 1 and 2 already measured, so this table \
         only means anything if it reproduces the empty payload before changing one thing about \
         the state"
    );
}

// ---------------------------------------------------------------------------
// Instrument 5 — can a recorded pool of this family be traded at all
// ---------------------------------------------------------------------------

/// The fifth table: one pool at a time, entered and left through the same pair of tokens.
///
/// The triangle's three pools share one 16 854-byte deployment whose reverts read `Pair: K`,
/// `Pair: insufficient input`, `Pair: transfer failed` — a different family from the two pools
/// M10 proved it can trade (14 954 bytes, `UniswapV2: K`, BlowSwap V2). Instruments 3 and 4
/// cleared the tokens and cleared native ether, which leaves this family's `swap` as the only
/// remaining author of an answer that names nothing. A two-leg round trip through *one* pool is
/// the smallest route the contract's own `_checkRoute` accepts — continuity, a round trip and a
/// chained amount all hold — so it calls a recorded pool's `swap` with nothing else in the way.
/// If the family cannot be traded through M10's executor, that is the finding this table
/// reports, and §37's controlled 3-hop gets built on the family that can.
const SELF_TABLE: &str = "fixtures/simulation-m11/probe-37224031-self-cycle.json";

fn self_table_path() -> PathBuf {
    workspace_root().join(SELF_TABLE)
}

#[derive(Clone, Debug, Serialize)]
struct SelfRow {
    pool: Address,
    token_a: Address,
    token_b: String,
    claims: Vec<String>,
    answer: String,
    contract_error: Option<String>,
    revert_kind: Option<String>,
    revert_bytes: Option<String>,
    gas_used: Option<u64>,
    market_moved: Option<bool>,
    reserves_moved: usize,
    /// Words this run left different *inside the pool* — the difference between a pool that
    /// reverted after writing and one that reverted before touching its own storage.
    pool_changed_words: usize,
    verdict: String,
}

#[derive(Clone, Debug, Serialize)]
struct SelfTable {
    _provenance: String,
    recording: String,
    chain_id: u64,
    block_number: u64,
    rows: Vec<SelfRow>,
    reading: String,
    not_measured: Vec<String>,
}

/// `A → pool → B → pool → A`, both legs priced at the 0.3 % prediction on the pool's recorded
/// balances. The second leg is priced from the same balances the first leg will change, so a
/// `DeliveryMismatch` on it is expected and is not what this table is asking about.
fn self_cycle(
    read: &[(Address, U256, Shape)],
    dump: &StateDump,
    pool: Address,
    token_a: Address,
    token_b: Address,
) -> Result<Vec<ExecutorLeg>, String> {
    let balance_a = pool_side(read, dump, token_a, pool)?;
    let balance_b = pool_side(read, dump, token_b, pool)?;
    let out1 = exact_out(PROBE_AMOUNT, balance_a, balance_b);
    let out2 = exact_out(out1, balance_b, balance_a);
    if out1.is_zero() || out2.is_zero() {
        return Err(format!(
            "the recorded side prices a zero payout for {PROBE_AMOUNT} in against \
             {balance_a}/{balance_b}"
        ));
    }
    Ok(vec![
        ExecutorLeg {
            pool,
            token_in: token_a,
            token_out: token_b,
            amount_in: PROBE_AMOUNT,
            amount_out: out1,
            min_amount_out: out1,
        },
        ExecutorLeg {
            pool,
            token_in: token_b,
            token_out: token_a,
            amount_in: out1,
            amount_out: out2,
            min_amount_out: out2,
        },
    ])
}

/// The executor's own rejections that fire before its first call enters a pool: the route
/// checks and `_pullInput`. A name from this list is a fact about the run's funding or about the
/// route's shape, and attributing it to a pool would be a false attribution — POOL_2 answered
/// `InsufficientBalance` only because this probe funds the cycle's first token.
fn stops_before_the_pool(name: &str) -> bool {
    matches!(
        name,
        "NotOperator"
            | "ReentrancyDetected"
            | "ZeroAddress"
            | "ZeroAmount"
            | "NoLegs"
            | "TooManyLegs"
            | "PairNotAllowed"
            | "TokenNotAllowed"
            | "LegSelfLoop"
            | "BrokenContinuity"
            | "NotRoundTrip"
            | "AskBelowFloor"
            | "AmountChainBroken"
            | "InsufficientBalance"
            | "InsufficientAllowance"
            | "InputDeliveryMismatch"
    )
}

fn self_verdict(row: &SelfRow) -> String {
    match (
        row.contract_error.as_deref(),
        row.revert_kind.as_deref(),
        row.reserves_moved,
    ) {
        (Some(name), _, _) if stops_before_the_pool(name) => format!(
            "the run stopped at the executor's own `{name}` before any pool was called, so this \
             row measures the probe's funding rather than the pool — it is not an answer from \
             this family"
        ),
        (Some(name), _, _) => format!(
            "the pool let the route through to a rejection the contract names itself — {name} — \
             so this family answers `execute` and the three-leg run's empty payload came from \
             somewhere else in the cycle"
        ),
        (None, Some("Empty"), _) => "the same empty payload, with one pool and nothing between \
         the pull and its `swap`: this family's `swap` is the author"
            .to_string(),
        (None, Some(kind), _) => format!(
            "an external contract answered with a payload of kind {kind} that the executor did \
             not raise — the pool ran far enough to reach its own counterparty"
        ),
        (None, None, 0) => "the run settled and left every reserve where it found them".to_string(),
        (None, None, moved) => format!(
            "the run delivered and {moved} reserve rows moved: this pool trades through M10's \
             executor"
        ),
    }
}

/// Enter each recorded pool twice and write down what it answers.
#[tokio::test]
#[ignore = "measurement: runs a two-leg round trip through each recorded pool of the triangle \
            recording on its own, to find whether this pool family answers M10's executor at \
            all, then writes fixtures/simulation-m11/probe-37224031-self-cycle.json — invoke \
            with `cargo test -p evm-simulation --test triangle_probe -- --ignored --nocapture`"]
async fn measure_whether_a_recorded_pool_trades() {
    let (mut dump, source) = probed();
    let (slots, learned) = walk(&mut dump, &source, &probe_legs(), TOKEN_1).await;
    let sides: Vec<(Address, Address, Address)> = slots
        .reserves
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|row| (row.pool, row.token0, row.token1))
        .collect();
    let mut rows = Vec::new();
    for (pool, token_a, token_b) in sides {
        let legs = match self_cycle(&learned, &dump, pool, token_a, token_b) {
            Ok(legs) => legs,
            Err(reason) => {
                println!("  pool {pool:#x}: {reason}");
                continue;
            }
        };
        let claims: Vec<String> = legs
            .iter()
            .map(|leg| format!("{} in -> {} out", leg.amount_in, leg.amount_out))
            .collect();
        let provider: Arc<dyn StateProvider> =
            Arc::new(DumpStateProvider::new(dump.clone(), source.to_string()));
        let outcome = run(provider, &spec(&source, &legs, token_a))
            .await
            .unwrap_or_else(|error| panic!("the walk proved every word this run reads: {error}"));
        let record = Record::from_outcome(&outcome);
        let mut row = SelfRow {
            pool,
            token_a,
            token_b: format!("{token_b:#x}"),
            claims,
            answer: record.answer,
            contract_error: record.contract_error,
            revert_kind: record.revert_kind,
            revert_bytes: record.revert_bytes,
            gas_used: record.gas_used,
            market_moved: record.market_moved,
            reserves_moved: record
                .reserves
                .as_ref()
                .map(|rows| rows.iter().filter(|r| r.changed).count())
                .unwrap_or_default(),
            pool_changed_words: outcome.changed_slots_in(pool).len(),
            verdict: String::new(),
        };
        row.verdict = self_verdict(&row);
        println!("  pool {pool:#x}: {}", row.verdict);
        rows.push(row);
    }
    let empties = rows
        .iter()
        .filter(|row| row.revert_kind.as_deref() == Some("Empty"))
        .count();
    let reading = format!(
        "{empties} of {} recorded pools answer a two-leg round trip with the same empty payload \
         the three-leg cycle stopped on, and {} of {} leave a word different inside the pool.",
        rows.len(),
        rows.iter().filter(|row| row.pool_changed_words > 0).count(),
        rows.len()
    );
    let table = SelfTable {
        _provenance: format!(
            "crates/simulation/tests/triangle_probe.rs — a measurement of what each recorded \
             pool of {RECORDING} answers when M10's executor round-trips through it alone. It \
             measures no fee and judges no market"
        ),
        recording: RECORDING.to_string(),
        chain_id: CHAIN.0,
        block_number: BLOCK,
        rows,
        reading,
        not_measured: vec![
            "which line inside the pool's `swap` refuses — a payload with no name is not a \
             classification, and the recording holds no bytecode for the contracts the pool \
             itself calls, so the family's authorship is what this table can and does establish"
                .to_string(),
            "whether any direction of this cycle is profitable — nothing in this repository trades \
             these pools, so the question is not asked here and §41 keeps \
             `REAL_PROFITABLE_ARBITRAGE` at `UNKNOWN`"
                .to_string(),
        ],
    };
    write_json(&self_table_path(), &table);
    println!("{}", table.reading);
    for row in &table.rows {
        println!(
            "  pool {:#x} {:#x} -> {}: [{}] {} gas, reserves moved {}, changed words {} — {} ({})",
            row.pool,
            row.token_a,
            row.token_b,
            row.claims.join(" | "),
            row.gas_used
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".to_string()),
            row.reserves_moved,
            row.pool_changed_words,
            row.verdict,
            row.answer,
        );
    }
    println!("wrote {}", self_table_path().display());
}

// ---------------------------------------------------------------------------
// Instrument 6 — is the gate the account this probe declared codeless?
// ---------------------------------------------------------------------------

/// The address every OP-stack chain of this shape puts its fee voucher at, and the one account
/// the first instrument's loop had to invent: the walk's very first refusal was
/// `missing state: … account 0x4200…0011`, and [`Move::Account`] answers that by declaring the
/// account *empty* — no code. M10's recording of the same chain has 2 059 bytes of code and
/// 225 ether at that address, which is what a predeploy looks like.
///
/// Instrument 5 narrowed the empty payload down to this pool family's `swap`, and instrument 3
/// cleared every token of a plain `transfer`. The one thing a `swap` does that a `transfer` does
/// not is move tokens *into and out of a pair*, which on a fee-voucher chain is exactly the
/// condition those tokens gate their voucher call on. So the shape fits: the pair's `swap` calls
/// a token, the token calls the voucher predeploy, the predeploy has no code in this recording
/// because we wrote that row ourselves, and the caller's answer to an empty return is a revert
/// with no data — which is the payload the whole file has been chasing.
const VAT: Address = address!("0x4200000000000000000000000000000000000011");

/// The sixth table: one pool, one route, two versions of one account row.
const VAT_TABLE: &str = "fixtures/simulation-m11/probe-37224031-vat-row.json";

fn vat_table_path() -> PathBuf {
    workspace_root().join(VAT_TABLE)
}

/// M10's own recording of the chain at block 37 530 593, read for one account row.
///
/// The provenance has to be said out loud: this is the same chain and the same predeploy,
/// recorded 306 562 blocks after the pin this fixture is on. Using it is a diagnostic that asks
/// "does the run change its mind when this row has code at all", and it is not yet the state the
/// §37 fixture gets committed on — that one has to come from the node at the pin.
fn recorded_vat() -> Option<(U256, u64, Bytes)> {
    let dump = StateDump::from_file(&workspace_root().join(executor_state::FIXTURE)).ok()?;
    let row = dump.account(VAT)?;
    let balance = U256::from_str_radix(&row.balance, 10).ok()?;
    let code = row.code.parse::<Bytes>().ok()?;
    Some((balance, row.nonce, code))
}

#[derive(Clone, Debug, Serialize)]
struct VatRow {
    variant: String,
    vat_code_bytes: usize,
    vat_balance: String,
    claims: Vec<String>,
    answer: String,
    contract_error: Option<String>,
    revert_kind: Option<String>,
    gas_used: Option<u64>,
    market_moved: Option<bool>,
    reserves_moved: usize,
    verdict: String,
}

#[derive(Clone, Debug, Serialize)]
struct VatTable {
    _provenance: String,
    recording: String,
    chain_id: u64,
    block_number: u64,
    /// The pool the round trip goes through, named rather than left to the prose.
    pool: Address,
    vat: Address,
    vat_row_source: String,
    rows: Vec<VatRow>,
    not_measured: Vec<String>,
}

/// Run POOL_1's round trip over two dumps that differ only in the voucher's account row.
#[tokio::test]
#[ignore = "measurement: runs the same two-leg round trip with the fee-voucher predeploy \
            declared codeless (as instruments 1-5 saw it) and with its row taken from M10's \
            recording of this chain, then writes \
            fixtures/simulation-m11/probe-37224031-vat-row.json — invoke with `cargo test -p \
            evm-simulation --test triangle_probe -- --ignored --nocapture`"]
async fn measure_whether_the_vat_row_is_the_gate() {
    let Some((balance, nonce, code)) = recorded_vat() else {
        panic!("M10's committed fixture holds no row for {VAT:#x}");
    };
    let (mut dump, source) = probed();
    let (slots, learned) = walk(&mut dump, &source, &probe_legs(), TOKEN_1).await;
    let first = slots
        .reserves
        .as_deref()
        .and_then(|rows| rows.first())
        .expect("the walk reports each pool's reserve rows");
    let pool = first.pool;
    let legs = self_cycle(&learned, &dump, first.pool, first.token0, first.token1)
        .expect("pool 1's recorded sides price the round trip");
    let claims: Vec<String> = legs
        .iter()
        .map(|leg| format!("{} in -> {} out", leg.amount_in, leg.amount_out))
        .collect();

    let mut rows = Vec::new();
    for (variant, vat) in [
        (
            "the voucher declared codeless, the way this probe's first loop declared it",
            None,
        ),
        (
            "the voucher as M10 recorded this chain's predeploy: with code",
            Some((balance, nonce, code.clone())),
        ),
    ] {
        let mut local = dump.clone();
        if let Some((balance, nonce, code)) = &vat {
            local.insert_account(VAT, *balance, *nonce, code);
        }
        let provider: Arc<dyn StateProvider> =
            Arc::new(DumpStateProvider::new(local, source.to_string()));
        let outcome = run(provider, &spec(&source, &legs, first.token0))
            .await
            .unwrap_or_else(|error| panic!("the walk proved every word this run reads: {error}"));
        let record = Record::from_outcome(&outcome);
        let reserves_moved = record
            .reserves
            .as_ref()
            .map(|rows| rows.iter().filter(|r| r.changed).count())
            .unwrap_or_default();
        let verdict = match (
            record.contract_error.as_deref(),
            record.revert_kind.as_deref(),
            reserves_moved,
        ) {
            (Some(name), _, _) => format!(
                "the run reached a rejection the contract names itself — {name} — so the empty \
                 payload was the codeless row, and the route now gets as far as a claim the \
                 pools disagree with"
            ),
            (None, Some("Empty"), _) => "the same empty payload with the voucher's code in \
             place, so this probe's own declaration is not what was refusing"
                .to_string(),
            (None, Some(kind), _) => format!(
                "the answer moved from an empty payload to a payload of kind {kind}, so the \
                 voucher row changed what the run reports"
            ),
            (None, None, 0) => {
                "the run settled and left the reserves where it found them".to_string()
            }
            (None, None, moved) => format!(
                "the round trip delivered and {moved} reserve rows moved — the pool trades once \
                 the voucher account has its code"
            ),
        };
        println!("  {variant}: {verdict}");
        rows.push(VatRow {
            variant: variant.to_string(),
            vat_code_bytes: vat.as_ref().map(|(_, _, c)| c.len()).unwrap_or(0),
            vat_balance: vat
                .as_ref()
                .map(|(b, _, _)| b.to_string())
                .unwrap_or_else(|| "0".to_string()),
            claims: claims.clone(),
            answer: record.answer,
            contract_error: record.contract_error,
            revert_kind: record.revert_kind,
            gas_used: record.gas_used,
            market_moved: record.market_moved,
            reserves_moved,
            verdict,
        });
    }

    let table = VatTable {
        _provenance: format!(
            "crates/simulation/tests/triangle_probe.rs — a measurement of what M10's executor \
             answers for one round trip through {pool:#x} when the chain's fee-voucher predeploy at \
             {VAT:#x} is declared codeless and when it carries the code M10 recorded for it. It \
             measures no fee and judges no market"
        ),
        recording: RECORDING.to_string(),
        chain_id: CHAIN.0,
        block_number: BLOCK,
        pool,
        vat: VAT,
        vat_row_source: executor_state::FIXTURE.to_string(),
        rows,
        not_measured: vec![
            "the voucher's code and storage at block {BLOCK} — the row here comes from the same \
             chain one height later, which is enough to test whether a codeless row is what \
             refused and is not enough to be the fixture's state"
                .to_string(),
            "whether any direction of this cycle is profitable — nothing in this repository trades \
             these pools, so the question is not asked here and §41 keeps \
             `REAL_PROFITABLE_ARBITRAGE` at `UNKNOWN`"
                .to_string(),
        ],
    };
    write_json(&vat_table_path(), &table);
    for row in &table.rows {
        println!(
            "  vat code {} bytes, balance {}: {} gas, reserves moved {} — {}",
            row.vat_code_bytes,
            row.vat_balance,
            row.gas_used
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".to_string()),
            row.reserves_moved,
            row.answer,
        );
    }
    println!("wrote {}", vat_table_path().display());
}

// ---------------------------------------------------------------------------
// The seventh instrument: does a pool's cached reserve equal its balance?
// ---------------------------------------------------------------------------

/// A V2 pair does not price a swap against the tokens it holds; it prices against the
/// reserves it *cached* in its own storage, and then checks the balance it received against
/// that cache. The two numbers agree only while nothing moves tokens into or out of the pair
/// without calling `mint`, `burn` or `sync` — so a pair whose cache overstates its balances
/// refuses a swap at any price, and the refusal is a fact about the state rather than about
/// the quote.
///
/// Instruments 1–6 chased the triangle's empty revert to the pool family's `swap` and cleared
/// the executor, the three tokens' plain transfers, native endowment, and the fee-voucher
/// account row. This instrument tests the remaining claim, which is the one that decides
/// §37's fixture: it writes the pair's own `getReserves()` answer beside the pair's recorded
/// balances, and runs the same round trip priced from each.
const SIDE_TABLE: &str = "fixtures/simulation-m11/probe-37224031-reserves-vs-balances.json";

fn side_table_path() -> PathBuf {
    workspace_root().join(SIDE_TABLE)
}

/// `numerator ÷ denominator` in permille, or nothing when the denominator is zero — an integer
/// ratio, because §48 keeps floating point out of anything financial.
fn permille(numerator: U256, denominator: U256) -> Option<U256> {
    if denominator.is_zero() {
        return None;
    }
    numerator
        .checked_mul(U256::from(1_000u64))
        .map(|scaled| scaled / denominator)
}

/// One price, one run. `priced: false` means the sides could not price a payout at all, which
/// is reported as its own answer rather than as a run that reverted.
#[derive(Clone, Debug, Serialize)]
struct PriceRun {
    priced_from: String,
    side_in: U256,
    side_out: U256,
    priced: bool,
    claims: Vec<String>,
    answer: String,
    contract_error: Option<String>,
    revert_kind: Option<String>,
    gas_used: Option<u64>,
    delivered: Option<U256>,
    reserves_moved: usize,
}

#[derive(Clone, Debug, Serialize)]
struct SideRow {
    pool: Address,
    token0: Address,
    token1: Address,
    reserve0: U256,
    balance0: U256,
    /// `reserve0 ÷ balance0`, permille; `1000` is a pair whose cache equals its holdings.
    gap0_permille: Option<U256>,
    reserve1: U256,
    balance1: U256,
    gap1_permille: Option<U256>,
    runs: Vec<PriceRun>,
    verdict: String,
}

#[derive(Clone, Debug, Serialize)]
struct SideTable {
    _provenance: String,
    recording: String,
    chain_id: u64,
    block_number: u64,
    probe_amount: U256,
    rows: Vec<SideRow>,
    reading: String,
    not_measured: Vec<String>,
}

/// `A → pool → B → pool → A`, both legs priced on the sides the caller supplies.
fn round_trip(
    pool: Address,
    token_a: Address,
    token_b: Address,
    side_a: U256,
    side_b: U256,
) -> Result<Vec<ExecutorLeg>, String> {
    let out1 = exact_out(PROBE_AMOUNT, side_a, side_b);
    let out2 = exact_out(out1, side_b, side_a);
    if out1.is_zero() || out2.is_zero() {
        return Err(format!(
            "these sides price a zero payout for {PROBE_AMOUNT} in against {side_a}/{side_b}"
        ));
    }
    Ok(vec![
        ExecutorLeg {
            pool,
            token_in: token_a,
            token_out: token_b,
            amount_in: PROBE_AMOUNT,
            amount_out: out1,
            min_amount_out: out1,
        },
        ExecutorLeg {
            pool,
            token_in: token_b,
            token_out: token_a,
            amount_in: out1,
            amount_out: out2,
            min_amount_out: out2,
        },
    ])
}

/// Price the round trip on one pair of sides and run it, and write down the answer it gave. The
/// pool and its two sides come from the row the reserves instrument already produced, so the
/// only thing this call is free to choose is which pair of numbers prices the legs.
async fn price_and_run(
    dump: &StateDump,
    source: &str,
    side: &ReserveRow,
    priced_from: &str,
    side_a: U256,
    side_b: U256,
) -> PriceRun {
    let (pool, token_a, token_b) = (side.pool, side.token0, side.token1);
    let legs = match round_trip(pool, token_a, token_b, side_a, side_b) {
        Ok(legs) => legs,
        Err(reason) => {
            println!("  pool {pool:#x} priced from {priced_from}: {reason}");
            return PriceRun {
                priced_from: priced_from.to_string(),
                side_in: side_a,
                side_out: side_b,
                priced: false,
                claims: Vec::new(),
                answer: reason,
                contract_error: None,
                revert_kind: None,
                gas_used: None,
                delivered: None,
                reserves_moved: 0,
            };
        }
    };
    let claims: Vec<String> = legs
        .iter()
        .map(|leg| format!("{} in -> {} out", leg.amount_in, leg.amount_out))
        .collect();
    let provider: Arc<dyn StateProvider> =
        Arc::new(DumpStateProvider::new(dump.clone(), source.to_string()));
    let outcome = run(provider, &spec(source, &legs, token_a))
        .await
        .unwrap_or_else(|error| panic!("the walk proved every word this run reads: {error}"));
    let record = Record::from_outcome(&outcome);
    let reserves_moved = record
        .reserves
        .as_ref()
        .map(|rows| rows.iter().filter(|row| row.changed).count())
        .unwrap_or_default();
    PriceRun {
        priced_from: priced_from.to_string(),
        side_in: side_a,
        side_out: side_b,
        priced: true,
        claims,
        answer: record.answer,
        contract_error: record.contract_error,
        revert_kind: record.revert_kind,
        gas_used: record.gas_used,
        delivered: record.delivered,
        reserves_moved,
    }
}

fn balanced(gap: Option<U256>) -> Option<bool> {
    gap.map(|value| value == U256::from(1_000u64))
}

fn side_verdict(row: &SideRow) -> String {
    let ran: Vec<&PriceRun> = row.runs.iter().filter(|run| run.priced).collect();
    let settled = ran.iter().find(|run| {
        run.contract_error.is_none() && run.revert_kind.is_none() && run.reserves_moved > 0
    });
    if let Some(run) = settled {
        return format!(
            "the round trip settles when priced from {} and moves {} reserve rows, so this pair \
             does trade through M10's executor and the other price is the one it rejects",
            run.priced_from, run.reserves_moved
        );
    }
    let sides: Vec<(U256, U256)> = ran.iter().map(|run| (run.side_in, run.side_out)).collect();
    let degenerate = sides
        .first()
        .is_some_and(|first| sides.iter().all(|side| side == first));
    let named: Vec<String> = ran
        .iter()
        .filter_map(|run| run.contract_error.clone())
        .collect();
    let pre_pool = !named.is_empty()
        && named
            .iter()
            .all(|name| stops_before_the_pool(name.as_str()));
    if pre_pool {
        return format!(
            "every run stopped at the executor's own funding gate ({}) before any pool was \
             called, so this row measures the probe's funding rather than the pair — and it says \
             nothing about whether this family trades",
            named.join(", ")
        );
    }
    let empties = ran
        .iter()
        .filter(|run| run.revert_kind.as_deref() == Some("Empty"))
        .count();
    if degenerate && empties == ran.len() && ran.len() > 1 {
        return "the pair's cached reserves are exactly the balances it holds on both sides, so \
                the two prices this table ran are one measurement: what it establishes is that \
                identity, and the empty payload it answers is therefore not a disagreement \
                between two price sources"
            .to_string();
    }
    if degenerate {
        return "the pair's cached reserves are exactly the balances it holds, so the two prices \
                this table ran are one measurement — the identity is the finding, and the answer \
                it gave is in the runs beside it"
            .to_string();
    }
    if empties == ran.len() && ran.len() > 1 {
        return "every price this table ran — the pair's own cache and its recorded balances — is \
                answered with the same empty payload, and the two disagree about the pool's side, \
                so the refusal is not which number the quote used"
            .to_string();
    }
    format!(
        "no run settles and not every run is the empty payload{} — the table records each answer \
         as it came",
        if named.is_empty() {
            String::new()
        } else {
            format!(", the named answers being {}", named.join(", "))
        }
    )
}

/// Write the pair's own reserves beside the pair's own balances, and let it price itself twice.
#[tokio::test]
#[ignore = "measurement: runs the same two-leg round trip through each recorded pool priced \
            from the pool's cached `getReserves()` answer and from the pool's recorded token \
            balances, then writes \
            fixtures/simulation-m11/probe-37224031-reserves-vs-balances.json — invoke with \
            `cargo test -p evm-simulation --test triangle_probe -- --ignored --nocapture`"]
async fn measure_whether_the_cached_reserves_match_the_balances() {
    let (mut dump, source) = probed();
    let (slots, learned) = walk(&mut dump, &source, &probe_legs(), TOKEN_1).await;
    let reserve_rows: Vec<ReserveRow> = slots.reserves.clone().unwrap_or_default();
    let mut rows = Vec::new();
    for side in reserve_rows {
        let (Ok(balance0), Ok(balance1)) = (
            pool_side(&learned, &dump, side.token0, side.pool),
            pool_side(&learned, &dump, side.token1, side.pool),
        ) else {
            println!(
                "  pool {:#x}: the recording holds no balance of one of its sides, so this \
                 table cannot compare cache with holdings",
                side.pool
            );
            continue;
        };
        let mut runs = Vec::new();
        for (label, side_in, side_out) in [
            ("the pool's recorded token balances", balance0, balance1),
            (
                "the pool's own `getReserves()` answer",
                side.reserve0_before,
                side.reserve1_before,
            ),
        ] {
            runs.push(price_and_run(&dump, &source, &side, label, side_in, side_out).await);
        }
        let mut row = SideRow {
            pool: side.pool,
            token0: side.token0,
            token1: side.token1,
            reserve0: side.reserve0_before,
            balance0,
            gap0_permille: permille(side.reserve0_before, balance0),
            reserve1: side.reserve1_before,
            balance1,
            gap1_permille: permille(side.reserve1_before, balance1),
            runs,
            verdict: String::new(),
        };
        row.verdict = side_verdict(&row);
        println!("  pool {:#x}: {}", side.pool, row.verdict);
        rows.push(row);
    }

    let unbalanced = rows
        .iter()
        .filter(|row| {
            [row.gap0_permille, row.gap1_permille]
                .iter()
                .any(|gap| matches!(balanced(*gap), Some(false)))
        })
        .count();
    let reading = format!(
        "{unbalanced} of {} recorded pools hold a cached reserve that is not the balance they \
         actually hold, and {} of {} answer every price this table ran with the empty payload.",
        rows.len(),
        rows.iter()
            .filter(|row| {
                let ran: Vec<&PriceRun> = row.runs.iter().filter(|run| run.priced).collect();
                !ran.is_empty()
                    && ran
                        .iter()
                        .all(|run| run.revert_kind.as_deref() == Some("Empty"))
            })
            .count(),
        rows.len()
    );
    let table = SideTable {
        _provenance: format!(
            "crates/simulation/tests/triangle_probe.rs — a measurement of whether each recorded \
             pool of {RECORDING} caches a reserve equal to the tokens it holds, and what \
             M10's executor answers for the same round trip priced from each of the two. It \
             measures no fee and judges no market"
        ),
        recording: RECORDING.to_string(),
        chain_id: CHAIN.0,
        block_number: BLOCK,
        probe_amount: PROBE_AMOUNT,
        rows,
        reading,
        not_measured: vec![
            "why the pair's cache and its holdings diverged — that would need the pool's own \
             history (`mint`, `burn`, `sync`, `skim`), and this recording is one height, not a \
             trace"
                .to_string(),
            "whether any direction of this cycle is profitable — nothing in this repository trades \
             these pools, so the question is not asked here and §41 keeps \
             `REAL_PROFITABLE_ARBITRAGE` at `UNKNOWN`"
                .to_string(),
        ],
    };
    write_json(&side_table_path(), &table);
    println!("{}", table.reading);
    for row in &table.rows {
        println!(
            "  pool {:#x} reserve0/balance0 {:?} reserve1/balance1 {:?} — {}",
            row.pool, row.gap0_permille, row.gap1_permille, row.verdict
        );
        for run in &row.runs {
            println!(
                "    priced from {}: gas {} kind {:?} error {:?} reserves moved {} [{}]",
                run.priced_from,
                run.gas_used
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "null".to_string()),
                run.revert_kind,
                run.contract_error,
                run.reserves_moved,
                run.claims.join(" | "),
            );
        }
    }
    println!("wrote {}", side_table_path().display());
}

// ---------------------------------------------------------------------------
// The eighth instrument: does a recorded token pay a pool at all?
// ---------------------------------------------------------------------------

/// The third instrument cleared each of these tokens on a transfer to the fixture's payee, and
/// the fifth put the empty payload inside a pool's `swap`. There is one call shape between the
/// two that neither tested: a transfer whose *destination is a pair*. A token of this family is
/// 7 218 bytes against the recorded WETH's 2 846, which is the size difference between a plain
/// ERC-20 and one that consults a map of its own on every move — and the map that decides
/// whether a route can be built at all is the one that answers about the pool.
///
/// So this instrument pays the same declared holding through the same rescue door, once to the
/// payee that already answered and once to each recorded pool. A token that pays the wallet and
/// reverts with no payload against a pair is the empty payload's author, and the finding is that
/// no route over these pools executes — not that M10's executor is wrong.
const POOL_PAY_TABLE: &str = "fixtures/simulation-m11/probe-37224031-transfers-into-pools.json";

fn pool_pay_table_path() -> PathBuf {
    workspace_root().join(POOL_PAY_TABLE)
}

#[derive(Clone, Debug, Serialize)]
struct PoolPayRow {
    token: Address,
    payee: Address,
    payee_kind: String,
    balance_slot: Option<u64>,
    declared: U256,
    paid: Option<U256>,
    answer: String,
    contract_error: Option<String>,
    revert_kind: Option<String>,
    revert_bytes: Option<String>,
    gas_used: Option<u64>,
    verdict: String,
}

#[derive(Clone, Debug, Serialize)]
struct PoolPayTable {
    _provenance: String,
    recording: String,
    chain_id: u64,
    block_number: u64,
    declared: U256,
    rows: Vec<PoolPayRow>,
    reading: String,
    not_measured: Vec<String>,
}

fn pool_pay_verdict(row: &PoolPayRow, record: &Record) -> String {
    match (
        row.paid,
        record.contract_error.as_deref(),
        record.revert_kind.as_deref(),
    ) {
        (Some(paid), _, _) if paid == row.declared => {
            "this payee was paid the whole declared holding, so the token moves into this account \
             without a gate"
                .to_string()
        }
        (Some(paid), _, _) => format!(
            "this payee received {paid} of the {} declared, so the token keeps part of what it \
             moves here",
            row.declared
        ),
        (None, Some(name), _) => format!(
            "the executor's own `{name}` answered before the payee's row changed — a funding \
             gate of this fixture's, not the payee's"
        ),
        (None, None, Some("Empty")) => "the token's `transfer` to this payee reverted with no \
         payload — an unnamed rejection inside the token, which is the shape the whole probe has \
         been chasing"
            .to_string(),
        (None, None, Some(kind)) => format!(
            "the token reverted against this payee with a payload of kind {kind} that the \
             executor did not raise"
        ),
        (None, None, None) => if record.completed {
            "the run settled and the payee's row reports no delta"
        } else {
            "the provider refused a word, so the token was never asked"
        }
        .to_string(),
    }
}

/// Hand the executor a declared holding and ask it to pay one named account with it.
async fn pays(
    dump: &StateDump,
    source: &str,
    read: &[(Address, U256, Shape)],
    token: Address,
    payee: Address,
    payee_kind: &str,
) -> PoolPayRow {
    let slot = learned_balance_slot(read, token);
    let mut paid = None;
    let mut local = dump.clone();
    if let Some(slot) = slot {
        for holder in [OPERATOR, EXECUTOR, RECIPIENT, payee] {
            let key = mapping_slot(holder, slot);
            if local.storage(token, key).is_none() {
                local.insert_storage(token, key, U256::ZERO);
            }
        }
        local.insert_storage(token, mapping_slot(EXECUTOR, slot), PROBE_AMOUNT);
    }
    let provider: Arc<dyn StateProvider> =
        Arc::new(DumpStateProvider::new(local, source.to_string()));
    let record = match run(
        provider,
        &withdrawal_spec(source, token, payee, PROBE_AMOUNT),
    )
    .await
    {
        Ok(outcome) => {
            paid = outcome
                .balance_row(token, payee)
                .and_then(|row| row.after.checked_sub(row.before));
            Record::from_outcome(&outcome)
        }
        // A word this probe is not allowed to write is a finding, not a crash: the row says the
        // token was never asked.
        Err(error) => Record::refused(&error),
    };
    let mut row = PoolPayRow {
        token,
        payee,
        payee_kind: payee_kind.to_string(),
        balance_slot: slot,
        declared: if slot.is_some() {
            PROBE_AMOUNT
        } else {
            U256::ZERO
        },
        paid,
        answer: record.answer.clone(),
        contract_error: record.contract_error.clone(),
        revert_kind: record.revert_kind.clone(),
        revert_bytes: record.revert_bytes.clone(),
        gas_used: record.gas_used,
        verdict: String::new(),
    };
    row.verdict = pool_pay_verdict(&row, &record);
    row
}

/// Ask every recorded token to pay every recorded pool, and the payee that already answered.
#[tokio::test]
#[ignore = "measurement: pays a declared holding from M10's executor to each recorded pool and \
            to the fixture's payee with the contract's own `withdraw`, to find whether the \
            triangle's empty payload is a token refusing a pair, then writes \
            fixtures/simulation-m11/probe-37224031-transfers-into-pools.json — invoke with \
            `cargo test -p evm-simulation --test triangle_probe -- --ignored --nocapture`"]
async fn measure_whether_the_tokens_pay_a_pool() {
    let (mut dump, source) = probed();
    let (_, learned) = walk(&mut dump, &source, &probe_legs(), TOKEN_1).await;
    let mut rows = Vec::new();
    for token in TOKENS {
        rows.push(
            pays(
                &dump,
                &source,
                &learned,
                token,
                RECIPIENT,
                "the fixture's payee",
            )
            .await,
        );
        for pool in POOLS {
            rows.push(pays(&dump, &source, &learned, token, pool, "a recorded pool").await);
        }
    }
    for row in &mut rows {
        println!(
            "  token {:#x} -> payee {:#x} ({}): {}",
            row.token, row.payee, row.payee_kind, row.verdict
        );
    }

    let paid_wallet = rows
        .iter()
        .filter(|row| row.payee_kind == "the fixture's payee" && row.paid.is_some())
        .count();
    let refused_pools = rows
        .iter()
        .filter(|row| {
            row.payee_kind == "a recorded pool" && row.revert_kind.as_deref() == Some("Empty")
        })
        .count();
    let reading =
        format!(
        "{paid_wallet} of {} payee rows to the fixture's own payee were paid, and {refused_pools} \
         of {} payee rows against a recorded pool answered with the empty payload.",
        rows.iter().filter(|row| row.payee_kind == "the fixture's payee").count(),
        rows.iter().filter(|row| row.payee_kind == "a recorded pool").count(),
    );
    let table = PoolPayTable {
        _provenance: format!(
            "crates/simulation/tests/triangle_probe.rs — a measurement of whether each recorded \
             token of {RECORDING} pays a recorded pool when M10's executor hands it the whole \
             declared holding. It measures no fee and judges no market"
        ),
        recording: RECORDING.to_string(),
        chain_id: CHAIN.0,
        block_number: BLOCK,
        declared: PROBE_AMOUNT,
        rows,
        reading,
        not_measured: vec![
            "which line inside the token refuses — `withdraw` pays by measuring a delta, so an \
             unnamed revert is reported as the token's own answer and not as a classification"
                .to_string(),
            "whether any direction of this cycle is profitable — nothing in this repository trades \
             these pools, so the question is not asked here and §41 keeps \
             `REAL_PROFITABLE_ARBITRAGE` at `UNKNOWN`"
                .to_string(),
        ],
    };
    write_json(&pool_pay_table_path(), &table);
    println!("{}", table.reading);
    println!("wrote {}", pool_pay_table_path().display());
}

// ---------------------------------------------------------------------------
// Instrument 9: which entry points does each recorded contract expose?
// ---------------------------------------------------------------------------

const ENTRY_TABLE: &str = "fixtures/simulation-m11/probe-37224031-entry-points.json";

/// M10's own recording is the positive half of this measurement: it holds two pools of the
/// pair family that M10's committed evidence shows a real route through — the same executor,
/// the same `execute(...)` call, settling with the reserves moved.
const M10_POOL_A: Address = address!("0x2a3ceafba30f6626170cbb0cd67392efb94bd9a4");
const M10_POOL_B: Address = address!("0x5b3c1e3fb6a97c0130ae015ff10f53a1a30c353e");
const M10_TOKEN_A: Address = address!("0x07d4af6e2bc8dd82beb06b4fd279df4c9028f26f");
const M10_WETH: Address = address!("0x4200000000000000000000000000000000000006");

/// The signatures this instrument looks for. No selector is pasted as a hex literal: each one
/// is computed here from the signature, so a reader can recompute it, and a wrong signature
/// shows up as a wrong selector in the row rather than as a silent miss.
const ENTRIES: &[(&str, &str)] = &[
    (
        "the swap entry M10's executor issues on each leg",
        "swap(uint256,uint256,address,bytes)",
    ),
    (
        "the reserve read the graph and the price are built from",
        "getReserves()",
    ),
    ("the side read discovery asks a pair for", "token0()"),
    ("the ERC20 metadata read", "name()"),
    (
        "the token move the executor makes to settle",
        "transfer(address,uint256)",
    ),
    (
        "the token pull the executor makes to fund",
        "transferFrom(address,address,uint256)",
    ),
];

/// The 4-byte selector of a Solidity signature, computed rather than remembered.
fn selector(signature: &str) -> [u8; 4] {
    let digest = keccak256(signature.as_bytes());
    [digest[0], digest[1], digest[2], digest[3]]
}

fn recorded_code(dump: &StateDump, address: Address) -> Option<Vec<u8>> {
    let account = dump.account(address)?;
    let hexed = account.code.strip_prefix("0x").unwrap_or(&account.code);
    if hexed.is_empty() {
        return None;
    }
    hex::decode(hexed).ok().filter(|bytes| !bytes.is_empty())
}

/// Every place the code writes `PUSH4 <selector>`: the form a Solidity dispatcher uses to
/// compare an incoming selector, and the form an outbound call site uses to build one.
fn push_sites(code: &[u8], sel: &[u8; 4]) -> usize {
    code.windows(5)
        .filter(|window| window[0] == 0x63 && &window[1..] == sel)
        .count()
}

/// Every place the 4 bytes appear at all, in instruction position or not. Reported beside
/// [`push_sites`] so a coincidence in embedded data cannot be mistaken for an entry point.
fn raw_sites(code: &[u8], sel: &[u8; 4]) -> usize {
    code.windows(4).filter(|window| *window == *sel).count()
}

#[derive(Clone, Debug, Serialize)]
struct EntryAnswer {
    purpose: String,
    signature: String,
    selector: String,
    push_sites: usize,
    raw_sites: usize,
}

#[derive(Clone, Debug, Serialize)]
struct EntryPointRow {
    recording: String,
    address: Address,
    role: String,
    code_bytes: usize,
    swap_entry: Option<bool>,
    entries: Vec<EntryAnswer>,
    verdict: String,
}

#[derive(Clone, Debug, Serialize)]
struct EntryPointTable {
    _provenance: String,
    chain_id: u64,
    triangle_block: u64,
    executor_block: u64,
    rows: Vec<EntryPointRow>,
    rpc_count: u64,
    reading: String,
    not_measured: Vec<String>,
}

fn entry_table_path() -> PathBuf {
    workspace_root().join(ENTRY_TABLE)
}

/// One row per named contract of the two recordings, plus M10's executor.
fn entry_rows(triangle: &StateDump, executor_fixture: &StateDump) -> Vec<EntryPointRow> {
    let mut rows = Vec::new();
    let mut add =
        |recording: &str, dump: &StateDump, address: Address, role: &str, is_pair: bool| {
            let Some(code) = recorded_code(dump, address) else {
                rows.push(EntryPointRow {
                    recording: recording.to_string(),
                    address,
                    role: role.to_string(),
                    code_bytes: 0,
                    swap_entry: None,
                    entries: Vec::new(),
                    verdict: "this recording holds no code at this address, so nothing about its \
                          entry points is measured by this row"
                        .to_string(),
                });
                return;
            };
            let entries: Vec<EntryAnswer> = ENTRIES
                .iter()
                .map(|(purpose, signature)| {
                    let sel = selector(signature);
                    EntryAnswer {
                        purpose: (*purpose).to_string(),
                        signature: (*signature).to_string(),
                        selector: format!("0x{}", hex::encode(sel)),
                        push_sites: push_sites(&code, &sel),
                        raw_sites: raw_sites(&code, &sel),
                    }
                })
                .collect();
            let swap = entries
                .first()
                .map(|entry| entry.push_sites > 0)
                .unwrap_or(false);
            let swap_selector = &entries[0].selector;
            let verdict = if is_pair {
                if swap {
                    format!(
                    "its dispatcher writes `PUSH4 {swap_selector}` — a call of that shape has an \
                     arm here, which is what M10's committed route through this family measures"
                )
                } else {
                    format!(
                    "no `PUSH4 {swap_selector}` appears anywhere in its runtime code, so a call \
                     of the shape M10's executor issues has no dispatcher arm here — and the \
                     answer instruments 1 through 8 recorded for such a call is an empty payload"
                )
                }
            } else {
                "not a pair, so the swap entry is not expected here; this row is the control that \
                 keeps the search honest about where it does and does not fire"
                    .to_string()
            };
            rows.push(EntryPointRow {
                recording: recording.to_string(),
                address,
                role: role.to_string(),
                code_bytes: code.len(),
                swap_entry: if is_pair { Some(swap) } else { None },
                entries,
                verdict,
            });
        };
    add(
        "M10's executor runtime bytecode",
        &{
            let mut dump = StateDump::default();
            let code = executor_runtime_code();
            dump.insert_account(EXECUTOR, U256::ZERO, 0, &code);
            dump
        },
        EXECUTOR,
        "the contract every route of this repository calls",
        false,
    );
    for pool in POOLS {
        add(
            RECORDING,
            triangle,
            pool,
            "a recorded pool of the triangle",
            true,
        );
    }
    for token in TOKENS {
        add(
            RECORDING,
            triangle,
            token,
            "a recorded token of the triangle",
            false,
        );
    }
    add(
        executor_state::FIXTURE,
        executor_fixture,
        M10_POOL_A,
        "a recorded pool M10's route trades",
        true,
    );
    add(
        executor_state::FIXTURE,
        executor_fixture,
        M10_POOL_B,
        "the other recorded pool M10's route trades",
        true,
    );
    add(
        executor_state::FIXTURE,
        executor_fixture,
        M10_TOKEN_A,
        "a recorded token M10's route trades",
        false,
    );
    add(
        executor_state::FIXTURE,
        executor_fixture,
        M10_WETH,
        "the chain's own wrapped-native token",
        false,
    );
    rows
}

/// Count the pairs in the table whose runtime code carries the swap entry, per recording.
fn pairs_with_swap(rows: &[EntryPointRow], recording: &str) -> (usize, usize) {
    let pair_rows: Vec<&EntryPointRow> = rows
        .iter()
        .filter(|row| row.recording == recording && row.swap_entry.is_some())
        .collect();
    let total = pair_rows.len();
    let carrying = pair_rows
        .iter()
        .filter(|row| row.swap_entry == Some(true))
        .count();
    (carrying, total)
}

/// Read the two committed recordings and count, in each contract's own runtime bytes, where it
/// writes the selector M10's executor issues on a leg.
///
/// Ignored because it is an instrument over committed files, not a gate on every `cargo test`;
/// the recompute gate replays it.
#[test]
#[ignore = "measurement: counts `PUSH4 <selector>` sites for six named signatures in the runtime \
            code of every pool and token of the recorded triangle and of M10's recording, plus \
            M10's executor itself, then writes \
            fixtures/simulation-m11/probe-37224031-entry-points.json — invoke with `cargo test \
            -p evm-simulation --test triangle_probe -- --ignored --nocapture`"]
fn measure_which_entry_points_each_recorded_contract_exposes() {
    let triangle = StateDump::from_file(&recording_path())
        .unwrap_or_else(|error| panic!("{}: {error}", recording_path().display()));
    let executor_path = workspace_root().join(executor_state::FIXTURE);
    let executor_fixture = StateDump::from_file(&executor_path)
        .unwrap_or_else(|error| panic!("{}: {error}", executor_path.display()));
    let rows = entry_rows(&triangle, &executor_fixture);

    let (triangle_carrying, triangle_total) = pairs_with_swap(&rows, RECORDING);
    let (m10_carrying, m10_total) = pairs_with_swap(&rows, executor_state::FIXTURE);
    let reserves_answered = rows
        .iter()
        .filter(|row| row.entries.get(1).is_some_and(|entry| entry.push_sites > 0))
        .count();
    let rows_with_code = rows.iter().filter(|row| row.code_bytes > 0).count();
    let reading = format!(
        "{m10_carrying} of {m10_total} pools of {} carry the swap entry M10's executor issues; \
         {triangle_carrying} of {triangle_total} pools of {RECORDING} do. The same search finds \
         the reserve read in {reserves_answered} of the {rows_with_code} contracts that have code \
         at all, so the entry is not simply missing from every file the search looked at.",
        executor_state::FIXTURE
    );

    let table = EntryPointTable {
        _provenance: format!(
            "crates/simulation/tests/triangle_probe.rs — a measurement of which function \
             selectors each contract of {RECORDING} and of {} exposes, counted in the runtime \
             bytes those recordings already hold. Every selector here is computed from the \
             signature printed beside it. It measures no price and judges no market",
            executor_state::FIXTURE
        ),
        chain_id: CHAIN.0,
        triangle_block: triangle.block_number,
        executor_block: executor_fixture.block_number,
        rows,
        rpc_count: 0,
        reading,
        not_measured: vec![
            "what the triangle's pools call their swap entry instead — naming a function needs a \
             signature, and a guessed signature is a claim rather than a measurement; the row \
             prints the raw sites so a reader who knows the family can check it"
                .to_string(),
            "whether a call with no dispatcher arm is what produced the empty payload — the two \
             facts are recorded side by side here and in instruments 1 through 8, and no run in \
             this table was made against the triangle's pools"
                .to_string(),
            "whether any direction of this cycle is profitable — no row of this table trades, so \
             this instrument does not ask it; §41 keeps `REAL_PROFITABLE_ARBITRAGE` at `UNKNOWN` \
             while the only runs in this repository that deliver a multi-hop cycle do so over the \
             declared venues of tests/multihop_e2e.rs"
                .to_string(),
        ],
    };
    write_json(&entry_table_path(), &table);
    for row in &table.rows {
        println!(
            "  {} {:#x} ({} bytes, {}): swap entry {}",
            row.recording,
            row.address,
            row.code_bytes,
            row.role,
            match row.swap_entry {
                Some(true) => "carried",
                Some(false) => "absent",
                None => "not asked (not a pair)",
            }
        );
        for entry in &row.entries {
            println!(
                "      {} {} push={} raw={}",
                entry.selector, entry.signature, entry.push_sites, entry.raw_sites
            );
        }
    }
    println!("{}", table.reading);
    println!("wrote {}", entry_table_path().display());
}
// ---------------------------------------------------------------------------
// The replay gate over instrument 9's committed table
// ---------------------------------------------------------------------------

/// The committed table's shape, as read back. Only the measured columns are parsed: the
/// commentary columns (`_provenance`, `reading`, `not_measured`, and each row's `verdict`) are
/// prose about the numbers, so the gate compares the numbers and leaves the prose to a reader.
#[derive(Clone, Debug, Deserialize)]
struct CommittedAnswer {
    signature: String,
    selector: String,
    push_sites: usize,
    raw_sites: usize,
}

#[derive(Clone, Debug, Deserialize)]
struct CommittedRow {
    recording: String,
    address: Address,
    role: String,
    code_bytes: usize,
    swap_entry: Option<bool>,
    entries: Vec<CommittedAnswer>,
}

#[derive(Clone, Debug, Deserialize)]
struct CommittedTable {
    chain_id: u64,
    triangle_block: u64,
    executor_block: u64,
    rpc_count: u64,
    rows: Vec<CommittedRow>,
}

/// Every way a recomputed row disagrees with the committed one. A row is matched by
/// `(recording, address)` — the address it measured and the file that supplied it — never by
/// its position in the table, so reordering rows cannot make a real disagreement disappear or a
/// coincidence pass.
fn entry_table_mismatches(rows: &[EntryPointRow], committed: &CommittedTable) -> Vec<String> {
    let mut diffs = Vec::new();
    let mut matched = std::collections::BTreeSet::new();
    for row in rows {
        let key = (row.recording.as_str(), row.address);
        let Some(expected) = committed
            .rows
            .iter()
            .find(|candidate| (candidate.recording.as_str(), candidate.address) == key)
        else {
            diffs.push(format!(
                "{} {} was measured this run and has no row in the committed table",
                row.recording, row.address
            ));
            continue;
        };
        matched.insert(key);
        if row.role != expected.role {
            diffs.push(format!(
                "{} {}: this run reads it as «{}», the table as «{}»",
                row.recording, row.address, row.role, expected.role
            ));
        }
        if row.code_bytes != expected.code_bytes {
            diffs.push(format!(
                "{} {}: {} runtime bytes here, {} in the table",
                row.recording, row.address, row.code_bytes, expected.code_bytes
            ));
        }
        if row.swap_entry != expected.swap_entry {
            diffs.push(format!(
                "{} {}: swap entry {:?} here, {:?} in the table",
                row.recording, row.address, row.swap_entry, expected.swap_entry
            ));
        }
        for entry in &row.entries {
            let Some(was) = expected
                .entries
                .iter()
                .find(|candidate| candidate.signature == entry.signature)
            else {
                diffs.push(format!(
                    "{} {}: the table has no answer for `{}`",
                    row.recording, row.address, entry.signature
                ));
                continue;
            };
            if was.selector != entry.selector {
                diffs.push(format!(
                    "{} {}: `{}` selects {} here and {} in the table",
                    row.recording, row.address, entry.signature, entry.selector, was.selector
                ));
            }
            if was.push_sites != entry.push_sites || was.raw_sites != entry.raw_sites {
                diffs.push(format!(
                    "{} {}: `{}` is written at {} push / {} raw sites here and {} push / {} raw \
                     sites in the table",
                    row.recording,
                    row.address,
                    entry.signature,
                    entry.push_sites,
                    entry.raw_sites,
                    was.push_sites,
                    was.raw_sites
                ));
            }
        }
        if expected.entries.len() != row.entries.len() {
            diffs.push(format!(
                "{} {}: this run answers {} signatures, the table {}",
                row.recording,
                row.address,
                row.entries.len(),
                expected.entries.len()
            ));
        }
    }
    for expected in &committed.rows {
        if !matched.contains(&(expected.recording.as_str(), expected.address)) {
            diffs.push(format!(
                "the committed table carries {} {}, which this run did not measure",
                expected.recording, expected.address
            ));
        }
    }
    diffs
}

/// Replay instrument 9 from the two committed dumps and require the committed table to be that
/// replay: same rows, same byte counts, same per-signature site counts, and no RPC. This is §43's
/// independent recompute for the artifact that carries §40's verdict, so a reader who never ran an
/// ignored instrument can still check the numbers the verdict rests on.
#[test]
fn the_committed_entry_point_table_is_the_measurement_replayed() {
    let triangle = StateDump::from_file(&recording_path())
        .unwrap_or_else(|error| panic!("{}: {error}", recording_path().display()));
    let executor_path = workspace_root().join(executor_state::FIXTURE);
    let executor_fixture = StateDump::from_file(&executor_path)
        .unwrap_or_else(|error| panic!("{}: {error}", executor_path.display()));
    let rows = entry_rows(&triangle, &executor_fixture);

    let table_path = entry_table_path();
    let raw = std::fs::read_to_string(&table_path)
        .unwrap_or_else(|error| panic!("{}: {error}", table_path.display()));
    let committed: CommittedTable = serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("{}: {error}", table_path.display()));

    assert_eq!(
        committed.rpc_count, 0,
        "the table claims the measurement needed no node; this replay read no node either, so \
         the two are comparable"
    );
    assert_eq!(
        committed.chain_id, CHAIN.0,
        "the table was measured on chain {} and this replay is reading the same two dumps",
        CHAIN.0
    );
    assert_eq!(
        committed.triangle_block, triangle.block_number,
        "the triangle dump now on disk is not the block the table recorded"
    );
    assert_eq!(
        committed.executor_block, executor_fixture.block_number,
        "the executor dump now on disk is not the block the table recorded"
    );
    assert!(
        !rows.is_empty(),
        "the replay measured no rows at all — an empty measurement would match an empty table"
    );

    let diffs = entry_table_mismatches(&rows, &committed);
    assert!(
        diffs.is_empty(),
        "instrument 9 replayed to a different table than the one committed:\n{}",
        diffs.join("\n")
    );

    // The negative control: a count changed by one wei of bytecode must go red. Without it the
    // comparison above could be passing because it compares nothing.
    let mut falsified = rows.clone();
    let mut changed = None;
    'rows: for row in &mut falsified {
        for entry in &mut row.entries {
            if entry.push_sites > 0 {
                entry.push_sites -= 1;
                changed = Some((row.address, entry.signature.clone()));
                break 'rows;
            }
        }
    }
    let (address, signature) = changed.expect(
        "the replay needs a nonzero site count to falsify — every count zero would mean the \
         search found nothing anywhere and this gate would be comparing an all-zero table",
    );
    let falsifier = entry_table_mismatches(&falsified, &committed);
    assert!(
        falsifier
            .iter()
            .any(|diff| diff.contains(&format!("{address}")) && diff.contains(&signature)),
        "removing one `PUSH4 {signature}` site from {address} did not make the gate disagree — \
         the comparison is not reading the counts it claims to read"
    );

    // And the measurement itself, restated as a number rather than as prose: the two recordings
    // have to land on opposite sides of the entry question, or the table would be describing a
    // search that cannot tell one contract from another.
    let (triangle_carrying, triangle_total) = pairs_with_swap(&rows, RECORDING);
    let (m10_carrying, m10_total) = pairs_with_swap(&rows, executor_state::FIXTURE);
    assert_eq!(
        (triangle_carrying, m10_carrying),
        (0, m10_total),
        "{triangle_carrying} of {triangle_total} triangle pools and {m10_carrying} of \
         {m10_total} of M10's pools carry the swap entry; the table's reading is that M10's \
         recording is the family its executor is written against and the triangle is not"
    );
}
