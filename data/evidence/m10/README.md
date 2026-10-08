# M10 — the Arbitrage Executor Contract evidence directory

Chain 91342, fixture block 37530593, one deployed executor contract, and one nonce
ladder that carries the deployment, the approvals and allowances, the funded wrap, the
execute call (0x1105c89b06a8edc84995d614fe1e2b79344e2d36d48c62c00f533946f2f82771) and a deliberate failure (0x95407d7ee1e440f700c5336e7fa60baf7e9b5bfbff40a97aeb60254e295596ec) that reverted as
`FinalShortfall`. 8 ladder rows are recomputed in
`execution/lifecycle.json`. 14 scenario rows, 9 distinct
REVM runs behind them, plus the live run's own rows.

Two crates wrote this directory.

* `crates/simulation/tests/executor_evidence.rs` is the assembler: it owns the
fixtures, runs them in REVM over the real deployed bytecode, and publishes what it
observed. It also ran the live deployment and the two real transactions.
* `crates/execution/tests/executor_evidence_gate.rs` is the independent recompute: it reads **only the JSON files in this
directory**, rebuilds every plan, re-encodes every call, replays every run, and
writes the eight files marked `(gate)` below. It imports no fixture builder, no
recipe constant and no address literal from the assembler.

A gate that reused the assembler's harness would be a receipt: the same code, the same
state, the same answer quoted back. That is why this directory is judged from a second
crate, compiled and started by a different `cargo test` invocation.

## Re-running it

```text
CC=clang CXX=clang++ CXXFLAGS="-include cstdint" cargo test -p evm-simulation --test executor_evidence -- --test-threads=1
```

```text
CC=clang CXX=clang++ CXXFLAGS="-include cstdint" cargo test -p evm-execution --test executor_evidence_gate -- --test-threads=1
```

Both must run with `--test-threads=1`: the assembly scratch under `target/` is shared
across processes and parallel runs corrupt each other's files. Those two command lines
are the only supported way to rebuild this directory.

## What is here

`(assembler)` = written by the harness that ran the fixtures and the live chain;
`(gate)` = written by the recompute, and the only files it may write.

```text
contract/
  bytecode_hash.json    (assembler)
  deployment.json    (assembler)
  abi.json    (gate)
  runtime_bytecode.json    (gate)
execution/
  lifecycle.json    (gate)
fixtures/
  profit_guard.json    (assembler)
  revert.json    (assembler)
  slippage.json    (assembler)
  success.json    (assembler)
negative_controls/
  broken_route.json    (assembler)
  final_profit.json    (assembler)
  forced_second_leg_revert.json    (assembler)
  invalid_pair.json    (assembler)
  min_output.json    (assembler)
  token_not_allowed.json    (assembler)
  wrong_operator.json    (assembler)
  zero_amount.json    (assembler)
  wrong_chain.json    (gate)
  wrong_executor.json    (gate)
real/
  giwa_execution.json    (assembler)
  giwa_failure.json    (assembler)
  giwa_ladder_steps.json    (assembler)
  preconditions.json    (assembler)
recompute/
  deterministic.json    (gate)
simulation/
  failure.json    (assembler)
  success.json    (assembler)
states/
  caller_is_not_the_stored_operator.json    (assembler)
  committed.json    (assembler)
  pair_not_in_the_allowlist.json    (assembler)
  token_not_in_the_allowlist.json    (assembler)
```

## The five determinism gates (§49)

| gate | question | this run's answer |
|---|---|---|
| D1 | same plan → same plan hash | 14 of 14 rows hash the same from their two
| | | spellings; 28 of 28 mutants change the hash |
| D2 | same plan → same calldata | 14 rows encode to the published bytes,
| | | 14 to the published digest |
| D3 | same fixture → same simulation result | 9 of 9 REVM runs are
| | | JSON-identical to the published `observed` block |
| D4 | same failure → same classification | 14 rows re-judged,
| | | 0 classification and 0 judgement problems |
| D5 | same route → same route id | 14 rows stable, 14 name the
| | | pools and tokens the row publishes |

The per-row tables are in `recompute/deterministic.json`, `execution/lifecycle.json`,
and in the phase file each row names.

