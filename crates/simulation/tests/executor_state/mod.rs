//! The state an M10 executor run is asked about, and exactly what was added to it.
//!
//! §25 forbids the cheap version of this milestone: simulating the executor means running
//! real deployed bytecode, real pair bytecode and real token bytecode against real
//! canonical state, not a Rust restatement of the constant-product formula. So the base
//! here is a recording — `fixtures/simulation-m7/dump-37530593-07D4af6E.json`, the state
//! the M7 run froze out of the archive node at block 37 530 593 on chain 91 342, which
//! carries two real GIWA pools, both real tokens and the real header. Nothing in that file
//! is edited by anything below.
//!
//! What a recording cannot contain is the contract M10 itself writes. An executor that had
//! not been deployed when the block was produced, and a test wallet that pays for the run,
//! are added one row at a time, and every row carries the sentence in [`Row::reason`] that
//! says what it is and why it is allowed. [`Fixture::difference_from_recorded`] enumerates
//! the difference between the fixture and the recording, so "only what we declared was
//! added" is a checked claim rather than a comment — and the committed fixture file is
//! rebuilt byte for byte from the recording plus that list by
//! [`Fixture::reconstructs_the_committed_file`]. That is §29's `CONTROLLED_FIXTURE` in
//! file form: the pools and their reserves are the market's, the deployment and the
//! funding are ours.
//!
//! Two additions are worth naming in advance, because they are the ones a reader asks
//! about:
//!
//! - the operator's WETH balance, which the recording says nothing about (the M7 run spent
//!   native, not this wallet's tokens), and
//! - the executor's own storage words, including `_lock`, which must read 1 for the
//!   contract's `nonReentrant` modifier to accept a call at all. Replaying deployment as
//!   runtime bytecode skips the constructor, so its write becomes a row.
//!
//! The storage-slot constants at the top are layout claims, and each is checked by
//! execution rather than by a comment: a balance key derived from the wrong slot does not
//! resolve in the recording (see [`pool_balance_is_the_recorded_reserve`]), and an
//! allowlist key derived from the wrong slot makes every run revert `PairNotAllowed` or
//! `TokenNotAllowed` instead of reaching a swap.
//!
//! Nothing here signs, broadcasts, or holds a key. The operator is an address.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use alloy_primitives::{address, keccak256, Address, Bytes, U256};
use serde::{Deserialize, Serialize};

use evm_core::{BlockNumber, ChainId};
use evm_protocol::{ExecutorCall, ExecutorLeg};
use evm_simulation::error::SimulationError;
use evm_simulation::executor::{run, ExecutorOutcome, ExecutorRun};
use evm_simulation::state::{DumpStateProvider, StateDump, StateProvider};
use evm_simulation::{EvmRules, GasPricing};

// ---------------------------------------------------------------------------
// The recorded world
// ---------------------------------------------------------------------------

pub const CHAIN: ChainId = ChainId(91_342);
pub const BLOCK: u64 = 37_530_593;

/// The mid token of the M7 route: the ordinary, untaxed ERC-20 that both pools quote
/// against WETH.
pub const MID: Address = address!("0x07d4af6e2bc8dd82beb06b4fd279df4c9028f26f");
pub const WETH: Address = address!("0x4200000000000000000000000000000000000006");

/// The two real pools. They quote the same pair at prices roughly tenfold apart, which is
/// the gap M7 measured and M10 executes into. Pool A is the cheap-WETH side: the route
/// buys MID there and sells the MID back for WETH in pool B.
pub const POOL_A: Address = address!("0x2a3ceafba30f6626170cbb0cd67392efb94bd9a4");
pub const POOL_B: Address = address!("0x5b3c1e3fb6a97c0130ae015ff10f53a1a30c353e");

/// The wallet the recording already knows: no code, no balance, no nonce. It is the
/// operator because the recording carries an account for it, not because a key exists.
pub const OPERATOR: Address = address!("0x953e7e98562714c23bc22c7d186cdf516f9dfa6f");

/// The executor and the recipient appear nowhere in the recording, which is the point: the
/// executor is M10's own contract, and the recipient is where the profit is supposed to
/// land.
pub const EXECUTOR: Address = address!("0x1000000000000000000000000000000000000010");
pub const RECIPIENT: Address = address!("0x2000000000000000000000000000000000000020");

