# M8.3.3 — controlled RPC parallelism experiment

9 live runs on one commit (0a1d867f5e6141a06b2e50a46cc3066e70d7cbf6), one fixed block replayed at three bounds, all of them build-only against the endpoint this repository records as `rpc-faa716cada04a9ef` on chain 91342. Nothing in this directory is a production default: the concurrency bound a run was given came from a command line, the build's own default is 1, and §3 forbids promoting this experiment's result into one.

```text
在不改变 state、simulation、risk、execution 语义的前提下，受控提高 state RPC 并发度，是否可以显著降低 simulation latency？
```

## 1. What this directory is for
An experiment, not an optimization: three concurrency bounds measured against each other on the same route, with the semantics of the run held still by `fixed-block/` (§1). The tables are the measurements; this file is the index. The two figures §15 insists on staying apart are in every table — `configured_concurrency` is what a run was told and `observed_max_concurrency` is what its scheduler saw outstanding.

## 2. Where the baseline comes from
§10 makes M8.3.2 the starting line and the stop rule: `data/evidence/m8/post-reuse/` measured this route after the reuse boundary went in, and a C1 run here has to match it field for field before any louder arm means anything. The three post-reuse simulations and the 3 C1 runs in this directory all report 39 calls, 0 duplicate state reads, a wire max concurrency of 1, `serial` and an overlap of 0 ns, with the cache's 77 hits and 38 misses beside them. So the rule was checked and did not fire.

## 3. The arms
| arm | bound asked for | runs | simulations | blocks | scheduler peaks | wire peaks | simulations with an overlap on the wire | never above its bound |
|---|---|---|---|---|---|---|---|---|
| C1 | 1 | 3 | 3 | 37636187,37636292,37636313 | 1,1,1 | 1,1,1 | 0 | true |
| C2 | 2 | 3 | 3 | 37636334,37636354,37636376 | 2,2,2 | 2,2,2 | 3 | true |
| C4 | 4 | 3 | 3 | 37636396,37636418,37636437 | 4,4,4 | 4,4,4 | 3 | true |

C8 was not run. §13 allows it only after C2, C4 and the correctness gate pass, and it was not asked for; nothing in this directory is evidence about eight concurrent reads. The sentence printed once for the whole directory and repeated on every line is: observed_peak counts the state reads this simulation's provider held outstanding at the node at one instant; a read answered from this simulation's own cache never enters it, and a read the adapter retried is still one read here while the wire trace counts its attempts. configured is the bound that was asked for and is never evidence that it happened (§15).

## 4. The runs, and what each one measured
| run | bound | block | calls | duplicate state reads | attempts | retried | cache hits | cache misses | scheduler peak | wire peak | wire | simulation duration ns | RPC union ns | RPC sum ns | RPC overlap ns |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| c1-01 | 1 | 37636187 | 39 | 0 | 39 | 0 | 77 | 38 | 1 | 1 | serial | 9933816167 | 9899301618 | 9899301618 | 0 |
| c1-02 | 1 | 37636292 | 39 | 0 | 39 | 0 | 77 | 38 | 1 | 1 | serial | 10340024750 | 10301948502 | 10301948502 | 0 |
| c1-03 | 1 | 37636313 | 39 | 0 | 39 | 0 | 77 | 38 | 1 | 1 | serial | 9737400125 | 9702391165 | 9702391165 | 0 |
| c2-01 | 2 | 37636334 | 39 | 0 | 39 | 0 | 77 | 38 | 2 | 2 | overlap | 8251942209 | 8221308417 | 10324395331 | 2103086914 |
| c2-02 | 2 | 37636354 | 39 | 0 | 39 | 0 | 77 | 38 | 2 | 2 | overlap | 8625671000 | 8599929669 | 10744819085 | 2144889416 |
| c2-03 | 2 | 37636376 | 39 | 0 | 39 | 0 | 77 | 38 | 2 | 2 | overlap | 8357987500 | 8326533333 | 10394739085 | 2068205752 |
| c4-01 | 4 | 37636396 | 39 | 0 | 39 | 0 | 77 | 38 | 4 | 4 | overlap | 7849502500 | 7815431752 | 11211010420 | 3395578668 |
| c4-02 | 4 | 37636418 | 39 | 0 | 39 | 0 | 77 | 38 | 4 | 4 | overlap | 7851949417 | 7827296126 | 11280152541 | 3452856415 |
| c4-03 | 4 | 37636437 | 39 | 0 | 39 | 0 | 77 | 38 | 4 | 4 | overlap | 7690466000 | 7659420294 | 11036037087 | 3376616793 |

