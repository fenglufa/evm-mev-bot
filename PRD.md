# EVM MEV Bot

## Product Requirements Document

项目名称：EVM MEV Bot
当前产品目标：GIWA Testnet Arbitrage Bot
当前 PRD 版本：v0.2
目标语言：Rust
目标链：GIWA Testnet
协议方向：V2-style AMM Arbitrage
项目阶段：从历史 Replay 基础设施进入真实 EVM Simulation / Live / Execution 阶段

---

# 1. 文档目的

本文档不是单纯用于描述软件功能。

本文档负责定义：

1. 当前项目到底要解决什么问题；
2. 当前项目真正的产品目标；
3. 当前阶段应该做什么；
4. 当前阶段明确不应该做什么；
5. 系统核心架构；
6. 数据流；
7. 模块职责；
8. GIWA Testnet 的特殊能力；
9. 后续开发里程碑；
10. 每个里程碑的验收标准；
11. 项目防跑偏规则。

任何后续需求，如果与本文档定义的当前产品目标冲突：

> 优先修改 PRD，而不是直接进入代码。

---

# 2. 产品定义

## 2.1 一句话定义

> EVM MEV Bot 当前是一个基于 Rust 构建、专注于 GIWA Testnet 的低延迟 DEX Arbitrage Bot。

系统最终要完成的核心闭环是：

```text
GIWA Chain Event
        ↓
Market State
        ↓
Liquidity Graph
        ↓
Arbitrage Opportunity
        ↓
EVM Simulation
        ↓
Profitability
        ↓
Risk Control
        ↓
Transaction Construction
        ↓
Signing
        ↓
GIWA Sequencer
        ↓
On-chain Result
        ↓
Actual Profit
        ↓
Replay / Metrics
```

当前项目不是为了构建：

* 通用区块链分析平台；
* 通用 EVM 数据平台；
* 通用 ABI 数据库；
* 通用 Token 分析平台；
* 通用 AI Agent；
* 通用套利研究平台；
* 多链交易终端。

项目唯一核心目标是：

> **在 GIWA Testnet 上真正跑通一个可验证的套利 Bot。**

---

# 3. 当前阶段的产品目标

## 3.1 第一目标

最终必须实现：

```text
GIWA Testnet
    ↓
发现真实套利机会
    ↓
本地 EVM Simulation
    ↓
确认真实可执行
    ↓
计算真实 Gas / Profit
    ↓
Risk Decision
    ↓
构造套利交易
    ↓
签名
    ↓
发送到 GIWA
    ↓
链上执行
    ↓
Receipt
    ↓
验证实际结果
```

这条链路完整跑通，才认为当前项目真正完成了第一阶段产品目标。

---

# 4. 当前阶段必须收敛

## 4.1 当前只做 GIWA

当前产品只针对：

```text
GIWA Testnet
```

不把以下链作为当前运行目标：

```text
Ethereum
BSC
Base
Arbitrum
Optimism
Polygon
其他 EVM Chain
```

---

# 5. 关于多 EVM 的处理原则

项目底层代码仍然保持合理的 EVM 抽象。

但是：

> **“具备 EVM 抽象能力”不等于“当前实现多链切换”。**

当前不要求实现：

```text
--chain giwa
--chain base
--chain bsc
--chain ethereum
```

也不要求：

```text
BaseAdapter
BscAdapter
EthereumAdapter
```

当前不为了多链提前增加：

* 多链配置系统；
* 多链 Provider 管理；
* 多链运行模式；
* 多链测试矩阵；
* 多链部署；
* 多链监控；
* 多链 Execution；
* 多链私有交易。

---

# 6. 为什么暂时不做多链

多链支持并不是简单修改：

```text
chain_id
RPC URL
```

不同 EVM 链可能在以下方面存在实际差异：

* RPC；
* WebSocket；
* Block timing；
* Gas model；
* Transaction propagation；
* Mempool；
* Sequencer；
* Private transaction；
* Bundle；
* DEX；
* Factory；
* Router；
* Pool；
* Token；
* Finality；
* MEV submission。

因此：

> 在 GIWA 的 Live + Simulation + Execution 完成之前，不提前抽象多链运行系统。

---

# 7. 当前项目的真正成功标准

不是：

```text
能够支持很多链
```

而是：

```text
能够在 GIWA Testnet 上稳定完成套利闭环
```

最终至少需要证明：

1. 能够接收 GIWA 实时链数据；
2. 能够正确维护 Pool State；
3. 能够发现套利 Opportunity；
4. 能够在本地 EVM 中模拟；
5. 模拟结果与实际 EVM 行为一致；
6. 能够计算 Gas；
7. 能够计算可执行 Profit；
8. Risk 能够阻止不安全机会；
9. 能够构造合法交易；
10. 能够签名；
11. 能够发送交易；
12. GIWA 能够实际打包；
13. 能够读取 Receipt；
14. 能够计算实际结果；
15. 能够 Replay 当时发生的一切。

---

# 8. 当前 North Star

```text
                    GIWA Testnet
                         │
                         ▼
                   Chain Event
                         │
                         ▼
                  Protocol Decode
                         │
                         ▼
                    State Update
                         │
                         ▼
                    Market Graph
                         │
                         ▼
                 Opportunity Detection
                         │
                         ▼
                     Simulation
                         │
                         ▼
                   Profitability
                         │
                         ▼
                     Risk Check
                         │
                         ▼
                Transaction Builder
                         │
                         ▼
                       Signer
                         │
                         ▼
                  GIWA Sequencer
                         │
                         ▼
                   On-chain Result
                         │
                         ▼
                     Metrics
                         │
                         ▼
                      Replay
                         │
                         └───────────────↺
```

---

# 9. 核心设计原则

## 9.1 Correctness First

当前优先级：

```text
Correctness
    >
Completeness
    >
Latency
    >
Complexity
```

进入 GIWA Live MEV 后：

```text
Correctness ≈ Latency
```

但：

> 不能为了追求低延迟而牺牲状态正确性。

---

# 10. State First

禁止：

```text
Opportunity
    ↓
RPC Query
    ↓
RPC Query
    ↓
RPC Query
```