/// A second address that holds nothing, for the runs that must fail on who is asking.
pub const BYSTANDER: Address = address!("0x3000000000000000000000000000000000000030");

/// The recorded file the fixture is derived from, relative to the workspace root.
pub const RECORDED: &str = "fixtures/simulation-m7/dump-37530593-07D4af6E.json";

/// The fixture this module builds, committed so a report can quote the bytes it ran on.
pub const FIXTURE: &str = "fixtures/simulation-m10/fixture-37530593-executor.json";

/// The same fixture's added rows, with their reasons, committed next to it.
pub const FIXTURE_ADDITIONS: &str =
    "fixtures/simulation-m10/fixture-37530593-executor-additions.json";

/// The label every M10 fixture source string carries, so a run's evidence cannot be read
/// as a market observation by accident (§29).
pub const FIXTURE_LABEL: &str = "CONTROLLED_FIXTURE";

// ---------------------------------------------------------------------------
// Where a token keeps a balance, and where the executor keeps its words
// ---------------------------------------------------------------------------

/// WETH's `balances` mapping.
pub const WETH_BALANCE_SLOT: u64 = 0;
/// WETH's `allowances` mapping.
pub const WETH_ALLOWANCE_SLOT: u64 = 1;
/// The mid token's `balances` mapping.
pub const MID_BALANCE_SLOT: u64 = 1;

/// `ArbitrageExecutor`: `address public operator`.
pub const EXECUTOR_OPERATOR_SLOT: U256 = U256::ZERO;
/// `mapping(address => bool) public pairAllowed`.
pub const EXECUTOR_PAIR_ALLOWED_SLOT: u64 = 1;
/// `mapping(address => bool) public tokenAllowed`.
pub const EXECUTOR_TOKEN_ALLOWED_SLOT: u64 = 2;
/// `uint256 private _lock`, which a deployment reads 1.
pub const EXECUTOR_LOCK_SLOT: U256 = U256::from_limbs([3, 0, 0, 0]);

fn pad(address: Address) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(address.as_slice());
    word
}

/// A plain storage slot's key — for the executor's non-mapping words.
pub const fn plain_slot(slot: u64) -> U256 {
    U256::from_limbs([slot, 0, 0, 0])
}

/// `keccak256(pad(key) ‖ pad(slot))`: one mapping's word for a flat key.
pub fn mapping_slot(key: Address, slot: u64) -> U256 {
    let mut preimage = [0u8; 64];
    preimage[..32].copy_from_slice(&pad(key));
    preimage[32..].copy_from_slice(&plain_slot(slot).to_be_bytes::<32>());
    keccak256(preimage).into()
}

/// The balance word of `holder` in `token`.
pub fn balance_key(token: Address, holder: Address) -> U256 {
    mapping_slot(holder, balance_slot(token))
}

/// The allowance word of `owner` for `spender` in `token`: in a nested mapping the inner
/// key is itself a hash and the outer key is the *spender*.
pub fn allowance_key(token: Address, owner: Address, spender: Address) -> U256 {
    let inner = mapping_slot(owner, allowance_slot(token));
    let mut preimage = [0u8; 64];
    preimage[..32].copy_from_slice(&pad(spender));
    preimage[32..].copy_from_slice(&inner.to_be_bytes::<32>());
    keccak256(preimage).into()
}

fn balance_slot(token: Address) -> u64 {
    match token {
        WETH => WETH_BALANCE_SLOT,
        MID => MID_BALANCE_SLOT,
        other => panic!("no recorded balance slot is known for {other}; M10 trades WETH and MID"),
    }
}

fn allowance_slot(token: Address) -> u64 {
    match token {
        WETH => WETH_ALLOWANCE_SLOT,
        other => panic!("no recorded allowance slot is known for {other}"),
    }
}

/// An address as the EVM stores it in a word.
pub fn address_word(address: Address) -> U256 {
    U256::from_be_slice(&pad(address))
}

/// A `bool` as Solidity stores it: one word, 0 or 1.
pub fn allowed_word(allowed: bool) -> U256 {
    if allowed {
        U256::ONE
    } else {
        U256::ZERO
    }
}

