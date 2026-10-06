# M9.3 证据目录（有界环搜索：GraphSnapshot → CycleCandidate[]）

## 一句话（白话版）

M9.2 交出的是某一区块上「哪些池子的价格是成立的」。M9.3 只做一件事：在这些池子连成的市场图上，把**能绕回起点的闭环路线**全部数出来——最多 3 跳，同一个池子不许走两遍，中途站过的代币不许再站。产出是 68 条**环候选**（cycle candidates），不是 68 个套利机会：这一层不看储备、不算金额、不出利润，它只回答「图里有几条路能绕回来」。

## 这次到底证明了什么

| 问题 | 数字 | 逐条证据 |
|---|---|---|
| 读的图（M9.2 目标块） | 50 代币 / 160 有向边 / 80 池 | `summary.json#graph_*`，拓扑逐边对照 `data/evidence/m9/m9.2/graph-integration.json` |
| 环候选总数 | 68 | `cycles.json`，每条一行 |
| 其中 2 跳 / 3 跳 | 66 / 2 | `cycles.json#counts` |
| 费率全已举证 / 至少一边未举证 | 0 / 68 | `fee-status.json` |
| 同一规范键重复出现 | 0 | `summary.json#duplicate_cycles` |
| 两次运行是否逐字节相同 | true | `determinism.json` |
| DFS 状态数 / 墙钟耗时 | 14860 / 31 ms | `benchmark.json` |
| 被至少一条候选走到的边 | 138 / 160 | `candidate-edges.json#totals` |

两个等式必须成立：`68 = 66 + 2`，`0 + 68 = 68`。它们由这个目录自己算出来，不是抄来的。

## 为什么「费率全未举证」不是坏消息

M9.2 留下的图里，160 条边的 `fee` 都是 `null`——这条链上没人替这些池子举证过费率。所以本目录 68 条候选**全部**是 `incomplete`。这不是搜索失败，恰恰是它应当给出的答案：任务书 §16–§18 要求「未知」永远不许被读成 0、也不许默认成 997/1000，而 §35 的 NC6 要求「带未举证费率的候选照样被枚举，但一条都不能进财务层」。这一格等于把 NC6 在真实规模上又跑了一遍：68 条可枚举、0 条可用。

## 表格清单

- `benchmark.json` — 2757 字节
- `candidate-edges.json` — 43885 字节
- `cycles.json` — 67629 字节
- `determinism.json` — 2868 字节
- `fee-status.json` — 49426 字节
- `manifest.json` — 4027 字节
- `negative-controls.json` — 4602 字节
- `summary.json` — 4470 字节
## 原始记录

`raw/` 是装配那一次跑的完整输出（候选逐条 + 状态数 + 墙钟毫秒），`manifest.json#raw_files` 逐个列字节数与摘要。图本身不重复存一份：它由
`data/evidence/m9/m9.1/raw/pass-a.json` 与 `data/evidence/m9/m9.2/raw/reconstruction-pass-a.json` 用生产函数重建，再逐边对照 `data/evidence/m9/m9.2/graph-integration.json`；重建不一致，这个目录就没资格说自己读的是 M9.2 的图（§36）。

## 重算与门禁

```text
M93_EVIDENCE_REFRESH=1 cargo test -p evm-discovery --test pathfinder_evidence_gate -- --test-threads=1
cargo test -p evm-discovery --test pathfinder_evidence_gate -- --test-threads=1
```

第一条重建并写入；第二条只读比对，任何一格对不上就失败。此外
`the_independent_recompute_agrees_with_every_committed_number` 不调用任何 pathfinder / discovery 函数，把 `raw/` 与 M9.2 已提交的表当 JSON 读进来逐项重算；`an_injected_wrong_number_is_caught_by_the_recompute` 分别篡改原始记录与证人表（删一条候选、把一条候选的规范键换成它自己的另一个轮转、给某条路线走过的每条边都补上已举证费率、删掉某条候选真正走过的那条边），确认四类缺陷各自都会让门变红——只有前一道门绿的目录不能算被验过。

## 这个目录没有说的事

- 没有任何一条候选被声称「有利润」「可成交」：gross / net / realized profit 一律记 `N/A`（§56——写 `0` 是另一句话，意思是「算过了，不值钱」）。
- 术语固定：本目录的计数叫 **cycle candidates（环候选）**，不叫 arbitrage opportunities，也不叫 profitable opportunities（§38/§55）。有测试逐表扫描字段名形状，防止任何像「机会 / 金额 /  gas / 模拟状态」的字段被塞进候选里。
- 全程 0 次 RPC：本目录的输入全是仓库里已提交的 JSON；寻路 crate 的生产依赖只有 evm-core / evm-graph / serde / thiserror 四个，没有一个能开网络（测试逐行读那两个 `Cargo.toml` 核对，§42）。
- discovery crate 只在 dev-dependencies 里引用 pathfinder，生产流水线没有任何新增接线（§50：candidate → risk → sign → submit 这条链在本里程碑不存在）。
- 端点在本目录只以摘要 `rpc-faa716cada04a9ef` 出现，URL 字面量一个都没有（有测试逐文件检查）。
