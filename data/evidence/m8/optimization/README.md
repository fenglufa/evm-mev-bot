# M8.3.1 — state read reuse A/B evidence

One milestone's single variable, measured twice on the live chain: the same route run 
with state read reuse on and with it off, plus a controlled A/B over one pinned block 
where the two arms' results can be compared rather than only described.

Nothing in this directory was typed in. Every file here is assembled by 
`crates/pipeline/tests/reuse_ab_evidence.rs` from the runs under `raw/`, and the suite 
requires these committed files to be byte-identical to a fresh assembly of those same 
runs — so an edited number fails a test. Regenerate with `M831_AB_REFRESH=1 cargo test 
-p evm-pipeline --test reuse_ab_evidence`.

## What is here

- `m8.3.1-ab.json` — §15's headline metrics, each with its per-run figures beside its 
total, the reduction in integer terms, and the controlled stub A/B embedded as it was 
written.
- `rpc-count-comparison.json` — §7's per-method request counts for both arms, the calls 
removed per method, and the duplicate tallies.
- `cache-stats.json` — §12's hits and misses per kind per arm, each beside that kind's 
wire count, so `misses_equal_wire` can be checked without trusting either number.
- `baseline/run-00N.json`, `cached/run-00N.json` — one file per run, carrying §14's 
per-simulation fields: `block`, `simulation_id`, `arm`, `rpc_count`, `cache_hit`, 
`cache_miss`, `simulation_duration`, `result`.
- `raw/` — what the runs themselves wrote: each run's evidence directory, its 
`diagnosis/` directory from `--rpc-trace`, and its `latency/` directory from 
`--latency-trace`, plus `raw/controlled/m8.3.1-ab.json` from the stub A/B. Every 
figure above is read out of these files.

## Where each number comes from

| figure | file | key |
| --- | --- | --- |
| `rpc_count` | `raw/<arm>/run-00N/diagnosis/<session>/simulation-traces.jsonl` | `call_count`, re-derived from `calls[]` and compared |
| per-method counts | the same line | `methods[].count`, recounted from `calls[]` |
| `cache_hit`, `cache_miss` | the same line | `state_read_cache.cache_hits`, `.cache_misses` |
| duplicate reads | the same line | `duplicates.duplicate_state_reads` |
| `simulation_duration` | the same line | `simulation_duration_ns` |
| the same span, timed again | `raw/<arm>/run-00N/latency/<session>/summary.json` | `sources[].latencies_ns.simulation_duration.min_ns` |
| `rpc_wall_duration` | the same line | `rpc.rpc_wall_duration_ns` |
| which arm a run was | `<session>/route-run.json` | `state_read_reuse` |
| `result` | `<session>/route-run.json` | `simulation.status`, `.completed`, `.outcome`, `.fingerprint` |
| result equality | `raw/controlled/m8.3.1-ab.json` | `comparison.result_equal` |

## Two accounts of one boundary

The wire count and the cache tally are independent measurements of the same thing: one 
counts calls that reached the node, the other counts lookups the boundary answered. In 
the reuse arm they reconcile exactly — every miss is a wire call and every wire call is 
a miss (`misses_equal_wire`). In the baseline arm the account kinds carry no lookups at 
all, because the switch does not let the boundary be asked about them.

## What these runs did and did not do

Both arms ran `--execution-mode build-only`: no ETH was spent, nothing was signed, 
nothing was broadcast (§14). Each run's simulation completed; what happened after the 
simulation belongs to M7's lifecycle ladder and is recorded per run in `refusal` and 
`successful_real_arbitrage`, untouched by this milestone.

## Runs read

baseline:
- `raw/baseline/run-001` — session `route-91342-37607876-1790952994619`, pinned block 37607876, git revision `33c4d6b`
- `raw/baseline/run-002` — session `route-91342-37607984-1790953102182`, pinned block 37607984, git revision `33c4d6b`
- `raw/baseline/run-003` — session `route-91342-37608070-1790953188257`, pinned block 37608070, git revision `33c4d6b`

cached:
- `raw/cached/run-001` — session `route-91342-37607780-1790952898072`, pinned block 37607780, git revision `33c4d6b`
- `raw/cached/run-002` — session `route-91342-37607955-1790953073809`, pinned block 37607955, git revision `33c4d6b`
- `raw/cached/run-003` — session `route-91342-37608041-1790953159484`, pinned block 37608041, git revision `33c4d6b`