// ---------------------------------------------------------------------------
// The route, derived from the recording rather than typed in
// ---------------------------------------------------------------------------

/// The two-leg round trip's numbers, each read out of the recorded pool balances.
///
/// A V2 pair's reserve *is* its balance of that token, so the reserves come from the
/// tokens' own books rather than from a decode of the pool's packed slot. The tests then
/// compare them with the pool's answer to `getReserves()`, which makes two independent
/// derivations check each other.
#[derive(Clone, Copy, Debug)]
pub struct Route {
    pub amount_in: U256,
    pub mid_out_leg1: U256,
    pub weth_out_leg2: U256,
}

impl Route {
    /// The route the recorded state prices: `amount_in` WETH through pool A, the MID that
    /// comes back through pool B, the WETH that comes back out.
    pub fn from_recorded(dump: &StateDump, amount_in: U256) -> Self {
        let leg1 = exact_out(
            amount_in,
            pool_balance(dump, WETH, POOL_A),
            pool_balance(dump, MID, POOL_A),
        );
        let leg2 = exact_out(
            leg1,
            pool_balance(dump, MID, POOL_B),
            pool_balance(dump, WETH, POOL_B),
        );
        Self {
            amount_in,
            mid_out_leg1: leg1,
            weth_out_leg2: leg2,
        }
    }

    /// Both legs at their exact asks, each floor equal to its own ask — the tightest route
    /// the plan can claim. Controls start here and move one number.
    pub fn legs(&self) -> Vec<ExecutorLeg> {
        vec![
            ExecutorLeg {
                pool: POOL_A,
                token_in: WETH,
                token_out: MID,
                amount_in: self.amount_in,
                amount_out: self.mid_out_leg1,
                min_amount_out: self.mid_out_leg1,
            },
            ExecutorLeg {
                pool: POOL_B,
                token_in: MID,
                token_out: WETH,
                amount_in: self.mid_out_leg1,
                amount_out: self.weth_out_leg2,
                min_amount_out: self.weth_out_leg2,
            },
        ]
    }

    /// The same legs with leg `index`'s ask and floor moved by `delta` wei (negative
    /// subtracts). The floor follows a *lowered* ask and stays put when the ask rises, so
    /// `delta == -1` produces `DeliveryMismatch`-shaped tightness and `delta == +1`
    /// produces a pool refusal rather than a contract one.
    pub fn legs_moved(&self, index: usize, delta: i64) -> Vec<ExecutorLeg> {
        let mut legs = self.legs();
        let leg = &mut legs[index];
        let step = U256::from(delta.unsigned_abs());
        let ask = if delta < 0 {
            leg.amount_out - step
        } else {
            leg.amount_out + step
        };
        if leg.min_amount_out > ask {
            leg.min_amount_out = ask;
        }
        leg.amount_out = ask;
        // The next leg's input is this leg's output, or the contract's own chain check
        // fires instead of the thing under test.
        if index + 1 < legs.len() {
            legs[index + 1].amount_in = ask;
        }
        legs
    }
}

/// `getAmountOut` for a 0.30 % pool, floored the way the Solidity divides.
///
/// The fee pair is a claim, and the run is what checks it: the contract asks the pool for
/// exactly this number and reverts `DeliveryMismatch` unless that is what arrives, so a
/// wrong fee cannot pass quietly. The ±1 wei controls pin it from both sides.
pub fn exact_out(amount_in: U256, reserve_in: U256, reserve_out: U256) -> U256 {
    let retained = amount_in * U256::from(997u32);
    (retained * reserve_out) / (reserve_in * U256::from(1000u32) + retained)
}

/// A pool's recorded balance of a token — which is the reserve the pair reports.
pub fn pool_balance(dump: &StateDump, token: Address, pool: Address) -> U256 {
    dump.storage(token, balance_key(token, pool))
        .unwrap_or_else(|| panic!("the recording holds no balance word for {pool} in {token}"))
}

// ---------------------------------------------------------------------------
// The additions
// ---------------------------------------------------------------------------

