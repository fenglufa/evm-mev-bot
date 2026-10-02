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
- Detection-stage reads (the reserves priced before a simulation is built) are not in
these traces: the sink is attached to the adapter the simulation itself reads through, so a call in this directory is a call that simulation made.
- Submission, receipt and header reads on other transports — the WebSocket client's own request path, and everything `crates/execution` sends through its submitter — are outside what this adapter sees, and are named here rather than counted as zero.
- A source whose adapter cannot hand out a traced clone reports no sink at all. That is recorded as `diagnosis_refusals`, not as a simulation with zero calls.

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
