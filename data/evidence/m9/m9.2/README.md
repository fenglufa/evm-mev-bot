# M9.2 证据目录（V2 历史市场状态重建 + 目标块新图）

## 一句话（白话版）

M9.1 验出 80 个池子，但只有 24 个能进图：因为图只肯用「和它
同一个块的价格」，其余 56 个池子的价格来自更早的块。M9.2 做的事，是去链上把每个
池子在目标块之前**最后一次真实报价**找回来，并且证明那之后到目标块之间它没有再报过价
——于是图还是那张图，规则一个字没改，只是终于拿到了它能接受的输入。

## 这次到底证明了什么

| 问题 | 数字 | 逐条证据 |
|---|---|---|
| M9.1 验过的池子 | 80 | 输入是 `data/evidence/m9/m9.1/raw/pass-a.json (M9.1 raw pass a)`，未重新发现 |
| 目标块 37224031 上状态可重建 | 80 | `reconstruction.json`，每池一行 |
| 目标块上没有任何权威 Sync | 0 | `rejected-pools.json` |
| 有 Sync 但某侧储备为 0 | 0 | `rejected-pools.json` |
| 最终进入目标块图 | 80 | `graph-integration.json` |
| 被图跳过（每个恰好一个原因） | 0 | `graph-integration.json#skipped` |

两个等式必须成立：`80 = 80 + 0 + 0`（§21），
`80 + 0 = 80`（§22）。它们由这个目录自己算出来，不是抄来的。

## 为什么要分两种口径讲「状态」

图里的边带着它自己的时间戳：价格出自哪个块、哪条日志。这一点在 M9.2 之后仍然成立——
目标块是「这张图算哪一块」，最后报价位置是「这个数从哪来」，两个数各记各的，
谁也不许覆盖谁（§14「不许销毁来源」）。所以「在 37224031 有效」**不等于**「价格是 37224031 报的」，
它等于「37224031 之前最后一次报价是 X，并且我们扫过了 X 到 37224031 之间所有块，没有更新的报价」。

## 表格清单

- `graph-integration.json` — 100068 字节
- `manifest.json` — 5847 字节
- `node-capacity.json` — 8896 字节
- `reconstruction.json` — 60766 字节
- `rejected-pools.json` — 2526 字节
- `rpc-calls.json` — 6269 字节
- `strategy-comparison.json` — 11256 字节
- `summary.json` — 15027 字节
- `sync-events.json` — 67073 字节

## 原始记录

`raw/` 下每一次实跑的文档与 RPC 轨迹都在这里，`manifest.json#raw_files` 逐个列字节数与摘要。
实跑命令（§11 探针 → §12 两臂对照 → §20/§27 目标块两把）：

```text
GIWA_RPC_URL=<endpoint from the environment> M92_STRATEGY=<pool|census> M92_TARGET=37224031 \
cargo test -p evm-discovery --test sync_reconstruction_live -- \
--ignored --nocapture --test-threads=1 <target name>
```

## 重算与门禁

```text
M92_EVIDENCE_REFRESH=1 cargo test -p evm-discovery --test reconstruction_evidence_gate -- --test-threads=1
cargo test -p evm-discovery --test reconstruction_evidence_gate -- --test-threads=1
```

第一条重建并写入；第二条只读比对，任何一格对不上就失败。此外
`the_independent_recompute_agrees_with_every_committed_number` 不调用任何 discovery
函数，直接按原始字段重算；`an_injected_wrong_number_is_caught_by_the_recompute` 往原始
记录里塞一个错数，确认那道门真的会红——只有前一道门绿的目录不能算被验过。

## 这个目录没有说的事

- 没有任何池子被声称「可交易」：M9.2 到 GraphSnapshot 为止，寻路是下一个里程碑（§30）。
- 没有任何池子被声称「新鲜」：这里证明的是**历史块**上的状态，不是当前状态（§36）。
- 费率依旧全未举证（`fee: null`），和 M9.1 留下时一样。
- 全程只读：`eth_getLogs` / `eth_chainId`，签名 0、广播 0、真实套利 0（§28）。这条
不是自我声明：`rpc-calls.json#calls_outside_the_read_only_set` 是把 `raw/` 下
五份轨迹逐行数出来的，`manifest.json#raw_files` 列全了那五份。一个 sink 只留
20000 条轨迹、其余只计数不落地，所以「逐行数出来」覆盖的是轨迹里的调用；每个跑
自己记三个数——落地的行数、超出上限被计数没落地的次数、扫描按块区间本身该发多少次
请求——判据是「落地 + 计数 = 请求 + 一次链 ID」（
`summary.json#rpc.per_run_call_accounting`，四个跑一行一条，本次合计被计数而未
落地的调用 1440 次）。一次扫描解释不了的调用，无论有没有被写进轨迹，都会
让这个和对不上。
- 端点在本目录只以摘要 `rpc-faa716cada04a9ef` 出现，URL 字面量一个都没有（有测试逐文件检查）。