/// One row the fixture puts on top of the recording.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    /// The contract whose state is being described — a token, or the executor.
    pub contract: Address,
    /// The storage word, as the 64-hex key the dump itself uses.
    pub key: U256,
    /// What it is set to.
    pub value: U256,
    /// A short name, so the additions file reads as a table.
    pub what: String,
    /// Why this exists, and what makes it scaffolding rather than a market fact (§29).
    pub reason: String,
}

/// One account the fixture introduces, with the three fields a state read answers with.
///
/// An account an EVM touches and the source has never heard of is a hole, not a zero:
/// `DumpStateProvider` refuses it rather than inventing one. So the recipient a profit is
/// paid to and the bystander a refusal is asked of have to be declared here before a run can
/// speak about them. That is the same discipline as [`Row`], at the level of accounts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRow {
    pub address: Address,
    pub balance: U256,
    pub nonce: u64,
    /// The code as the dump prints it. `0x` for a wallet — the absence is the finding, since
    /// §58's operator has to be an account that signs rather than a contract.
    pub code: String,
    pub what: String,
    pub reason: String,
}

/// The committed additions file: both kinds of declared row, in one object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Additions {
    pub fixture: String,
    pub recorded: String,
    pub label: String,
    pub accounts: Vec<AccountRow>,
    pub words: Vec<Row>,
}

/// The shape a fixture is built with: who calls, who the contract thinks may call, what the
/// caller holds and has approved, what the deployment's words read, and what the executor
/// is already carrying.
///
/// `amount_in` is the route's claim; `balance` is what the wallet actually holds. They are
/// separate knobs because the two disagree in a control — and which one is the smaller
/// decides whether the run gets as far as an `allowance` read.
#[derive(Clone, Copy, Debug)]
pub struct Knobs {
    pub caller: Address,
    pub deployed_operator: Address,
    pub recipient: Address,
    pub amount_in: U256,
    pub balance: U256,
    pub allowance: U256,
    pub dust: U256,
    pub pair_allowed: bool,
    pub token_allowed: bool,
    pub lock: U256,
    pub deploy_executor: bool,
}

impl Knobs {
    /// Everything the route needs in place, so a run is meant to succeed. Each control
    /// starts here and moves exactly one field.
    pub fn standard() -> Self {
        Self {
            caller: OPERATOR,
            deployed_operator: OPERATOR,
            recipient: RECIPIENT,
            amount_in: amount_in(),
            balance: amount_in(),
            allowance: amount_in(),
            dust: U256::ZERO,
            pair_allowed: true,
            token_allowed: true,
            lock: plain_slot(1),
            deploy_executor: true,
        }
    }
}

/// The fixture: the recording, the declared additions, and the source string that names
/// the pair of them.
pub struct Fixture {
    pub dump: StateDump,
    pub accounts: Vec<AccountRow>,
    pub additions: Vec<Row>,
    pub source: String,
    pub knobs: Knobs,
    pub route: Route,
}

