//! M11 §37's 3-hop fixture needs a real three-pool cycle, and the only one this
//! repository has ever attested is M9.3's triangle at block 37 224 031. This file goes and
//! records it: pools, tokens, their bytecode, their storage, and the node's own answers to
//! `token0()`, `token1()`, `getReserves()` and `balanceOf()` at that one pinned height,
//! written to `fixtures/simulation-m11/` for `tests/triangle_probe.rs` to measure against.
//!
//! ```text
//! GIWA_RPC_URL=https://sepolia-rpc.giwa.io \
//!   cargo test -p evm-simulation --test multihop_capture -- --ignored --nocapture --test-threads=1
//! ```
//!
//! ## Why this is a recording and not a generation
//!
//! §25 of M10 is still in force: a multi-hop simulation that restates the constant-product
//! formula in Rust has measured the formula, not the market. So every byte of code and
//! every storage word here comes from the node, and nothing here is computed from a
//! reserve. The one thing the node cannot supply is the executor itself — it was deployed
//! at block 38 023 919, a million blocks *after* this pin — and that stays M10's
//! arrangement: the deployment, the operator's funding and the approvals are declared rows
//! in the fixture, labelled `CONTROLLED_FIXTURE`, and this file records only what the chain
//! was.
//!
//! ## Why slots are enumerated rather than driven by a run
//!
//! M4's and M7's dumps were captured by running a simulation against
//! [`evm_simulation::state::RpcStateProvider`] and committing whatever it happened to read.
//! That trick needs the run to exist, and here it cannot: the run this fixture will serve
//! needs bytecode and balances that only the fixture adds. So the recording is made by
//! asking a bounded question — *what is in slots 0x00 to 0x14* — instead of by observing
//! one execution. The bound is not a guess at the layout: it is a superset of every slot
//! the M7 and M10 recordings of pools and tokens of this chain contain, and any word the
//! fixture turns out to need beyond it surfaces as a loud
//! [`evm_simulation::state::ProviderError::Missing`] that names the missing slot, which is
//! then added to the list and re-recorded. Incompleteness here fails loudly by
//! construction, which is what makes the enumeration honest rather than hopeful.
//!
//! ## Where each number comes from
//!
//! ```text
//! chain id, block hash, header   the node, at this height
//! pool and token bytecode        eth_getCode at this height
//! pool scalar slots              eth_getStorageAt, enumerated 0x00..0x14
//! token scalar slots             eth_getStorageAt, enumerated 0x00..0x0a
//! reserves, sides                the pool's own eth_call answers
//! a token's balances slot        found by matching the derived keccak key against
//!                                that same pool's balanceOf() answer, per token
//! balance words                  eth_getStorageAt at the matched key, for every holder
//! ```
//!
//! The slot *identification* is the part a reader is right to question, so it is done twice
//! per token and against two different holders: a `balances` mapping is only accepted as
//! found when the word at `keccak256(pad(holder) ‖ pad(slot))` equals the token's own
//! `balanceOf(holder)` answer for *both* pools that hold it, and the two witnesses are
//! required to disagree about nothing. A wrong slot yields a match rate of zero against a
//! pool that holds the token, which is why a pool holding a real, large balance is the
//! witness rather than an account that holds nothing.
//!
//! ## What this run does not do
//!
//! - it does not broadcast. There is no signing key in this target and the only account
//!   named is an address;
//! - it does not measure a fee. Nothing here asks these three pools what rate they keep; the
//!   closest any run comes is the second instrument of `tests/triangle_probe.rs`, which reads
//!   out of each pool's own `DeliveryMismatch` arguments the payout it made on one specific
//!   trade at one specific size — the fee *kept on a trade*, not a fee *parameter*, because a
//!   fee is a boundary in the pool's behaviour and not a field in its storage;
//! - it does not decide whether these pools are a market. It records what they held at one
//!   historical block, and the notes file says so.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use alloy_primitives::{address, Address, Bytes, U256};

