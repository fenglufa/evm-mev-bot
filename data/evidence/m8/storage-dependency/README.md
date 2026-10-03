# M8.4.1 — storage dependency and end-to-end RPC diagnosis

3 live runs on one commit (ba10d58edafaac8afc0bf2e546f6e74a480f2f23), 3 fixed-block runs of the offline fixture beside them, all of them `build-only`. This directory is a diagnosis and nothing else: §1 forbids, for this milestone, the very changes these tables would make a case for — storage concurrency, the C1/C2/C4 scheduler, batch JSON-RPC, multicall, prefetch, new caches, connection and keep-alive tuning, provider swaps, and anything in REVM, Graph, Opportunity, Risk or Execution — so no figure below is evidence that a run got faster, and none is offered as a reason to make one of those changes.

```text
Q1：20 个 storage read 是否存在真实 dependency？
Q2：如果没有，为什么实际上还是串行？
Q3：simulation 外的 RPC 时间去了哪里？
```

## 1. The three answers
**Q1 — yes, on every read the code could argue about.** The pooled map covers 60 storage reads, 20 of them in each of the 3 live runs; §3 sets the `fixed-block` arm's own counts beside them. 0 of the reads are proved independent of every other; 57 are `ordered`, each naming the read it waited for and the leg that makes the two sequential; 3 is `unknown`, which is one per simulation — its first storage read, where there is no earlier read to argue from. Nothing was promoted: §3 forbids it and §5 forbids the other route to the same conclusion, since a different slot is not evidence of independence. The legs those reads sit on are fiber ×60, and the reason a read on `fiber` cannot be argued independent is in its own proof text: REVM's async database holds one outstanding request there, so the next slot is a function of the value just returned.
**Q2 — the order the interpreter chose, and the wire agrees.** Over the 3 live runs the pooled pair table counts 570 pairs of storage reads inside one simulation, of which 570 are `ordered` and 0 `independent`; and on the wire each run's calls went out one at a time — `max_concurrency` 1, `rpc_overlap_duration_ns` 0 in every run's row of `pipeline-summary.json`. That is as far as a diagnosis goes: whether the reads *could* be issued together is a design question §1 takes off this milestone's table.
**Q3 — into the stages around the simulation, and it is not a small part.** 226 of the arm's 235 recorded calls fall in the four buckets §7 names — build: 22 calls / 5.285 s of union; detection: 18 calls / 5.444 s of union; preflight: 69 calls / 18.506 s of union; simulation: 117 calls / 32.222 s of union. The remaining 9 are 6 in `stages_the_seven_do_not_name` and 3 the issuing code stamped no stage at all, both rows printing in §5 — one population of 235 read at two granularities, not two counts to add to it. A bucket's union is measured inside one run and then added across runs, never swept across two clocks, so these are sums of 3 separate measurements and not one distribution. `pipeline-calls.json` has the row each figure came from.

## 2. The runs
| run | block | sim calls | lifecycle calls | storage reads | independent | ordered | unknown | ladder window s | RPC union s | union past ladder s | local (non-RPC) s | max concurrency | RPC overlap s |
| — | — | — | — | — | — | — | — | — | — | — | — | — | — |
| route-91342-37677656-1791022773599 | 37677656 | 39 | 32 | 20 | 0 | 19 | 1 | 18.975 | 18.919 | null | 0.056 | 1 | 0.000 |
| route-91342-37677684-1791022801925 | 37677684 | 39 | 43 | 20 | 0 | 19 | 1 | 20.709 | 23.317 | 2.608 | 0.000 | 1 | 0.000 |
| route-91342-37677710-1791022828033 | 37677710 | 39 | 43 | 20 | 0 | 19 | 1 | 20.097 | 22.678 | 2.581 | 0.000 | 1 | 0.000 |

