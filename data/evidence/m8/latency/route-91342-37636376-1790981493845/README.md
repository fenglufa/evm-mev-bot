# M8.1 latency baseline

Written by git revision `0a1d867f5e6141a06b2e50a46cc3066e70d7cbf6` with execution mode `build-only`.

| file | contents |
| --- | --- |
| `traces.jsonl` | 1 line(s): one JSON trace per opportunity lifecycle, always fourteen stages per line, nanoseconds |
| `summary.json` | the same traces folded into per-source percentile tables, plus the §44 metadata |

Sources in this directory: `live` — 1 trace(s), chain id 91342.

## Reading rules

- `null` means **not measured**, never zero. A stage the lifecycle never reached is `skipped` and has no duration; a percentile with too few samples behind it is `null` with an `insufficient_sample` reason.
- Percentiles are nearest-rank — the same computation M5's `metrics.json` uses. A rank is reported only when its index is strictly below the last sample, so it is never a reprint of `max`: p50 needs 2 sample(s), p90 10, p95 20, p99 100.
- Durations come from one monotonic clock, never from wall time. `granularity` says how finely that stage's instants were actually taken; `millisecond` means the figure is a reading of a stamp the run wrote anyway, not a nanosecond measurement.
- `live`, `replay` and `fixture` rows are never blended into one percentile, and a fixture's number is a test's latency rather than a market's.
- `domain` is §11's cost class for the span as it was actually earned, and `nominal_domain` is the class the stage's name suggests; where they differ, the span held a node (a simulation that read its state over RPC), and `totals_ns` files that time under `mixed_reads_and_compute` rather than under `local_processing`.
- `generated_at_unix_ms` is metadata about the file and enters no duration.