impl Fixture {
    /// The recorded dump plus exactly what [`Knobs`] implies, and nothing else.
    pub fn build(knobs: Knobs) -> Self {
        let base = recorded_dump();
        let mut dump = base.clone();
        let mut additions = Vec::new();
        let mut accounts = Vec::new();

        if knobs.deploy_executor {
            let code = executor_runtime_code();
            let reason = format!(
                "{FIXTURE_LABEL}: the contract M10 exists to run. Its bytecode is the committed \
                 solc artifact, byte for byte, and it is deployed at an address the recording has \
                 never seen — replaying the runtime code skips the constructor, which is why the \
                 words it would have written are declared rows below"
            );
            dump.insert_account(EXECUTOR, U256::ZERO, 0, &code);
            accounts.push(AccountRow {
                address: EXECUTOR,
                balance: U256::ZERO,
                nonce: 0,
                code: format!("{code:?}"),
                what: "the M10 executor".to_string(),
                reason,
            });
        }
        for address in [knobs.recipient, BYSTANDER] {
            if dump.account(address).is_some() {
                continue;
            }
            dump.insert_account(address, U256::ZERO, 0, &Bytes::new());
            accounts.push(AccountRow {
                address,
                balance: U256::ZERO,
                nonce: 0,
                code: "0x".to_string(),
                what: "a codeless account this fixture introduced".to_string(),
                reason: format!(
                    "{FIXTURE_LABEL}: {address} appears nowhere in the recording. A run pays \
                     its profit here (or is asked to be a caller), and the EVM has to be able to \
                     touch the account, so it is declared empty rather than left as a hole. It \
                     holds no native value and has no bytecode"
                ),
            });
        }

        // The executor's own words. Each is what a configuration call would have written;
        // the deployment replays runtime bytecode, so the constructor's write is a row too.
        let mut words: Vec<(U256, U256, &str, String)> = vec![(
            EXECUTOR_OPERATOR_SLOT,
            address_word(knobs.deployed_operator),
            "operator",
            format!(
                "{FIXTURE_LABEL}: what `constructor(address)` would have stored. The fixture \
                 replays the committed runtime bytecode rather than running a constructor, so \
                 the constructor's own write has to be a declared row"
            ),
        )];
        for pool in [POOL_A, POOL_B] {
            words.push((
                mapping_slot(pool, EXECUTOR_PAIR_ALLOWED_SLOT),
                allowed_word(knobs.pair_allowed),
                "pairAllowed",
                format!(
                    "{FIXTURE_LABEL}: the entry `setPairAllowed({pool}, {})` would have \
                     written. The pool is the recorded one and none of its state is touched",
                    knobs.pair_allowed
                ),
            ));
        }
        for token in [WETH, MID] {
            words.push((
                mapping_slot(token, EXECUTOR_TOKEN_ALLOWED_SLOT),
                allowed_word(knobs.token_allowed),
                "tokenAllowed",
                format!(
                    "{FIXTURE_LABEL}: the entry `setTokenAllowed({token}, {})` would have \
                     written",
                    knobs.token_allowed
                ),
            ));
        }
        words.push((
            EXECUTOR_LOCK_SLOT,
            knobs.lock,
            "_lock",
            format!(
                "{FIXTURE_LABEL}: the manual lock reads {}. The contract's `nonReentrant` \
                 modifier refuses anything other than 1, and a replayed deployment has no \
                 constructor to set it",
                knobs.lock
            ),
        ));
        for (key, value, what, reason) in words {
            declare(
                &base,
                &mut dump,
                &mut additions,
                Row {
                    contract: EXECUTOR,
                    key,
                    value,
                    what: what.to_string(),
                    reason,
                },
            );
        }

        // The balances the run measures. Every (token, holder) pair needs a word, because
        // the recording has never heard of two of the three holders and a provider that
        // invented a zero would be inventing state. Where the recording *has* heard of a
        // holder and already answers with the value the run needs, [`declare`] materializes
        // the word but does not declare it — that balance is the market's, not ours.
        for token in [WETH, MID] {
            for holder in [knobs.caller, EXECUTOR, knobs.recipient] {
                let key = balance_key(token, holder);
                let value = if token == WETH && holder == knobs.caller {
                    knobs.balance
                } else if token == MID && holder == EXECUTOR {
                    knobs.dust
                } else {
                    U256::ZERO
                };
                declare(
                    &base,
                    &mut dump,
                    &mut additions,
                    Row {
                        contract: token,
                        key,
                        value,
                        what: format!("balanceOf({holder}) in {token}"),
                        reason: if token == MID && holder == EXECUTOR && !value.is_zero() {
                            format!(
                                "{FIXTURE_LABEL}: the executor starts this run already holding \
                                 {value} of the route's mid token, which a real deployment could \
                                 acquire by an earlier run leaving residue. Nothing in the \
                                 recorded market is touched by this row"
                            )
                        } else if value.is_zero() {
                            format!(
                                "{FIXTURE_LABEL}: an address this fixture introduced cannot have \
                                 a recorded balance, and the provider refuses to guess one, so \
                                 the word is written as zero and the run asks the token instead"
                            )
                        } else {
                            format!(
                                "{FIXTURE_LABEL}: the test wallet holds {value} WETH, which is \
                                 what the run is asked to spend. No pool, reserve, or market \
                                 balance is touched by this row"
                            )
                        },
                    },
                );
            }
        }
        let allowance = allowance_key(WETH, knobs.caller, EXECUTOR);
        declare(
            &base,
            &mut dump,
            &mut additions,
            Row {
                contract: WETH,
                key: allowance,
                value: knobs.allowance,
                what: format!("allowance({} → executor) in WETH", knobs.caller),
                reason: format!(
                    "{FIXTURE_LABEL}: the approval an operator would have signed for a real \
                     deployment, granted here as state so a fixture run needs no signature"
                ),
            },
        );

        // WETH's own code consults the sender's self-allowance while the executor moves its
        // WETH, and both halves of that word name an address this fixture introduced. The
        // consult is inside the token's `transfer`, not the pair's — the pool bytecode,
        // identical at both recorded addresses, has no `allowance` call site. The row is
        // load-bearing rather than merely tidy, and `a_declared_word_the_run_does_not_ask_about_
        // would_be_an_invention` proves it: take the word out of the dump and the run is
        // refused at exactly this slot instead of being handed a guessed zero. The node answers
        // 0 for the slot at the pinned block (`eth_getStorageAt`), which is what this row writes.
        let self_allowance = allowance_key(WETH, EXECUTOR, EXECUTOR);
        declare(
            &base,
            &mut dump,
            &mut additions,
            Row {
                contract: WETH,
                key: self_allowance,
                value: U256::ZERO,
                what: "allowance(executor → executor) in WETH".to_string(),
                reason: format!(
                    "{FIXTURE_LABEL}: the executor is an address this fixture introduced, so no \
                     recording can hold a mapping word about it, and the provider refuses to \
                     guess one. The run asks WETH for it during the route's transfers, so the \
                     word is written as 0 — the same answer the node gives for this slot at \
                     block {BLOCK} (eth_getStorageAt). Nothing about the market is claimed by \
                     this row: a self-allowance of zero is what every address that has never \
                     approved itself holds"
                ),
            },
        );

        let route = Route::from_recorded(&dump, knobs.amount_in);
        let source = format!(
            "{FIXTURE_LABEL}: {RECORDED} (real recorded pools, tokens and header) plus {} \
             declared accounts and {} declared storage rows: the executor deployment with its \
             configuration words, the holder balances and the allowances this run reads. A row \
             is declared only where it differs from the recording — a word the recording already \
             carries at the same value is read, not claimed. The recorded market state is unedited",
            accounts.len(),
            additions.len(),
        );
        Self {
            dump,
            accounts,
            additions,
            source,
            knobs,
            route,
        }
    }

