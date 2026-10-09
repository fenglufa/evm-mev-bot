//! §37's controlled three-hop market, built on M10's real pieces.
//!
//! M10 proved the two-hop round trip `WETH → MID → WETH` across two *recorded* pools
//! ([`executor_state`]). A three-hop cycle needs a third token and two more pools, and the
//! committed recordings do not contain them as tradable state: the 80-pool table M9.2 froze
//! holds exactly one triangle (enumerated from that table's own rows in
//! `tests/triangle_probe.rs`), and instrument 9 of the same file measured that the swap entry
//! M10's executor issues appears in the dispatcher of both M10 pools and appears zero times in
//! all three pools of that triangle — so the only real cycle on the shelf cannot be driven by
//! this contract at all.
//!
//! So this module does what §37 asks and no more. Every part that executes is real — the
//! executor's committed bytecode, the pair's recorded runtime code, the tokens' recorded
//! runtime code, the header, the chain, the fee — and the topology on top of it is *declared*,
//! row by row, labelled `CONTROLLED_FIXTURE`:
//!
//! ```text
//! leg A   WETH      → MID      POOL_A   recorded: code, reserves and balances all the market's
//! leg B   MID       → TOKEN_C  POOL_BC  POOL_A's runtime code replayed, pair words declared
//! leg C   TOKEN_C   → WETH     POOL_CA  POOL_A's runtime code replayed, pair words declared
//! ```
//!
//! ## What is whose
//!
//! ```text
//! pair implementation       real — POOL_A's recorded runtime code, byte for byte
//! executor                  real — M10's committed solc artifact
//! header, chain, base fee   real — block 37 530 593 as the recording spells it
//! POOL_A and its reserves   real — the recording's own balance words
//! TOKEN_C, POOL_BC, POOL_CA declared accounts
//! their pair words, their depths as balances, the self-allowances their token code consults,
//! and the executor's new allow-list entries            declared words, each row with a reason
//! delivery, gas, revert     the EVM's answer, i.e. Simulated — never the pricing's claim
//! ```
//!
//! A declared pool is not a hand-shaped contract: its storage map is copied from the recorded
//! pool word for word and then re-pointed in exactly three places — `token0`, `token1`, and the
//! packed reserve slot, which is written from the balances the fixture also declares, so
//! `getReserves()` and the tokens' books cannot disagree (§16's rule: the pool answers, the
//! caller does not assume). `blockTimestampLast` is read out of the recorded pool rather than
//! chosen, because that is the clock the real pools ran at this header.
//!
//! Nothing here signs, broadcasts, holds a key, or asks a node. Every read is answered from the
//! dump, so `rpc_count` is zero by construction (§44), and no number in this module publishes a
//! market verdict (§41: a cycle that delivers here is a controlled delivery, not an arbitrage
//! that existed).

// Included by more than one test binary (`multihop_revm.rs` and `multihop_e2e.rs`), so each one
// sees the other's helpers as unused. The same allowance the sibling fixture modules carry.
#![allow(dead_code)]

use alloy_primitives::{address, keccak256, Address, Bytes, U256};
use evm_protocol::{ExecutorCall, ExecutorLeg};
use evm_simulation::state::StateDump;

use crate::executor_state::{
    allowed_word, exact_out, mapping_slot, plain_slot, recorded_dump, AccountRow, Fixture, Knobs,
    Row, BLOCK, CHAIN, EXECUTOR, EXECUTOR_PAIR_ALLOWED_SLOT, EXECUTOR_TOKEN_ALLOWED_SLOT,
    FIXTURE_LABEL, MID, MID_BALANCE_SLOT, POOL_A, RECIPIENT, RECORDED, WETH, WETH_ALLOWANCE_SLOT,
    WETH_BALANCE_SLOT,
};
use crate::multihop_market;

// ---------------------------------------------------------------------------
// The declared topology
// ---------------------------------------------------------------------------

/// The third token: WETH's recorded runtime code, replayed at an address the recording has
/// never seen. Its storage layout is therefore WETH's — a claim the runs check, because a wrong
/// slot would hand the pool a reserve that disagrees with its own books.
pub const TOKEN_C: Address = address!("0x60000000000000000000000000000000000000c3");

/// Leg B's venue: `MID ↔ TOKEN_C`, POOL_A's recorded pair implementation replayed.
pub const POOL_BC: Address = address!("0x7000000000000000000000000000000000000bc1");