正确方式：

```text
Chain Event
    ↓
State Update
    ↓
Memory State
    ↓
Graph
    ↓
Opportunity
```

Hot Path 中的 Pool State、Graph、Opportunity 必须以内存数据为主。

---

# 11. Replay First

所有生产逻辑必须遵循：

```text
Replay
   ↓
Validate
   ↓
Benchmark
   ↓
Live
```

不能：

```text
先写 Live
再想办法测试
```

---

# 12. Live 和 Replay 同源

Replay 和 Live 必须共享：

```text
Protocol Decoder
State Engine
Graph
Opportunity
Simulation
```

即：

```text
              ┌── Replay
              │
Input ────────┤
              │
              └── Live
                    │
                    ▼
               Same State
               Same Graph
               Same Opportunity
```

不能维护两套业务逻辑。

---

# 13. Simulation First

Graph 只能回答：

> “这里可能存在套利机会。”

Simulation 才回答：

> “这笔真实交易在真实 EVM 状态下是否真的能够执行？”

因此：

```text
Graph
  ↓
Candidate
  ↓
Simulation
  ↓
Executable Opportunity
```

---

# 14. 当前产品边界

## 当前核心策略

第一阶段只实现：

> Two-pool V2-style DEX Arbitrage

即：

```text
Token A
   ↓
Pool A
   ↓
Token B
   ↓
Pool B
   ↓
Token A
```

暂不把以下策略纳入当前主线：

* Sandwich；
* Backrun；
* Liquidation；
* NFT MEV；
* Intent；
* Cross-chain Arbitrage；
* V3；
* StableSwap；
* Lending Arbitrage；
* Generalized MEV。

---

# 15. 当前协议范围

第一阶段：

```text
V2-style Constant Product AMM
```

数学模型：

```text
x * y = k
```

池状态至少包括：

```text
reserve0
reserve1
fee
token0
token1
```

---

# 16. M1-M3 已完成基础

当前项目已经完成：

```text
M1
GIWA Historical Data Correctness
        ↓
M2
GIWA Market Graph
        ↓
M3
GIWA Arbitrage Opportunity
```

这些成果属于当前系统的基础设施，不应推倒重做。

---

# 17. M1：Historical Data Correctness

状态：

> COMPLETE

M1 已验证：

* GIWA Testnet Chain ID；
* Historical Block；
* Pool；
* Token；
* Sync；
* Reserve；
* Event ordering；
* Evidence；
* Replay 基础；
* Unattested emitter rejection；
* U256 数值安全。

核心原则：

> Reserve 必须来自经过身份验证的 Sync，而不是从 Swap 推测。

---

# 18. M2：Market Graph

状态：

> COMPLETE

M2 建立：

```text
Pool Registry
      ↓
StateStore
      ↓
GraphBuilder
      ↓
GraphSnapshot
```

图模型：

```text
Token
  ↕
Pool
  ↕
Token
```

同一 Token Pair 的多个 Pool 必须保留。

GraphSnapshot 必须绑定：

```text
chain_id
block_number
block_hash
```

---

# 19. M3：Arbitrage Opportunity

状态：

> COMPLETE

M3 已完成：

```text
GraphSnapshot
      ↓
Two-pool candidate
      ↓
Proven fee
      ↓
Exact U256 math
      ↓
Optimal input search
      ↓
Gross Profit
      ↓
Opportunity
```

M3 的 Opportunity 是：

> 理论上具有套利价值的候选机会。

它不是：

> 已经确认可以真实执行的交易。

---

# 20. Opportunity 生命周期

最终生命周期：

```text
Detected
   ↓
Estimated
   ↓
Simulated
   ↓
Risk Approved
   ↓
Execution Submitted
   ↓
Included
   ↓
Confirmed
   ↓
Executed Result
```

失败：

```text
Detected
   ↓
Rejected
```

或者：

```text
Simulated
   ↓
Failed
```

或者：

```text
Submitted
   ↓
Execution Failed
```

---

# 21. 数据可信度

系统必须严格区分：

```text
Observed
Derived
Estimated
Simulated
Executed
Unknown
```

例如：

| 数据                | 类型                   |
| ----------------- | -------------------- |
| Chain ID          | Verified             |
| Pool Address      | Verified             |
| Reserve           | Observed             |
| Price             | Derived              |
| Gross Profit      | Estimated            |
| Simulation Output | Simulated            |
| Gas Used          | Simulated / Executed |
| Receipt           | Executed             |
| Actual Profit     | Executed             |

禁止把：

```text
Estimated
```

写成：

```text
Executed
```

---

# 22. Simulation

## 22.1 目标

M4 开始实现真实 EVM Simulation。

核心：

```text
Opportunity
     ↓
Transaction
     ↓
Real EVM Execution
     ↓
Execution Result
```

优先使用：

> REVM

作为本地 EVM execution engine。

---

# 23. Simulation 不是数学模拟

禁止只实现：

```text
reserve math
```

然后声称：

```text
EVM Simulation
```

真实 Simulation 必须执行：

* Token bytecode；
* Pool bytecode；
* Router / Executor bytecode；
* ERC20 transfer；
* approve；
* swap；
* fee；
* tax；
* revert；
* gas；
* state transition。

---

# 24. Simulation State

Simulation 必须与 Opportunity 对应到同一个历史状态。

至少需要：

```text
chain_id
block_number
block_hash
```

理想情况下：

```text
Opportunity State
        ==
Simulation State
```

禁止：

```text
Opportunity @ Block N
Simulation @ latest
```

导致状态漂移。

---

# 25. Simulation Request

概念模型：

```text
SimulationRequest
├── chain
├── block
├── opportunity
├── from
├── to
├── value
├── calldata
├── gas_limit
├── block_context
└── state_source
```

---

# 26. Simulation Result

至少包含：

```text
SimulationResult
├── success
├── revert_reason
├── gas_used
├── output
├── logs
├── state_changes
├── token_deltas
├── gross_profit
├── gas_cost
└── net_profit
```

所有字段必须区分：

```text
Known
Unknown
NotComputable
```

