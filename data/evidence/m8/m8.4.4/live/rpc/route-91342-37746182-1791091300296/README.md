# M8.2 simulation state acquisition diagnosis

1 simulation(s) are recorded here — one line of `simulation-traces.jsonl` each, with the
per-method and per-source tables in `rpc-summary.json`, `simulation-summary.json` and
`duplicate-reads.json`. Nothing here changed what the run decided: the difference between a
traced run and an untraced one is that a call which was already about to happen got two
clock readings and one `Vec` push around it.

What the run was configured to do about the repeats it shows is a separate question, and
it is answered per run rather than per directory: a build from M8.3.1 onward may reuse a
state read it already made at the same pinned block, so the same line can describe either
arm. Each line's `state_read_cache.reuse` field says which one it was. `duplicate-reads.json`
counts how many simulations ran with reuse on and off, and sums the tallies over both — so
an A/B comparison is two runs into two directories, never one directory with both arms in
it.

## Data source

Each line names its own `source`: `live` (a node answering over the network), `replay` (recorded state read from disk), `fixture` (a deterministic in-process stand-in). The tables are built per source and no figure here adds two of them together. A fixture's numbers demonstrate the machinery; they are not market latency.

The bucket key is `sources_are_never_blended: true` in each summary file.

## Sample policy

Percentiles are nearest-rank with M8.1's minimum sample counts: p50 needs 2, p90 10,
p95 20, p99 100. A rank too few samples cannot support is `null` with
`"reason": "insufficient_sample"` and its `minimum_samples` beside it, never an
extrapolated number. A ratio is two integers — a numerator and a denominator — and
never a float (§13). `generated_at_unix_ms` is a wall-clock reading and is used for
nothing but the file's own timestamp: no duration in these files is computed from it.

## RPC instrumentation methodology

A call is recorded at the one place this repository turns a method name and a params
array into bytes on the wire (`HttpChainAdapter::request_with`). So:

- `method` is the literal string that went out, not a name inferred from a caller.
- `duration_ns` is the whole logical call: construction, the POST, the node's own
work, the response body and its decode. §17's sub-split is not separable at this
point and is recorded as `breakdown_unavailable`; the same sentence is carried in
every summary as `provider_duration_field` so it travels with the numbers it
qualifies, not only in prose.
- `attempts[]` holds one entry per HTTP try, so a call that burned the client's 20 s
timeout and then retried is distinguishable from one slow answer — which is what
lets a reader tell §25's E (connection) from its B (node) at all.
- Every stamp is nanoseconds since this run's own monotonic origin, the same origin
the M8.1 latency trace reads, so an event and a stage span subtract cleanly.
- **The instrumentation issues no requests.** It wraps calls the run was already
going to make (§18); it never asks for a block, a balance or a storage word in
order to complete a record.
- A `trace` flag that is off leaves the request path as it was, including the single
retry and the order of calls (§19).

`provider_total_duration: this build records one call at the point it becomes bytes on the wire, so request construction, the wait on the node, response decoding and state conversion are not separable there (§17's breakdown_unavailable) and are reported as one provider duration`

## Duplicate detection methodology

A state read's identity is §12's tuple for its method, built from the params already
in hand: storage by (chain, block, address, slot); balance, code and transaction
count by (chain, block, address); eth_call by (chain, block, to, data) plus a value
term when the request carried one; blocks by (chain, block-or-hash, hydrated). Hex
heights normalize to decimal and a tag (`latest`, `pending`, `finalized`, `safe`) is
kept as the tag, so a numbered read and a tagged read never key alike. One word
written two ways — `0x8` and a zero-padded 64-digit slot, or an address in either
checksum case — is one key. A method with no rule here, or params that do not match
the rule, is counted as a call and reported under `unkeyed_calls` with the `key_note`
naming which of the two it was; it is not handed a borrowed key and not dropped.

`duplicate_state_reads` counts repeats, not asks: a read made three times adds 2, so
the figure names the avoidable asks rather than the total ones.

`state_read_cache` is a second, independent account of the same subject, counted at the
boundary the simulation reads through rather than at the wire: its hits are the asks
that never became calls. The two are meant to be checked against each other (§7), which
is why both are here and why neither is derived from the other.

## Serial / overlap methodology

One sweep over the call intervals measures, per simulation, how much of the
simulation's own window had exactly one call in flight
(`serial_wait_duration_ns`), how much had two or more (`rpc_overlap_duration_ns`,
which is `sum − union`), and how much had none (`rpc_gap_duration_ns`).
`rpc_wall_duration_ns` is last end minus first start; `non_rpc_duration_ns` is the
simulation's span minus the *covered* measure — not span minus sum, which is the
mistake §11 calls out once anything overlaps. Intervals that merely touch count as
serial, not as overlapping. A simulation that made no calls reports `null` for every
one of these, never 0: `0` would say the RPC part was instant, which is a different
fact (§9).

## Missing data

`null` means nothing was measured, and the field beside it says why. `0` means a
measurement landed on zero. A series with one sample carries a `note` saying it is
one measurement and not a distribution. A per-method sample list that reached 10 000
entries reports `samples_capped`. `calls_clipped` counts calls whose stamps fell
outside the simulation's own window and were clipped to it; `simulations_without_calls`
counts simulations that ran, were traced, and asked the node for nothing.