§30's provenance, per run and in the same order: `c1-01` — block 37636187, bound 1, reuse true, mode `build-only`, commit 0a1d867f5e6141a06b2e50a46cc3066e70d7cbf6, endpoint rpc-faa716cada04a9ef; `c1-02` — block 37636292, bound 1, reuse true, mode `build-only`, commit 0a1d867f5e6141a06b2e50a46cc3066e70d7cbf6, endpoint rpc-faa716cada04a9ef; `c1-03` — block 37636313, bound 1, reuse true, mode `build-only`, commit 0a1d867f5e6141a06b2e50a46cc3066e70d7cbf6, endpoint rpc-faa716cada04a9ef; `c2-01` — block 37636334, bound 2, reuse true, mode `build-only`, commit 0a1d867f5e6141a06b2e50a46cc3066e70d7cbf6, endpoint rpc-faa716cada04a9ef; `c2-02` — block 37636354, bound 2, reuse true, mode `build-only`, commit 0a1d867f5e6141a06b2e50a46cc3066e70d7cbf6, endpoint rpc-faa716cada04a9ef; `c2-03` — block 37636376, bound 2, reuse true, mode `build-only`, commit 0a1d867f5e6141a06b2e50a46cc3066e70d7cbf6, endpoint rpc-faa716cada04a9ef; `c4-01` — block 37636396, bound 4, reuse true, mode `build-only`, commit 0a1d867f5e6141a06b2e50a46cc3066e70d7cbf6, endpoint rpc-faa716cada04a9ef; `c4-02` — block 37636418, bound 4, reuse true, mode `build-only`, commit 0a1d867f5e6141a06b2e50a46cc3066e70d7cbf6, endpoint rpc-faa716cada04a9ef; `c4-03` — block 37636437, bound 4, reuse true, mode `build-only`, commit 0a1d867f5e6141a06b2e50a46cc3066e70d7cbf6, endpoint rpc-faa716cada04a9ef.

The route half of each run — the pipeline's own `route-run.json`, `metrics.json`, the latency session and the execution lane's empty files — is in `route-runs/<name>/`, named for the same run. Every run's `route-run.json` records the session id its latency directory was opened under.

## 5. RPC count per run (§23, §25, §16)
Every one of the 9 runs made 39 calls inside its simulation and 351 across the directory — eth_getStorageAt 180, eth_getCode 54, eth_getBalance 54, eth_getTransactionCount 54, eth_getBlockByNumber 9. The multiset is unchanged between arms, which is the shape §23 asks for: a bound that bought speed by asking for less would not be a scheduling experiment. `attempts` is 39 per run with 0 retried, so no call was made twice anywhere in this sample, and a retry would have been one record with two attempts rather than two records (crates/chain/tests/rpc_trace_safety.rs).

Two other sets of reads sit beside that count, and this directory neither folds them into it nor drops them to a zero. The first is measured, just not here: the 21 reads each run's route lifecycle made outside its simulation — eth_blockNumber 1, eth_call 18, eth_getBlockByNumber 2, 189 across the directory, the same multiset in every arm — held in `outside-simulation-rpc.json`, which also writes down that the simulation's 39 belong to the other file so that no call is in both lists. The second is measured by nothing in this directory, and `outside-simulation-rpc.json`'s `unreachable_reads` names its three sources: `eth_chainId`, asked before an adapter exists for a sink to attach to; the WebSocket transport, which a route run's HTTP lifecycle never reaches; and the execution lane's own second adapter — head, nonce, balance, fee parameters, receipts, submissions. That last one is what read the wallet: an `eth_getBalance` line is quoted in the preflight record of 9 of the 9 runs, and the 3 that reached `status = built` each quoted 3 reads of its funding snapshot besides, 9 in all. Those 18 quoted reads appear in no table above, and whatever else that untraced socket asked is not even quoted — §12 carries both as limitations, not as zeros.

