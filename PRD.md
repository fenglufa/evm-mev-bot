# EVM MEV Bot

## Product Requirements Document

**项目名称：** EVM MEV Bot
**当前产品目标：** GIWA Testnet Arbitrage Bot
**PRD 版本：** v0.3
**目标语言：** Rust
**目标链：** GIWA Testnet
**核心协议方向：** V2-style Constant Product AMM Arbitrage
**当前阶段：** Phase 3 / M8.6 RPC Reduction Opportunity Census
**下一阶段：** Phase 4 / M9 Graph Search & PathFinder

---

# 1. 文档目的

本文档是本项目的长期产品与技术路线基准。

它负责定义：

1. 项目的最终目标；
2. 当前产品边界；
3. 系统核心架构；
4. 数据流；
5. 模块职责；
6. GIWA 特殊能力；
7. Flashblocks 使用原则；
8. 自建 GIWA Node 与 Flashblocks-aware RPC 规划；
9. M1～M14 完整路线；
10. 每个里程碑的验收目标；
11. 已完成能力；
12. 当前进行中的能力；
13. 明确禁止进入项目的范围；
14. 防止后续开发路线偏移的规则。

任何后续需求，如果与本文档定义的当前路线冲突：

> **优先修改 PRD，再进入代码。**

不得因为“以后可能有用”而直接进入当前实现。

---

# 2. 产品定义

## 2.1 一句话定义

> 一个基于 Rust 构建、专注 GIWA 的低延迟 EVM DEX Arbitrage Bot。

最终目标不是简单实现“价格差检测”，而是实现：

```text
GIWA Chain
      ↓
Market / Pool Discovery
      ↓
Pool Registry
      ↓
Canonical State + Flashblocks Early State
      ↓
Liquidity Graph
      ↓
PathFinder
      ↓
Arbitrage Candidate
      ↓
Optimal Input
      ↓
REVM Simulation
      ↓
Risk
      ↓
Arbitrage Executor
      ↓
Execution Planner
      ↓
Multi-Lane
      ↓
Signer
      ↓
GIWA Submission
      ↓
Receipt
      ↓
Settlement
      ↓
Realized Profit
      ↓
Metrics / Alert / Replay
```

---

# 3. 最终产品目标

最终系统必须具备以下能力：

### 市场发现

* 自动发现候选 Pool；
* 验证 Pool 身份；
* 建立 Pool Registry；
* 维护 Token / Pool Graph。

### 市场状态

* Canonical Chain State；
* Flashblocks Early State；
* State Update；
* Canonical Reconciliation；
* State Freshness 判断。

### 策略

* V2-style AMM；
* Two-pool Arbitrage；
* Multi-hop Arbitrage；
* Optimal Input；
* Candidate Ranking；
* PathFinder。

### Simulation

* REVM；
* Historical State；
* Block-pinned Simulation；
* Executor Contract Simulation；
* Gas；
* Token Tax；
* Revert；
* State Transition。

### Execution

* Transaction Build；
* Signing；
* Submission；
* Receipt；
* Settlement；
* Realized Profit；
* Arbitrage Executor；
* Multi-Lane。

### 基础设施

* Self-hosted GIWA Node；
* Flashblocks-aware RPC；
* Official / Self-hosted Provider；
* RPC HA；
* Node HA；
* Flashblocks Reconciliation；
* Conditional Private / Direct Sequencer。

### 生产能力

* 7×24 Runtime；
* Health；
* Metrics；
* Structured Logging；
* AlertManager；
* Telegram；
* Webhook；
* Capital Management；
* Nonce Recovery；
* Graceful Restart；
* KMS / HSM；
* Dashboard。

---

# 4. 当前明确不做

以下能力冻结，不进入当前主线：

```text
V3
Curve
Balancer
Liquidation
Sandwich
Cross-chain Arbitrage
AI Strategy
LLM Hot Path
```

暂不做其他 EVM Chain 的实际运行。

未来可能支持：

```text
BSC
Base
Ethereum
Arbitrum
```

但必须等 GIWA 主线完成生产验证后重新评估。

---

# 5. 当前目标链

当前唯一运行目标：

```text
GIWA Testnet
```

项目底层可以保持合理 EVM 抽象，但：

> EVM 抽象能力 ≠ 当前实现多链运行。

当前不实现：

* Multi-chain Runtime；
* Multi-chain Provider Manager；
* Multi-chain Execution；
* Multi-chain Monitoring；
* Multi-chain Deployment。

---

# 6. 最终 North Star

```text
                         GIWA
                           │
             ┌─────────────┴─────────────┐
             │                           │
       Canonical Chain              Flashblocks
             │                           │
             ↓                           ↓
       Canonical State          Early State / Events
             │                           │
             └─────────────┬─────────────┘
                           ↓
                    Pool Discovery
                           ↓
                    Pool Registry
                           ↓
                         Graph
                           ↓
                      PathFinder
                           ↓
                 Arbitrage Candidates
                           ↓
                  Optimal Input
                           ↓
                  REVM Simulation
                           ↓
                         Risk
                           ↓
                 Arbitrage Executor
                           ↓
                 Execution Planner
                           ↓
              ┌────────────┼────────────┐
              ↓            ↓            ↓
           Lane A       Lane B       Lane C
              │            │            │
              └────────────┼────────────┘
                           ↓
                 Private / Direct*
                           ↓
                    GIWA Submit
                           ↓
                    Receipt
                           ↓
                    Settlement
                           ↓
                 Realized Profit
                           ↓
                  Metrics / Alert
                           ↓
                       Replay
                           ↺
```

`*` Private / Direct Sequencer 当前为 CONDITIONAL。

---

# 7. 核心设计原则

## 7.1 Correctness First

当前原则：

```text
Correctness
    >
Completeness
    >
Latency
    >
Complexity
```

