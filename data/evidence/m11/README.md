# M11 evidence — multi-hop pricing, optimisation, simulation, risk, lanes

§42's tree, assembled by `crates/simulation/tests/multihop_evidence.rs` and checked by `crates/execution/tests/multihop_evidence_gate.rs`. The gate reads these files and writes nothing.

## What is here

```text
data/evidence/m11/pricing/recorded_2hop.json
data/evidence/m11/pricing/declared_3hop.json
data/evidence/m11/pricing/declared_4hop.json
data/evidence/m11/optimizer/recorded_2hop.json
data/evidence/m11/optimizer/declared_3hop.json
data/evidence/m11/simulation/recorded_2hop.json
data/evidence/m11/simulation/declared_3hop.json
data/evidence/m11/risk/decision.json
data/evidence/m11/lanes/lane_matrix.json
data/evidence/m11/controlled/2hop/chain.json
data/evidence/m11/controlled/3hop/chain.json
data/evidence/m11/real/execution.json
data/evidence/m11/real/failure.json
data/evidence/m11/real/reconciliation.json
data/evidence/m11/manifest.json
data/evidence/m11/README.md
```

Every row carries the pipeline's figure and this file's own recomputation of it beside each other: pricing as a constant-product fold over the published reserves and fees, route identity as a minimum rotation over the published edge list, the optimizer's best as a re-ranked scan over a grid rebuilt from the published domain and policy, the simulation's hashes as keccak over the published bytes, the risk decision as the six comparisons §26–§29 make, the lane ledger as a capital addition.

## Commands

```text
assemble:  CC=clang CXX=clang++ CXXFLAGS="-include cstdint" cargo test -p evm-simulation --test multihop_evidence -- --test-threads=1
check:     CC=clang CXX=clang++ CXXFLAGS="-include cstdint" cargo test -p evm-execution --test multihop_evidence_gate -- --test-threads=1
```

## What this directory is not

No row here is a real-market verdict (§41). The three-hop's pools, the four-hop's whole graph, the executor deployment and the funding rows are declared; the recorded pair's reserves are real and its execution is simulated. `real/` answers `UNKNOWN` to all three of §42's questions, with the reason each one has. `rpc_count` is 0: every run here is served from a committed dump file by a provider that holds no endpoint, and the ignored capture test that does pay an RPC price publishes its own measured cost elsewhere.

Nothing here signs, broadcasts, or holds a key.