## 6. Observed max concurrency, and the time beside it
| arm | simulation duration min / p50 / max ns | RPC union p50 ns | RPC sum p50 ns | RPC overlap p50 ns | serial wait p50 ns | gap p50 ns |
|---|---|---|---|---|---|---|
| C1 | 9737400125 / 9933816167 / 10340024750 | 9899301618 | 9899301618 | 0 | 9899301618 | 32466127 |
| C2 | 8251942209 / 8357987500 / 8625671000 | 8326533333 | 10394739085 | 2103086914 | 6258327581 | 26273250 |
| C4 | 7690466000 / 7849502500 / 7851949417 | 7815431752 | 11211010420 | 3395578668 | 5675387961 | 28266581 |

Each arm has 3 samples, so `p50_ns` is a measured median and `p90_ns`, `p95_ns`, `p99_ns` are `null` with an `insufficient_sample` reason and its minimum beside them (§21). Nothing here subtracts one arm from another, and the table says why in its own words: §20 lets these runs sit on different blocks, so a difference between two arms' published medians is a difference between two blocks as much as between two bounds. Each arm states its own integers and the reader is told which subtraction this evidence can carry.

## 7. Correctness (§17, §34)
`correctness-comparison.json` compares the three fixed-block arms at block 37191169 on one commit (0a1d867f5e6141a06b2e50a46cc3066e70d7cbf6), and the answer is that they are the same run: 21 fields compared, of which 21 are `identical_across_bounds`, a whole-result fingerprint identical at `"0xd54891737d934871622ce879b55c67a90bba5b343a7954821b20d138ad9dbf67"`, and every gate in it true (every_bound_serial_at_1=true, every_state_read_named_the_pin=true, no_bound_exceeded=true, no_retry_in_this_fixture=true, overlap_positive_above_1=true). The comparison is field by field — outcome, revert, gas used, gas charge, logs, return data, net profit, state changes, plan summary, slippage, measurements and the rest — and not a profit-only one.

Its own witnesses are in `fixed-block/{c1,c2,c4}.json`: bounds 1, 2 and 4, run walls 656047625 ns, 496593750 ns and 484140000 ns, with wire overlap 0 ns, 116218626 ns and 151766041 ns on the same block. A bound of 1 is serial there and `gates.every_bound_serial_at_1` is the field that says so.

The fixture's call surface is its own, and it is not §5's: at block 37191169 one fixture arm makes 40 requests — 21 `eth_getStorageAt`, 6 `eth_getCode`, 6 `eth_getBalance`, 6 `eth_getTransactionCount`, 1 `eth_getBlockByNumber` — where one live run makes 39 (20 / 6 / 6 / 6 / 1 in the same order). The equality a reader will notice is between two different sets: `call_counts.state_reads_per_bound` is the four chain-state methods with the header left out (39), while §5's 39 is every call the simulation's sink saw, the header included. The fixture is one route instance replayed on a historical block, and no figure in §5 depends on it.

## 8. Did this run sign anything, broadcast anything, spend any ETH? (§19, §38)
No, no, and no. `signed-transactions.jsonl` holds 0 lines across the 9 route runs and `submissions.jsonl` holds 0. The submission method a broadcast would use appears in no recorded call and in no file of this directory. Every run is `execution_mode = build-only`, every `route-run.json` says `successful_real_arbitrage = false`, and every execution record has `completed = false`, an empty transaction list, `after = null`, `cost = null` and an empty delta audit — so the asset difference that would show ETH moving is not merely zero, it was never taken. Every run read the wallet it would have funded, and read it twice where it got far enough to build a sequence: 9 preflight input-asset checks, plus one native read in each of the 3 funding snapshots of `c2-02`, `c4-01`, `c4-03`. All 12 reads returned the same balance — `0x5b889028070f10` = 25764375608889104 wei — each of them an `eth_getBalance`, with `eth_call balanceOf` on the route's two tokens beside it in a snapshot: a read is not a spend, and the field a spend would move does not move across any of them. 6 runs stopped one stage earlier, at the fee check that runs before any transaction is built: the base fee read at their block came back above the ceiling the plan was priced against, which is a market verdict about this route at this second and says nothing about how long the state reads took — all 9 of them ran their simulation, and that is what §19 asks of a run. A signing key enters this build through one door, the `GIWA_EXECUTION_PRIVATE_KEY` environment variable, and §38's scan is the one the directory can carry: no long hex word appears in these files that a run did not write into its own evidence first.