    pub fn provider(&self) -> Arc<dyn StateProvider> {
        Arc::new(DumpStateProvider::new(
            self.dump.clone(),
            self.source.clone(),
        ))
    }

    /// `execute` over these legs, with the caller's own final floor.
    pub fn execute(&self, legs: Vec<ExecutorLeg>, min_final_amount: U256) -> ExecutorCall {
        ExecutorCall::Execute {
            legs,
            input_token: WETH,
            amount_in: self.route.amount_in,
            min_final_amount,
            recipient: self.knobs.recipient,
        }
    }

    /// The tight route: both asks exact, the final floor at the priced output.
    pub fn standard_call(&self) -> ExecutorCall {
        self.execute(self.route.legs(), self.route.weth_out_leg2)
    }

    /// The run spec for `call`, built so a control can move one field of it. The provider
    /// is handed over bare: [`evm_simulation::executor::run`] layers [`ExecutorRun::setup`]
    /// itself, which is what keeps §58's single permitted state edit in one place.
    pub fn spec(&self, call: ExecutorCall) -> ExecutorRun {
        ExecutorRun {
            chain_id: CHAIN,
            priced_at: BlockNumber(BLOCK),
            state_source: self.source.clone(),
            executor: EXECUTOR,
            operator: self.knobs.caller,
            call,
            gas_limit: GAS_LIMIT,
            rules: EvmRules::Prague,
            pricing: GasPricing::Eip1559 {
                priority_fee_per_gas: 0,
                provenance: format!(
                    "block {BLOCK}'s own base fee as the recorded header reports it, with no \
                     tip: a hypothetical transaction on a historical block is not competing to \
                     be included in it"
                ),
            },
            endowment: Some(ENDOWMENT),
        }
    }

    /// The run for a spec. A refusal comes back as an error rather than being hidden: §71
    /// keeps "the EVM refused to answer" apart from "the EVM answered: no".
    pub async fn try_run(&self, call: ExecutorCall) -> Result<ExecutorOutcome, SimulationError> {
        self.try_run_spec(&self.spec(call)).await
    }