禁止伪造数据。

---

# 27. Gross Profit 与 Net Profit

必须严格区分：

```text
Gross Profit
=
Output - Input
```

最终：

```text
Net Profit
=
Output
- Input
- Gas
- Protocol Fee
- Bribe
- Execution Cost
```

如果 Gas 与 Profit Token 无法可靠换算：

> Net Profit 必须为 Unknown / NotComputable。

不能使用未经证明的价格进行“看起来完整”的计算。

---

# 28. Token Tax / Transfer Tax

M4 必须验证：

```text
Analytical Result
        vs
Actual EVM Simulation
```

特别是已经观察到的 GIWA Testnet Token Tax 场景。

如果：

```text
Analytical Output
!=
Simulation Output
```

必须解释原因。

不能修改 M3 数学模型强行让结果一致。

---

# 29. M4 验收目标

M4 必须至少证明：

### A

存在独立 `simulation` crate。

### B

真正运行本地 EVM。

### C

M3 Opportunity 可以转换为 SimulationRequest。

### D

真实 Pool / Token / Router / Executor bytecode 可以参与执行。

### E

Simulation Block 与 Opportunity Block 一致。

### F

可以获得：

```text
success
output
gas_used
logs
```

### G

可以区分：

```text
success
revert
out-of-gas
state mismatch
invalid transaction
```

### H

无 Tax 场景：

```text
Analytical
≈
Simulation
```

差异必须可解释。

### I

Tax Token 场景：

```text
Analytical
!=
Simulation
```

并解释差异。

### J

得到真实 Simulation Gross Profit。

### K

得到真实 Gas Used / Gas Cost。

### L

如果可以可靠换算：

```text
Net Profit
```

否则：

```text
Unknown / NotComputable
```

### M

Risk Policy 可以基于 Simulation Result 工作。

### N

至少一个真实 GIWA Historical Opportunity 完成本地 Simulation。

### O

Simulation 必须 deterministic。

### P

必须通过：

```text
cargo fmt --check
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

### Q

必须产生 M4 Completion Report。

---

# 30. PathFinder

当前不预设最终设计。

M3 当前只需要：

```text
Two-pool candidate enumeration
```

M4 不因为未来需要多跳套利而提前引入复杂 PathFinder。

M4 完成后，根据：

* Opportunity 数据结构；
* Simulation 结果；
* 实际 GIWA 流动性规模；
* 两池套利覆盖率；
* 多跳机会数量；
* Simulation 成本；

再决定：

```text
PathFinder
```

的最终设计。

候选方案可以包括：

```text
Direct Enumeration
Bellman-Ford
SPFA
Bounded DFS
Graph Search
Hybrid
```

但在没有真实需求证据前：

> 不提前实现。

---

# 31. contracts/

当前仓库暂不强制建立最终套利合约架构。

M4 完成后再决定。

未来可能需要：

```text
EOA
 ↓
ArbitrageExecutor
 ↓
Pool A
 ↓
Pool B
 ↓
Profit Check
```

但具体设计必须基于：

* Simulation；
* Transaction Construction；
* Gas；
* Atomicity；
* Router；
* GIWA execution semantics；

进行决定。

不能为了“以后可能需要”提前实现一个复杂的 Universal Arbitrage Contract。

---

# 32. M5：GIWA Live Pipeline

M5 的目标：

> 从 Historical Replay 进入 GIWA Testnet Live。

核心：

```text
GIWA WebSocket
       ↓
New Block
       ↓
Logs
       ↓
Decode
       ↓
State Update
       ↓
Graph Update
       ↓
Opportunity
```

必须与 Replay 使用相同：

```text
Protocol Decoder
State Engine
Graph
Opportunity
```

---

# 33. Live Data Source

初始：

```text
WebSocket
+
RPC
```

必须具备：

* New Block；
* Log；
* Receipt；
* Block Context；
* Reconnect；
* Timeout；
* Basic retry。

---

# 34. FlashblockSource

GIWA 后续 Live Hot Path 必须考虑：

```text
FlashblockSource
```

目标：

> 在完整 Block 产生之前获得更早的 GIWA 市场状态变化。

概念：

```text
GIWA Flashblock
       ↓
Partial State Change
       ↓
State Update
       ↓
Graph
       ↓
Opportunity
       ↓
Simulation
```

如果 Flashblock 数据不可用：

```text
Flashblock
    ↓
Fallback
    ↓
Normal Block
```

---

# 35. Flashblock Stale Protection

如果 Opportunity 在 Flashblock 状态下产生：

```text
Flashblock N
    ↓
Simulation
    ↓
Flashblock N+1
```

则必须检查：

```text
Opportunity State
==
Current State
```

如果状态已经变化：

> 原 Opportunity 必须失效。

禁止使用过期 Opportunity 直接执行。

---

# 36. Live Latency

M5 开始正式记录：

```text
block_received
decode_started
decode_finished
state_updated
graph_updated
opportunity_detected
simulation_started
simulation_finished
execution_started
```

至少能够计算：

```text
Block → Opportunity
Opportunity → Simulation
Simulation → Execution
```

---

# 37. M6：GIWA Execution

M6 的目标：

```text
Simulation
    ↓
Risk
    ↓
Transaction Builder
    ↓
Signer
    ↓
GIWA Submission
```

---

# 38. Transaction Builder

负责：

```text
Opportunity
    ↓
Transaction
```

包括：

* target；
* calldata；
* value；
* gas limit；
* gas parameters；
* nonce；
* chain ID。

禁止 Transaction Builder 自己决定：

```text
Opportunity
```

它只负责把已经批准的 Opportunity 转换成交易。

---

# 39. Signer

Signer 独立于 Execution。

未来可以支持：

```text
Private Key
Hardware Wallet
Remote Signer
KMS
```

当前 GIWA Testnet 阶段优先支持：

> 安全的本地测试签名方案。

私钥不得：

* 写入源码；
* 提交 Git；
* 写入 fixture；
* 写入日志。

---

# 40. SequencerDirect

GIWA Execution 必须支持：

```text
SequencerDirect
```

它是 GIWA 专属 Execution Path。

概念：

```text
Opportunity
      ↓