The two columns headed `union past ladder s` and `local (non-RPC) s` belong together. `runs` has 3 runs and they did not all stop in the same place: 2 of them (route-91342-37677684-1791022801925, route-91342-37677710-1791022828033) got as far as the execution lane's `build` stage, which issues calls and writes no closed span of its own, so on those runs the calls cover more time than the ladder's window and `non_rpc_duration_ns` clamps to 0. Publishing the excess beside it — `union_extends_past_the_ladder_window_ns` — is what stops that 0 from being read as 「this run had no local time」, which would be §11's forbidden swap in a field nobody thought about.
The run that stopped earlier is the one with the short call column: route-91342-37677656-1791022773599 held 71 calls and its own `route-run.json` counters read execution_preflight_blocked=1, execution_sequence_attempt=1, execution_sequence_stopped=1, opportunity_count=1, risk_accept_count=1, simulation_count=1, where the others read route-91342-37677684-1791022801925 (82 calls): execution_build_success=1, execution_preflight_success=1, execution_sequence_attempt=1, execution_sequence_blocked=1, execution_sequence_unsettled=1, opportunity_count=1, risk_accept_count=1, simulation_count=1; route-91342-37677710-1791022828033 (82 calls): execution_build_success=1, execution_preflight_success=1, execution_sequence_attempt=1, execution_sequence_blocked=1, execution_sequence_unsettled=1, opportunity_count=1, risk_accept_count=1, simulation_count=1. That is why the two accounts of the same run's wall time differ between runs — a preflight that blocks never reaches the `build` reads, so they are not in its union either.

## 3. The fixed-block arm and the live arm
`fixed-block/` is §14's arm: the offline fixture's recorded dump, replayed 3 times — one block, no network, `all_state_reads_pinned` true in every run. Its 63 storage reads against the live arm's 60 differ by the slots this route touches on a real head, and the call lists differ the same way — 41 inside the simulation against 39. What is identical is the verdict, which is the only thing §27 asks the two arms to share: `fixed-block/run-*/dependency-map.json` reports 0 independent, 20 ordered, 1 unknown, and `runs/*/dependency-map.json` 0 independent, 19 ordered, 1 unknown per run. An arm with a network in it changed nothing about what the code proves.

## 4. What the dependency map says read by read
`dependency-map.json` is §6's machine-readable shape: 60 `nodes`, 57 `edges`, one `summary`, plus `per_simulation` and `pair_summary`. A node names its `caller` (the REVM phase that asked, verbatim), its `leg`, its `dependency`, the `depends_on` list it waited behind, and an `evidence` array carrying the proof text for exactly that claim — so §27's 「所有 dependency 分类必须有 evidence」 is checkable per read rather than per paragraph. The legs this build can tell apart are the 5 in `leg_vocabulary.proofs` (fiber, materialised_audit, unstamped, ambiguous_label, unattributed_site), each matched against the simulation crate's own phase constants, and every storage read of this route landed on one leg: fiber ×60. Only `materialised_audit` is a leg whose definition is a proof of independence — its whole key list exists before its first request goes out — and no read here is on it. Two reads on different legs are neither: their pair is `undecidable`, a relation of its own rather than a side to file it on.
`uses_prior_response` — §4's fourth field — never says `yes` here: not_determined ×3, possible_not_determined ×57 in the pooled nodes. Where the leg's order is what makes a read wait it says `possible_not_determined`; where there was no earlier read on the leg to argue from, `not_determined`. A value flowing through the interpreter is not in this record, only the requests are, so a `yes` would be a claim built out of the wrong material.
Pairs, which is how §19's summary reads: of the 570 pairs of reads inside one simulation, 570 are ordered, 0 independent, 0 undecidable, 0 contradiction. That is why the pair count sits far above the node count and why `independent` stays where §20 wants it.