进入真实 MEV Hot Path 后：

```text
Correctness ≈ Latency
```

但：

> 不允许为了降低延迟而牺牲状态正确性。

---

# 8. State First

禁止：

```text
Opportunity
    ↓
RPC
    ↓
RPC
    ↓
RPC
```

优先：

```text
Chain / Flashblocks
       ↓
State Update
       ↓
StateStore
       ↓
Graph
       ↓
Opportunity
```

Hot Path 中：

* Pool State；
* Graph；
* Opportunity；

应尽可能以内存数据为主。

---

# 9. Replay First

所有核心生产逻辑必须遵循：

```text
Replay
   ↓
Validate
   ↓
Benchmark
   ↓
Live
```

禁止：

```text
先写 Live
再想办法测试
```

---

# 10. Replay / Live 同源

Replay 和 Live 必须共享：

```text
Protocol Decoder
State Engine
Graph
Opportunity
Simulation
Risk
```

不能维护两套业务逻辑。

---

# 11. 数据可信度

系统必须区分：

```text
Observed
Verified
Derived
Estimated
Simulated
Executed
Realized
Unknown
```

特别禁止：

```text
Estimated → Executed
Simulated → Realized
```

---

# 12. Profit 数据必须严格区分

系统必须区分：

```text
Estimated Opportunity
        ↓
Simulated Profit
        ↓
Executed Profit
        ↓
Realized Profit
```

其中：

### Estimated

数学模型得到的理论结果。

### Simulated

REVM 在指定状态下得到的结果。

### Executed

实际提交并执行的交易结果。

### Realized

实际钱包 / 资金账户余额变化确认的最终结果。

---

# 13. Discovery ≠ Trust

Pool Discovery 不等于 Pool Trust。

正确架构：

```text
GIWA
 ↓
Factory Events
 ↓
Historical Scan
 ↓
Candidate Pool
 ↓
Protocol Verification
 ↓
Pool Registry
 ↓
Graph
```

Registry 必须记录：

* Pool Address；
* Token0；
* Token1；
* Factory；
* Protocol；
* AMM Model；
* Fee；
* Verification；
* Evidence。

不能因为发现了一个 `Sync`-shaped Event 就直接信任其为合法 Pool。

---

# 14. PoolMeta 与 PoolState

必须分离：

```text
PoolMeta
```

负责：

* 地址；
* Token；
* Factory；
* Protocol；
* Fee；
* Verification。

```text
PoolState
```

负责：

* Reserve；
* Block；
* State Version；
* Freshness。

这样避免把静态身份和动态状态混在一起。

---

# 15. Graph

Graph 模型：

```text
Token = Node
Pool  = Edge
```

例如：

```text
WETH
  ↕
Pool A
  ↕
USDC
```

同一 Token Pair 的多个 Pool 必须全部保留：

```text
WETH ─ Pool A ─ USDC
WETH ─ Pool B ─ USDC
WETH ─ Pool C ─ USDC
```

不能覆盖。

---

# 16. GraphSnapshot

GraphSnapshot 必须具有明确状态身份：

```text
chain_id
block_number
block_hash
```

禁止仅使用：

```text
height
latest
tag
```

作为完整状态身份。

---

# 17. V2-style AMM

当前第一策略只实现：

```text
V2-style Constant Product AMM
```

基本模型：

```text
x * y = k
```

至少包括：

```text
reserve0
reserve1
token0
token1
fee
```

---

# 18. Optimal Input

输入金额不是：

> 越大越好。

必须寻找：

```text
argmax NetProfit(x)
```

即：

```text
NetProfit(x)
=
Output(x)
- Input(x)
- Gas
- L1 Fee
- Protocol Cost
- Other Execution Cost
```

必须使用精确整数计算。

Financial Core 禁止：

```text
f32
f64
```

---

# 19. REVM Simulation

Simulation 必须是真实 EVM Execution，而不是简单 Reserve Math。

必须支持：

* Token bytecode；
* Pool bytecode；
* Executor bytecode；
* ERC20 transfer；
* approve；
* swap；
* fee；
* tax；
* revert；
* gas；
* state transition。

---

# 20. Simulation State Identity

Simulation 必须明确：

```text
chain_id
block_number
block_hash
```

禁止：

```text
Opportunity @ Block N
Simulation @ latest
```

---

# 21. eth_call State Identity

`eth_call` 的 canonical identity 至少为：

```text
chain
block
to
calldata
```

特别强调：

> Block 是 eth_call State Identity 的组成部分。

同一个：

```text
pool
+
calldata
```

在不同 block 下不能默认复用。

---

# 22. State Ownership

必须区分：

```text
StateStore
GraphSnapshot
REVM Canonical State
RPC Node State
```

它们不是同一个东西。

禁止：

```text
GraphSnapshot
   ↓
直接当作
   ↓
REVM Canonical State
```

---

# 23. State Freshness

必须区分：

```text
verified
```

和：

```text
fresh
```

即：

> Verified ≠ Fresh Forever。

---

# 24. RPC Reduction 原则

RPC 优化必须遵循：

```text
Trace
 ↓
Measure
 ↓
Classify
 ↓
Prove
 ↓
Optimize
 ↓
Re-measure
```

禁止凭感觉优化。

必须区分：

```text
Duplicate
Reusable
Safe to Reuse
```

三者不是同一个概念。

---

# 25. Safe Propagation ≠ RPC Saving

如果某个 block context 能够安全从 Producer 传播到 Consumer：

```text
SAFE PROPAGATION
```

并不意味着：

```text
RPC SAVING
```

只有实际减少了 RPC 请求或等待时间，才能计入 RPC Reduction。

---

# 26. Flashblocks 定义

Flashblocks 是：

> GIWA 的低延迟 Early State / Event Signal。

它主要解决：

> 更早发现市场状态变化。

它不是 Canonical Final State。