/// Leg C's venue: `WETH ↔ TOKEN_C`, the same replay.
pub const POOL_CA: Address = address!("0x7000000000000000000000000000000000000ca2");

/// The four depths this fixture hands the two declared pools. These are the only numbers in the
/// module chosen rather than read, and they are chosen so the cycle's price product sits above
/// one while each leg stays a small fraction of the depth it trades against — the shape §37's
/// controlled cycle needs. They are scaffolding: they touch no row of the recorded market, and
/// leg A's reserves come from the recording instead ([`Cycle::reserves`]).
pub const MID_RESERVE_IN_BC: u128 = 2_000_000_000_000_000_000_000;
pub const TC_RESERVE_IN_BC: u128 = 2_000_000_000_000_000_000;
pub const TC_RESERVE_IN_CA: u128 = 2_000_000_000_000_000_000;
pub const WETH_RESERVE_IN_CA: u128 = 2_400_000_000_000_000;

/// The stake the cycle is asked to trade: a hundredth of the stake M10's two-hop fixture
/// carries, and a thousandth of the recorded pool's WETH depth, so leg A's slippage stays small
/// and a delivery that misses the priced number by a whole unit means the pricing was wrong
/// rather than the market deep.
pub const CYCLE_AMOUNT_IN_WEI: u128 = 1_000_000_000_000;

pub fn cycle_amount_in() -> U256 {
    U256::from(CYCLE_AMOUNT_IN_WEI)
}

/// One pool as this fixture states it: the pair in the order the addresses sort, and the depth
/// of each side.
#[derive(Clone, Copy, Debug)]
pub struct DeclaredPool {
    pub pool: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
}

/// The two pools this module introduces.
pub fn declared_pools() -> Vec<DeclaredPool> {
    vec![
        DeclaredPool {
            pool: POOL_BC,
            token0: MID,
            token1: TOKEN_C,
            reserve0: U256::from(MID_RESERVE_IN_BC),
            reserve1: U256::from(TC_RESERVE_IN_BC),
        },
        DeclaredPool {
            pool: POOL_CA,
            token0: WETH,
            token1: TOKEN_C,
            reserve0: U256::from(WETH_RESERVE_IN_CA),
            reserve1: U256::from(TC_RESERVE_IN_CA),
        },
    ]
}

/// The cycle's three venues in trade order: `WETH → MID → TOKEN_C → WETH`.
pub fn pools_in_order() -> [Address; 3] {
    [POOL_A, POOL_BC, POOL_CA]
}

/// The three tokens the cycle steps through, starting and ending on the input token.
pub fn tokens_in_order() -> [Address; 3] {
    [WETH, MID, TOKEN_C]
}

// ---------------------------------------------------------------------------
// Storage keys, including for the token this module introduces
// ---------------------------------------------------------------------------

/// The balance-mapping slot of a token this cycle trades. WETH's and MID's come from M10's
/// table unchanged; `TOKEN_C` runs WETH's bytecode, so its books are WETH's.
pub fn balance_slot(token: Address) -> u64 {
    if token == WETH || token == TOKEN_C {
        WETH_BALANCE_SLOT
    } else if token == MID {
        MID_BALANCE_SLOT
    } else {
        panic!("no recorded balance slot is known for {token}")
    }
}

/// The allowance-mapping slot of a token this cycle trades.
///
/// MID is refused rather than guessed: M10's fixture needed no allowance word for it — the
/// route pulls WETH only — and a slot invented here would be a claim about a contract this
/// module did not read.
pub fn allowance_slot(token: Address) -> u64 {
    if token == WETH || token == TOKEN_C {
        WETH_ALLOWANCE_SLOT
    } else {
        panic!("no recorded allowance slot is known for {token}")
    }
}

/// The balance word of `holder` in `token`.
pub fn holder_balance_key(token: Address, holder: Address) -> U256 {
    mapping_slot(holder, balance_slot(token))
}

/// The allowance word of `owner` for `spender` in `token`: the inner key is itself a hash and
/// the outer key is the spender, the same shape [`executor_state::allowance_key`] builds.
pub fn holder_allowance_key(token: Address, owner: Address, spender: Address) -> U256 {
    let inner = mapping_slot(owner, allowance_slot(token));
    let mut preimage = [0u8; 64];
    preimage[..32].copy_from_slice(&pad32(spender));
    preimage[32..].copy_from_slice(&inner.to_be_bytes::<32>());
    keccak256(preimage).into()
}