## 5. Where the run's RPC time is
| bucket | code stage names | calls (3 runs) | union total s | per-run min s | per-run max s | runs reporting it |
| — | — | — | — | — | — | — |
| build | build | 22 | 5.285 | 0.000 | 2.655 | 3 |
| detection | opportunity_detection | 18 | 5.444 | 1.483 | 2.427 | 3 |
| execution_preparation |  | 0 | 0.000 | 0.000 | 0.000 | 3 |
| opportunity |  | 0 | 0.000 | 0.000 | 0.000 | 3 |
| preflight | preflight, risk | 69 | 18.506 | 5.591 | 7.142 | 3 |
| simulation | simulation | 117 | 32.222 | 10.453 | 10.895 | 3 |
| state_update | state_update, graph_update | 0 | 0.000 | 0.000 | 0.000 | 3 |
| stages_the_seven_do_not_name | unstamped + a stage no bucket folds | 6 | 1.524 | 0.499 | 0.524 | 3 |

Two accounts of the same call are kept beside each other rather than merged: `stage` on a `pipeline-calls.json` row is what the issuing code said, and `attribution_accounts` in `pipeline-summary.json` is what that run's ladder says held the instant. They disagree on 3 of the 235 call rows in this arm, and the disagreement is published as a pair rather than resolved — either the stamp is wrong or the socket is wired to the wrong leg, and both are findings §11 would rather see than a table that agrees with itself: route-91342-37677656-1791022773599: `eth_chainId` stamped null, held by `preflight`; route-91342-37677684-1791022801925: `eth_chainId` stamped null, held by `preflight`; route-91342-37677710-1791022828033: `eth_chainId` stamped null, held by `preflight`. Every row with no stamp at all is eth_chainId ×3 — the call that produces the adapter a sink could be attached to, so it cannot carry a stamp issued by one. 0 of the 235 rows carry a stamp the run itself marks untrustworthy — no label here had to be guessed at because two calls were in flight at once, which is the same fact §2's last two columns record from the other side.
A rank over 3 runs is `null`: p90 needs 10 samples by the repository's own rule (`minimum_samples_for_rank`), so the per-run columns above are the measurements and the total is a sum. Nothing in this directory pools a duration across two clocks.

## 6. Which height every read named
| run | state reads in the run | at this run's block | at another height | at a tag that is not a height | all pinned |
| — | — | — | — | — | — |
| route-91342-37677656-1791022773599 | 41 | 38 | 2 | 1 | false |
| route-91342-37677684-1791022801925 | 45 | 39 | 4 | 2 | false |
| route-91342-37677710-1791022828033 | 45 | 39 | 4 | 2 | false |

The boolean has two independent causes, and the three counts before it separate them. Every state read of a **simulation**, in every run of both arms, named the block that simulation pinned — 114 of the 114 in this arm, and all of them in each `fixed-block` run. The head read is not a state read at all: its job is to ask what height there is. What makes a live row `false` is the execution lane, and its rows are these: preflight eth_getTransactionCount at 37677670 ×1, preflight eth_getTransactionCount at pending ×3, preflight eth_getBalance at 37677669 ×1, preflight eth_getTransactionCount at 37677699 ×1, preflight eth_getBalance at 37677698 ×1, build eth_getBalance at 37677698 ×1, build eth_getTransactionCount at 37677706 ×1, build eth_getTransactionCount at pending ×2, preflight eth_getTransactionCount at 37677726 ×1, preflight eth_getBalance at 37677725 ×1, build eth_getBalance at 37677725 ×1, build eth_getTransactionCount at 37677731 ×1. That is §13's business too, and it is why this milestone publishes the split instead of a bare `false` a reader has to take on faith. In the fixture arm there is no execution lane, so `true` there means what it says.

## 7. §16: did the instrumentation change an answer?
`correctness/comparison.json`: the same fixed-block route run with the sink off and with it on. 21 result fields compared, 21 of them `identical`, the whole result's fingerprint `0xd54891737d934871622ce879b55c67a90bba5b343a7954821b20d138ad9dbf67` with `fingerprint_identical` true, the endpoint seeing 41 calls with `identical_in_order` true, and `gas_charge` among the fields that came out the same on both sides. The silent arm recorded 0 rows into the sink against the instrumented arm's 41 — that gap is the control, and without it an equality of two identical arms would be a tautology: nothing was observed, so nothing could disagree.

