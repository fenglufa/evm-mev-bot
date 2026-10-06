# M9.4 证据目录（官方 Flashblocks → Early Radar）

## 一句话（白话版）

任务书把 Flashblocks 想成一条会推帧、每帧自带序号的流。真实端点不是这样：它只回答「再问我一次那个还没封的块」，
而那个块在我每次去问的时候都更长一点。这个目录记录把它做成**早雷达**之后实测到的一切：162 次预确认读取、1507 条事件、88 个高度（其中 5 个真的长出过 ≥3 种视图）、116 次 canonical 对账（覆盖 91 个高度）、4240 笔交易、命中已注册池 **0** 次。

做不成的三件事同样如实登记：帧序号在协议里不存在（所以「缺一帧」这句话在这条传输上问不出来）；pending 视图的 `stateRoot` 是全零占位（所以它没有任何状态凭据）；真实流量里没有任何一笔交易打中我们举证的 80 个池。

## 这次到底证明了什么

| 问题 | 数字 | 逐条证据 |
|---|---|---|
| 预确认读取 / 事件 | 162 / 1507 | `raw/window-*/`，逐行落在 `flashblocks.jsonl` |
| 高度 / 长出多种视图 / ≥3 种视图 | 88 / 64 / 5 | `sequence.json#windows` |
| 同一高度同一视图被重复读到 | 5 | `sequence.json#rows.duplicate_reads` |
| 交易（按不同视图去重） | 4240 | `negative-controls.json`（NC10 的 witness 里带这个数；`summary.json#recompute_agreement_section_32` 只列轴名，不列值） |
| canonical 对账次数 / 覆盖高度 | 116 / 91 | `reconciliation.json#windows` |
| 内容与封块完全相同 / 是其前缀 / 不一致 | 79 / 30 / 0（另 7 次无视图可查） | `reconciliation.json#rows` |
| 命中已注册池 | 0（分母 80 池 / 160 边 / 50 代币，块高 37224031） | `affected-pools.json`（含非零阳性对照） |
| lead（高度口径） | n=84，中位 1825 ms，最大 3701 ms，非正 0 | `latency.json#lead_distribution_height_level` |
| lead（交易口径） | n=2792，中位 1831 ms，p95 3111 ms，非正 0 | `latency.json#lead_distribution_transaction_level` |
| 新增生产 RPC | 0（雷达不读任何端点，门也不读） | `summary.json#rpc_accounting_section_34` |

## 两套数字必须一起看

每张表都把**生产运行自己上报的计数**（`raw/window-*/live-run-report.json`）和**这个门从 `raw/` 逐行重算的计数**并列写出。
§32 禁止「生产码输出 summary，然后 summary 就是证据」：`reconciliation.json` 每行都用两条交易列表独立重算 verdict，
`latency.json` 每行都用原始时间戳独立重算 lead / lag / 对账耗时（允许 ±2 ms，因为秒表读的是同一次交付的两个瞬间）。

重算确实抓到了一件生产计数不会自己说的事：116 次对账只覆盖 91 个高度——同一个已封块被重复对账 25 次。
原因写在 `reconciliation.json#repeat_closures`：closing 臂每次轮询 `latest` 都会把同一个摘要再交一次，而雷达为了 §19 的迟到帧比对会保留已封视图。
重复对账之间判决类别从未改变（`verdict_stability` = 0），所以这是**计数口径**的差，不是正确性的差——
但把它抹平成「116 个高度」就是一次谎报。

## 表格清单

- `summary.json` — §28 的 A/B 输出、§30 的端点身份与观测范围、§34 的 RPC 核算，以及「不主张什么」的清单
- `protocol.json` — §5.1/§6 协议取证：实际出现的字段、没有的字段、只能写 UNKNOWN 的能力面
- `flashblocks.jsonl` — 一次预确认读取一行：高度 / 视图哈希 / 父哈希 / 交易数 / 载荷摘要 / 是否随附原文
- `sequence.json` — 一个高度一行：读了几次、几种视图、重复几次、视图生长了多少毫秒、§48 的达标与未达标
- `affected-pools.json` — 命中 0，以及这个 0 需要的两个分母和一个非零阳性对照
- `reconciliation.json` — 每次对账一行，上报 verdict 与重算 verdict 并列
- `latency.json` — §26 时间戳 / §27 派生延迟逐条重算，两个口径的分布，和「不许用选择性窗口」的警示
- `negative-controls.json` — NC1–NC12 绑定的测试，与真实窗口恰好行使（或没行使）的那一半
- `manifest.json` — 每个文件的字节数与摘要、装配命令、检查命令、原始记录清单

## 原始记录

`raw/window-a`、`raw/window-b` 各五份：链路事件流、预确认臂逐次读取的投影、有界前缀的逐字响应、
canonical 臂逐次读取与补齐摘要、运行报告。逐字载荷按 §31 的字节预算截断（预算耗尽即停，剩余预算记在运行报告里）；
投影与摘要覆盖**全部**读取，所以任何一张表都能在不调用任何 RPC 的前提下重算。

## 复现

```text
cargo test -p evm-live --test preconf_evidence_gate -- --test-threads=1
```

默认模式不写任何文件，只做字节对照与独立重算。要重新装配（只有码或原始记录真的变了才该做）：

```text
M94_EVIDENCE_REFRESH=1 cargo test -p evm-live --test preconf_evidence_gate -- --test-threads=1
```

真实窗口是另一条命令，列在 `manifest.json#_provenance.capture_commands`：它花的每一次请求都在 `raw/` 里有一行对应。