---

# 27. Flashblocks 与 Canonical RPC 的关系

最终架构：

```text
Flashblocks
     ↓
Early Signal
     ↓
Candidate / State Update
     ↓
Canonical Verification
     ↓
Simulation
     ↓
Risk
     ↓
Execution
```

原则：

> Flashblocks = Radar
> Canonical RPC = Final Judge

禁止：

```text
Flashblocks
 ↓
直接相信
 ↓
直接执行
```

---

# 28. 官方 Flashblocks Endpoint

当前开发阶段：

> **直接使用官方 Flashblocks Endpoint。**

不需要等待自建 Flashblocks-aware RPC 完成。

架构：

```text
MEV Bot
   │
   ├── Canonical RPC Provider
   │
   └── Flashblocks Provider
```

当前：

```text
Canonical → Official RPC
Flashblocks → Official Flashblocks Endpoint
```

---

# 29. Flashblocks Provider 抽象

业务代码不得直接绑定：

```text
官方 Flashblocks URL
```

必须通过：

```text
FlashblocksProvider
```

访问。

未来：

```text
Official Flashblocks
        ↓
Self-hosted Flashblocks-aware RPC
```

原则上只切换：

```text
FLASHBLOCKS_URL
```

而不重写：

* Graph；
* PathFinder；
* Simulation；
* Risk；
* Execution。

---

# 30. Canonical RPC Provider

同样必须抽象：

```text
CanonicalRpcProvider
```

当前：

```text
Official GIWA RPC
```

未来：

```text
Self-hosted GIWA Node
```

程序配置：

```text
GIWA_RPC_URL
```

例如同机：

```text
http://127.0.0.1:8545
```

Docker / private network：

```text
http://giwa-node:8545
```

业务代码不关心 Provider 来源。

---

# 31. M1～M14 总路线

最终路线固定为：

```text
Phase 1
Core Market Engine
M1
M2
M3
M4

        ↓

Phase 2
Live Execution
M5
M6
M7

        ↓

Phase 3
Hot-path Hardening
M8.1
M8.2
M8.3.1
M8.3.2
M8.3.3
M8.4.1
M8.4.2
M8.4.3
M8.4.4
M8.5.1
M8.6

        ↓

Phase 4
Strategy Expansion
M9
M10
M11

        ↓

Phase 5
Low-Latency Infrastructure
M12

        ↓

Phase 6
Production Operations
M13

        ↓

Phase 7
Production Validation
M14
```

---

# 32. M1 — Chain / Pool / State Foundation

**状态：COMPLETE**

完成：

* GIWA Chain；
* Chain ID；
* Historical Block；
* Pool Registry；
* PoolMeta；
* PoolState；
* Sync；
* Pool Attestation；
* Event Ordering；
* Replay 基础；
* U256；
* Unattested Emitter Rejection。

核心结论：

> Reserve 必须来自经过验证的 Sync。

---

# 33. M2 — Graph Infrastructure

**状态：COMPLETE**

完成：

* Token Node；
* Pool Edge；
* Directed Edge；
* Multiple Pools；
* GraphSnapshot；
* Registry Evidence；
* Conflict Rejection。

注意：

> M2 不是 PathFinder。

---

# 34. M3 — Arbitrage Math

**状态：COMPLETE**

完成：

* V2 Math；
* Fee；
* U256；
* Optimal Input；
* Gross Profit；
* Two-pool Opportunity。

M3 Opportunity：

> 只是理论上具有套利价值的候选机会。

不是：

> 已确认可执行交易。

---

# 35. M4 — REVM Simulation + Risk

**状态：COMPLETE**

完成：

* REVM；
* Historical State；
* Block-pinned State；
* Code；
* Storage；
* Balance；
* Nonce；
* eth_call；
* Gas；
* Revert；
* Token Tax；
* Risk。

M4 明确：

> Mathematical Opportunity ≠ EVM Executable Opportunity。

---

# 36. M5 — Live Pipeline

**状态：COMPLETE**

完成：

```text
GIWA
 ↓
Head
 ↓
Logs
 ↓
Decode
 ↓
State Update
 ↓
Graph
 ↓
Opportunity
 ↓
Simulation
```

Replay / Live 共用同一 StateUpdate Pipeline。

M5 当前已冻结。

---

# 37. M6 — Transaction Execution

**状态：COMPLETE**

完成：

```text
RiskApproved
 ↓
Intent
 ↓
Build
 ↓
Sign
 ↓
Submit
 ↓
Receipt
```

包括：

* Transaction Builder；
* RLP；
* Signer；
* Nonce；
* Fee；
* Submitter；
* ReceiptTracker；
* Lifecycle。

当前 signer 使用环境变量 private key，仅适合开发 / 测试阶段。

---

# 38. M7 — Real Arbitrage

**状态：COMPLETE**

完成真实 GIWA Arbitrage：

```text
Detected
 ↓
Simulated
 ↓
RiskApproved
 ↓
Preflighted
 ↓
Built
 ↓
Signed
 ↓
Submitted
 ↓
Included
 ↓
Settled
 ↓
ProfitVerified
```

已验证真实链上交易和 Realized Profit。

M7 证明：

> 当前系统具备真实 Arbitrage Execution 能力。

但 M7 仍然是：

> Direct Pool Execution。

不等于 M10 Executor Contract 已完成。

---

# 39. M8.1 — Lifecycle Instrumentation

**状态：COMPLETE**

完成：

* Stage lifecycle；
* Monotonic Clock；
* Duration；
* Failure；
* Skip；
* Cancel；
* No-extra-RPC instrumentation。

---

# 40. M8.2 — RPC Trace

**状态：COMPLETE**

目标：

> 找出 Simulation RPC Hotspot。

已经证明：

* RPC 是主要耗时来源；
* Duplicate State Read 大量存在；
* RPC 基本串行。