/// An address as the EVM right-pads it into a word.
fn pad32(address: Address) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(address.as_slice());
    word
}

/// An address as a pair stores it in a word.
fn address_word_of(address: Address) -> U256 {
    U256::from_be_slice(&pad32(address))
}

/// The address carried in a storage word's low 20 bytes.
pub fn address_of_word(word: U256) -> Address {
    let bytes = word.to_be_bytes::<32>();
    Address::from_slice(&bytes[12..])
}

/// The pair's packed reserve word: `uint112 reserve0 | uint112 reserve1 << 112 | uint32
/// blockTimestampLast << 224` — the layout read out of recorded state by M10's delivery tests
/// and instrument 7 of `tests/triangle_probe.rs`, not assumed here. 256-bit arithmetic
/// throughout, so no reserve is ever narrowed to fit (§48's discipline).
pub fn pack_reserves(reserve0: U256, reserve1: U256, clock: U256) -> U256 {
    let mask = (U256::from(1u8) << 112) - U256::from(1u8);
    (reserve0 & mask) | ((reserve1 & mask) << 112) | (clock << 224)
}

/// A pool's `blockTimestampLast`: the high 32 bits of its packed reserve slot, kept as a word
/// rather than narrowed to a clock unit.
pub fn recorded_clock(dump: &StateDump, pool: Address) -> U256 {
    let packed = dump
        .storage(pool, plain_slot(8))
        .unwrap_or_else(|| panic!("no packed reserve word is recorded for {pool}"));
    packed >> 224
}

/// The recorded runtime code of an account, as bytes — the copy a declared clone is made from.
pub fn recorded_code(dump: &StateDump, address: Address) -> Bytes {
    let account = dump
        .account(address)
        .unwrap_or_else(|| panic!("the recording holds no account at {address}"));
    let text = account.code.trim_start_matches("0x");
    let mut bytes = Vec::with_capacity(text.len() / 2);
    let mut index = 0;
    while index < text.len() {
        bytes.push(u8::from_str_radix(&text[index..index + 2], 16).expect("hex code"));
        index += 2;
    }
    Bytes::from(bytes)
}

/// The key a dump files an account's storage map under: lower-case, `0x`-prefixed, exactly as
/// `StateDump` writes it.
fn dump_key(address: Address) -> String {
    format!("0x{:x}", address)
}

/// A word as a dump spells it — decimal, as M7's recording writes every value, or hex. The
/// reader accepts both because a fixture is read from disk and the disk is the authority, not
/// this module's preference.
fn dumped_word(text: &str) -> U256 {
    let text = text.trim();
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex_digits) => U256::from_str_radix(hex_digits, 16).expect("a hex value"),
        None => text.parse::<U256>().expect("a decimal value"),
    }
}

/// Every storage word a recorded account carries.
fn recorded_words(dump: &StateDump, address: Address) -> Vec<(U256, U256)> {
    let words = dump
        .storage
        .get(&dump_key(address))
        .unwrap_or_else(|| panic!("the recording holds no storage map for {address}"));
    words
        .iter()
        .map(|(slot, value)| {
            (
                U256::from_str_radix(slot.trim_start_matches("0x"), 16).expect("a 64-hex slot key"),
                dumped_word(value),
            )
        })
        .collect()
}

/// A pool's balance of a token, for any token this cycle trades.
pub fn dumped_balance(dump: &StateDump, token: Address, holder: Address) -> U256 {
    dump.storage(token, holder_balance_key(token, holder))
        .unwrap_or_else(|| panic!("the fixture carries no balance word for {holder} in {token}"))
}

// ---------------------------------------------------------------------------
// The route, derived from the fixture's own state
// ---------------------------------------------------------------------------

/// The three legs' numbers, each read out of the balances the state carries.
///
/// The same discipline as M10's `Route`: a V2 pair's reserve *is* its balance of that token, so
/// the asks come from the tokens' books and the run then checks them against the pool's own
/// `getReserves()` — two independent derivations holding each other to account.
#[derive(Clone, Copy, Debug)]
pub struct CycleRoute {
    pub amount_in: U256,
    pub mid_out_leg_a: U256,
    pub tc_out_leg_b: U256,
    pub weth_out_leg_c: U256,
}

