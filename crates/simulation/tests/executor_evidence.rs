//! M10 §60/§48: the simulation half of the evidence directory.
//!
//! Every row here is a *rebuild recipe* before it is a result. §48 asks for
//! `fixture → rebuild → simulate → compare`, and a reader cannot rebuild from a screenshot of a
//! number, so each scenario publishes the two things a run needs and nothing else it happened to
//! observe:
//!
//! * **state** — the committed fixture file, its hash, and the difference this scenario's variant
//!   makes to it, expressed as the [`StateOverride`] rows the provider itself applies. The
//!   reason string on each row is the fixture's own declaration, copied from
//!   `fixtures/simulation-m10/fixture-37530593-executor-additions.json` rather than rewritten
//!   here, so an override that no longer means what the fixture says it means is visible.
//! * **run spec** — chain, pinned block, state source string, executor, operator, the decoded
//!   call with every leg and floor, the gas allowance, the pricing rule and the endowment, plus
//!   the calldata bytes and their hash.
//!
//! With those, a second, independent program can run the same call. That program is
//! `crates/execution/tests/executor_evidence_gate.rs`: it lives in another crate, reads only
//! these files, re-runs REVM through the library API (never this file's harness), and compares
//! its answer against the `observed` object published here. A row that cannot be reproduced is a
//! row that was not evidence.
//!
//! ## What is *not* claimed here
//!
//! Every run on this page is a `CONTROLLED_FIXTURE` run (§29): the pools and reserves are the
//! recording's, the deployment, the funding and the allowlists are declared rows. A successful
//! row is proof that the contract's invariants hold against real bytecode on real state — it is
//! not proof that the market pays that amount, and §29/§52 forbid reading it that way. The real
//! chain's answer is in `real/`, produced by `crates/execution/tests/executor_giwa_live.rs`.
//!
//! One row per scenario, and the same row object is written verbatim into every file that names
//! the scenario. `fixtures/slippage.json` and `negative_controls/min_output.json` therefore
//! carry byte-identical rows for the same run, and the gate checks that they do — a file that
//! quietly grew its own copy of a number is how an evidence tree rots.
//!
//! ```text
//! cargo test -p evm-simulation --test executor_evidence -- --test-threads=1
//! ```
//!
//! Runs serially by design: it writes the shared evidence directory, and the workspace's tests
//! are serial for that reason.

use std::collections::BTreeMap;
use std::path::Path;

use alloy_primitives::{keccak256, Address, Bytes, U256};
use serde_json::{json, Map, Value};

use evm_protocol::{ExecutorCall, ExecutorLeg};
use evm_simulation::executor::ExecutorOutcome;
use evm_simulation::state::{StateDump, StateOverride};

mod executor_state;
use executor_state::{
    amount_in, workspace_root, Fixture, Knobs, Route, BLOCK, BYSTANDER, CHAIN, ENDOWMENT, EXECUTOR,
    FIXTURE, FIXTURE_LABEL, GAS_LIMIT, MID, OPERATOR, POOL_A, POOL_B, RECIPIENT, WETH,
};

// ---------------------------------------------------------------------------
// The directory
// ---------------------------------------------------------------------------

/// §60's tree, relative to the workspace root. Every path below is one of its files; the prefix is
/// written out in full on each constant so a reader can grep a published path and find the writer.
const EVIDENCE: &str = "data/evidence/m10";

const SUCCESS: &str = "data/evidence/m10/fixtures/success.json";
const REVERT: &str = "data/evidence/m10/fixtures/revert.json";
const SLIPPAGE: &str = "data/evidence/m10/fixtures/slippage.json";
const PROFIT_GUARD: &str = "data/evidence/m10/fixtures/profit_guard.json";
const SIM_SUCCESS: &str = "data/evidence/m10/simulation/success.json";
const SIM_FAILURE: &str = "data/evidence/m10/simulation/failure.json";
const NC_WRONG_OPERATOR: &str = "data/evidence/m10/negative_controls/wrong_operator.json";
const NC_BROKEN_ROUTE: &str = "data/evidence/m10/negative_controls/broken_route.json";
const NC_ZERO_AMOUNT: &str = "data/evidence/m10/negative_controls/zero_amount.json";
const NC_MIN_OUTPUT: &str = "data/evidence/m10/negative_controls/min_output.json";
const NC_FINAL_PROFIT: &str = "data/evidence/m10/negative_controls/final_profit.json";
const NC_INVALID_PAIR: &str = "data/evidence/m10/negative_controls/invalid_pair.json";
const NC_TOKEN_NOT_ALLOWED: &str = "data/evidence/m10/negative_controls/token_not_allowed.json";
const NC_FORCED_LEG2: &str = "data/evidence/m10/negative_controls/forced_second_leg_revert.json";

/// The one file format id in this directory, so a reader knows which keys are guaranteed.
const SCHEMA: &str = "m10-evidence-v1";

// ---------------------------------------------------------------------------
// Printing the numbers
// ---------------------------------------------------------------------------

fn addr(address: Address) -> String {
    format!("0x{}", hex::encode(address.as_slice()))
}

fn dec(value: U256) -> String {
    value.to_string()
}