## 8. Did these runs sign, broadcast, or spend anything?
No. 0 lines across `route-runs/*/signed-transactions.jsonl` and 0 across `route-runs/*/submissions.jsonl` for the 3 route runs, `mode` is `build-only` in every one of them, `successful_real_arbitrage` is false in every one, and no call row anywhere in this directory carries `eth_sendRawTransaction`. §15's 「不得 sign / broadcast」 held, and no key entered the process to take these figures: the execution lane got as far as building an intent and, in the run that stopped earliest, did not get past its preflight fee check.

## 9. What this directory cannot see
- **market event stream** — findings arrive as WebSocket notifications, not as JSON-RPC calls this build makes, so no sink of this run holds one. A run that consumed hundreds of events can and does show no rows for them, and that absence is this line, not a count of zero reads
- **the node's own queueing and the network between here and there** — provider_total_duration: this build records one call at the point it becomes bytes on the wire, so request construction, the wait on the node, response decoding and state conversion are not separable there (§17's breakdown_unavailable) and are reported as one provider duration
- **a state answer that cost no request** — a read the reuse boundary held, or one a recorded dump answered, is absent from this timeline by construction — it issued nothing. It is tallied in `duplicate-reads.json` and in the cache stats, so `rpc_count` here is not a count of state reads made
- **a stage the run's mode never reached** — a build-only run skips `sign`, `submit`, `receipt` and the rest; the lifecycle records them as skipped and they appear in `stages_not_completed` below with that outcome, which is a statement about the mode rather than a measured zero-duration read
- **a stage that issues calls and writes no span** — the execution lane's `build` reads facts to build an intent and leaves no closed interval behind, which is why `union_extends_past_the_ladder_window_ns` exists as a field and 2 of the 3 runs have a non-null value in it.

## 10. How to regenerate this directory
```bash
# 1. the build, with the recipe this repository needs on this machine
CC=clang CXX=clang++ CXXFLAGS="-include cstdint" GIT_REVISION=$(git rev-parse HEAD) \
  cargo build --bin evm-mev-bot

# 2. §14's fixed-block arm and §16's correctness pair, offline on the recorded dump
M841_FIXTURE_EVIDENCE=data/evidence/m8/storage-dependency \
  cargo test -p evm-simulation --test storage_dependency_experiment

# 3. §15's live arm: three build-only runs, no key in the environment. `GIWA_RPC_URL`
#    holds the endpoint; each run opens its own session directory under both roots.
./target/debug/evm-mev-bot arbitrage \
  --rpc-url "$GIWA_RPC_URL" --execution-mode build-only \
  --sender <the test sender> --input-token <the input token> \
  --candidate-mid <the router> --candidate-pool <pool A> --candidate-pool <pool B> \
  --input-wei 100000000000000 --fee-num 997 --fee-den 1000 \
  --fee-evidence data/evidence/m7/candidate-fee-measurement.json \
  --market real-market \
  --market-evidence "reserves and blockTimestampLast read at the pinned live head by this run" \
  --latency-trace --rpc-trace --diagnose-state-acquisition --diagnose-storage-dependency \
  --evidence-dir data/evidence/m8/storage-dependency/route-runs \
  --rpc-output data/evidence/m8/storage-dependency/runs

# 4. the pooled tables, this README, and the byte-for-byte check
M841_STORAGE_DEPENDENCY_REFRESH=1 cargo test -p evm-pipeline --test storage_dependency_evidence
cargo test -p evm-pipeline --test storage_dependency_evidence
```

The last command's second invocation is the one that fails if a number in this directory was hand-edited. Step 3's refresh is the only way these root files change; `runs/`, `route-runs/`, `fixed-block/` and `correctness/` are the runs' own output and are copied into place, never rewritten. Each live run's stage ladder is beside it in 
`data/evidence/m8/latency/<session>/`, under the same session name as its `route-runs/` directory.

