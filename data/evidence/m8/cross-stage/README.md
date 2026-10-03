# M8.4.2 — cross-stage RPC redundancy and reuse (evidence)

The question §1 asks: 不同 pipeline stage 之间，是否存在可以安全复用的重复 RPC/state read？ This directory answers it for three live runs of one build. It is a diagnosis: nothing here caches, reuses across stages, schedules differently, batches, or prefetches, and no duplicate found here has been removed.

## Headline

- 246 asks across 3 runs, of which 78 are the same ask as an earlier one (§4's duplicate classes).
- 42 of those pairs are reuse candidates (§6's level B); **0** are safe to reuse (§6's level C).
- 30 pairs are refused at §23's block rule: the two asks name different heights.
- Scope: 72 cross-stage, 6 intra-stage, 0 same-logical-request (§17 keeps these apart).

## The runs

| run | block | endpoint | execution mode | state-read reuse | concurrency | cache hits | call rows | asks | pairs | candidates | safe to reuse |
|---|---|---|---|---|---|---|---|---|---|---|---|
| route-91342-37700740-1791045857463 | 37700740 | rpc-faa716cada04a9ef | build-only | true | 1 | 77 | 82 | 82 | 26 | 14 | 0 |
| route-91342-37700778-1791045896094 | 37700778 | rpc-faa716cada04a9ef | build-only | true | 1 | 77 | 82 | 82 | 26 | 14 | 0 |
| route-91342-37700806-1791045924232 | 37700806 | rpc-faa716cada04a9ef | build-only | true | 1 | 77 | 82 | 82 | 26 | 14 | 0 |

All 3 runs are the same build (`f0031ad8c74d8b2c0ed11ff45bfbec6d29b60805`), the same chain, and build-only. A cache hit cost no request and so has no call row: the tables count asks, never lookups — §18's 「cache hit ≠ physical duplicate」 (§30's limit; §11's settings).

## Duplicate classes (§4)

| class | pairs |
|---|---|
| exact_duplicate | 42 |
| semantic_duplicate | 0 |
| same_target_different_block | 30 |
| same_method_different_semantics | 0 |
| same_target_block_undetermined | 6 |

The class counts sum to 78 of 78 pairs; `not_a_duplicate` is never counted, so a pair that is not a duplicate of any class is not in this table at all (§4's sixth class is a verdict about a pair of asks, not a pair this file lists).

## Per-stage direction (§5, §10)

| producer stage → consumer stage | class | scope | pairs | methods |
|---|---|---|---|---|
| observation → build | exact_duplicate | cross_stage | 6 | eth_getBlockByNumber |
| observation → preflight | exact_duplicate | cross_stage | 6 | eth_getBlockByNumber |
| observation → preflight | same_target_block_undetermined | cross_stage | 3 | eth_getBlockByNumber |
| observation → preflight | same_target_different_block | cross_stage | 3 | eth_getBlockByNumber |
| observation → simulation | exact_duplicate | cross_stage | 3 | eth_getBlockByNumber |
| opportunity_detection → preflight | same_target_different_block | cross_stage | 18 | eth_call |
| preflight → build | exact_duplicate | cross_stage | 12 | eth_getBalance, eth_getBlockByNumber, eth_getTransactionCount, eth_maxPriorityFeePerGas |
| preflight → preflight | exact_duplicate | intra_stage | 6 | eth_getBlockByNumber, eth_maxPriorityFeePerGas |
| simulation → build | exact_duplicate | cross_stage | 3 | eth_getBalance |
| simulation → build | same_target_different_block | cross_stage | 3 | eth_getTransactionCount |
| simulation → preflight | same_target_block_undetermined | cross_stage | 3 | eth_getTransactionCount |
| simulation → preflight | same_target_different_block | cross_stage | 6 | eth_getBalance, eth_getTransactionCount |
| unstamped → build | exact_duplicate | cross_stage | 3 | eth_chainId |
| unstamped → preflight | exact_duplicate | cross_stage | 3 | eth_chainId |

The cells add to 78 pairs, which is the headline figure above (§10's per-stage reading is this table, §16's per-method one is in `duplicate-summary.json`).

## The eleven named stage pairs (§13)

| pair | status | pairs | pairs the other way | candidates | safe to reuse |
|---|---|---|---|---|---|
| Detection -> Preflight | measured | 18 | 0 | 0 | 0 |
| Detection -> Simulation | measured_no_duplicates | 0 | 0 | 0 | 0 |
| Detection -> Build | measured_no_duplicates | 0 | 0 | 0 | 0 |
| State Update -> Preflight | measured_one_side_issued_no_calls | 0 | 0 | 0 | 0 |
| State Update -> Simulation | measured_one_side_issued_no_calls | 0 | 0 | 0 | 0 |
| Opportunity -> Preflight | not_applicable_no_such_stage | 0 | 0 | 0 | 0 |
| Opportunity -> Simulation | not_applicable_no_such_stage | 0 | 0 | 0 | 0 |
| Preflight -> Simulation | measured_no_duplicates | 0 | 9 | 0 | 0 |
| Preflight -> Build | measured | 12 | 0 | 12 | 0 |
| Simulation -> Build | measured | 6 | 0 | 3 | 0 |
| Simulation -> Execution Preparation | not_applicable_no_such_stage | 0 | 0 | 0 | 0 |

A row reading `not_applicable_no_such_stage` is §13's own instruction not to invent data: this build stamps no stage by that name. A measured zero is a different sentence, and the two statuses are kept apart rather than both reported as `0`. The two directions are separate cells: a zero across the pair and a non-zero back across it is one finding, not a contradiction (§5).

## What these numbers cannot say

- `safe_to_reuse` is 0 because two of §6's five conditions — who owns an answer after it arrives, and whether the consumer needs a fresh one — are not fields of a recorded call. Each row of `reuse-candidates.json` carries all five conditions with the reason for each, so the zero is a statement about this record, not about the pipeline.
- Equal data is not a licence to reuse (§7). Nothing here is a recommendation to build a cache, and §30 forbids this report from becoming one.
- `semantic_duplicate` is reported as `not_measurable_from_this_record`: the recorded row holds the collapsed form of a call, so `latest` against `pending` cannot be told apart after the fact (§4.2). The count of 0 in these tables is a measurement of the record, not a claim that no such pair happened.
- §12's 3 fixed-block arms replay one pinned block offline and record 41 / 41 / 41 asks each, in the stage shapes 40 simulation, 1 unstamped | 40 simulation, 1 unstamped | 40 simulation, 1 unstamped — so the arms do carry two stage names, and the 0 / 0 / 0 pairs between them are measured, not avoided: every one of an arm's asks (41 / 41 / 41 per arm) is the first ask for its own identity, which says nothing inside one simulation repeats. 「Which stages overlap」 is answered by the live runs above, whose lifecycle spans more than one stage; these arms answer 「does one block fold the same way twice」.

## Files

| file | what it holds |
|---|---|
| README.md | this file: the figures, the runs behind them, and what they cannot say |
| duplicate-matrix.json | one row per (producer stage, consumer stage, class, scope), pooled |
| duplicate-summary.json | §16's figures, broken out by method, producer stage and consumer stage |
| reuse-candidates.json | §15's directed rows: duplicate, reusable, safe-to-reuse as three verdicts |
| stage-pairs.json | §13's eleven named pairs, each with a status and a reason |

Raw evidence: `runs/<run>/cross-stage-duplicates.json` (one run's own pairs), `runs/<run>/pipeline-calls.json` (every call row the tables were folded from), `fixed-block/run-01..03` (§12's three fixed-block arms), `correctness/` (§27's baseline against instrumented), `route-runs/` (the ladder's own records).

## How to regenerate

```
M842_CROSS_STAGE_REFRESH=1 cargo test -p evm-pipeline --test cross_stage_evidence
```
writes this README and the four tables from `runs/`. The runs themselves come from `crates/cli` with `--diagnose-cross-stage`; regenerating them needs a live node.

## Safety boundary (§28)

Every run here is build-only: 3 of 3 route records say `successful_real_arbitrage: false`, and `route-runs/*/signed-transactions.jsonl` and `route-runs/*/submissions.jsonl` exist and are empty. Nothing was signed, broadcast, or spent; the endpoint appears in these tables only as a digest.