## What the gate re-derived about the contract

* `37` function and event entries in `contract/abi.json`, tied to the
`1` rows that publish the execute selector, and `12` of
`12` published revert-byte rows decoded back to the label the row
carries. This is §47's ban made testable: the ABI is read out of the Solidity
source, and the selectors it implies are compared with the bytes the runs executed.
* `7347` runtime bytes and `7565` creation bytes hashed in
`contract/runtime_bytecode.json`; `4` of `4` state
recipes put the compiler's runtime artifact at the executor address.
* `84` recomputed checks in `execution/lifecycle.json`, `84`
of them agreeing with the row they quote — the gas bill, the balance chain, the nonce
ladder, the §31 asset rows, the §32 reconciliation, §8's freshness bound judged by the
plan's own function, and §27's zero-residue claim.
* `4` state recipes verified row by row against the committed fixture in
               `state_integrity`, so a replay below cannot be run on a state nobody attested.

## What may **not** be inferred from this directory

* **Not a realised profit.** `real/giwa_execution.json`'s `realized_profit`,
`gross_profit`, `input_amount` and `profit_status` cells are `null`, not zero,
because the route settles in WETH while the gas bill is paid in the native asset —
the two sides do not add (§34). An included receipt with `status = 1` is not a
profitable arbitrage (§52), and §30's real profitable round trip is NOT_PROVEN (§59).
* **There is no `real/giwa_success.json`, and none is missing.** §60's last line says
not to create a fabricated one; the successful real transaction that exists is the
round trip whose profit was never proved, and it is published as
`real/giwa_execution.json`.
* **The fixtures are not a market.** Every scenario row is a CONTROLLED_FIXTURE over a
recorded block plus declared words; `states/` names the recipe a row was built from.
* **A plan hash over a fixture row is this gate's declaration, not a published fact.**
No scenario row carries §5's plan-side fields, so `recompute/deterministic.json`
publishes them as null and labels the bound it declares.
* **Nothing here was broadcast by the gate.** No key is read, nothing is signed, no
node is contacted (§51).

## Secrets, network, time

No private key and no endpoint address appears in any file in this directory. The live
run's `real/preconditions.json` names the environment variable it reads and nothing of
its value. No wall-clock value appears in the eight `(gate)` files, so rebuilding this
directory is byte-for-byte equal forever; the assembler's lifecycle record does carry
its own stage timestamps, which is why byte identity is stated per file rather than for
the directory as a whole.

## §50's planted negative controls

10 controls, each bound to a file and to the refusal that file must show:
  * `wrong chain` — `negative_controls/wrong_chain.json`, refused at the plan layer as `wrong_chain`
  * `wrong executor` — `negative_controls/wrong_executor.json`, refused at the plan layer as `wrong_executor`
  * `wrong operator` — `negative_controls/wrong_operator.json`, refused at the contract layer as `NotOperator`
  * `wrong token continuity` — `negative_controls/broken_route.json`, refused at the contract layer as `BrokenContinuity`
  * `zero amount` — `negative_controls/zero_amount.json`, refused at the contract layer as `ZeroAmount`
  * `min output violation` — `negative_controls/min_output.json`, refused at the contract layer as `FinalShortfall`
  * `final profit violation` — `negative_controls/final_profit.json`, refused at the contract layer as `AskBelowFloor`
  * `pair not allowed` — `negative_controls/invalid_pair.json`, refused at the contract layer as `PairNotAllowed`
  * `token not allowed` — `negative_controls/token_not_allowed.json`, refused at the contract layer as `TokenNotAllowed`
  * `forced second-leg revert` — `negative_controls/forced_second_leg_revert.json`, refused at the contract layer as `Error(string)`

2 of them are planted at the plan layer and written by this gate; the
8 contract-layer ones are refused by the deployed bytecode running in
REVM.

## Judgment

This directory stands or falls on `recompute/deterministic.json`'s `verdict` and on the
single assertion at the end of the gate: a published claim that disagrees with this
run's recomputation is collected as drift, written into that file, and named there —
never panicked past, never averaged away.