    pub async fn try_run_spec(
        &self,
        spec: &ExecutorRun,
    ) -> Result<ExecutorOutcome, SimulationError> {
        run(self.provider(), spec).await
    }

    pub async fn run_spec(&self, spec: &ExecutorRun) -> ExecutorOutcome {
        self.try_run_spec(spec)
            .await
            .unwrap_or_else(|error| panic!("the fixture refused to run: {error}"))
    }

    pub async fn run(&self, call: ExecutorCall) -> ExecutorOutcome {
        self.try_run(call)
            .await
            .unwrap_or_else(|error| panic!("the fixture refused to run: {error}"))
    }

    /// Every storage word the fixture carries that the recording does not already carry with
    /// the same value.
    ///
    /// This is the audit form of "the market was not touched": the list is produced by
    /// comparing the two dumps, not by reading the builder's own bookkeeping, so a row
    /// written by accident anywhere else shows up here.
    pub fn difference_from_recorded(&self) -> Vec<(Address, U256, Option<U256>, U256)> {
        let recorded = recorded_dump();
        let mut rows: Vec<(Address, U256, Option<U256>, U256)> = Vec::new();
        for (address, words) in &self.dump.storage {
            let contract = parse_address(address);
            for (key, value) in words {
                let slot = U256::from_str_radix(key.trim_start_matches("0x"), 16)
                    .expect("a 64-hex slot key");
                let wanted = parse_number(value);
                let already = recorded.storage(contract, slot);
                if already != Some(wanted) {
                    rows.push((contract, slot, already, wanted));
                }
            }
        }
        rows.sort_by_key(|(contract, slot, _, _)| (*contract, *slot));
        rows
    }

    /// The accounts the fixture introduced, by address: the same claim one level up. An
    /// account a run touches has to be answered by the source, so a fixture that silently
    /// created one would be hiding a state edit from the table above.
    pub fn introduced_accounts(&self) -> Vec<Address> {
        let recorded = recorded_dump();
        let mut seen: Vec<Address> = Vec::new();
        for (address, account) in &self.dump.accounts {
            let contract = parse_address(address);
            let before = recorded.account(contract);
            let new = before.is_none();
            let changed = before.is_some_and(|before| before != account);
            if (new || changed) && !seen.contains(&contract) {
                seen.push(contract);
            }
        }
        seen.sort();
        seen
    }

    /// The declared additions, in the same order-independent form as
    /// [`Fixture::difference_from_recorded`], for the test that says the two are equal.
    pub fn declared_difference(&self) -> Vec<(Address, U256, Option<U256>, U256)> {
        let recorded = recorded_dump();
        let mut rows: Vec<(Address, U256, Option<U256>, U256)> = self
            .additions
            .iter()
            .map(|row| {
                (
                    row.contract,
                    row.key,
                    recorded.storage(row.contract, row.key),
                    row.value,
                )
            })
            .collect();
        rows.sort_by_key(|(contract, slot, _, _)| (*contract, *slot));
        rows
    }

    /// The fixture's own bytes, exactly as [`StateDump::write_file`] would produce them.
    pub fn fixture_bytes(&self) -> Vec<u8> {
        let mut text = serde_json::to_string_pretty(&self.dump)
            .expect("the fixture serializes")
            .into_bytes();
        text.push(b'\n');
        text
    }

    pub fn addition_bytes(&self) -> Vec<u8> {
        let additions = Additions {
            fixture: FIXTURE.to_string(),
            recorded: RECORDED.to_string(),
            label: FIXTURE_LABEL.to_string(),
            accounts: self.accounts.clone(),
            words: self.additions.clone(),
        };
        let mut text = serde_json::to_string_pretty(&additions)
            .expect("the additions serialize")
            .into_bytes();
        text.push(b'\n');
        text
    }

    /// Write both files. Reached by the ignored setup test, which is the only place the
    /// fixture is ever generated; the gates only read it.
    pub fn write_files(&self) -> (PathBuf, PathBuf) {
        let fixture = workspace_root().join(FIXTURE);
        if let Some(parent) = fixture.parent() {
            std::fs::create_dir_all(parent).expect("the fixture directory exists");
        }
        std::fs::write(&fixture, self.fixture_bytes()).expect("the fixture file writes");
        let additions = workspace_root().join(FIXTURE_ADDITIONS);
        std::fs::write(&additions, self.addition_bytes()).expect("the additions file writes");
        (fixture, additions)
    }