Transaction
      ↓
Signer
      ↓
SequencerDirect
      ↓
GIWA Sequencer
```

与通用：

```text
eth_sendRawTransaction
```

路径分离。

---

# 41. SequencerDirect 的职责

负责：

* GIWA Sequencer submission；
* RPC endpoint；
* timeout；
* retry；
* submission latency；
* transaction hash；
* receipt tracking。

如果存在多个可用 submission endpoint，可以后续支持：

```text
Endpoint A
Endpoint B
Endpoint C
      ↓
First successful submission
```

但必须基于真实 GIWA endpoint 行为验证后实现。

---

# 42. M7：GIWA Testnet Real Arbitrage

这是当前项目第一个真正意义上的产品验收阶段。

必须实现：

```text
Live Event
   ↓
State
   ↓
Graph
   ↓
Opportunity
   ↓
Simulation
   ↓
Risk
   ↓
Transaction
   ↓
Signer
   ↓
SequencerDirect
   ↓
GIWA
   ↓
Receipt
   ↓
Actual Result
```

---

# 43. M7 成功标准

至少成功验证一次：

```text
Real Opportunity
        ↓
Real Simulation
        ↓
Risk Approved
        ↓
Real Transaction
        ↓
GIWA Included
        ↓
Receipt Success
        ↓
Expected Token Delta
        ↓
Actual Profit Verified
```

如果最终交易没有盈利，也必须能够解释：

* Opportunity 预测；
* Simulation；
* Gas；
* Actual execution；
* State difference；
* Profit difference。

---

# 44. 实际 Profit

最终 Profit 必须来自：

```text
Transaction Receipt
+
Token Balance Delta
+
Gas Cost
+
Execution Cost
```

而不是：

```text
Opportunity.gross_profit
```

直接当作实际利润。

---

# 45. M8：GIWA Optimization & Hardening

真实 Testnet 跑通之后，再优化：

```text
Latency
Throughput
State Update
Graph Update
Simulation
RPC
Submission
Recovery
```

重点：

* Flashblock latency；
* Decode parallelism；
* lock reduction；
* memory allocation；
* RPC latency；
* simulation latency；
* transaction submission latency；
* stale opportunity rejection；
* retry；
* recovery；
* circuit breaker。

---

# 46. 当前版本路线

当前项目不再按照“先把多链做出来”的路线推进。

新的路线：

```text
M1
GIWA Historical Data Correctness
        ✓

M2
GIWA Market Graph
        ✓

M3
GIWA Arbitrage Opportunity
        ✓

M4
EVM Simulation / Profitability
        ← CURRENT

M5
GIWA Live Pipeline
        ↓

M6
GIWA Execution
        ↓

M7
GIWA Testnet Real Arbitrage
        ↓

M8
GIWA Latency / Reliability / Hardening
```

---

# 47. 当前版本与未来多链

多链不是当前版本目标。

只有当：

```text
M7 GIWA Testnet
```

真正完成后，才重新评估：

```text
Ethereum
BSC
Base
Arbitrum
其他 EVM
```

届时再决定是否引入：

```text
ChainProfile
ChainAdapter
ProviderManager
ExecutionAdapter
```

---

# 48. 未来多链的原则

如果未来开始多链：

```text
ChainProfile
      ↓
ChainAdapter
      ↓
Normalized Chain Data
      ↓
Shared State
      ↓
Shared Graph
      ↓
Shared Opportunity
```

业务层禁止：

```rust
if chain_id == ...
```

链特化逻辑必须位于：

```text
Chain
Protocol
Execution
```

边界以内。

---

# 49. GIWA 与未来 EVM 的关系

GIWA 是当前唯一目标链。

但是核心业务逻辑不应该写成：

```text
GiwaArbitrageCalculator
GiwaOpportunity
GiwaGraph
```

而应该保持：

```text
Generic EVM
     ↓
GIWA-specific Chain Source
     ↓
GIWA-specific Execution
```

这样未来才能在真实需求出现时进行多链扩展。

---

# 50. 最终架构

当前最终目标架构：

```text
                       GIWA TESTNET
                            │
             ┌──────────────┴──────────────┐
             │                             │
       Normal Block                   Flashblock
             │                             │
             └──────────────┬──────────────┘
                            │
                       Chain Layer
                            │
                            ▼
                   Protocol Adapter
                            │
                            ▼
                       State Engine
                            │
                            ▼
                       StateStore
                            │
                            ▼
                     Graph Builder
                            │
                            ▼
                    Opportunity
                            │
                            ▼
                       Simulation
                         REVM
                            │
                            ▼
                     Profitability
                            │
                            ▼
                         Risk
                            │
                            ▼
                  Transaction Builder
                            │
                            ▼
                         Signer
                            │
                            ▼
                   SequencerDirect
                            │
                            ▼
                       GIWA Chain
                            │
                            ▼
                         Receipt
                            │
                            ▼
                    Actual Result
                            │
                  ┌─────────┴─────────┐
                  │                   │
                Metrics             Replay
```

---

# 51. Workspace

当前规划：

```text
evm-mev-bot/
├── Cargo.toml
├── crates/
│   ├── core/
│   ├── chain/
│   ├── protocol/
│   ├── state/
│   ├── graph/
│   ├── opportunity/
│   ├── simulation/
│   ├── risk/
│   ├── execution/
│   ├── signer/
│   ├── pipeline/
│   ├── replay/
│   ├── metrics/
│   └── cli/
├── contracts/
├── config/
├── fixtures/
├── data/
├── docs/
└── tests/
```

注意：

> `contracts/` 当前只是未来可能需要的能力边界，不代表当前必须立即实现最终套利合约。

---

# 52. 模块职责

## core

负责：

* ChainId；
* BlockNumber；
* Address；
* Token；
* PoolId；
* Amount；
* 公共错误；
* 公共类型。

---

## chain

负责：

* RPC；
* WebSocket；
* Block；
* Log；
* Receipt；
* Chain Event；
* GIWA Live Source；
* FlashblockSource。

不负责：

* Arbitrage；
* Opportunity；
* Profit calculation。

---

## protocol

负责：

* V2-style AMM；
* Event Decode；
* Pool Discovery；
* Fee Evidence；
* Protocol-specific normalization。

---

## state

负责：

```text
Event
 ↓