/// A storage slot as a 32-byte word. Amounts in this directory are always decimal (`dec`), and
/// slots are always hex — one shape per kind of number, so a reader never has to guess which
/// base a 66-character string is in.
fn slot(value: U256) -> String {
    format!("0x{}", hex::encode(value.to_be_bytes::<32>()))
}

/// How the recording answers for an account, or `null` when it has never heard of it. The `null`
/// is the point of the field: it is the reason a declared account row exists at all, and a reader
/// can check it against the recording instead of taking the row's word for it.
fn recorded_account(recorded: &StateDump, address: Address) -> Value {
    match recorded.account(address) {
        Some(account) => json!({
            "balance": account.balance,
            "nonce": account.nonce,
            "code": account.code,
        }),
        None => Value::Null,
    }
}

fn bytes_hex(data: &[u8]) -> String {
    format!("0x{}", hex::encode(data))
}

fn keccak_of_bytes(data: &[u8]) -> String {
    format!("{:#x}", keccak256(data))
}

/// A leg, in the same field order the contract's `Leg` struct and the plan both use.
fn leg_json(leg: &ExecutorLeg) -> Value {
    json!({
        "pool": addr(leg.pool),
        "token_in": addr(leg.token_in),
        "token_out": addr(leg.token_out),
        "amount_in": dec(leg.amount_in),
        "amount_out": dec(leg.amount_out),
        "min_amount_out": dec(leg.min_amount_out),
    })
}

/// The decoded call: what the bytes mean, alongside the bytes themselves.
fn call_json(call: &ExecutorCall) -> Value {
    match call {
        ExecutorCall::Execute {
            legs,
            input_token,
            amount_in,
            min_final_amount,
            recipient,
        } => json!({
            "signature": call.signature(),
            "selector": format!("0x{}", hex::encode(call.selector())),
            "kind": "execute",
            "legs": legs.iter().map(leg_json).collect::<Vec<_>>(),
            "input_token": addr(*input_token),
            "amount_in": dec(*amount_in),
            "min_final_amount": dec(*min_final_amount),
            "recipient": addr(*recipient),
        }),
        ExecutorCall::Withdraw { token, to, amount } => json!({
            "signature": call.signature(),
            "selector": format!("0x{}", hex::encode(call.selector())),
            "kind": "withdraw",
            "legs": [],
            "input_token": addr(*token),
            "amount_in": dec(*amount),
            "min_final_amount": dec(*amount),
            "recipient": addr(*to),
        }),
        other => panic!("this evidence publishes route runs; {other:?} is not one"),
    }
}

/// The request, in exactly the fields `ExecutorRun` holds. The gate builds its own `ExecutorRun`
/// out of this object and nothing else.
fn run_spec_json(fx: &Fixture, call: &ExecutorCall, gas_limit: u64) -> Value {
    let calldata = call.encode();
    json!({
        "chain_id": CHAIN.0,
        "priced_at_block": BLOCK,
        "state_source": fx.source,
        "executor": addr(EXECUTOR),
        "operator": addr(fx.knobs.caller),
        "gas_limit": gas_limit,
        "rules": "Prague",
        "pricing": {
            "kind": "eip1559",
            "priority_fee_per_gas": 0,
            "provenance": pricing_provenance(),
        },
        "endowment_wei": dec(ENDOWMENT),
        "call": call_json(call),
        "calldata": bytes_hex(&calldata),
        "calldata_len": calldata.len(),
        "calldata_keccak256": keccak_of_bytes(&calldata),
        "units": {
            "amounts": "decimal wei words of the token the call names",
            "addresses": "0x-prefixed 20-byte hex",
            "slots": "0x-prefixed 32-byte hex words",
        },
    })
}

/// The provenance sentence the fixture's own spec carries, in one place so the published string
/// and the run's string cannot drift apart.
fn pricing_provenance() -> String {
    format!(
        "block {BLOCK}'s own base fee as the recorded header reports it, with no tip: a \
         hypothetical transaction on a historical block is not competing to be included in it"
    )
}

// ---------------------------------------------------------------------------
// State: the recipe that rebuilds the state a scenario ran on
// ---------------------------------------------------------------------------