    /// Rebuild the standard fixture from the recording and compare bytes with the committed
    /// files, then hand back the in-memory fixture. §49's D3 gate starts here: if the state
    /// a run is asked about is not the state on disk, no result computed from it is
    /// comparable to a result computed from the file a report quotes.
    pub fn committed() -> Self {
        let built = Self::build(Knobs::standard());
        let fixture = std::fs::read(workspace_root().join(FIXTURE)).unwrap_or_else(|error| {
            panic!(
                "{}: {error}\nThe committed M10 fixture is what these tests run on. It is \
                 written by the ignored setup test:\n    cargo test -p evm-simulation --test \
                 executor_revm -- --ignored --nocapture",
                workspace_root().join(FIXTURE).display()
            )
        });
        let additions = std::fs::read(workspace_root().join(FIXTURE_ADDITIONS))
            .expect("the committed additions file");
        assert_eq!(
            built.fixture_bytes(),
            fixture,
            "the fixture rebuilt from the recording plus its declared rows differs from the \
             committed fixture file"
        );
        assert_eq!(
            built.addition_bytes(),
            additions,
            "the declared rows rebuilt here differ from the committed additions file"
        );
        built
    }
}

/// The executor's runtime bytecode, read from the committed artifact rather than embedded,
/// so the fixture and the deployment in §57 cannot drift apart from what `solc` emitted.
pub fn executor_runtime_code() -> Bytes {
    let path = workspace_root().join("contracts/artifacts/ArbitrageExecutor.bin-runtime");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    Bytes::from(hex::decode(text.trim()).expect("the runtime bytecode file is hex"))
}

/// Materialize one word the run reads, and declare it only where it differs from the
/// recording.
///
/// The dump has to carry every word the EVM asks about — [`DumpStateProvider`] refuses an
/// unrecorded key rather than guessing a zero — but a word the recording already answers at
/// the same value is not something this fixture *added*. The operator's balance of the mid
/// token is that case: the M7 recording read it and found zero, so the fixture reuses the
/// market's own answer instead of claiming a row for it. Keeping the two ideas apart is what
/// lets [`Fixture::difference_from_recorded`] and [`Fixture::declared_difference`] be equal
/// without either one rounding the truth.
fn declare(base: &StateDump, dump: &mut StateDump, additions: &mut Vec<Row>, row: Row) {
    dump.insert_storage(row.contract, row.key, row.value);
    if base.storage(row.contract, row.key) != Some(row.value) {
        additions.push(row);
    }
}

/// The recorded dump, with nothing added: for the questions the market alone answers, and
/// for the diff that proves the fixture added only what it declared.
pub fn recorded_dump() -> StateDump {
    StateDump::from_file(&workspace_root().join(RECORDED))
        .unwrap_or_else(|error| panic!("{RECORDED}: {error}"))
}

/// The route's stake: 1e14 WETH wei, which is 0.0001 WETH. Small enough that the same
/// shape is a rounding error on a testnet, large enough that the pools' integer math is
/// not degenerate.
pub const AMOUNT_IN_WEI: u128 = 100_000_000_000_000;

/// Native money for the operator (§58's one permitted state edit) and the per-transaction
/// allowance. 1e18 wei covers the whole run at the recorded base fee by a wide margin.
pub const ENDOWMENT: U256 = U256::from_limbs([10_000_000_000_000_000_000u64, 0, 0, 0]);
pub const GAS_LIMIT: u64 = 3_000_000;

pub const fn amount_in() -> U256 {
    U256::from_limbs([AMOUNT_IN_WEI as u64, 0, 0, 0])
}

pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn parse_address(text: &str) -> Address {
    text.parse::<Address>().expect("a hex address key")
}

fn parse_number(text: &str) -> U256 {
    let text = text.trim();
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex_digits) => U256::from_str_radix(hex_digits, 16).expect("a hex value"),
        None => text.parse::<U256>().expect("a decimal value"),
    }
}