StateUpdate
 ↓
StateStore
```

保证：

* deterministic；
* ordered；
* no hidden RPC；
* no state pollution。

---

## graph

负责：

```text
StateStore
 ↓
GraphSnapshot
```

当前：

> Two-pool candidate discovery。

未来：

> PathFinder。

但 PathFinder 的最终设计在 M4 后确定。

---

## opportunity

负责：

* Arbitrage Candidate；
* Route；
* Input；
* Output；
* Gross Profit；
* Search；
* Opportunity lifecycle。

不负责：

* EVM execution；
* transaction signing；
* network submission。

---

## simulation

负责：

* REVM；
* state loading；
* transaction execution；
* gas；
* output；
* revert；
* state changes；
* simulation profit。

---

## risk

负责：

* Minimum Profit；
* Maximum Gas；
* Slippage；
* Simulation success；
* Stale state；
* Token risk；
* Execution risk；
* Circuit breaker。

---

## execution

负责：

```text
Opportunity
 ↓
Transaction
 ↓
Submission
```

GIWA 当前重点：

```text
SequencerDirect
```

---

## signer

负责：

```text
Transaction
 ↓
Signature
 ↓
Signed Transaction
```

---

## replay

负责：

```text
Historical Data
 ↓
Same Pipeline
 ↓
Same Result
```

---

## metrics

负责：

* latency；
* opportunity；
* simulation；
* execution；
* profit；
* failures；
* system health。

---

# 53. Hot Path

最终 Hot Path：

```text
GIWA Event
   ↓
Decode
   ↓
State
   ↓
Graph
   ↓
Opportunity
   ↓
Simulation
   ↓
Risk
   ↓
Execution
```

Hot Path 不允许依赖：

```text
LLM
Database
External Search
External API
Explorer
```

数据库可以用于：

* History；
* Metrics；
* Debug；
* Replay；
* Research。

但不能成为 Hot Path 的必经依赖。

---

# 54. 数值安全

所有核心金额使用：

```text
U256
```

禁止：

```text
f32
f64
```

参与最终：

* Swap；
* Profit；
* Gas；
* Token amount；
* Execution。

如果使用浮点数：

> 只能用于非权威的近似展示或筛选。

最终结论必须使用精确整数。

---

# 55. Error Handling

不可控输入路径禁止：

```rust
unwrap()
expect()
panic!()
```

尤其包括：

```text
RPC
Log
ABI
Token
Pool
Amount
Simulation
Execution
Receipt
```

所有异常必须明确处理。

---

# 56. Determinism

以下输入相同：

```text
Chain
Block
Input Data
Configuration
```

必须得到：

```text
State A == State B
Graph A == Graph B
Opportunity A == Opportunity B
Simulation A == Simulation B
```

对于存在环境依赖的执行结果，必须明确记录：

```text
block hash
state source
configuration
```

---

# 57. Same Block Ordering

同一个 Block 内：

```text
transaction_index
    ↓
log_index
```

必须作为确定性排序依据。

不能依赖：

* RPC 返回顺序；
* HashMap iteration；
* 并发完成顺序。

---

# 58. Evidence First

所有真实链数据优先使用：

```text
Verified Evidence
      ↓
Index
      ↓
Raw Data
      ↓
RPC Validation
      ↓
Re-fetch
```

禁止：

> 为了让测试通过而制造假的链上数据。

---

# 59. Real Data First

任何关键结论必须尽可能通过真实 GIWA 数据验证。

例如：

```text
Pool
Token
Factory
Fee
Reserve
Block
Transaction
Receipt
```

必须能够追溯到：

```text
Historical Block
Evidence
Fixture
RPC
```

---

# 60. 测试策略

## Unit

覆盖：

* Math；
* Fee；
* Pool；
* State；
* Graph；
* Opportunity；
* Simulation；
* Risk。

## Fixture

覆盖：

* Real Pool；
* Real Event；
* Real Block；
* Real Transaction；
* Real Receipt。

## Replay

覆盖：

```text
Historical Block Range
```

## Integration

覆盖：

```text
GIWA
 ↓
State
 ↓
Graph
 ↓
Opportunity
 ↓
Simulation
```

## End-to-End

最终覆盖：

```text
GIWA Live
 ↓
Opportunity
 ↓
Simulation
 ↓
Risk
 ↓
Execution
 ↓