/// The state a scenario ran on, published as **`recording` + every row that differs from it**, in
/// the exact shape [`StateOverride`] carries — which is the mechanism
/// [`evm_simulation::state::DumpStateProvider`] applies, so the published file is not a
/// description of the rebuild but the rebuild itself.
///
/// The rows come from [`Fixture::difference_from_recorded`], a comparison of the two dumps, and
/// not from the builder's own list. A knob that wrote somewhere unexpected therefore shows up as a
/// row with no declared `what` attached, and this function panics on that rather than publishing a
/// state edit nobody named.
///
/// The base is the M7 recording, not the M10 fixture file, because the recipe has to be complete:
/// a difference measured against the committed fixture would leave out the rows the committed
/// fixture itself declares, and a reader could not rebuild a variant from it at all.
fn recipe_rows(fx: &Fixture) -> Vec<(StateOverride, Value)> {
    let mut out: Vec<(StateOverride, Value)> = Vec::new();
    let recorded = executor_state::recorded_dump();

    let introduced = fx.introduced_accounts();
    for address in &introduced {
        let declared = fx
            .accounts
            .iter()
            .find(|account| account.address == *address)
            .unwrap_or_else(|| {
                panic!(
                    "{} is an account this fixture's dump carries differently from the \
                     recording, and no declared account row explains it",
                    addr(*address)
                )
            });
        out.push((
            StateOverride {
                address: declared.address,
                balance: Some(declared.balance),
                nonce: Some(declared.nonce),
                code: Some(code_of(&declared.code)),
                slots: Vec::new(),
                reason: declared.reason.clone(),
            },
            json!({
                "kind": "account",
                "address": addr(declared.address),
                "in_the_recording": recorded_account(&recorded, declared.address),
                "balance": dec(declared.balance),
                "nonce": declared.nonce,
                "code": declared.code,
                "code_bytes": code_of(&declared.code).len(),
                "what": declared.what,
                "reason": declared.reason,
            }),
        ));
    }
    assert_eq!(
        introduced.len(),
        fx.accounts.len(),
        "the fixture declares {} account rows and the comparison finds {} account(s) that \
         differ from the recording — one of the two is not telling the truth",
        fx.accounts.len(),
        introduced.len()
    );

    for (contract, key, recorded_value, value) in fx.difference_from_recorded() {
        let declared = fx
            .additions
            .iter()
            .find(|word| word.contract == contract && word.key == key)
            .unwrap_or_else(|| {
                panic!(
                    "{} {} differs from the recording in this fixture, and no declared row \
                     explains it",
                    addr(contract),
                    slot(key)
                )
            });
        out.push((
            StateOverride {
                address: contract,
                balance: None,
                nonce: None,
                code: None,
                slots: vec![(key, value)],
                reason: declared.reason.clone(),
            },
            json!({
                "kind": "word",
                "contract": addr(contract),
                "slot": slot(key),
                "value": dec(value),
                "value_hex": format!("{value:#x}"),
                "in_the_recording": recorded_value.map(dec),
                "what": declared.what,
                "reason": declared.reason,
            }),
        ));
    }
    assert_eq!(
        fx.difference_from_recorded().len(),
        fx.additions.len(),
        "the fixture declares {} storage row(s) and the comparison finds {} word(s) that differ \
         from the recording",
        fx.additions.len(),
        fx.difference_from_recorded().len()
    );
    out
}

/// The recipe file for one distinct state, so scenarios that share a fixture share one file and
/// there is exactly one copy of every number in it.
fn recipe_json(fx: &Fixture, name: &str, rows: &[(StateOverride, Value)]) -> Value {
    let recorded = std::fs::read(workspace_root().join(executor_state::RECORDED))
        .expect("the recording is committed");
    let values: Vec<Value> = rows.iter().map(|(_, value)| value.clone()).collect();
    let accounts = values
        .iter()
        .filter(|row| row["kind"].as_str() == Some("account"))
        .count();
    let words = values
        .iter()
        .filter(|row| row["kind"].as_str() == Some("word"))
        .count();
    json!({
        "schema": SCHEMA,
        "milestone": "M10",
        "kind": "state_recipe",
        "name": name,
        "assembled_by": "crates/simulation/tests/executor_evidence.rs",
        "rebuilds": format!("{} + the rows below", executor_state::RECORDED),
        "market": FIXTURE_LABEL,
        "state_source": fx.source,
        "recording": {
            "file": executor_state::RECORDED,
            "bytes": recorded.len(),
            "keccak256": keccak_of_bytes(&recorded),
        },
        "committed_fixture": {
            "file": FIXTURE,
            "note": "the standard knobs' dump, already on disk. A recipe whose rows are the \
                     committed fixture's rows rebuilds this file's state; the gate checks that \
                     for the scenarios that run the committed fixture"
        },
        "row_count": {
            "accounts": accounts,
            "words": words,
            "total": values.len(),
        },
        "number_shapes": {
            "amounts": "decimal wei, plus a `value_hex` mirror for a word",
            "slots": "0x-prefixed 32-byte hex",
            "addresses": "0x-prefixed 20-byte hex",
            "code": "0x-prefixed hex bytecode, as the dump prints it"
        },
        "rows": values,
        "how_to_apply": "StateDump::from_file(recording), then \
                         DumpStateProvider::new(dump, state_source).with_overrides(rows) — one \
                         StateOverride per row, with `slots` from the word rows and \
                         `balance`/`nonce`/`code` from the account rows",
    })
}

fn code_of(text: &str) -> Bytes {
    let trimmed = text.trim();
    if trimmed == "0x" || trimmed.is_empty() {
        return Bytes::new();
    }
    Bytes::from(hex::decode(trimmed.trim_start_matches("0x")).expect("dump code is hex"))
}

// ---------------------------------------------------------------------------
// Observed
// ---------------------------------------------------------------------------

/// The run's whole answer, in the library's own serialized shape.
///
/// The gate compares this object with the one it produces from the published spec, field for
/// field. Publishing the entire outcome rather than a curated summary is the point: a curated
/// summary can leave out the field that disagrees.
fn observed(outcome: &ExecutorOutcome) -> Value {
    serde_json::to_value(outcome).expect("an outcome serializes")
}