use evm_chain::{ChainAdapter, HttpChainAdapter};
use evm_core::BlockNumber;
use evm_protocol::{CallReturn, V2Call};
use evm_simulation::state::{RecordedHeader, StateDump};

mod executor_state;

use executor_state::{mapping_slot, plain_slot};

/// The chain the M9.3 graph was built on, asserted against the node rather than assumed.
const CHAIN: u64 = 91_342;

/// M9.3's `target_block` — the height its committed cycle table was projected at, so the
/// candidate and this recording are about the same block.
const BLOCK: u64 = 37_224_031;

/// The triangle, row for row from `data/evidence/m9/m9.3/cycles.json`'s two `hop_count: 3`
/// rows: pool 1 holds tokens 1 and 2, pool 2 holds 2 and 3, pool 3 holds 3 and 1.
const POOL_1: Address = address!("0x0e70f2af6bff7c9810ee613814039d95a939e3b5");
const POOL_2: Address = address!("0xe1a9db8507570806ef64ff8d3583787c43f9eff0");
const POOL_3: Address = address!("0x354f408aa2f45fd8d955070e9ef1e47c368bfc42");

const TOKEN_1: Address = address!("0x03a884af9fa7af6557a21496f67e781fc8d00f95");
const TOKEN_2: Address = address!("0xc5bf73bddef871bafb49d12f5d05a5b300422cdf");
const TOKEN_3: Address = address!("0x7c20cb4ab7c6d731c6a167433986ffd17c16ddc6");

/// The deterministic test sender every fixture of this repository funds (§58). Recorded
/// here as the chain knows it — which is as an account with no tokens.
const OPERATOR: Address = address!("0x953e7e98562714c23bc22c7d186cdf516f9dfa6f");

const POOLS: [Address; 3] = [POOL_1, POOL_2, POOL_3];
const TOKENS: [Address; 3] = [TOKEN_1, TOKEN_2, TOKEN_3];

/// Pool scalar slots recorded: `0x00..=0x14`. The recorded M7 and M10 pool states occupy
/// `0x06..=0x0c`; the extra range is the difference between "what one pair happened to
/// touch" and "what a different pair of the same factory could touch".
const POOL_SLOTS: u64 = 0x15;
/// Token scalar slots recorded: `0x00..=0x0a`.
const TOKEN_SLOTS: u64 = 0x0b;
/// Mapping slots probed when identifying a token's `balances` map: `0..=8`.
const MAPPING_PROBE_SLOTS: u64 = 9;

fn rpc_url() -> String {
    std::env::var("GIWA_RPC_URL").expect(
        "GIWA_RPC_URL is required: this target reads a live node, and no endpoint is \
         hardcoded (M5 §44, still in force)",
    )
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn dump_path() -> PathBuf {
    workspace_root().join(format!(
        "fixtures/simulation-m11/dump-{BLOCK}-triangle-03a884af.json"
    ))
}

fn notes_path() -> PathBuf {
    workspace_root().join(format!(
        "fixtures/simulation-m11/capture-{BLOCK}-triangle-notes.json"
    ))
}

/// One pool, as the node answered for it at this height.
#[derive(Clone, Copy, Debug)]
struct Pool {
    address: Address,
    token0: Address,
    token1: Address,
    reserve0: U256,
    reserve1: U256,
    block_timestamp_last: U256,
}

impl Pool {
    fn holds(&self, token: Address) -> bool {
        self.token0 == token || self.token1 == token
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "pool": self.address.to_string(),
            "token0": self.token0.to_string(),
            "token1": self.token1.to_string(),
            "reserve0": self.reserve0.to_string(),
            "reserve1": self.reserve1.to_string(),
            "blockTimestampLast": self.block_timestamp_last.to_string(),
        })
    }
}

/// A token's `balances` mapping slot, found by matching two witnesses rather than by
/// naming a layout.
#[derive(Clone, Copy, Debug)]
struct BalancesSlot {
    token: Address,
    slot: u64,
    /// `(pool, the pool's balanceOf answer, the word at the derived key)` for each witness.
    witnesses: [(Address, U256, U256); 2],
}