Receipt
```

---

# 61. M4 测试重点

必须至少拥有：

```text
No-opportunity
Profitable
Unprofitable
Revert
Out-of-gas
State mismatch
Tax token
No-tax token
Gas calculation
```

以及真实 GIWA Historical Opportunity。

---

# 62. 可观测性

至少记录：

```text
blocks_received
blocks_processed
logs_processed
pools_updated
graph_updates
opportunities_detected
opportunities_rejected
simulation_started
simulation_finished
simulation_failed
execution_started
execution_submitted
execution_confirmed
execution_failed
```

以及：

```text
block_to_opportunity_latency
opportunity_to_simulation_latency
simulation_latency
simulation_to_execution_latency
execution_latency
total_latency
```

---

# 63. Profit Metrics

必须区分：

```text
Theoretical Gross Profit
Simulated Gross Profit
Simulated Net Profit
Executed Gross Profit
Executed Net Profit
```

不能只保存：

```text
profit
```

而不知道它来自哪个阶段。

---

# 64. Opportunity 与 Execution 的边界

Opportunity：

> “应该交易什么？”

Simulation：

> “真实 EVM 执行会发生什么？”

Risk：

> “现在是否允许交易？”

Execution：

> “如何把批准的交易送上链？”

Signer：

> “如何合法签名？”

SequencerDirect：

> “如何最快提交给 GIWA？”

这些职责必须保持分离。

---

# 65. 不能因为当前是 Testnet 而降低正确性要求

Testnet 不是：

> 可以随便模拟。

Testnet 的意义是：

> 在真实 EVM / 真实 GIWA execution environment 中低成本验证完整系统。

因此：

```text
Testnet
≠
Mock Chain
```

---

# 66. 当前不做的事情

以下全部不进入当前开发主线：

## 多链

```text
BSC
Base
Ethereum
Arbitrum
Polygon
```

## 多协议

```text
V3
StableSwap
其他复杂 AMM
```

## 其他 MEV Strategy

```text
Sandwich
Backrun
Liquidation
NFT
Intent
Cross-chain
```

## 通用基础设施

```text
Universal ABI Database
Universal Explorer
Universal Blockchain Indexer
AI Agent
Generic Analytics
```

## 过早优化

```text
复杂微服务
分布式系统
数据库驱动 Hot Path
```

---

# 67. 当前尤其不做

以下内容如果没有 M4/M5/M6 的真实需求，不得提前实现：

```text
Universal Router
Universal Arbitrage Contract
Complex Flash Loan Framework
Multi-chain Runtime
Multi-chain Config UI
Generic PathFinder
Complex Bundle Engine
General Token Risk Platform
```

---

# 68. GIWA 专属能力

当前可以实现 GIWA 专属能力。

原因不是：

> 把系统写死。

而是：

> 当前产品本身就是 GIWA Testnet Arbitrage Bot。

GIWA 专属能力包括：

```text
FlashblockSource
SequencerDirect
GIWA Chain Source
GIWA Execution
GIWA latency metrics
```

这些能力必须被隔离在 Chain / Execution 边界中。

---

# 69. FlashblockSource 与 SequencerDirect 的关系

二者分别解决：

```text
FlashblockSource
=
更早发现机会
```

和：

```text
SequencerDirect
=
更快提交交易
```

完整链路：

```text
FlashblockSource
       ↓
State
       ↓
Graph
       ↓
Opportunity
       ↓
Simulation
       ↓
Risk
       ↓
SequencerDirect
```

因此它们不是独立的“附加功能”。

它们最终共同服务于：

> GIWA Arbitrage Hot Path。

---

# 70. GIWA Testnet → Mainnet

当前项目首先完成：

```text
GIWA Testnet
```

未来 GIWA Mainnet 上线后：

> 优先目标是通过 Chain Configuration / Network Profile 切换网络身份，而不是重写 State / Graph / Opportunity。

但是：

> 不能假设 Testnet 与 Mainnet 完全一致。

Mainnet 切换前必须重新验证：

* Chain ID；
* RPC；
* WebSocket；
* Flashblock；
* Sequencer；
* DEX；
* Factory；
* Pool；
* Token；
* Gas；
* Finality；
* Transaction submission；
* Contract deployment；
* Execution semantics。

因此：

```text
Testnet → Mainnet
```

应该是：

```text
配置切换
+
重新验证
```

而不是：

```text
代码重写
```

---

# 71. Provider

当前不建立复杂的多链 Provider Manager。

GIWA Testnet 阶段至少需要：

```text
Primary RPC
Fallback RPC
```

具备：

* timeout；
* retry；
* health check；
* latency measurement。

复杂 Provider Failover 放到 M8。

---

# 72. Configuration

当前配置重点：

```text
GIWA Testnet
```

例如概念：

```text
chain_id
rpc_http
rpc_ws
flashblock_ws
sequencer_rpc
native_token
protocol_registry
execution_config
simulation_config
risk_config
```

配置用于：

> GIWA 环境切换和运行参数。

不是用于当前实现多链运行。

---

# 73. CLI

最终目标：

```text
evm-mev replay
evm-mev simulate
evm-mev scan
evm-mev live
evm-mev dry-run
evm-mev execute
```

当前只实现当前阶段真正需要的命令。

不为了“未来 CLI 完整”提前实现全部命令。

---

# 74. Replay CLI

目标：

```text
evm-mev replay \
  --from-block X \
  --to-block Y
```

输出：

```text
blocks
pool_updates
graph_updates
opportunities
simulation_results
profit
latency
```

---

# 75. Live CLI

未来：

```text
evm-mev live
```

启动：

```text
GIWA Live Source
      ↓
State
      ↓
Graph
      ↓
Opportunity
```

---

# 76. Dry Run

在真实 Execution 前必须提供：

```text
DryRun
```

模式：

```text
Opportunity
 ↓
Simulation
 ↓
Risk
 ↓
Transaction
 ↓
Log
```

但：

```text
不签名
不广播
```

---

# 77. Real Execution

真实执行必须明确开启。

例如概念：

```text
--execute
```

默认：

```text
Disabled
```

防止：

> Bot 启动后意外发送真实交易。

---

# 78. 安全原则

私钥：

```text
Never commit
Never log
Never fixture
Never hardcode
```

Execution 必须明确区分：

```text
Simulation
DryRun
RealExecution
```

不能因为配置错误从：

```text
DryRun
```

静默变成：

```text
RealExecution
```

---

# 79. Risk

至少：

```text
Simulation Success
AND
Net Profit > Minimum Profit
AND
Gas < Maximum Gas
AND
Opportunity State Fresh
AND
Slippage < Maximum Slippage
```

才可以：

```text
Accept
```

否则：

```text
Reject
```

无法确定：

```text
Unknown
```

不能默认 Accept。

---

# 80. Stale Opportunity

Opportunity 必须绑定：

```text
chain_id
block_number
block_hash
state_version
```

如果状态发生变化：

```text
Opportunity
      ↓
Stale
```

必须重新：

```text
Detect
Simulation
Risk
```

不能继续使用。

---

# 81. Transaction Atomicity

套利交易必须保证：

```text
Swap A
  ↓
Swap B
  ↓