---

# 41. M8.3.1 — Simulation-local Cache

**状态：COMPLETE**

完成：

```text
80 RPC
 ↓
39 RPC
```

Duplicate：

```text
> 0
```

下降为：

```text
0
```

证明：

> Simulation-local State Read Cache 是当前已经证明安全的优化。

---

# 42. M8.3.2 — RPC Bucket Diagnosis

**状态：COMPLETE**

完成：

* Storage RPC Diagnosis；
* Account Triple Diagnosis；
* Pipeline RPC Visibility；
* RPC Duration Attribution。

---

# 43. M8.3.3 — Bounded Concurrency

**状态：COMPLETE**

验证：

```text
C1
C2
C4
```

证明：

> 有真实 RPC overlap，可以通过 bounded concurrency 降低 wall-clock latency。

但：

> C4 不作为当前生产默认值。

---

# 44. M8.4.1 — Storage Dependency

**状态：COMPLETE**

当前结论：

> 当前 REVM fiber path 的 Storage Reads 表现为顺序访问。

但：

> 不能推出 Storage 天然不可并发。

没有 semantic independence proof 时，不强制并发。

---

# 45. M8.4.2 — Evidence Gate

**状态：COMPLETE**

完成：

* Semantic Identity；
* Evidence Gate；
* Negative Controls；
* Fixed-block Equality；
* Serialization Equality；
* Clock Validation。

核心：

> Presentation Order 不能作为 Semantic Identity。

---

# 46. M8.4.3 — State Ownership

**状态：COMPLETE**

完成：

* State Ownership；
* Freshness；
* Invalidation；
* Lifecycle；
* Reuse Analysis。

结论：

```text
safe_to_reuse_now = 0
```

---

# 47. M8.4.4 — Block Context Propagation

**状态：COMPLETE**

最终 verdict：

```text
SAFE_PROPAGATION_PROVEN_NO_NET_RPC_SAVING
```

即：

> 可以安全传播 block context，但当前没有形成实际 RPC saving。

---

# 48. M8.5.1 — eth_call Ownership

**状态：COMPLETE**

最终 verdict：

```text
REUSE_BLOCKED
```

核心结论：

```text
eth_call
=
chain
+
block
+
to
+
calldata
```

当前：

```text
safe_to_reuse_now = 0
```

Detection → Preflight 的第二次 eth_call：

> 暂不复用。

---

# 49. M8.6 — RPC Reduction Opportunity Census

**状态：COMPLETE**

M8.6 的目标：

> 对整个 RPC Surface 做完整 Census 和 Reduction Diagnosis。

必须输出：

```text
data/evidence/m8/m8.6/rpc-reduction-candidates.json
data/evidence/m8/m8.6/rpc-census.json
data/evidence/m8/m8.6/reduction-matrix.json
data/evidence/m8/m8.6/rejected-opportunities.json
data/evidence/m8/m8.6/priority-queue.json
```

必须回答：

* RPC Semantic Role；
* Identity；
* Duplicate；
* Reusable；
* Safe；
* Verification Responsibility；
* Alternative Verification；
* Theoretical Saving；
* Safe Saving；
* Rejected Reason；
* Priority。

M8.6：

> **只诊断，不优化。**

禁止：

* 新 Cache；
* 新 RPC；
* 行为改变；
* Signing；
* Broadcast；
* Real Arbitrage。

M8.6 完成后才决定下一轮 RPC 优化。

最终 verdict：

```text
NO_SAFE_RPC_REDUCTION_FOUND
```
M8 RPC Reduction Census 已完成。当前没有被证实可以安全消除的 RPC。后续如果出现 RPC Reduction 机会，必须产生新的证据与独立里程碑，不得因为 M8 已完成而直接向生产代码增加缓存、复用或跨阶段状态载体。

---

# 50. M9 — Pool Discovery + Graph Search + PathFinder

**状态：NEXT**

M9 正式进入 Strategy Expansion。

---

## 50.1 Pool Discovery

实现：

```text
Factory Events
+
Historical Scan
+
Candidate Sources
 ↓
Candidate Pool
 ↓
Protocol Verification
 ↓
Pool Registry
```

不允许：

> 手工维护完整 DEX 列表作为核心发现机制。

Known Factory / Allowlist 可以作为：

* Trust；
* Performance；
* Optimization；

但不是 Discovery 唯一来源。

---

## 50.2 V2 Protocol Adapter

统一：

```text
PoolMeta
PoolState
Fee
Token0
Token1
Factory
```

---

## 50.3 Graph

```text
Token = Node
Pool = Edge
```

---

## 50.4 PathFinder

正式实现：

* Cycle Detection；
* Bounded Graph Search；
* Bellman-Ford / SPFA；
* Path Ranking；
* Candidate Generation。

具体算法可以根据真实 GIWA Graph 数据决定，但 M9 必须真正具备 PathFinder 能力。

---

## 50.5 Multi-hop Candidate

例如：

```text
WETH
 ↓
USDC
 ↓
BLS
 ↓
WETH
```

PathFinder 负责：

> 哪条路径值得模拟？

Amount Optimizer 负责：

> 输入多少？

Simulation 负责：

> EVM 中到底能不能执行？

---

# 51. M9 Flashblocks 接入

M9 开始正式把：

> **官方 Flashblocks Endpoint**

作为开发数据源之一。

架构：

```text
Official Flashblocks
        ↓
FlashblocksProvider
        ↓
Early State / Event
        ↓
Candidate Update
        ↓
Canonical Verification
        ↓
Graph / PathFinder
```

重要：

> M9 不等待自建 Flashblocks-aware RPC。

---

# 52. M10 — Arbitrage Executor Contract

**状态：PLANNED**

目标：

```text
EOA
 ↓
ArbitrageExecutor
 ↓
Pool A
 ↓
Pool B
 ↓
Pool C
 ↓
Executor
 ↓
EOA
```