## 11. What may not be concluded from these tables
- 「storage 是独立的」 — no read here carries a proof of it, and the count of such proofs is in §1 above; a slot that differs is not one
- 「provider 是瓶颈」 — one duration is recorded at the point a request becomes bytes on the wire, so node work, network round trip and this process's decode are not separable there (§9's second bullet)
- 「C4 一定更优」 — no concurrency arm is in this directory, and the leg a proof of independence would have to be built on is the one §1 forbids this milestone to touch
- 「execution RPC = 0」 — the execution lane's own adapter has no sink in this build: what its reads cost is in the lifecycle rows and the buckets above where the same adapter was traced, and the surfaces that stayed out are named rather than counted as none
- 「network latency」 — nothing measured a round trip separately from a request, so every duration here is a total
- 「this run got faster」 — nothing was changed to make a run faster, and the three live runs sit on three different blocks, so a difference between two of them is a difference between blocks as much as between anything else

## 12. Provenance and which file answers what
Every pooled file here carries `assembled_from`: one row per run, with its directory, source, block, endpoint digest, commit, mode, per-sink call counts, storage read counts and dependency verdicts, its own ladder figures, and its route run's counters. The wall-clock stamp is the latest of the runs' own, so re-assembly is byte-reproducible and no duration reads it.
- `storage-reads.json` — §4's per-read record: address, slot, height, caller, stage, dependency, depends_on, and the evidence beside each — one row per storage read
- `dependency-map.json` — §6's nodes / edges / summary over the same rows
- `dependency-summary.json` — §19's one-screen tally: the three counts, the pair counts, the per-bucket call and union figures
- `pipeline-calls.json` — §9's raw layer: every call of both sinks, one row each, with its stage, caller, height tag, duration and attempts
- `pipeline-summary.json` — §7/§10's per-run totals and stage rows, the two attribution accounts, §13's pin table, and what the build cannot observe
- `stage-summary.json` — the same stage arrays lifted out per run, plus the pooled buckets
- `simulation-traces.jsonl` — §9's raw layer for this arm: one line per simulation, with its window, height, endpoint digest and every call row, which is what the tables above rebuild from
- `outside-simulation-rpc.json` — the lifecycle's own calls, pooled per run and never across two clocks, beside the reads no sink in this build can reach
- `account-read-matrix.json` — M8.3.2 §10's matrix: one row per address a run read, split by which of the three legs asked, and `required_by` beside it
- `storage-breakdown.json` — M8.3.2 §8's storage reads per slot, and the same rows grouped by address
- `duplicate-reads.json` — M8.3.2 §9's duplicate tally per source: the state reads that repeat a key and so cost no request, which is why `rpc_count` is not a count of reads
- `rpc-gaps.json` — M8.3.2 §7's waits between one simulation's consecutive calls, per simulation and pooled
- `rpc-summary.json` — the per-method and per-source call tallies, and the sources that refused to be traced
- `simulation-summary.json` — one row per simulation: window, call count, duration and cache stats
- `bottleneck-classification.json` — §17's classification: every candidate word with the threshold that fired it and the ones that did not
Every rank in these files has `minimum_samples_for_rank` beside it, so a p90 over three samples is `null` and says so rather than estimating.

The three files the runs wrote and this assembly did not: `correctness/` (§16's baseline/instrumented/comparison triple, from 
`crates/simulation/tests/storage_dependency_experiment.rs`), `fixed-block/` (§14's three fixture runs, same source), and `route-runs/` (the ladder's own record per live run, from `crates/cli`). The nodes of the dependency graph are identified as `<run>:<simulation>:rpc<n>`, so a node id from one arm is never comparable with one from another and the cross-arm check above compares verdicts, not ids.

## 13. What this directory is not
Not an optimization plan, and not a result. It records calls and the order the engine asked for them in, and it records them the same way whether or not a sink is attached — which §16 is the record of. §22's completion report is where the eight questions this milestone was asked get answered in prose, and §28's rule — 「没有证明 dependency，就不能 说 independent」 — is the one line these tables were built to make checkable.