Profit Check
```

如果最终无法达到预期：

```text
Revert
```

不能出现：

```text
只完成第一腿
```

导致资产损失。

具体 Atomic Arbitrage Executor 设计：

> M4 后确定。

---

# 82. Gas

Gas 必须来自真实 EVM Simulation。

优先：

```text
gas_used
```

而不是固定：

```text
estimated_gas = 300000
```

Gas Cost：

```text
gas_used
×
effective_gas_price
```

必须使用精确整数。

---

# 83. Latency

最终必须记录：

```text
T0 = event received
T1 = decoded
T2 = state updated
T3 = graph updated
T4 = opportunity detected
T5 = simulation started
T6 = simulation finished
T7 = risk approved
T8 = transaction signed
T9 = transaction submitted
T10 = included
T11 = confirmed
```

最终可以计算：

```text
T4 - T0
T6 - T4
T9 - T6
T10 - T9
T11 - T0
```

---

# 84. Benchmark

M8 前不进行过度优化。

但必须建立 Benchmark：

```text
Decode
State Update
Graph Update
Opportunity Search
Simulation
Transaction Build
Signing
Submission
```

---

# 85. 数据库原则

Hot Path：

```text
RAM
```

数据库用于：

```text
Historical Data
Metrics
Replay
Debug
Research
```

不允许：

```text
每一个 Pool Update
    ↓
Database
    ↓
Opportunity
```

成为核心实时路径。

---

# 86. 日志原则

日志服务于：

```text
Debug
Replay
Performance
Incident
Audit
```

不输出大量无意义数据。

Hot Path 日志必须可控。

---

# 87. 错误分类

错误至少区分：

```text
Data Error
Decode Error
State Error
Graph Error
Opportunity Error
Simulation Error
Risk Rejection
Transaction Error
Signing Error
Submission Error
Execution Error
Receipt Error
```

不要所有错误统一为：

```text
Unknown Error
```

---

# 88. 代码质量

必须保持：

```text
cargo fmt --check
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

全部通过。

---

# 89. Production Code 原则

禁止在 Production Code 中出现：

```text
unwrap()
expect()
panic!()
```

除非经过明确证明：

> 该状态在类型系统 / 构造器 / 不变量中绝对成立。

测试代码可以根据测试目的使用，但 Production Hot Path 必须严格控制。

---

# 90. 数值原则

核心数学：

```text
U256
```

必须覆盖：

* Overflow；
* Underflow；
* Zero；
* Division by zero；
* Extreme reserve；
* Extreme amount；
* Fee；
* Decimal；
* Rounding。

非法 Pool State：

> 不能导致整个 Bot Panic。

---

# 91. M1-M3 不回退

后续开发不能破坏已经验证的：

```text
M1 Data Correctness
M2 Graph Correctness
M3 Opportunity Correctness
```

任何新功能必须：

```text
cargo test
```

保持既有测试全部通过。

---

# 92. M4 不修改 M3 数学模型以适配 Simulation

如果出现：

```text
M3 Analytical
!=
M4 Simulation
```

必须首先检查：

1. Pool State；
2. Block；
3. Bytecode；
4. Router；
5. Token behavior；
6. Transfer Tax；
7. Fee；
8. Rounding；
9. Execution order；
10. Gas；
11. State override。

只有证明 M3 模型错误，才能修改 M3。

禁止：

> 为了让测试通过而强行让 Simulation 等于 Analytical。

---

# 93. M4 的核心原则

M4 最重要的不是：

> “把 REVM 接进来。”

而是：

> **证明 Opportunity → Real EVM Execution 的桥梁是正确的。**

---

# 94. M5 的核心原则

M5 最重要的不是：

> “接上 WebSocket。”

而是：

> **证明 Replay 与 Live 使用同一套 State Semantics。**

---

# 95. M6 的核心原则

M6 最重要的不是：

> “能够发送交易。”

而是：

> **只发送经过 Simulation + Risk 验证的交易。**

---

# 96. M7 的核心原则

M7 最重要的不是：

> “成功发出一笔交易。”

而是：

> **完成一次从机会发现到链上结果验证的完整套利闭环。**

---

# 97. M8 的核心原则

M8 才开始真正回答：

> “这个 Bot 能不能在 GIWA Testnet 上长期、稳定、低延迟运行？”

---

# 98. 未来扩展顺序

当前推荐：

```text
GIWA Testnet
      ↓
Simulation
      ↓
Live
      ↓
Execution
      ↓
Real Arbitrage
      ↓
Latency Optimization
      ↓
Reliability
      ↓
PathFinder
      ↓
More Protocols
      ↓
More Chains
```

而不是：

```text
多链
 ↓
多协议
 ↓
复杂 Graph
 ↓
Simulation
 ↓
Execution
```

---

# 99. 未来 Multi-chain Gate

只有满足以下条件后，才进入多链：

```text
GIWA Testnet Real Arbitrage
        ✓
Simulation Stable
        ✓
Execution Stable
        ✓
Flashblock Stable
        ✓
SequencerDirect Stable
        ✓
Latency Measured
        ✓
Failure Recovery
        ✓
```

届时重新评估：

```text
ChainProfile
ChainAdapter
ExecutionAdapter
ProviderManager
```

---

# 100. 最终产品演进

当前：

```text
GIWA Testnet Arbitrage Bot
```

未来：

```text
GIWA Mainnet Arbitrage Bot
```

再未来：

```text
EVM Arbitrage Engine
```

最终才可能：

```text
Multi-EVM MEV Bot
```

顺序必须是：

```text
先做深
再做广
```

而不是：

```text
先做广
再做深
```

---

# 101. 防跑偏机制

任何新需求必须回答：

## Q1

是否直接服务：

```text
Opportunity
Simulation
Risk
Execution
Latency
Profit
```

之一？

如果不是：

> 默认不进入核心项目。

---

## Q2

是否属于：

```text
GIWA Testnet Arbitrage
```

当前目标？

如果不是：

> 默认延后。

---

## Q3

是否能够提高：

```text
Opportunity Detection
Simulation Accuracy
Execution Success
Latency
Profitability
Reliability
```

之一？

如果都不能：

> 不作为当前重点。

---

## Q4

是否为了未来“可能支持多链”而提前增加复杂度？

如果是：

> 延后。

---

## Q5

是否建立了一个全新的“大系统”？

例如：

```text
Universal ABI Platform
Blockchain Data Platform
AI Agent Platform
Universal Analytics
```

如果是：

> 不进入核心项目。

---

# 102. 新需求进入标准

所有新增需求必须标记：