必须支持：

* Multi-hop；
* Atomic Execution；
* Min Output；
* Slippage Protection；
* Profit Check；
* Safe Transfer；
* Approval；
* Access Control；
* Reentrancy Protection；
* Emergency Pause；
* Target Validation；
* Token Validation；
* Pool Validation。

---

# 53. M10 Executor 安全原则

任何一项不满足：

```text
final balance
>=
initial balance + minimum profit
```

则：

```text
revert
```

所有步骤必须：

```text
success
```

否则：

```text
entire transaction revert
```

---

# 54. M10 Deployment

必须保存：

```text
Contract Address
ABI
Bytecode Hash
Deployment Tx
Deployment Block
Configuration
```

并验证：

```text
Rust
 ↓
ABI
 ↓
Calldata
 ↓
Executor
 ↓
Pool
```

---

# 55. M11 — Multi-Hop Simulation / Execution Integration

**状态：PLANNED**

最终完整链路：

```text
Flashblocks / Chain
 ↓
Pool State
 ↓
Graph
 ↓
PathFinder
 ↓
Candidate
 ↓
Optimal Input
 ↓
REVM
 ↓
Risk
 ↓
Executor Calldata
 ↓
Build
 ↓
Sign
 ↓
Submit
 ↓
Receipt
 ↓
Settlement
 ↓
Realized Profit
```

---

# 56. M11 — Multi-Lane

最终支持：

```text
Opportunity
 ├── Lane A
 │    └── Wallet A / Nonce
 │
 ├── Lane B
 │    └── Wallet B / Nonce
 │
 └── Lane C
      └── Wallet C / Nonce
```

必须处理：

* Pool Conflict；
* Capital Conflict；
* Nonce Conflict；
* Wallet Isolation；
* Execution Conflict；
* Scheduling。

增加：

```text
Execution Planner
Conflict Detector
Lane Scheduler
```

Multi-Lane 不等于简单线程并发。

---

# 57. M12 — Low-Latency Infrastructure

**状态：PLANNED**

M12 是生产低延迟基础设施阶段。

包含：

```text
Self-hosted GIWA Node
Flashblocks-aware RPC
Flashblocks Event Pipeline
Canonical Reconciliation
RPC HA
Node HA
Monitoring
Conditional Private / Direct Sequencer
```

---

# 58. M12 — Self-hosted GIWA Node

目标：

```text
MEV Bot
 ↓
Self-hosted GIWA Node
 ↓
GIWA
```

同机部署时：

```text
GIWA_RPC_URL=http://127.0.0.1:8545
```

Docker / private network 时：

```text
GIWA_RPC_URL=http://giwa-node:8545
```

核心原则：

> 业务代码不能关心 RPC 是官方还是自建。

---

# 59. M12 — Flashblocks-aware RPC

最终：

```text
MEV Bot
   │
   ├── CanonicalRpcProvider
   │
   └── FlashblocksProvider
```

生产：

```text
Canonical
   ↓
Self-hosted GIWA Node

Flashblocks
   ↓
Self-hosted Flashblocks-aware RPC
```

---

# 60. M12 — Provider 可替换原则

开发：

```text
Official RPC
+
Official Flashblocks
```

生产：

```text
Self-hosted GIWA Node
+
Self-hosted Flashblocks-aware RPC
```

必须做到：

> Provider 切换不影响 Strategy / Simulation / Execution 业务逻辑。

---

# 61. M12 — Canonical Reconciliation

必须处理：

```text
Flashblocks State
        ↓
Canonical Block
        ↓
Reconciliation
```

处理：

* stale；
* conflicting；
* missing；
* ordering；
* block identity；
* canonical transition。

---

# 62. M12 — RPC / Node HA

Canonical：

```text
Self-hosted Node
       ↓
Public RPC Fallback
```

Flashblocks：

```text
Self-hosted Flashblocks
       ↓
Official Flashblocks Fallback
```

需要：

* Health Check；
* Timeout；
* Reconnect；
* Failover；
* Latency Measurement。

---

# 63. M12 — Private / Direct Sequencer

目标：

```text
Bot
 ↓
Direct / Private Sequencer
 ↓
Sequencer
```

作用：

* 降低提交路径延迟；
* 改善 inclusion；
* 改善 ordering；
* 降低 public RPC exposure。

当前状态：

```text
BLOCKED / CONDITIONAL
```

原因：

> GIWA 当前相关方法返回 `-32601 Method Not Found`。

因此：

> 不允许让 Private / Direct Sequencer 阻塞 M9～M12 的其他工作。

如果 GIWA 后续提供可用接口，再进行集成。

---

# 64. M13 — Production Operations

**状态：PLANNED**

目标：

> 让 Bot 能够 7×24 运行。

---

# 65. Runtime State

必须支持：

```text
STARTING
RUNNING
DEGRADED
RECOVERING
STOPPING
STOPPED
```

---

# 66. Health

监控：

```text
RPC
GIWA Node
Flashblocks
Chain Head
Simulation
Execution
Wallet
Executor Contract
Lane
```

---

# 67. Metrics

Chain：

```text
blocks
block latency
Flashblocks events
reconciliation
```

RPC：

```text
requests
latency
errors
timeouts
```

Strategy：

```text
opportunities
simulations
risk rejected
```

Execution：

```text
built
signed
submitted
included
reverted
settled
```

Profit：

```text
estimated
simulated
executed
realized
gross
gas
L1
L2
net
```

---

# 68. Structured Logging

每个关键事件至少包含：

```text
timestamp
block
tx
opportunity
route
pool
lane
wallet
stage
duration
result
error
```

---

# 69. AlertManager

架构：

```text
AlertManager
   ├── Telegram
   ├── Webhook
   ├── Log
   └── Future Channel
```

---