## Known limitations

- `live`: 1 simulation(s) measured fully serial, 0 with any overlap, 0 with no recorded call inside the window. A source with one simulation cannot support p90 or above, and says so rather than estimating.
- The record is taken at the wire, so a slow decode and a slow node are one number (§17). What separates them here is the attempt list, not a sub-timer.
- Detection-stage reads (the reserves priced before a simulation is built) and the gate's own reads are **not** in these traces: the sink here is attached to the adapter the simulation itself reads through, so a call in a trace line is a call that simulation made. The rest of the lifecycle is reported beside them, in `outside-simulation-rpc.json`, and classified by the stage that held it.
- The dependency file says which leg issued a read and what that leg's own proof is; it does not say whether the read *could* have been issued earlier. Whether a slot is needed before a later call is a question about values, and the only values recorded here are the ones the requests carried.
- This run did not ask for M8.4.2's cross-stage tables (`duplicate-matrix.json` and the three beside it are written only under that switch), so nothing here says whether two stages of this run asked the same thing.
- Submission, receipt and header reads on other transports — the WebSocket client's own request path, and everything `crates/execution` sends through its submitter — are outside what this adapter sees, and are named here rather than counted as zero.
- A source whose adapter cannot hand out a traced clone reports no sink at all. That is recorded as `diagnosis_refusals`, not as a simulation with zero calls.

## Outside-simulation RPC

`outside-simulation-rpc.json` counts the lifecycle's other reads and keeps them out of the 39-call state-read baseline, which is §14's rule: a saving on the gate's reads and a saving on state acquisition are different findings, and adding one to the other would make the first look like the second. A call is classified by the stage span that holds its `started_ns` — the latency trace's own stamps, on the same monotonic origin, so containment is a comparison rather than a second measurement. This run: simulation-state=0, simulation-context=2, detection=6, preflight=24, orchestration=0, unknown=0.

Three reads no sink of this build can see are listed in the file as `unreachable_reads` instead of being counted as zero: `eth_chainId` inside `HttpChainAdapter::connect` (it is the call that produces the adapter a sink could be attached to), the execution lane's own adapter, and the WebSocket transport. A span written from millisecond stamps holds nothing here: it is marked unusable rather than widened to fit a nanosecond call.

## Acquisition tables and bottleneck classification

§17's switch was on for this run, so the directory also holds four tables grouped over the lines above rather than measured again: `storage-breakdown.json` (every storage read with its address, slot and duration, then grouped by address — §6), `account-read-matrix.json` (code, balance and nonce per address — §9), `rpc-gaps.json` (every idle stretch between calls, one sample per wait — §12) and `bottleneck-classification.json` (§25's A–G verdict). Each is built from the `duration_row` projection of these same lines, so a figure that appears in both a line and a table is one figure copied, not two computed. This run: live → primary G (Mixed / insufficient evidence), secondary [A, B, C].

The classification file publishes the threshold it applied beside the integers it applied it to, per source, and never adds two sources together. A `not_measured` category is an absent figure; `ruled_out_by_measurement` is a figure that landed on the safe side of a declared line. §18: naming a category is the whole of what this file does — the directions it makes possible are written in the completion report as candidates and none of them is implemented here.

## Storage dependency and whole-pipeline RPC

M8.4.1's switch was on, so the directory also holds six tables that answer 「why must these reads be serial」 and 「where does the run's non-simulation RPC time go」, and answer nothing else: `storage-reads.json` (one §4 record per `eth_getStorageAt` — caller, address, slot, height, dependency, depends_on, and the evidence beside each), `dependency-map.json` (`nodes` / `edges` / `summary` over the same rows), `dependency-summary.json` (§19's one-screen tally), `pipeline-calls.json` (every call of both sinks of every run, one row each), `pipeline-summary.json` (§17's per-run totals and per-stage rows) and `stage-summary.json` (the same stage arrays lifted out per run). Not one of them issues a request or reads a clock: each is a re-read of the trace lines and the call rows this directory had already written.

This run: 20 storage read(s) — 0 independent, 19 ordered, 1 unknown; over pairs of reads within one simulation, ordered=190, independent=0, undecidable=0, contradiction=0.

Two words are load-bearing here. §5: different slots are *not* evidence of independence — a read is `independent` only when the leg that issued it is provably sequential in the other direction (the materialised audit pass, whose whole list exists before any of it is asked for), and the file names that leg and its proof rather than asserting it. §3: an `unknown` is never promoted to `independent`; the majority of read pairs in a normal run are `undecidable` because they sit on different legs, and this file says so instead of picking a side. `not observed` likewise appears where a surface has no sink at all — it is not a zero (§11), and `pipeline-summary.json`'s `not_observed` column lists which surfaces those are.

## What this directory is not

It is not a result. It records calls, and records them the same way whichever arm the
run was configured for.

Two of §26's candidates were later built; the rest were not. State read reuse (M8.3.1)
exists and is switched per run by `--no-state-read-reuse`, so a duplicate that still
appears in a line here is a duplicate that arm was left in place to measure — read
`state_read_cache.reuse` before concluding anything from a repeat. RPC batching, reading
the two venues concurrently, request prefetch and connection tuning are still not
implemented, and no code in this build does any of them: a simulation's calls stay one
per state read, in the order the EVM asked for them.