## 9. How to regenerate this directory
```bash
# 1. the build, with the recipe this repository needs on this machine
CC=clang CXX=clang++ CXXFLAGS="-include cstdint" GIT_REVISION=$(git rev-parse HEAD) \
  cargo build --bin evm-mev-bot

# 2. the fixed-block arms (§17) and their comparison, on the recorded dump at block 37191169
M833_ABC_EVIDENCE=data/evidence/m8/concurrency \
  cargo test -p evm-simulation --test concurrency_abc

# 3. one live run of one arm (§19): three per bound, bound 1 then 2 then 4. `GIWA_RPC_URL` holds
#    the endpoint; the run opens a session directory under each root, which is then renamed to the
#    arm's run name below.
./target/debug/evm-mev-bot arbitrage \
  --rpc-url "$GIWA_RPC_URL" --execution-mode build-only \
  --sender <the test sender> --input-token 0x4200000000000000000000000000000000000006 \
  --candidate-mid <the router> --candidate-pool <pool A> --candidate-pool <pool B> \
  --input-wei 100000000000000 --fee-num 997 --fee-den 1000 \
  --fee-evidence data/evidence/m7/candidate-fee-measurement.json \
  --market real-market \
  --market-evidence "reserves and blockTimestampLast read at the pinned live head by this run" \
  --latency-trace --rpc-trace --diagnose-state-acquisition \
  --state-read-concurrency <1 | 2 | 4> \
  --evidence-dir data/evidence/m8/concurrency/route-runs \
  --rpc-output data/evidence/m8/concurrency/runs

# 4. the pooled tables, this README, and the byte-for-byte check
M833_CONCURRENCY_REFRESH=1 cargo test -p evm-pipeline --test concurrency_evidence
cargo test -p evm-pipeline --test concurrency_evidence
```

Step 4 is the only thing that writes the twelve files in this directory, and its second command is
what fails if any of them was edited by hand. `GIWA_RPC_URL` is set in the environment rather than
repeated here: an endpoint is not evidence, and the directory names it as a digest, printed under
this block. Step 3's `<the test sender>` and `<the router>` are the sender and router this route
uses, named in every run's `route-run.json`.

The endpoint those runs went to is recorded as `rpc-faa716cada04a9ef` — a digest, not a URL.

## 10. Provenance
One git revision (0a1d867f5e6141a06b2e50a46cc3066e70d7cbf6) and one execution mode (build-only) across all 9 runs; an `assembled_from` block naming every run and the figures it contributed is in the header of each pooled table, including the two this milestone added. `concurrency-summary.json` groups by the bound a run was configured with and counts runs whose line carries no dispatch report separately (0, of 9); `dependency-map.json` records the dependency each state read has and what the runs did with it.

## 11. Which block every read was taken at (§18)
9 of the 9 simulations carry state reads, over 342 state calls; 342 of those calls name a pinned chain and block in both their outgoing parameter and their dedup key, and 9 name this run's own block in both. The block parameters ever seen on the wire are 37636187,37636292,37636313,37636334,37636354,37636376,37636396,37636418,37636437. A `latest`, `pending` or absent tag would appear in that last list as text rather than be argued away.

## 12. What this directory does not say (§20, §35)
The nine live runs sit on nine different blocks, so a difference between two arms' medians is as much a difference between blocks as between bounds — the same-block comparison is `fixed-block/`, and it is the only pair of walls in this directory that subtracts cleanly. Concurrency here is simulation-local and bounded: it says nothing about batch JSON-RPC, multicall, a connection pool, a prefetch, or a cache shared across simulations, none of which this build has. The RPC surface is wider than the two tables that count it: the execution lane opens its own sink-less adapter, and `unreachable_reads` lists what can go through it unseen — head, nonce, balance, fee parameters, receipts, submissions — of which only the 18 reads the 9 `route-run.json` files quote have a line anywhere here. So 「39 + 21」 is a run's watched surface, not its total. And an arm that got faster on a live block is not a production default: §3's 「尤其禁止：把本轮实验结果直接升级成默认生产并发实现」 is why the CLI's bound stays 1 when the flag is absent.