# 70. Critical Alert

必须覆盖：

```text
Bot stopped
RPC down
Node down
Flashblocks down
Nonce stuck
Unexpected revert
Wallet low balance
Executor abnormal
Lane abnormal
```

---

# 71. Info Alert

例如：

```text
Arbitrage Success
Profit
Route
Gas
Transaction
```

---

# 72. Capital Management

M13 必须增加：

* Wallet Balance；
* Trading Capital；
* Reserved Capital；
* Gas Capital；
* Minimum Balance；
* Capital Limit；
* Per-lane Capital。

---

# 73. Nonce Recovery

必须处理：

```text
Nonce Gap
Nonce Stuck
Replacement
Unknown Submission
Restart Recovery
```

特别：

> Unknown Submission 不允许盲目重复发送。

---

# 74. Graceful Restart

Bot 重启后必须能够：

```text
Recover
 ↓
Read Canonical State
 ↓
Recover Nonce
 ↓
Recover Pending Transactions
 ↓
Resume
```

不能简单：

```text
restart
 ↓
assume everything is fine
```

---

# 75. KMS / HSM

开发阶段：

```text
Environment Private Key
```

生产阶段：

```text
Bot
 ↓
Signing Request
 ↓
KMS / HSM
 ↓
Signature
 ↓
Bot
 ↓
GIWA
```

生产 Bot 不应该持有 raw private key。

---

# 76. Dashboard

最终 Dashboard 至少显示：

```text
Chain Head
Flashblocks
RPC Health
Node Health
Opportunities
Simulations
Executions
Success Rate
Latency
Gas
Profit
Capital
Lane
Errors
Alerts
```

---

# 77. M14 — Production Validation

**状态：PLANNED**

M14 不再验证：

> “程序能不能跑。”

而验证：

> “程序能不能长期稳定运行并在真实竞争环境下产生可验证结果。”

---

# 78. 24h Validation

检查：

* Runtime；
* Memory；
* CPU；
* RPC；
* Node；
* Flashblocks；
* Simulation；
* Execution；
* Nonce；
* Capital。

---

# 79. 72h Validation

增加：

* Recovery；
* Long-run stability；
* Opportunity density；
* Execution stability；
* Capital stability。

---

# 80. 7-day Validation

最终验证：

```text
Real Market
Real Competition
Real Opportunity
Real Cost
Real Profit
```

---

# 81. Failure Injection

必须主动验证：

```text
RPC Down
Node Down
Flashblocks Down
Network Delay
Timeout
Unknown Submission
Nonce Stuck
Executor Revert
Low Balance
Restart
```

每一种都必须验证：

```text
Failure
 ↓
Detection
 ↓
Recovery
 ↓
Resume
```

---

# 82. Latency Benchmark

最终记录：

```text
Flashblocks
 ↓
Detection
 ↓
Simulation
 ↓
Build
 ↓
Sign
 ↓
Submit
 ↓
Inclusion
```

至少：

```text
p50
p95
p99
```

---

# 83. Profitability Validation

必须记录：

```text
Opportunity Count
Accepted Count
Rejected Count
Simulation Success
Execution Success
Miss Rate
Gross Profit
Gas
L1 Fee
L2 Fee
Net Profit
Realized Profit
Capital Efficiency
```

---

# 84. M1～M14 状态总表

| Milestone | 内容                                            | 状态          |
| --------- | --------------------------------------------- | ----------- |
| M1        | Chain / Pool / State Foundation               | COMPLETE    |
| M2        | Graph Infrastructure                          | COMPLETE    |
| M3        | Arbitrage Math / Optimal Input                | COMPLETE    |
| M4        | REVM Simulation / Risk                        | COMPLETE    |
| M5        | Live Pipeline                                 | COMPLETE    |
| M6        | Transaction Execution                         | COMPLETE    |
| M7        | Real GIWA Arbitrage                           | COMPLETE    |
| M8.1      | Lifecycle Instrumentation                     | COMPLETE    |
| M8.2      | RPC Trace / Diagnosis                         | COMPLETE    |
| M8.3.1    | Simulation-local Cache                        | COMPLETE    |
| M8.3.2    | RPC Bucket Diagnosis                          | COMPLETE    |
| M8.3.3    | Bounded Concurrency                           | COMPLETE    |
| M8.4.1    | Storage Dependency                            | COMPLETE    |
| M8.4.2    | Evidence Gate                                 | COMPLETE    |
| M8.4.3    | State Ownership                               | COMPLETE    |
| M8.4.4    | Block Context Propagation                     | COMPLETE    |
| M8.5.1    | eth_call Ownership                            | COMPLETE    |
| M8.6      | RPC Reduction Census                          | IN PROGRESS |
| M9        | Discovery + Graph Search + PathFinder         | PLANNED     |
| M10       | Arbitrage Executor Contract                   | PLANNED     |
| M11       | Multi-Hop + Multi-Lane                        | PLANNED     |
| M12       | Self-hosted Node + Flashblocks Infrastructure | PLANNED     |
| M13       | Production Operations                         | PLANNED     |
| M14       | Production Validation                         | PLANNED     |

---

# 85. 当前项目路线

当前唯一主线：

```text
M8.6
 ↓
M9
 ↓
M10
 ↓
M11
 ↓
M12
 ↓
M13
 ↓
M14
```

不要跳跃。

---

# 86. M9～M12 的关系

```text
M9
Market Intelligence
 ↓
Pool Discovery
 ↓
Graph
 ↓
PathFinder

M10
Execution Primitive
 ↓
ArbitrageExecutor

M11
Strategy + Execution Integration
 ↓
Multi-hop
 ↓
Multi-lane

M12
Infrastructure
 ↓
Self-hosted Node
 ↓
Flashblocks-aware RPC
 ↓
HA
 ↓
Reconciliation
 ↓
Low Latency
```

