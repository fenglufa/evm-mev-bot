# M8.5.1 — `eth_call` 语义与生命周期诊断（证据）

## 一句话结论（白话）

结论：**不能信**，而且**省不出 RPC**。Preflight 要的那次读数就是「头块这次」，它的职责是当第二双眼睛：代码里 `Repricing::expected_output` 在头部读数缺失时是直接拒绝，不会退回用发现阶段的数字，所以把上游的副本递进去，闸门就变成拿一个数字跟自己比。记录里的块字段又是实测会改答案的（远距对照）。
本轮净省 RPC = **0**，判定 = **REUSE_BLOCKED**。

「省不出」不是「还没找到」：省不掉的那一次就是检查本身，删它等于删闸门。

§66 要求的那句原文（逐字来自 `reuse-verdicts.json`）：

Preflight could not trust the handed-over reading, because the reading Preflight needs is the one at the head and its job is to be the second opinion: `Repricing::expected_output` refuses when the head's reserves are missing rather than falling back to the finding's numbers, so a copy of the finding's answer would leave the gate comparing a value with a copy of itself while the block term the records show is measured load-bearing

## 数字

- 记录的 `eth_call` 请求：**60** 次（全部方法 246 次里的），来自 3 把已提交的 run；去重后带块的语义身份 **60** 个，去掉块 **26** 个
- 跨阶段候选对：**18** 对，其中 18 对是 `opportunity_detection → preflight`、18 对跨块；判定为可复用 **0** 对（结果标签 REUSE_BLOCKED）
- 6 个调用点里 `safe_to_reuse_now = true` 的：**0**；`reusable_in_principle = true` 的：**1**（两个字段是分开的，见 §27）
- 生命周期格子：**30** 格（6 站 × 5 步），状态分布 {"not_applicable":3,"partially_proven":8,"proven":15,"unknown":4}
- 块这个字段到底改不改答案：**同一次 run 内**（相差 13–15 块）比了 6 组，答案相同 6 组；**相隔 170147 块**的两把读数，2 口池答案**都不同**（2 组里 2 组不同）

短距的「相同」**不能**读成「块无关」：这 2 口池在 M7 的 20000 块 swap/sync 普查里**一次都没出现**（冷池），所以那 13–15 块本来就没有理由变。真正证明「块会改答案」的是远距那把——同一个 `to`、同一份 `getReserves()` 数据、两个高度，两个字段都变了。

## 7 张表

| 文件 | 回答什么 | 行/格数量 |
| --- | --- | --- |
| `call-surface.json` | 谁在什么阶段用什么 calldata 问了什么 | 15 |
| `normalized-identities.json` | 每个请求的语义身份 + 4 个非身份字段的「为什么不存在」 | 60 |
| `ownership-matrix.json` | 8 个复用轴 × 6 个调用点 | 6 |
| `lifecycle-contracts.json` | 5 个生命周期步骤 × 6 个调用点 | 30 |
| `dependency-matrix.json` | 块是否改变答案（选择器/配对/字节码/普查） | 18 |
| `negative-controls.json` | NC1–NC6 跑在真实身份上，不是写在纸上 | 6 |
| `reuse-verdicts.json` | 每站每对的复用判定 + §66 一句话答案 | 6 |

## 本轮没做、也不允许做的事

- 没有新增任何 RPC：所有数字都从已提交记录重算，没有为了「确认一次 eth_call」再打一次 eth_call；本轮真实运行次数 = 0
- 没有做任何优化：没加缓存、没加批处理、没减 eth_call、没删 preflight 的第二次读
- 没改生产逻辑：新增的只有诊断模块和它的证据表
- 没有把不存在的字段写成 null 当「已调查」：from / value / state_override / gas 四条都带「为什么不存在」的规则
- 没有从「Build 结果相同」反推「eth_call 结果相同」：所有相等比较都在同种已记录答案之间做
- 没有受控 A/B 复用实验（那是 M8.5 后续阶段，且必须先有本阶段的资格结论）

## 这套证据回答不了什么

- **答案本身没被记录**：trace 记的是请求，不是返回字节，所以「两次读数是否逐字节相同」只能在 artifact 层比（reserve0/reserve1 这些字段），不能在线上比
- **块内路径不可归因**：字节码里扫不到区块上下文 opcode，可以证明这个合约的读法与块无关；反过来不行——扫到 TIMESTAMP 不等于 getReserves() 走得到它
- **冷池之外没有第二个样本**：短距相等的对照只有「链上确实动过」的远距那把，同一口池、同一 calldata，样本就是那 2 个；要更多样本就得真跑，那是后续阶段的事
- **所有权不是记录字段**：谁能持有/失效一份答案，只能从代码结构判定；这条判定对生产代码零改动

## 怎么读锚点

每张表的 `evidence_refs` 都是装配时**解析出来的行号**：token 在生产区必须恰好命中一行，命中两行就装配失败。代码改了位置、表没重刷，门就红——这是 M8.4.3 立下的规矩，本轮沿用。本轮共发布 227 个锚点。

## 重新生成

```bash
M851_ETH_CALL_SEMANTICS_REFRESH=1 cargo test -p evm-pipeline --test eth_call_semantics_evidence -- --test-threads=1
```
装配只读 `data/evidence/m8/cross-stage/`、`data/evidence/m7/`、`fixtures/simulation-m7/` 里的已提交记录，不打开任何 socket。表内不含时钟字段，所以两次装配逐字节相同。

---
生成者：`crates/pipeline/tests/eth_call_semantics_evidence.rs`；模型：`crates/pipeline/src/eth_call_semantics.rs`。本 README 由同一装配写出，不在表里手抄数字。