impl BalancesSlot {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "token": self.token.to_string(),
            "balances_slot": self.slot,
            "witnesses": self.witnesses.iter().map(|(pool, expected, word)| serde_json::json!({
                "holder": pool.to_string(),
                "balanceOf_answer_from_node": expected.to_string(),
                "word_at_derived_key_from_node": word.to_string(),
                "agree": expected == word,
            })).collect::<Vec<_>>(),
        })
    }
}

/// The reads, in call order, plus the count §44 asks a recording to carry.
#[derive(Default)]
struct Log {
    reads: Vec<String>,
}

impl Log {
    fn note(&mut self, label: String) {
        self.reads.push(label);
    }
}

/// One pinned height, asked the same questions the fixture will ask later — through
/// [`ChainAdapter`], so the request shape is the pipeline's and not a curl.
struct Capture {
    chain: Arc<dyn ChainAdapter>,
    at: BlockNumber,
    log: Log,
    /// Reads that answered the node. A probe that is rejected still cost a request, and
    /// the notes file has to say what the recording actually paid for.
    paid: usize,
}

impl Capture {
    async fn storage(&mut self, address: Address, slot: U256) -> U256 {
        self.paid += 1;
        let value = self
            .chain
            .get_storage_at(self.at, address, slot)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "eth_getStorageAt({address}, {slot}) at block {}: {error}",
                    self.at.0
                )
            });
        self.log
            .note(format!("storage {address} {}", slot_to_decimal(slot)));
        value
    }

    async fn view(&mut self, to: Address, call: &V2Call) -> CallReturn {
        self.paid += 1;
        let raw = self
            .chain
            .call(
                self.at,
                &evm_chain::CallRequest {
                    to,
                    data: call.encode(),
                },
            )
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "eth_call({}, {}) at block {}: {error}",
                    to,
                    call.signature(),
                    self.at.0
                )
            });
        self.log.note(format!("call {to} {}", call.signature()));
        call.decode_return(&raw)
            .unwrap_or_else(|error| panic!("{} answered un-decodably: {error}", call.signature()))
    }

    async fn account(&mut self, address: Address) -> (U256, u64, Bytes) {
        self.paid += 1;
        let balance = self
            .chain
            .get_balance(self.at, address)
            .await
            .unwrap_or_else(|error| panic!("eth_getBalance({address}): {error}"));
        self.paid += 1;
        let nonce = self
            .chain
            .get_nonce(self.at, address)
            .await
            .unwrap_or_else(|error| panic!("eth_getTransactionCount({address}): {error}"));
        self.paid += 1;
        let code = self
            .chain
            .get_code(self.at, address)
            .await
            .unwrap_or_else(|error| panic!("eth_getCode({address}): {error}"));
        self.log.note(format!("account {address}"));
        self.log.note(format!("code {address}"));
        (balance, nonce, code)
    }

    /// The enumerated scalar slots of one address, all of them stored — including the
    /// words the node answered with zero, because a pair legitimately reads a zero
    /// observation index, and a recording that stored only the non-zeros would make the
    /// fixture refuse a word the chain was asked about and answered.
    async fn scalars(&mut self, dump: &mut StateDump, address: Address, count: u64) {
        for slot in 0..count {
            let key = plain_slot(slot);
            let value = self.storage(address, key).await;
            dump.insert_storage(address, key, value);
        }
    }
}

/// A storage slot as the recorded `reads` vocabulary spells it: decimal, the way
/// `RpcStateProvider` writes it, so a dump captured here and a dump captured by a run have
/// one format rather than two.
fn slot_to_decimal(slot: U256) -> String {
    slot.to_string()
}

#[tokio::test]
#[ignore = "reads a live node and writes a fixture; §44's rpc_count is measured by the run \
            that takes it, never quoted from an earlier one"]