---

# 87. 为什么不是先自建 Node

自建 Node 很重要，但它不是 M9/M10 的前置条件。

现在可以：

```text
Official RPC
+
Official Flashblocks Endpoint
```

继续开发。

M12 再切换：

```text
Self-hosted Node
+
Self-hosted Flashblocks-aware RPC
```

因此不会因为基础设施部署阻塞策略开发。

---

# 88. 为什么 Flashblocks 现在就进入 M9/M11

因为 Flashblocks 的价值是：

> 更早发现状态变化。

它应该参与：

```text
Discovery
 ↓
State
 ↓
Graph
 ↓
PathFinder
```

而不是等所有策略完成后才接入。

但是：

> Flashblocks 不是 Canonical State。

---

# 89. 最终 Hot Path

最终 Hot Path：

```text
Flashblocks / Chain
        ↓
State Update
        ↓
Graph Update
        ↓
PathFinder
        ↓
Candidate
        ↓
Optimal Input
        ↓
REVM
        ↓
Risk
        ↓
Executor Calldata
        ↓
Execution Planner
        ↓
Lane
        ↓
Build
        ↓
Sign
        ↓
Submit
```

Hot Path 中禁止：

```text
LLM
AI Decision
Remote AI API
Human Approval
Slow Analytics
```

---

# 90. Multi-Lane 原则

Multi-Lane 不是：

```text
Thread A
Thread B
Thread C
```

而是：

```text
Capital Isolation
+
Nonce Isolation
+
Execution Isolation
+
Conflict Detection
```

必须先确认：

```text
Pool Conflict
Capital Conflict
Nonce Conflict
```

才能调度。

---

# 91. Security Principles

必须避免：

* Private Key Leakage；
* Unauthorized Executor；
* Arbitrary Target；
* Arbitrary Token；
* Arbitrary Pool；
* Reentrancy；
* Unexpected Approval；
* Unexpected Transfer；
* Unbounded Slippage；
* Unbounded Gas；
* Stale State Execution。

---

# 92. Evidence Principles

所有重要结论必须具备证据。

禁止：

```text
理论上应该可以
```

直接写成：

```text
已经验证
```

必须明确：

```text
Observed
Verified
Measured
Simulated
Executed
Realized
```

---

# 93. Negative Control

涉及安全 / State / RPC Reduction 的实验必须包含 Negative Control。

例如：

```text
Identity Mutation
Verification Mutation
Freshness Mutation
Safe-saving Mutation
Clock Mutation
Row-order Mutation
```

如果 Negative Control 无法发现伪造结果：

> Evidence 不通过。

---

# 94. Cargo Gate

最终至少需要：

```text
cargo fmt --check
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

并进行：

* Secret Scan；
* Panic / unwrap / expect 检查；
* Evidence Recompute。

---

# 95. Cargo Serial Execution

Pipeline / Evidence Tests 使用共享：

```text
target/pipeline-tests/
```

时：

> 必须串行执行。

禁止因为并发 Cargo Test 导致：

* false failure；
* statistics corruption；
* evidence corruption。

---

# 96. Financial Core

禁止：

```text
f32
f64
```

必须：

```text
U256
Integer Math
```

所有金额必须明确：

```text
Token
Decimals
Unit
```

---

# 97. 重要概念边界

必须永久保持以下区别：

```text
Discovery ≠ Trust

Observed ≠ Verified

Verified ≠ Fresh

Duplicate ≠ Reusable

Reusable ≠ Safe

Safe Propagation ≠ RPC Saving

Graph Snapshot ≠ REVM State

Estimated ≠ Simulated

Simulated ≠ Executed

Executed ≠ Realized

Flashblocks ≠ Canonical State

Multi-Lane ≠ RPC Concurrency
```

---

# 98. 明确冻结的策略范围

当前项目不做：

```text
V3
Curve
Balancer
Liquidation
Sandwich
Cross-chain
AI Strategy
LLM Hot Path
```

除非未来单独修改 PRD，否则这些内容不得进入开发。

---

# 99. Multi-chain Gate

只有完成：

```text
GIWA Real Arbitrage
✓

Simulation Stable
✓

Execution Stable
✓

Flashblocks Stable
✓

Self-hosted Node Stable
✓

Latency Measured
✓

Failure Recovery
✓

7×24 Runtime
✓

Production Validation
✓
```

之后，才重新评估：

```text
ChainProfile
ChainAdapter
ExecutionAdapter
ProviderManager
```

---

# 100. Testnet → Mainnet

当前首先完成：

```text
GIWA Testnet
```

未来 Mainnet：

> 优先通过 Network Profile / Configuration 切换，而不是重写 State / Graph / Opportunity。

但是必须重新验证：

* Chain ID；
* RPC；
* WebSocket；
* Flashblocks；
* Sequencer；
* DEX；
* Factory；
* Pool；
* Token；
* Gas；
* Finality；
* Transaction Submission；
* Executor Contract；
* Execution Semantics。

因此：

```text
Testnet → Mainnet
=
Configuration Switch
+
Full Revalidation
```

不是：

```text
直接切 RPC
```

---

# 101. 生产 Provider 最终形态

Canonical：

```text
CanonicalRpcProvider
       │
       ├── Self-hosted GIWA Node
       │
       └── Official RPC Fallback
```

Flashblocks：

```text
FlashblocksProvider
       │
       ├── Self-hosted Flashblocks-aware RPC
       │
       └── Official Flashblocks Fallback