```text
Priority:
P0 / P1 / P2 / P3

Stage:
M4 / M5 / M6 / M7 / M8 / Future

Layer:
Chain / Protocol / State / Graph /
Opportunity / Simulation / Risk /
Execution / Signer / Replay / Metrics

GIWA Specific:
Yes / No

Hot Path:
Yes / No

Real Money Impact:
Yes / No
```

没有这些信息：

> 不进入开发。

---

# 103. 当前 P0

```text
P0
M4 Real EVM Simulation
```

之后：

```text
P0
GIWA Live Pipeline

P0
GIWA Execution

P0
GIWA Testnet Real Arbitrage
```

---

# 104. 当前 P1

```text
Flashblock Optimization
Latency Benchmark
Provider Reliability
Risk Hardening
Recovery
Circuit Breaker
```

---

# 105. 当前 P2

```text
PathFinder
Multi-hop
Additional GIWA Protocols
Advanced Token Risk
```

具体优先级根据真实数据重新决定。

---

# 106. 当前 P3 / Future

```text
BSC
Base
Ethereum
Arbitrum
V3
StableSwap
Backrun
Private Bundle
Cross-chain
```

---

# 107. 最终目标

当前项目真正需要达到的最终阶段不是：

> “我们实现了一个很漂亮的 EVM MEV Framework。”

而是：

> **“我们在 GIWA Testnet 上拥有一个能够实时发现、模拟、判断、执行并验证套利机会的真实 Bot。”**

---

# 108. 最终闭环

```text
                         GIWA TESTNET
                              │
                              ▼
                    ┌──────────────────┐
                    │  Block/Flashblock │
                    └────────┬─────────┘
                             │
                             ▼
                    ┌──────────────────┐
                    │ Protocol Decoder │
                    └────────┬─────────┘
                             │
                             ▼
                    ┌──────────────────┐
                    │   State Engine   │
                    └────────┬─────────┘
                             │
                             ▼
                    ┌──────────────────┐
                    │  Market Graph    │
                    └────────┬─────────┘
                             │
                             ▼
                    ┌──────────────────┐
                    │   Opportunity    │
                    └────────┬─────────┘
                             │
                             ▼
                    ┌──────────────────┐
                    │      REVM        │
                    │    Simulation    │
                    └────────┬─────────┘
                             │
                             ▼
                    ┌──────────────────┐
                    │  Profitability   │
                    └────────┬─────────┘
                             │
                             ▼
                    ┌──────────────────┐
                    │      Risk        │
                    └────────┬─────────┘
                             │
                             ▼
                    ┌──────────────────┐
                    │    Transaction   │
                    └────────┬─────────┘
                             │
                             ▼
                    ┌──────────────────┐
                    │      Signer      │
                    └────────┬─────────┘
                             │
                             ▼
                    ┌──────────────────┐
                    │ SequencerDirect  │
                    └────────┬─────────┘
                             │
                             ▼
                       GIWA CHAIN
                             │
                             ▼
                         Receipt
                             │
                             ▼
                     Actual Profit
                             │
                  ┌──────────┴──────────┐
                  ▼                     ▼
               Metrics                Replay
                  │                     │
                  └──────────┬──────────┘
                             │
                             └───────↺
```

---

# 109. 项目最终原则

整个项目只遵循几个核心原则：

### 1. 先做深，再做广

先把 GIWA 做通，再考虑多链。

### 2. 先模拟，再执行

没有真实 Simulation，不进入 Real Execution。

### 3. 先 Replay，再 Live

任何 Live 行为都必须可以被 Replay 验证。

### 4. State First

Opportunity 不应该依赖大量实时 RPC Query。

### 5. Simulation First

Graph 发现候选，Simulation 决定真实可执行性。

### 6. Evidence First

真实链数据优先，禁止猜测和伪造。

### 7. Exact Math

核心金额使用 U256，禁止浮点数参与最终决策。

### 8. Hot Path 极简

不要让：

```text
Database
HTTP
LLM
External Search
```

成为 Hot Path 必经依赖。

### 9. GIWA-specific capability 可以存在

但必须隔离在：

```text
Chain
Execution
```

边界中。

### 10. 不为未来需求提前复杂化

尤其：

```text
Multi-chain
PathFinder
contracts
Multi-protocol
Private Bundle
```

都必须在真实需求出现之后再设计。

---

# 110. 当前开发指令

截至本 PRD：

> **当前唯一开发任务是 M4：EVM Simulation / Profitability。**

M4 完成之前：

```text
不要实现多链
不要实现最终 PathFinder
不要实现复杂 contracts
不要实现 Flashblock Live Pipeline
不要实现 SequencerDirect
不要实现真实交易广播
```

M4 完成之后，再按照：

```text
M4
 ↓
重新评估 PathFinder / contracts
 ↓
M5 GIWA Live
 ↓
M6 GIWA Execution
 ↓
M7 GIWA Testnet Real Arbitrage
```

继续推进。

---

# 111. 最终验收定义

当且仅当以下链路能够在 GIWA Testnet 上完成：

```text
GIWA Event
   ↓
State
   ↓
Graph
   ↓
Opportunity
   ↓
Simulation
   ↓
Risk
   ↓
Transaction
   ↓
Signer
   ↓
GIWA Sequencer
   ↓
On-chain Inclusion
   ↓
Receipt
   ↓
Actual Profit Verification
```

并且：

```text
Replay
```

可以解释该次执行全过程时：

> 当前阶段的 GIWA Arbitrage Bot 才算真正完成。

---

# 112. 项目方向总结

本项目当前不是：

```text
“做一个支持所有 EVM 链的 MEV Framework”
```

而是：

```text
“先做出一个真正能在 GIWA Testnet 上跑起来的 Arbitrage Bot”
```

技术路线：

```text
正确性
   ↓
Opportunity
   ↓
真实 EVM Simulation
   ↓
GIWA Live
   ↓
低延迟
   ↓
Risk
   ↓
Execution
   ↓
真实套利
   ↓
优化
   ↓
扩展
```

最终：

> **先让 Bot 活起来，再让 Bot 变快，最后让 Bot 变广。**