/// What a scenario is supposed to end like, spelled as data so the gate can re-judge the
/// published `observed` object from these keys alone.
fn expects(
    outcome: &str,
    contract_error: Option<&str>,
    revert_kind: Option<&str>,
    delivered: &str,
    logs: Value,
    market_moved: bool,
    residue: &str,
) -> Value {
    json!({
        "outcome": outcome,
        "contract_error": contract_error,
        "revert_kind": revert_kind,
        "delivered": delivered,
        "logs": logs,
        "market_moved": market_moved,
        "residue": residue,
        "delivered_keys": {
            "route_weth_out_leg2": "the second leg's exact ask, as the fixture's own quote prices it",
            "none": "a call that never completed returns nothing",
            "any": "no claim is made about the returned number"
        },
        "residue_keys": {
            "none": "no reserve, no balance and no storage word is left different (§27)",
            "any": "the run traded, so the market moved — the amount is published, not judged"
        }
    })
}

/// The writer's own check that a run ended where the scenario claims. Kept small and typed: the
/// deep per-scenario assertions live in `executor_revm.rs`, which is where they can be read
/// against the contract's source. This one exists so no row can be published under an expectation
/// it does not meet.
fn judge(outcome: &ExecutorOutcome, expected: &Value, route: &Route) -> Result<(), String> {
    let wanted = expected["outcome"].as_str().expect("an expected outcome");
    if expected["residue"].as_str() == Some("none") {
        if outcome.market_moved() {
            return Err("a no-residue scenario moved a reserve or a balance".to_string());
        }
        for watched in [POOL_A, POOL_B, WETH, MID, EXECUTOR] {
            if !outcome.changed_slots_in(watched).is_empty() {
                return Err(format!(
                    "a no-residue scenario left {} changed word(s) at {watched}",
                    outcome.changed_slots_in(watched).len()
                ));
            }
        }
    }
    match wanted {
        "success" => {
            if !outcome.succeeded() {
                return Err(format!(
                    "expected success, the run ended: {}",
                    outcome.describe()
                ));
            }
        }
        "reverted" => {
            if outcome.revert().is_none() {
                return Err(format!(
                    "expected a revert, the run ended: {}",
                    outcome.describe()
                ));
            }
        }
        other => panic!("unknown expected outcome {other}"),
    }
    let error = expected["contract_error"].as_str();
    match (error, &outcome.contract_error) {
        (None, None) => {}
        (None, Some(name)) => return Err(format!("expected no executor error, got {name}")),
        (Some(wanted_name), Some(name)) if *name == *wanted_name => {}
        (Some(wanted_name), Some(name)) => {
            return Err(format!("expected {wanted_name}, the contract said {name}"))
        }
        (Some(wanted_name), None) => {
            return Err(format!(
                "expected the executor's {wanted_name}, but the revert was not the \
                 contract's: {:?}",
                outcome.revert_kind
            ))
        }
    }
    if let Some(kind) = expected["revert_kind"].as_str() {
        let got = outcome
            .revert_kind
            .ok_or_else(|| "expected a classified revert payload".to_string())?;
        if got != kind {
            return Err(format!("expected revert payload kind {kind}, found {got}"));
        }
    }
    match expected["delivered"].as_str() {
        Some("none") => {
            if let Some(claim) = outcome.delivered {
                return Err(format!("expected no return value, got {claim}"));
            }
        }
        Some("route_weth_out_leg2") => {
            let claim = outcome
                .delivered
                .ok_or_else(|| "a successful execute returns a number".to_string())?;
            if claim != route.weth_out_leg2 {
                return Err(format!(
                    "the contract delivered {claim} where the fixture's quote prices {}",
                    route.weth_out_leg2
                ));
            }
        }
        _ => {}
    }
    if let Some(count) = expected["logs"].as_u64() {
        if outcome.logs.len() as u64 != count {
            return Err(format!(
                "expected {count} log(s), the run emitted {}",
                outcome.logs.len()
            ));
        }
    }
    let moved = expected["market_moved"]
        .as_bool()
        .expect("a market_moved claim");
    if outcome.market_moved() != moved {
        return Err(format!(
            "expected market_moved={moved}, the run reports {}",
            outcome.market_moved()
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The scenarios
// ---------------------------------------------------------------------------

/// One published scenario.
struct Case {
    /// The semantic key: a name a reader can quote, never an index into a table.
    scenario: &'static str,
    spec_ref: &'static str,
    what_this_is: &'static str,
    /// How the control was planted, in one sentence. `null` in the JSON for the success row.
    planted: Option<Value>,
    knobs: fn() -> Knobs,
    call: fn(&Fixture) -> ExecutorCall,
    expected: Value,
    files: &'static [&'static str],
}

impl Case {
    fn is_committed(&self) -> bool {
        let knobs = (self.knobs)();
        knobs.caller == OPERATOR
            && knobs.deployed_operator == OPERATOR
            && knobs.recipient == RECIPIENT
            && knobs.amount_in == amount_in()
            && knobs.balance == amount_in()
            && knobs.allowance == amount_in()
            && knobs.dust.is_zero()
            && knobs.pair_allowed
            && knobs.token_allowed
            && knobs.deploy_executor
    }
}

fn standard() -> Knobs {
    Knobs::standard()
}

/// The ten §50 controls that live below the execution layer, plus §28's success fixture.
///
/// `wrong chain` and `wrong executor` are not here: both are refused by the *plan*, before any
/// state is read, so they are written and recomputed in the execution crate's gate, where the
/// refusal actually happens.
fn cases() -> Vec<Case> {
    vec![
        Case {
            scenario: "success_round_trip",
            spec_ref: "§28/§12/§17/§21",
            what_this_is: "the route the recording prices, run at its exact asks: both legs \
                           deliver what the plan claims, the recipient is paid, and the \
                           deployment holds nothing afterwards",
            planted: None,
            knobs: standard,
            call: |fx| fx.standard_call(),
            expected: expects(
                "success",
                None,
                None,
                "route_weth_out_leg2",
                json!(null),
                true,
                "any",
            ),
            files: &[SUCCESS, SIM_SUCCESS],
        },
        Case {
            scenario: "forced_second_leg_revert",
            spec_ref: "§27/§13 (NC10)",
            what_this_is: "leg 2 asks pool B for one wei more than the invariant can pay. Pool A \
                           has already traded when the second pool refuses in its own words, and \
                           the whole transaction is discarded — the finding is the empty residue \
                           list, not the revert",
            planted: Some(json!({
                "moved": "legs[1].amount_out and legs[1].amount_in and legs[0]'s carried input",
                "by": "+1 wei over pool B's exact ask",
                "why_this_fails": "a V2 pair pays exactly what it is asked to pay, then checks \
                                   its own constant product; one wei over the line is the pair's \
                                   assert, not the executor's"
            })),
            knobs: standard,
            call: |fx| fx.execute(fx.route.legs_moved(1, 1), fx.route.weth_out_leg2),
            expected: expects(
                "reverted",
                None,
                Some("Error(string)"),
                "none",
                json!(0),
                false,
                "none",
            ),
            files: &[REVERT, SIM_FAILURE, NC_FORCED_LEG2],
        },
        Case {
            scenario: "final_floor_above_the_priced_output",
            spec_ref: "§12/§14 (NC8)",
            what_this_is: "the route is exactly what the market prices and the plan simply \
                           demands one wei more at the end than exists. The floor is checked \
                           against the recipient's balance, which is why this is \
                           `FinalShortfall` after both pools have paid rather than a refusal \
                           before they do",
            planted: Some(json!({
                "moved": "min_final_amount",
                "by": "+1 wei over the priced output of leg 1",
                "legs": "unchanged, at their exact asks"
            })),
            knobs: standard,
            call: |fx| fx.execute(fx.route.legs(), fx.route.weth_out_leg2 + U256::ONE),
            expected: expects(
                "reverted",
                Some("FinalShortfall"),
                None,
                "none",
                json!(0),
                false,
                "none",
            ),
            files: &[SLIPPAGE, NC_MIN_OUTPUT],
        },
        Case {
            scenario: "leg_asking_below_its_own_floor",
            spec_ref: "§12/§14 (NC9)",
            what_this_is: "a plan that contradicts itself: leg 1's ask is below its own floor. \
                           `_checkLeg` fires before anything moves, which is a different finding \
                           from the shortfall above and gets a different error name",
            planted: Some(json!({
                "moved": "legs[1].amount_out",
                "by": "-1 wei below legs[1].min_amount_out",
                "min_final_amount": "unchanged, at the priced output"
            })),
            knobs: standard,
            call: |fx| {
                let mut legs = fx.route.legs();
                legs[1].amount_out = legs[1].min_amount_out - U256::ONE;
                fx.execute(legs, fx.route.weth_out_leg2)
            },
            expected: expects(
                "reverted",
                Some("AskBelowFloor"),
                None,
                "none",
                json!(0),
                false,
                "none",
            ),
            files: &[PROFIT_GUARD, NC_FINAL_PROFIT],
        },
        Case {
            scenario: "caller_is_not_the_stored_operator",
            spec_ref: "§20 (NC3)",
            what_this_is: "a wallet that holds the input and has approved it, and is not the \
                           operator the deployment stores at slot 0",
            planted: Some(json!({
                "knob": "caller",
                "from": addr(OPERATOR),
                "to": addr(BYSTANDER),
                "deployed_operator": "unchanged — the contract's own `operator` word still reads the fixture operator"
            })),
            knobs: || Knobs {
                caller: BYSTANDER,
                ..Knobs::standard()
            },
            call: |fx| fx.standard_call(),
            expected: expects(
                "reverted",
                Some("NotOperator"),
                None,
                "none",
                json!(0),
                false,
                "none",
            ),
            files: &[NC_WRONG_OPERATOR],
        },
        Case {
            scenario: "token_continuity_broken",
            spec_ref: "§16 (NC4)",
            what_this_is: "leg 1 pays out a token leg 2 does not take in: the route is not a \
                           route. The contract's own chain check names the index and both \
                           addresses rather than failing on a balance",
            planted: Some(json!({
                "moved": "legs[1].token_in",
                "from": "the mid token leg 0 pays out",
                "to": "WETH, which leg 0 never handed it",
                "note": "leg 1's amount_in still equals leg 0's amount_out, so this is a token \
                         failure and not an amount failure — §16 and §23 are two checks and the \
                         evidence keeps them two names"
            })),
            knobs: standard,
            call: |fx| {
                let mut legs = fx.route.legs();
                legs[1].token_in = WETH;
                legs[1].token_out = MID;
                fx.execute(legs, fx.route.weth_out_leg2)
            },
            expected: expects(
                "reverted",
                Some("BrokenContinuity"),
                None,
                "none",
                json!(0),
                false,
                "none",
            ),
            files: &[NC_BROKEN_ROUTE],
        },
        Case {
            scenario: "zero_amount_in_a_leg",
            spec_ref: "§10 (NC12)",
            what_this_is: "a leg that moves nothing. A zero-input swap is the cheapest way for a \
                           plan to look like a route while settling nothing, so the ask itself is \
                           refused before the pool is asked",
            planted: Some(json!({
                "moved": "the call's min_final_amount (the route's final floor)",
                "from": "100000000000000-ish: the priced output of leg 1",
                "to": "0",
                "legs": "unchanged, at their exact asks",
                "why": "§10 refuses a leg that asks to move nothing, and §12 refuses a floor of \
                        zero — a route with no claim at the end is an unhedged position with a \
                        calldata on it"
            })),
            knobs: standard,
            call: |fx| fx.execute(fx.route.legs(), U256::ZERO),
            expected: expects(
                "reverted",
                Some("ZeroAmount"),
                None,
                "none",
                json!(0),
                false,
                "none",
            ),
            files: &[NC_ZERO_AMOUNT],
        },
        Case {
            scenario: "pair_not_in_the_allowlist",
            spec_ref: "§43",
            what_this_is: "the deployment's `pairAllowed` word for pool A reads false: the \
                           operator configured this contract not to trade that pool. The refusal \
                           is the contract's own, read from its storage, and nothing moves",
            planted: Some(json!({
                "knob": "pair_allowed",
                "from": true,
                "to": false,
                "effect_on_state": "pairAllowed[POOL_A] and pairAllowed[POOL_B] both read 0"
            })),
            knobs: || Knobs {
                pair_allowed: false,
                ..Knobs::standard()
            },
            call: |fx| fx.standard_call(),
            expected: expects(
                "reverted",
                Some("PairNotAllowed"),
                None,
                "none",
                json!(0),
                false,
                "none",
            ),
            files: &[NC_INVALID_PAIR],
        },
        Case {
            scenario: "token_not_in_the_allowlist",
            spec_ref: "§44",
            what_this_is: "the mid token is not on the allowlist. A pool can be allowed and its \
                           pair token refused, and the contract checks the token list per leg, so \
                           this is a different refusal from the one above",
            planted: Some(json!({
                "knob": "token_allowed",
                "from": true,
                "to": false,
                "effect_on_state": "tokenAllowed[WETH] and tokenAllowed[MID] both read 0"
            })),
            knobs: || Knobs {
                token_allowed: false,
                ..Knobs::standard()
            },
            call: |fx| fx.standard_call(),
            expected: expects(
                "reverted",
                Some("TokenNotAllowed"),
                None,
                "none",
                json!(0),
                false,
                "none",
            ),
            files: &[NC_TOKEN_NOT_ALLOWED],
        },
    ]
}

// ---------------------------------------------------------------------------
// Running and publishing
// ---------------------------------------------------------------------------

/// One scenario, run and published: the row that goes into its evidence files, and the state
/// recipe file it points at.
struct Published {
    row: Value,
    recipe_file: String,
    /// The recipe exactly as it was hashed, so the bytes on disk are the bytes the row names.
    recipe_text: Vec<u8>,
}

async fn publish_case(base: &Fixture, case: &Case) -> Published {
    let variant = if case.is_committed() {
        None
    } else {
        Some(Fixture::build((case.knobs)()))
    };
    let fx = variant.as_ref().unwrap_or(base);

    let call = (case.call)(fx);
    let outcome = fx.run(call.clone()).await;
    let second = fx.run(call.clone()).await;
    let first_json = observed(&outcome);
    let identical = first_json == observed(&second);
    let verdict = judge(&outcome, &case.expected, &fx.route);

    // The state this run used, as the rows that rebuild it from the recording.
    let recipe_name = if case.is_committed() {
        "committed".to_string()
    } else {
        case.scenario.to_string()
    };
    let rows = recipe_rows(fx);
    let recipe = recipe_json(fx, &recipe_name, &rows);
    let mut recipe_text = serde_json::to_string_pretty(&recipe)
        .expect("a recipe serializes")
        .into_bytes();
    recipe_text.push(b'\n');
    let recipe_file = format!("{EVIDENCE}/states/{recipe_name}.json");

    let recording = std::fs::read(workspace_root().join(executor_state::RECORDED))
        .expect("the recording is committed");
    let committed_fixture = base.fixture_bytes();

    let route = &fx.route;
    let profit = route.weth_out_leg2 - route.amount_in;
    let row = json!({
        "schema": SCHEMA,
        "scenario": case.scenario,
        "spec_ref": case.spec_ref,
        "what_this_is": case.what_this_is,
        "layer": "revm",
        "market": FIXTURE_LABEL,
        "milestone": "M10",
        "planted_control": case.planted,
        "state": {
            "recipe_file": recipe_file,
            "recipe_keccak256": keccak_of_bytes(&recipe_text),
            "recipe_bytes": recipe_text.len(),
            "recipe_rows": rows.len(),
            "recording": {
                "file": executor_state::RECORDED,
                "bytes": recording.len(),
                "keccak256": keccak_of_bytes(&recording),
            },
            "committed_fixture": {
                "file": FIXTURE,
                "bytes": committed_fixture.len(),
                "keccak256": keccak_of_bytes(&committed_fixture),
                "additions_file": executor_state::FIXTURE_ADDITIONS,
            },
            "state_source": fx.source,
            "declared_accounts": fx.accounts.len(),
            "declared_words": fx.additions.len(),
            "uses_a_variant_of_the_committed_fixture": variant.is_some(),
            "rebuild": if variant.is_some() {
                "the recording plus the recipe file's rows. The control is in the state: a knob \
                 moved a declared row, and the recipe names the row it moved and what the \
                 recording answers there instead"
            } else {
                "the recording plus the recipe file's rows, which are the committed fixture's own \
                 rows — so the same rebuild lands on fixtures/simulation-m10/ byte for byte, and \
                 the control is in the call rather than in the state"
            }
            .to_string(),
        },
        "route": {
            "input_token": addr(WETH),
            "amount_in": dec(route.amount_in),
            "leg_0": { "pool": addr(POOL_A), "token_in": addr(WETH), "token_out": addr(MID), "ask": dec(route.mid_out_leg1) },
            "leg_1": { "pool": addr(POOL_B), "token_in": addr(MID), "token_out": addr(WETH), "ask": dec(route.weth_out_leg2) },
            "quote": "getAmountOut at the recorded reserves, 0.30 % fee, floored the way the \
                      Solidity divides — the claim the contract is required to check",
            "profit_before_gas": dec(profit),
            "profit_denomination": "WETH wei. Not comparable to the gas bill, which is native wei (§34)"
        },
        "run_spec": run_spec_json(fx, &call, GAS_LIMIT),
        "expected": case.expected,
        "observed": first_json,
        "verdict": match &verdict {
            Ok(()) => json!("PASS"),
            Err(why) => json!(format!("FAIL: {why}")),
        },
        "d3_double_run": {
            "runs": 2,
            "observed_json_identical": identical,
            "note": "the same fixture and the same call, twice in this process; §49's D3 is the \
                     cross-process and cross-crate form, which the execution crate's gate \
                     reproduces from the published spec"
        },
    });

    if let Err(why) = verdict {
        panic!(
            "{}: the run did not end where the scenario claims — {why}\n{}",
            case.scenario,
            outcome.describe()
        );
    }
    assert!(
        identical,
        "{}: two runs of one call disagreed",
        case.scenario
    );
    Published {
        row,
        recipe_file,
        recipe_text,
    }
}

fn write_rows(root: &Path, relative: &str, rows: &[(String, Value)]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().expect("a directory")).expect("the directory exists");
    let mut map = Map::new();
    for (key, row) in rows {
        map.insert(key.clone(), row.clone());
    }
    let tail = relative
        .strip_prefix(EVIDENCE)
        .unwrap_or(relative)
        .trim_start_matches('/');
    let directory = match tail.rfind('/') {
        Some(index) => tail[..index].to_string(),
        None => ".".to_string(),
    };
    let file = json!({
        "schema": SCHEMA,
        "milestone": "M10",
        "evidence_root": EVIDENCE,
        "directory": directory,
        "file": relative,
        "assembled_by": "crates/simulation/tests/executor_evidence.rs",
        "assemble_command": "CC=clang CXX=clang++ CXXFLAGS=\"-include cstdint\" cargo test -p \
                             evm-simulation --test executor_evidence -- --test-threads=1",
        "check_command": "CC=clang CXX=clang++ CXXFLAGS=\"-include cstdint\" cargo test -p \
                          evm-execution --test executor_evidence_gate -- --test-threads=1",
        "rows": map,
    });
    let text = serde_json::to_string_pretty(&file).expect("serializable");
    std::fs::write(&path, format!("{text}\n")).expect("the evidence file writes");
    println!("wrote {} ({} rows)", path.display(), rows.len());
}

/// §60's simulation half: run every scenario, publish it, and write each file its scenarios name.
#[tokio::test]
async fn m10_simulation_evidence() {
    let root = workspace_root();
    let base = Fixture::committed();
    let mut published: BTreeMap<String, Vec<(String, Value)>> = BTreeMap::new();
    let mut recipes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut scenarios: Vec<String> = Vec::new();

    for case in cases() {
        let outcome = publish_case(&base, &case).await;
        scenarios.push(case.scenario.to_string());
        // One file per distinct state. Two scenarios naming the same recipe file must have
        // published the same bytes for it, or the name means nothing.
        match recipes.entry(outcome.recipe_file.clone()) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(outcome.recipe_text.clone());
            }
            std::collections::btree_map::Entry::Occupied(existing) => assert_eq!(
                existing.get(),
                &outcome.recipe_text,
                "{} and {} claim the same state recipe file with different contents",
                existing.key(),
                outcome.recipe_file
            ),
        }
        for file in case.files {
            published
                .entry((*file).to_string())
                .or_default()
                .push((case.scenario.to_string(), outcome.row.clone()));
        }
        println!(
            "{} — {}",
            case.scenario,
            outcome.row["verdict"].as_str().expect("a verdict")
        );
    }

    let mut files: Vec<String> = published.keys().cloned().collect();
    files.sort();
    for file in &files {
        write_rows(&root, file, &published[file]);
    }
    for (file, text) in &recipes {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().expect("a directory"))
            .expect("the states directory exists");
        std::fs::write(&path, text).expect("the recipe file writes");
        println!("wrote {} (state recipe)", path.display());
    }
    println!(
        "published {} scenarios into {} files, over {} state recipe file(s)",
        scenarios.len(),
        files.len(),
        recipes.len()
    );

    // §60's five negative-control names, plus §50's list, are all present as files. The gate in
    // the execution crate re-checks this against the directory on disk; checking it here too
    // means a typo fails at the place that made it.
    for required in [
        SUCCESS,
        REVERT,
        SLIPPAGE,
        PROFIT_GUARD,
        SIM_SUCCESS,
        SIM_FAILURE,
        NC_WRONG_OPERATOR,
        NC_BROKEN_ROUTE,
        NC_ZERO_AMOUNT,
        NC_MIN_OUTPUT,
        NC_FINAL_PROFIT,
        NC_INVALID_PAIR,
        NC_TOKEN_NOT_ALLOWED,
        NC_FORCED_LEG2,
    ] {
        assert!(
            files.iter().any(|written| written == required),
            "{required} was not written"
        );
    }
}