```

两者职责必须保持独立。

---

# 102. 最终系统架构

```text
                         GIWA
                           │
             ┌─────────────┴─────────────┐
             │                           │
       Canonical Chain              Flashblocks
             │                           │
             ↓                           ↓
       GIWA Node                  Flashblocks RPC
             │                           │
             └─────────────┬─────────────┘
                           ↓
                  Chain / Market Layer
                           ↓
                    Pool Discovery
                           ↓
                    Protocol Adapter
                           ↓
                    Pool Registry
                           ↓
                      StateStore
                           ↓
                         Graph
                           ↓
                      PathFinder
                           ↓
                  Arbitrage Candidate
                           ↓
                  Optimal Input Search
                           ↓
                     REVM Simulation
                           ↓
                         Risk
                           ↓
                 ArbitrageExecutor
                           ↓
                 Execution Planner
                           ↓
                  Multi-Lane Scheduler
                           ↓
                     Transaction
                           ↓
                        Signer
                           ↓
                Private / Direct*
                           ↓
                     GIWA Submit
                           ↓
                       Receipt
                           ↓
                      Settlement
                           ↓
                  Realized Profit
                           ↓
               Metrics / AlertManager
                           ↓
                      Replay / Evidence
```

---

# 103. 最终产品演进

当前：

```text
GIWA Testnet Arbitrage Bot
```

然后：

```text
GIWA Testnet
 ↓
Real Arbitrage
 ↓
Multi-hop
 ↓
Low Latency
 ↓
Production
 ↓
Mainnet
```

再未来：

```text
EVM Arbitrage Engine
```

最终才考虑：

```text
Multi-EVM MEV Bot
```

原则：

> **先做深，再做广。**

---

# 104. 防跑偏机制

任何新增需求必须回答：

### Q1

是否直接服务：

```text
Opportunity
Simulation
Risk
Execution
Latency
Profit
Reliability
```

之一？

如果不是：

> 默认不进入核心项目。

### Q2

是否属于：

```text
GIWA Arbitrage
```

当前目标？

如果不是：

> 默认延后。

### Q3

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

### Q4

是否只是为了未来多链而提前增加复杂度？

如果是：

> 延后。

### Q5

是否创建一个全新的“大系统”？

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

# 105. 新需求标准

所有新增需求必须标记：

```text
Priority:
P0 / P1 / P2 / P3

Stage:
M8 / M9 / M10 / M11 / M12 / M13 / M14 / Future

Layer:
Chain
Protocol
State
Graph
Opportunity
Simulation
Risk
Execution
Signer
Replay
Metrics
Infrastructure

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

# 106. 当前优先级

## P0

```text
M8.6 RPC Reduction Census
M9 Pool Discovery
M9 PathFinder
M10 ArbitrageExecutor
M11 Multi-hop
M11 Multi-Lane
```

## P1

```text
Official Flashblocks Integration
Flashblocks Reconciliation
Latency Benchmark
Risk Hardening
Recovery
Circuit Breaker
```

## P2

```text
Self-hosted GIWA Node
Self-hosted Flashblocks-aware RPC
RPC HA
Node HA
Production Operations
```

## P3 / Future

```text
Mainnet
Multi-chain
V3
Curve
Balancer
Cross-chain
Sandwich
Liquidation
AI Strategy
```

---

# 107. 当前工作位置

当前：

```text
M1  ✅
M2  ✅
M3  ✅
M4  ✅
M5  ✅
M6  ✅
M7  ✅

M8.1  ✅
M8.2  ✅
M8.3.1 ✅
M8.3.2 ✅
M8.3.3 ✅
M8.4.1 ✅
M8.4.2 ✅
M8.4.3 ✅
M8.4.4 ✅
M8.5.1 ✅

M8.6  🚧
```

下一步：

```text
M8.6
 ↓
M9
```

M9 开始：

> **正式进入 Graph Search / PathFinder / Pool Discovery，并同时开始使用官方 Flashblocks Endpoint 作为开发阶段的 Early Market Data Source。**

---

# 108. 最终路线

```text
                         NOW
                          │
                          ▼
                M8.6 RPC Census
                          │
                          ▼
                M9 Discovery
                    + Graph
                    + PathFinder
                    + Flashblocks
                          │
                          ▼
                M10 Executor
                          │
                          ▼
                M11 Multi-Hop
                    + Multi-Lane
                          │
                          ▼
                M12 Low Latency
                    + GIWA Node
                    + Flashblocks RPC
                    + HA
                    + Reconciliation
                          │
                          ▼
                M13 Production
                    + 7×24
                    + Metrics
                    + Alerts
                    + KMS/HSM
                          │
                          ▼
                M14 Validation
                    + 24h
                    + 72h
                    + 7d
                    + Real Competition
                    + Real Profit
                          │
                          ▼
                  GIWA Production
```

---

# 109. 最终成功标准

只有同时满足以下条件，才认为本项目真正完成：

```text
✓ 自动发现 GIWA Pool
✓ 正确维护 Pool State
✓ 构建 Liquidity Graph
✓ PathFinder 找到 Multi-hop Route
✓ Optimal Input 正确
✓ REVM Simulation 正确
✓ Risk 正确
✓ ArbitrageExecutor 正确
✓ Multi-hop 执行正确
✓ Multi-Lane 正确
✓ Flashblocks 能提前发现状态变化
✓ Canonical Reconciliation 正确
✓ Self-hosted GIWA Node 稳定
✓ Self-hosted Flashblocks-aware RPC 稳定
✓ RPC / Node HA
✓ Transaction Submission 稳定
✓ Receipt / Settlement 正确
✓ Realized Profit 可验证
✓ 7×24 Runtime
✓ Failure Recovery
✓ KMS / HSM
✓ Telegram / Webhook Alert
✓ 24h Stability
✓ 72h Stability
✓ 7-day Stability
✓ Real Market Competition
✓ Real Profitability Evidence
```

最终系统不是：

> “一个能跑套利 Demo 的程序”。

而是：

> **一个从市场发现、状态感知、路径搜索、EVM 模拟、风险控制、原子执行，到低延迟基础设施、生产运维和真实利润验证完整闭环的 GIWA MEV Arbitrage System。**