impl CycleRoute {
    /// The cycle as `dump` prices it at `amount_in`.
    pub fn from_dump(dump: &StateDump, amount_in: U256) -> Self {
        let [pool_a, pool_bc, pool_ca] = pools_in_order();
        let [token_in, token_mid, token_tc] = tokens_in_order();
        let leg_a = exact_out(
            amount_in,
            dumped_balance(dump, token_in, pool_a),
            dumped_balance(dump, token_mid, pool_a),
        );
        let leg_b = exact_out(
            leg_a,
            dumped_balance(dump, token_mid, pool_bc),
            dumped_balance(dump, token_tc, pool_bc),
        );
        let leg_c = exact_out(
            leg_b,
            dumped_balance(dump, token_tc, pool_ca),
            dumped_balance(dump, token_in, pool_ca),
        );
        Self {
            amount_in,
            mid_out_leg_a: leg_a,
            tc_out_leg_b: leg_b,
            weth_out_leg_c: leg_c,
        }
    }

    /// Three legs at their exact asks, each floor equal to its own ask — the tightest route a
    /// plan can claim. Controls start here and move one number.
    pub fn legs(&self) -> Vec<ExecutorLeg> {
        let [pool_a, pool_bc, pool_ca] = pools_in_order();
        let [token_in, token_mid, token_tc] = tokens_in_order();
        let rows = [
            (
                pool_a,
                token_in,
                token_mid,
                self.amount_in,
                self.mid_out_leg_a,
            ),
            (
                pool_bc,
                token_mid,
                token_tc,
                self.mid_out_leg_a,
                self.tc_out_leg_b,
            ),
            (
                pool_ca,
                token_tc,
                token_in,
                self.tc_out_leg_b,
                self.weth_out_leg_c,
            ),
        ];
        rows.into_iter()
            .map(
                |(pool, token_in, token_out, amount_in, amount_out)| ExecutorLeg {
                    pool,
                    token_in,
                    token_out,
                    amount_in,
                    amount_out,
                    min_amount_out: amount_out,
                },
            )
            .collect()
    }

    /// The same legs with leg `index`'s ask and floor moved by `delta` wei (negative
    /// subtracts). The floor follows a lowered ask and stays put when the ask rises, so
    /// `delta == -1` produces delivery-shaped tightness while a positive delta makes a pool
    /// refuse instead of the contract.
    pub fn legs_moved(&self, index: usize, delta: i128) -> Vec<ExecutorLeg> {
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
        // The next leg's input is this leg's output, or the contract's own chain check fires
        // instead of the thing under test.
        if index + 1 < legs.len() {
            legs[index + 1].amount_in = ask;
        }
        legs
    }

    /// The same legs with leg `index` asking a multiple of what its pool can pay: §39's way to
    /// make one named leg fail while the legs before it have already traded.
    pub fn legs_inflated(&self, index: usize, multiple: u32) -> Vec<ExecutorLeg> {
        let mut legs = self.legs();
        let leg = &mut legs[index];
        let ask = leg.amount_out * U256::from(multiple);
        leg.amount_out = ask;
        leg.min_amount_out = ask;
        if index + 1 < legs.len() {
            legs[index + 1].amount_in = ask;
        }
        legs
    }

    /// The route's gross in the input token: the priced output against the stake. A claim about
    /// pricing, never about delivery — the run answers that separately.
    pub fn priced_gross(&self) -> Option<U256> {
        self.weth_out_leg_c.checked_sub(self.amount_in)
    }
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// One more word the run asked for and the fixture did not carry, at the value a node answers
/// for a slot that has never been set.
#[derive(Clone, Debug)]
pub struct ExtraRow {
    pub contract: Address,
    pub key: U256,
    pub value: U256,
    pub what: String,
    pub reason: String,
}

/// A zero word the crack loop found the run asking for.
pub fn learned_row(contract: Address, key: U256, what: impl Into<String>) -> ExtraRow {
    ExtraRow {
        contract,
        key,
        value: U256::ZERO,
        what: what.into(),
        reason: format!(
            "{FIXTURE_LABEL}: discovered by the crack loop rather than guessed — a run named this \
             word as one it reads, and the provider refuses to invent an answer for a slot that \
             is in no recording. Zero is the answer the node gives for a slot that has never been \
             set, and it is the answer every other holder of this mapping carries here"
        ),
    }
}

/// The cycle: M10's two-hop fixture extended with a declared third token and two declared
/// pools, plus the route those three pools price.
///
/// The extension is additive and visible — [`Cycle::added_words`] and [`Cycle::added_accounts`]
/// list what this module put on top of M10's fixture, and `Fixture::declared_difference()` still
/// audits the whole thing against the recording.
pub struct Cycle {
    pub fx: Fixture,
    pub route: CycleRoute,
    pub added_words: Vec<Row>,
    pub added_accounts: Vec<AccountRow>,
}

impl Cycle {
    /// The fixture at M10's standard knobs and this module's declared stake.
    pub fn build() -> Self {
        Self::build_at(cycle_amount_in(), &[])
    }