/// §48's own form, asked of the published files rather than of the runs: every row that claims a
/// rebuild recipe must actually name a state source the provider can serve, and its calldata hash
/// must be the hash of the bytes its fields encode. A row that passes `m10_simulation_evidence`
/// but fails this one is a row whose published spec is not the spec that ran.
#[test]
fn published_specs_encode_their_own_calldata() {
    let root = workspace_root();
    let mut checked = 0usize;
    for case in cases() {
        for file in case.files {
            let path = root.join(file);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let file_json: Value = serde_json::from_str(&text).expect("json");
            let row = &file_json["rows"][case.scenario];
            assert_eq!(
                row["schema"].as_str(),
                Some(SCHEMA),
                "{file}: the row lost its schema id"
            );
            let spec = &row["run_spec"];
            let decoded = hex::decode(
                spec["calldata"]
                    .as_str()
                    .expect("calldata")
                    .trim_start_matches("0x"),
            )
            .expect("hex calldata");
            assert_eq!(
                keccak_of_bytes(&decoded),
                spec["calldata_keccak256"].as_str().expect("a hash"),
                "{file}: the published hash is not keccak of the published bytes"
            );
            assert_eq!(
                decoded.len(),
                spec["calldata_len"].as_u64().expect("a length") as usize,
                "{file}: the published length disagrees with the published bytes"
            );

            // The row's state claim, checked against the recipe file it names rather than against
            // this process's memory: a variant scenario's source string is not the committed
            // fixture's, and the only honest question is whether the published file says the same
            // thing the published row does.
            let recipe_file = row["state"]["recipe_file"].as_str().expect("a recipe file");
            let recipe_path = root.join(recipe_file);
            let recipe_bytes = std::fs::read(&recipe_path)
                .unwrap_or_else(|error| panic!("{}: {error}", recipe_path.display()));
            assert_eq!(
                keccak_of_bytes(&recipe_bytes),
                row["state"]["recipe_keccak256"]
                    .as_str()
                    .expect("a recipe hash"),
                "{file}: {recipe_file} is not the bytes this row hashed"
            );
            assert_eq!(
                recipe_bytes.len(),
                row["state"]["recipe_bytes"]
                    .as_u64()
                    .expect("a recipe length") as usize,
                "{file}: {recipe_file} is not the size this row published"
            );
            let recipe: Value = serde_json::from_slice(&recipe_bytes).expect("a recipe json");
            assert_eq!(
                recipe["state_source"].as_str(),
                spec["state_source"].as_str(),
                "{file}: the row and its recipe file name different state sources, so no \
                 provider could serve both"
            );
            let recipe_name = recipe_file
                .rsplit('/')
                .next()
                .unwrap_or(recipe_file)
                .trim_end_matches(".json");
            assert_eq!(
                recipe["name"].as_str(),
                Some(recipe_name),
                "{file}: {} calls itself {:?}, not after its own file name — a recipe whose name \
                 is not its filename would let two scenarios claim one state without saying so",
                recipe_file,
                recipe["name"]
            );
            let fixture_bytes =
                std::fs::read(root.join(FIXTURE)).expect("the fixture is committed");
            assert_eq!(
                keccak_of_bytes(&fixture_bytes),
                row["state"]["committed_fixture"]["keccak256"]
                    .as_str()
                    .expect("a fixture hash"),
                "{file}: the published committed-fixture hash is not keccak of the file on disk"
            );
            let call = evm_protocol::decode_calldata(&decoded).unwrap_or_else(|error| {
                panic!("{file}: the published bytes do not decode: {error}")
            });
            let legs = call.legs().expect("an execute call carries legs");
            let published_legs = spec["call"]["legs"].as_array().expect("legs");
            assert_eq!(
                legs.len(),
                published_legs.len(),
                "{file}: the published call has {} legs, the fields say {}",
                legs.len(),
                published_legs.len()
            );
            for (index, leg) in legs.iter().enumerate() {
                assert_eq!(
                    dec(leg.amount_out),
                    published_legs[index]["amount_out"]
                        .as_str()
                        .expect("an ask"),
                    "{file}: leg {index}'s ask in the fields is not the ask in the bytes — §37's \
                     determinism claim would be about different bytes than the ones that ran"
                );
            }
            checked += 1;
        }
    }
    println!("re-derived {checked} published rows from their own bytes");
}