async fn capture_the_real_three_hop_cycle_state() {
    let adapter = Arc::new(
        HttpChainAdapter::connect(&rpc_url())
            .await
            .expect("the endpoint answers"),
    );
    let at = BlockNumber(BLOCK);

    // ---- 1. the height, from the node ------------------------------------------------
    let header = adapter
        .get_block_context(at)
        .await
        .expect("the node carries this historical block");
    assert_eq!(
        header.chain_id.0, CHAIN,
        "the endpoint is the chain the cycle is on"
    );
    let block = adapter.get_block(at).await.expect("the block itself");
    assert_eq!(
        block.hash, header.hash,
        "the block and its context are one block"
    );
    let latest = adapter
        .latest_block()
        .await
        .expect("the node answers for its head");
    assert!(
        BLOCK < latest.0,
        "this is a historical recording, so the height has to be behind the head: head is {}",
        latest.0
    );

    let mut capture = Capture {
        chain: adapter.clone() as Arc<dyn ChainAdapter>,
        at,
        log: Log::default(),
        paid: 0,
    };
    capture.log.note(format!("header {}", at.0));

    let mut dump = StateDump {
        chain_id: CHAIN,
        block_number: BLOCK,
        block_hash: format!("{:?}", header.hash),
        header: Some(RecordedHeader::of_block(&header)),
        ..StateDump::default()
    };

    // ---- 2. the three pools ----------------------------------------------------------
    let mut pools = Vec::with_capacity(3);
    for pool in POOLS {
        let (balance, nonce, code) = capture.account(pool).await;
        assert!(
            !code.is_empty(),
            "pool {pool} has no bytecode at block {BLOCK}, so the cycle M9.3 reported is \
             not a pair here"
        );
        let token0 = match capture.view(pool, &V2Call::Token0).await {
            CallReturn::Address(a) => a,
            other => panic!("token0() answered {other:?}"),
        };
        let token1 = match capture.view(pool, &V2Call::Token1).await {
            CallReturn::Address(a) => a,
            other => panic!("token1() answered {other:?}"),
        };
        let reserves = match capture.view(pool, &V2Call::GetReserves).await {
            CallReturn::Reserves(r) => r,
            other => panic!("getReserves() answered {other:?}"),
        };
        assert!(
            !reserves.reserve0.is_zero() && !reserves.reserve1.is_zero(),
            "pool {pool} has an empty side (reserve0 {}, reserve1 {}), so it cannot carry \
             a leg",
            reserves.reserve0,
            reserves.reserve1,
        );
        dump.insert_account(pool, balance, nonce, &code);
        capture.scalars(&mut dump, pool, POOL_SLOTS).await;
        pools.push(Pool {
            address: pool,
            token0,
            token1,
            reserve0: reserves.reserve0,
            reserve1: reserves.reserve1,
            block_timestamp_last: reserves.block_timestamp_last,
        });
    }

    // The topology, checked rather than assumed: three pools, three tokens, each token in
    // exactly two pools, and no pool holding the same token twice.
    for pool in pools.iter() {
        assert_ne!(
            pool.token0, pool.token1,
            "pool {} holds one token twice",
            pool.address
        );
        for token in [pool.token0, pool.token1] {
            assert!(
                TOKENS.contains(&token),
                "pool {} holds {token}, which is not one of the triangle's three tokens",
                pool.address
            );
        }
    }
    for token in TOKENS {
        let holders = pools.iter().filter(|p| p.holds(token)).count();
        assert_eq!(
            holders, 2,
            "{token} is held by {holders} of the three pools, so this is not a triangle"
        );
    }

    // ---- 3. the three tokens ---------------------------------------------------------
    let mut slots = Vec::with_capacity(3);
    for token in TOKENS {
        let (balance, nonce, code) = capture.account(token).await;
        assert!(
            !code.is_empty(),
            "token {token} has no bytecode at block {BLOCK}"
        );
        dump.insert_account(token, balance, nonce, &code);
        capture.scalars(&mut dump, token, TOKEN_SLOTS).await;

        // Two witnesses, each an eth_call the node answers plus the enumerated probe. The
        // balance is required to be non-zero, because a zero answer would match every
        // candidate slot and so prove nothing.
        let holders: Vec<&Pool> = pools.iter().filter(|p| p.holds(token)).collect();
        let mut found: Option<BalancesSlot> = None;
        for slot in 0..MAPPING_PROBE_SLOTS {
            let mut witness_rows = Vec::with_capacity(2);
            let mut agree = true;
            for holder in holders.iter() {
                let expected = match capture
                    .view(
                        token,
                        &V2Call::BalanceOf {
                            owner: holder.address,
                        },
                    )
                    .await
                {
                    CallReturn::Amount(v) => v,
                    other => panic!("balanceOf() answered {other:?}"),
                };
                let word = capture
                    .storage(token, mapping_slot(holder.address, slot))
                    .await;
                witness_rows.push((holder.address, expected, word));
                if expected.is_zero() || expected != word {
                    agree = false;
                }
            }
            if agree {
                assert!(
                    found.is_none(),
                    "token {token}'s balances mapping matched slot {slot} and also slot {}: \
                     two witnesses cannot tell them apart, so the recording must not guess",
                    found.unwrap().slot
                );
                found = Some(BalancesSlot {
                    token,
                    slot,
                    witnesses: [witness_rows[0], witness_rows[1]],
                });
            }
        }
        let slot = found.unwrap_or_else(|| {
            panic!(
                "no balances slot of 0..{MAPPING_PROBE_SLOTS} resolves token {token} against \
                 its two pools' own balanceOf() answers at block {BLOCK}; widen the probe \
                 range rather than assume a layout"
            )
        });
        slots.push(slot);

        // Store the balance words for every holder the fixture will ask about: the two
        // pools (whose balance *is* their reserve) and the sender, whose recorded balance
        // is what the fixture's declared funding row replaces.
        for holder in pools.iter().filter(|p| p.holds(token)).map(|p| p.address) {
            let word = slot
                .witnesses
                .iter()
                .find(|(a, _, _)| *a == holder)
                .expect("witness")
                .2;
            dump.insert_storage(token, mapping_slot(holder, slot.slot), word);
        }
        let operator_key = mapping_slot(OPERATOR, slot.slot);
        let operator_word = capture.storage(token, operator_key).await;
        dump.insert_storage(token, operator_key, operator_word);
    }

    // The reserve/balance identity, as a claim about the node rather than about this code:
    // for each pool and each of its tokens, the pool's own `getReserves()` word and the
    // token's own `balanceOf(pool)` word must be the same number in the same order.
    let mut identity_checks = Vec::new();
    for pool in pools.iter() {
        let reserve0 = pool.reserve0;
        let reserve1 = pool.reserve1;
        let balance0 = dump
            .storage(
                pool.token0,
                mapping_slot(pool.address, slot_of(&slots, pool.token0)),
            )
            .expect("recorded");
        let balance1 = dump
            .storage(
                pool.token1,
                mapping_slot(pool.address, slot_of(&slots, pool.token1)),
            )
            .expect("recorded");
        identity_checks.push(serde_json::json!({
            "pool": pool.address.to_string(),
            "token0_reserve": reserve0.to_string(),
            "token0_balance_of_pool": balance0.to_string(),
            "token1_reserve": reserve1.to_string(),
            "token1_balance_of_pool": balance1.to_string(),
            "agree": reserve0 == balance0 && reserve1 == balance1,
        }));
        assert_eq!(
            reserve0, balance0,
            "pool {} reports reserve0 {reserve0} but token0 holds {balance0}",
            pool.address
        );
        assert_eq!(
            reserve1, balance1,
            "pool {} reports reserve1 {reserve1} but token1 holds {balance1}",
            pool.address
        );
    }

    // ---- 4. the sender, as the chain knows it ----------------------------------------
    let (balance, nonce, code) = capture.account(OPERATOR).await;
    assert!(
        code.is_empty(),
        "{OPERATOR} has bytecode, so it is not the plain test sender the fixtures fund"
    );
    dump.insert_account(OPERATOR, balance, nonce, &code);

    // ---- 5. write -------------------------------------------------------------------
    dump.reads = std::mem::take(&mut capture.log.reads);
    let dump_path = dump_path();
    dump.write_file(&dump_path).expect("the dump writes");

    let notes = serde_json::json!({
        "_provenance": "crates/simulation/tests/multihop_capture.rs — recorded live from \
                        GIWA_RPC_URL at one pinned historical block; nothing here is \
                        generated, and the executor deployment lives in the fixture that \
                        reads this file, not here",
        "chain_id": CHAIN,
        "block_number": BLOCK,
        "block_hash": format!("{:?}", header.hash),
        "header": {
            "timestamp": header.timestamp,
            "gas_limit": header.gas_limit,
            "base_fee_per_gas": header.base_fee_per_gas.map(|v| v.to_string()),
            "beneficiary": header.beneficiary.to_string(),
        },
        "head_at_capture": latest.0,
        "cycle": {
            "source": "data/evidence/m9/m9.3/cycles.json, the two hop_count: 3 rows",
            "canonical_key": POOLS.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
            "start_token": TOKEN_1.to_string(),
        },
        "pools": pools.iter().map(Pool::json).collect::<Vec<_>>(),
        "balances_slots": slots.iter().map(BalancesSlot::json).collect::<Vec<_>>(),
        "reserve_equals_balance": identity_checks,
        "enumerated_slots": { "pool_range": [0, POOL_SLOTS - 1], "token_range": [0, TOKEN_SLOTS - 1] },
        "rpc_count": dump.reads.len(),
        "rpc_paid": capture.paid,
        "dump": dump_path.strip_prefix(workspace_root()).unwrap_or(&dump_path).display().to_string(),
        "not_measured": [
            "the three pools' fee *configuration* — `tests/triangle_probe.rs` measures the fee \
             each pool keeps on one specific trade at one specific size, which is a different \
             quantity from a parameter the contract stores",
            "whether any direction of this cycle is profitable at this height — no run of this \
             repository trades these three pools, and §41 keeps `REAL_PROFITABLE_ARBITRAGE` at \
             `UNKNOWN`",
            "the executor deployment, the operator's funding and the approvals — declared \
             rows in the fixture, CONTROLLED_FIXTURE",
        ],
    });
    let notes_path = notes_path();
    std::fs::write(
        &notes_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&notes).expect("notes serialize")
        ),
    )
    .expect("the notes write");

    println!(
        "wrote {} ({} accounts, {} storage words, {} reads; {} requests paid)",
        dump_path.display(),
        dump.accounts.len(),
        dump.storage.values().map(|w| w.len()).sum::<usize>(),
        dump.reads.len(),
        capture.paid,
    );
    println!(
        "wrote {} — block {}, head at capture {}",
        notes_path.display(),
        header.hash,
        latest.0
    );
    for pool in pools.iter() {
        println!(
            "  pool {} : {} / {} reserves {} / {} last-sync {}",
            pool.address,
            pool.token0,
            pool.token1,
            pool.reserve0,
            pool.reserve1,
            pool.block_timestamp_last,
        );
    }
    for slot in slots.iter() {
        println!(
            "  token {} balances mapping is slot {} (witnessed by {} and {})",
            slot.token, slot.slot, slot.witnesses[0].0, slot.witnesses[1].0
        );
    }
}

/// The slot a token's `balances` map was found at, by token. A lookup by address, not a
/// positional index: a recording that reordered its own tokens must not silently reorder
/// the keys it stored.
fn slot_of(slots: &[BalancesSlot], token: Address) -> u64 {
    slots
        .iter()
        .find(|s| s.token == token)
        .unwrap_or_else(|| panic!("no balances slot was recorded for {token}"))
        .slot
}