    /// The fixture with one more declared word per `extra` entry. The crack loop uses this: a
    /// provider that refuses an unrecorded word names the contract and the slot, so the set of
    /// words a run reads is *discovered* rather than guessed, and each one then becomes a row
    /// with a reason.
    pub fn build_at(amount_in: U256, extra: &[ExtraRow]) -> Self {
        let base = recorded_dump();
        let mut fx = Fixture::build(Knobs::standard());
        let mut added_words: Vec<Row> = Vec::new();
        let mut added_accounts: Vec<AccountRow> = Vec::new();

        let pair_code = recorded_code(&base, POOL_A);
        let pair_bytes = pair_code.len();
        let token_code = recorded_code(&base, WETH);
        let clock = recorded_clock(&base, POOL_A);
        let template = recorded_words(&base, POOL_A);

        // The three declared accounts. Each is a recorded contract's runtime code replayed at a
        // fresh address, so the machine that answers the call is the machine the market runs —
        // only the topology is this fixture's.
        fx.dump.insert_account(TOKEN_C, U256::ZERO, 0, &token_code);
        added_accounts.push(AccountRow {
            address: TOKEN_C,
            balance: U256::ZERO,
            nonce: 0,
            code: format!("{token_code:?}"),
            what: "the cycle's third token".to_string(),
            reason: format!(
                "{FIXTURE_LABEL}: WETH's recorded runtime code, byte for byte, replayed at an \
                 address the recording has never seen. Replaying runtime code skips a \
                 constructor, so the words that would have set this token's state are declared \
                 rows below. No recorded account is edited by this row"
            ),
        });
        for entry in declared_pools() {
            fx.dump
                .insert_account(entry.pool, U256::ZERO, 0, &pair_code);
            added_accounts.push(AccountRow {
                address: entry.pool,
                balance: U256::ZERO,
                nonce: 0,
                code: format!("{pair_code:?}"),
                what: format!("a pool trading {:#x} ↔ {:#x}", entry.token0, entry.token1),
                reason: format!(
                    "{FIXTURE_LABEL}: the recorded pair's runtime code ({pair_bytes} bytes, the \
                     code of {POOL_A}), byte for byte, replayed at a fresh address, so this leg \
                     is executed by the same implementation the market runs. Its storage map is \
                     copied from {POOL_A} word for word — {} words — and then re-pointed at this \
                     pair's own tokens, reserves and the balances that match them. Nothing in \
                     {POOL_A} itself is touched",
                    template.len(),
                ),
            });
        }

        // Each declared pool: the recorded pool's whole storage map, then the words that make it
        // a different pair. Copying rather than starting from an empty map is what keeps these
        // from being a second, simpler pair implementation — every word a swap reads arrives with
        // the value the recorded pool carries at this header.
        for entry in declared_pools() {
            for (key, value) in &template {
                fx.dump.insert_storage(entry.pool, *key, *value);
            }
            let packed = pack_reserves(entry.reserve0, entry.reserve1, clock);
            let mut rows = vec![
                Row {
                    contract: entry.pool,
                    key: plain_slot(6),
                    value: address_word_of(entry.token0),
                    what: "token0".to_string(),
                    reason: format!(
                        "{FIXTURE_LABEL}: this pool's lower-addressed token. The order is the \
                         pair's own rule, and a route that got it wrong is refused by the pool \
                         rather than by this file"
                    ),
                },
                Row {
                    contract: entry.pool,
                    key: plain_slot(7),
                    value: address_word_of(entry.token1),
                    what: "token1".to_string(),
                    reason: format!(
                        "{FIXTURE_LABEL}: this pool's higher-addressed token, the pair {:#x} ↔ \
                         {:#x}",
                        entry.token0, entry.token1
                    ),
                },
                Row {
                    contract: entry.pool,
                    key: plain_slot(8),
                    value: packed,
                    what: "packed reserves".to_string(),
                    reason: format!(
                        "{FIXTURE_LABEL}: {reserve0} of {token0} and {reserve1} of {token1}, with \
                         `blockTimestampLast` read out of {POOL_A} rather than chosen. The two \
                         magnitudes are this fixture's declared depth, and they are the same \
                         numbers the balance rows below hand this pool — so the reserve the pair \
                         reports and the balance the token keeps are one fact, not two claims",
                        reserve0 = entry.reserve0,
                        reserve1 = entry.reserve1,
                        token0 = entry.token0,
                        token1 = entry.token1,
                    ),
                },
            ];
            for (token, reserve) in [
                (entry.token0, entry.reserve0),
                (entry.token1, entry.reserve1),
            ] {
                rows.push(Row {
                    contract: token,
                    key: holder_balance_key(token, entry.pool),
                    value: reserve,
                    what: format!("balanceOf({:#x}) in {token}", entry.pool),
                    reason: format!(
                        "{FIXTURE_LABEL}: the depth this pool trades against, {reserve}. Declared \
                         as a balance as well as a reserve, because the pair prices from its own \
                         books: a run reads this word once through `getReserves()` and again \
                         through `balanceOf`, and disagrees with itself if the two differ"
                    ),
                });
            }
            // The self-allowance the token's code consults while a holder moves that token — the
            // consult M10 documents for WETH. Only the pairs this route actually sends from are
            // written: POOL_BC sends TOKEN_C, POOL_CA sends WETH.
            let sent = if entry.pool == POOL_BC { TOKEN_C } else { WETH };
            rows.push(Row {
                contract: sent,
                key: holder_allowance_key(sent, entry.pool, entry.pool),
                value: U256::ZERO,
                what: format!("allowance({:#x} → {:#x}) in {sent}", entry.pool, entry.pool),
                reason: format!(
                    "{FIXTURE_LABEL}: a pool this module introduced has no recorded allowance \
                     anywhere, and the provider refuses to guess one, so the word is written as \
                     0 — the answer a node gives for a slot that was never set. {sent} consults \
                     its sender's self-allowance while that sender moves it, and {:#x} sends \
                     {sent} on this route",
                    entry.pool
                ),
            });
            for row in rows {
                fx.dump.insert_storage(row.contract, row.key, row.value);
                added_words.push(row);
            }
        }

        // TOKEN_C's side of the holder rows the two-hop fixture already writes for WETH and MID:
        // the run has to be able to ask this token about every account it touches.
        for holder in [fx.knobs.caller, EXECUTOR, RECIPIENT] {
            let row = Row {
                contract: TOKEN_C,
                key: holder_balance_key(TOKEN_C, holder),
                value: U256::ZERO,
                what: format!("balanceOf({holder}) in {TOKEN_C}"),
                reason: format!(
                    "{FIXTURE_LABEL}: an account this fixture introduced cannot have a recorded \
                     balance in a token this fixture introduced, and the provider refuses to \
                     invent one, so the word is written as zero and the run asks the token \
                     instead. This route hands {holder} TOKEN_C only through leg B's delivery"
                ),
            };
            fx.dump.insert_storage(TOKEN_C, row.key, row.value);
            added_words.push(row);
        }
        let row = Row {
            contract: TOKEN_C,
            key: holder_allowance_key(TOKEN_C, EXECUTOR, EXECUTOR),
            value: U256::ZERO,
            what: format!("allowance({EXECUTOR:#x} → {EXECUTOR:#x}) in {TOKEN_C}"),
            reason: format!(
                "{FIXTURE_LABEL}: the self-allowance TOKEN_C's code consults while the executor \
                 pushes leg C's input. Zero is what an address that has never approved itself \
                 holds; M10 declares the same word for WETH and proves by removing it that the \
                 run asks for it"
            ),
        };
        fx.dump.insert_storage(TOKEN_C, row.key, row.value);
        added_words.push(row);

        // The executor's allow-list has to name the two new pools and the new token, or the
        // route is refused before any pool is asked. These are the words a `setPairAllowed` /
        // `setTokenAllowed` call would have written; a fixture run has no configuration
        // transaction, so they are rows.
        for pool in [POOL_BC, POOL_CA] {
            let row = Row {
                contract: EXECUTOR,
                key: mapping_slot(pool, EXECUTOR_PAIR_ALLOWED_SLOT),
                value: allowed_word(true),
                what: "pairAllowed".to_string(),
                reason: format!(
                    "{FIXTURE_LABEL}: the entry `setPairAllowed({pool}, true)` would have \
                     written. The pool is the one this module introduced"
                ),
            };
            fx.dump.insert_storage(EXECUTOR, row.key, row.value);
            added_words.push(row);
        }
        let row = Row {
            contract: EXECUTOR,
            key: mapping_slot(TOKEN_C, EXECUTOR_TOKEN_ALLOWED_SLOT),
            value: allowed_word(true),
            what: "tokenAllowed".to_string(),
            reason: format!(
                "{FIXTURE_LABEL}: the entry `setTokenAllowed({TOKEN_C}, true)` would have \
                 written. The token is this cycle's third asset, replayed from WETH's recorded \
                 code"
            ),
        };
        fx.dump.insert_storage(EXECUTOR, row.key, row.value);
        added_words.push(row);

        for extra in extra {
            let row = Row {
                contract: extra.contract,
                key: extra.key,
                value: extra.value,
                what: extra.what.clone(),
                reason: extra.reason.clone(),
            };
            fx.dump.insert_storage(row.contract, row.key, row.value);
            added_words.push(row);
        }

        fx.source = format!(
            "{FIXTURE_LABEL}: {RECORDED} (real recorded pools, tokens and header) plus {} \
             declared accounts and {} declared storage rows — {} of them M10's two-hop \
             scaffolding (the executor deployment, its configuration words, the wallet's balance \
             and approvals) and {} of them this cycle's declared topology (one third token and \
             two pools replayed from the recorded pair's runtime code, their pair words, their \
             depths as balances, the self-allowances that code consults, and the executor's \
             allow-list entries for them). A row is declared only where it differs from the \
             recording; the recorded market state is unedited, including {POOL_A}, which leg A \
             trades against its own recorded reserves at block {BLOCK} on chain {}",
            fx.accounts.len() + added_accounts.len(),
            fx.additions.len() + added_words.len(),
            fx.additions.len(),
            added_words.len(),
            CHAIN.0,
        );

        let route = CycleRoute::from_dump(&fx.dump, amount_in);
        Self {
            fx,
            route,
            added_words,
            added_accounts,
        }
    }

    /// The cycle's three pools as the fixture's own state describes them: the pair order read
    /// out of each pool's `token0`/`token1` words, the depths read out of the tokens' balance
    /// words.
    pub fn reserves(&self) -> Vec<multihop_market::Reserves> {
        let dump = &self.fx.dump;
        pools_in_order()
            .into_iter()
            .map(|pool| {
                let word = |slot: u64| {
                    dump.storage(pool, plain_slot(slot)).unwrap_or_else(|| {
                        panic!("no pair word recorded for {pool} at slot {slot}")
                    })
                };
                let token0 = address_of_word(word(6));
                let token1 = address_of_word(word(7));
                multihop_market::reserves(
                    pool,
                    token0,
                    token1,
                    dumped_balance(dump, token0, pool),
                    dumped_balance(dump, token1, pool),
                    Some(multihop_market::FEE),
                )
            })
            .collect()
    }

    /// `execute` over these legs, with the caller's own final floor.
    pub fn call(&self, legs: Vec<ExecutorLeg>, min_final_amount: U256) -> ExecutorCall {
        ExecutorCall::Execute {
            legs,
            input_token: WETH,
            amount_in: self.route.amount_in,
            min_final_amount,
            recipient: self.fx.knobs.recipient,
        }
    }

    /// The tight cycle: three asks exact, the final floor at the priced output.
    pub fn standard_call(&self) -> ExecutorCall {
        self.call(self.route.legs(), self.route.weth_out_leg_c)
    }

    /// Every storage word the cycle carries that the recording does not already carry at the
    /// same value — M10's audit form, over the extended dump.
    pub fn difference_from_recorded(&self) -> Vec<(Address, U256, Option<U256>, U256)> {
        self.fx.difference_from_recorded()
    }

    /// The declared pools' depths as the state carries them, for an evidence row.
    pub fn declared_depths(&self) -> Vec<(Address, Address, Address, U256, U256)> {
        let dump = &self.fx.dump;
        declared_pools()
            .into_iter()
            .map(|entry| {
                (
                    entry.pool,
                    entry.token0,
                    entry.token1,
                    dumped_balance(dump, entry.token0, entry.pool),
                    dumped_balance(dump, entry.token1, entry.pool),
                )
            })
            .collect()
    }
}
