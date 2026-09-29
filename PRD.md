# EVM MEV Bot

## Product Requirements Document

**项目名称：** EVM MEV Bot
**当前版本：** v0.1
**文档性质：** 项目总 PRD + 长期范围约束
**目标语言：** Rust
**目标平台：** EVM-compatible Blockchains
**项目阶段：** 从零开始的新项目

---

# 1. 文档目的

本文档不是单纯用于描述产品功能。

本文档的核心职责是：

1. 定义 EVM MEV Bot 到底是什么；
2. 明确项目长期技术方向；
3. 明确 v0.1 应该做什么；
4. 明确 v0.1 不应该做什么；
5. 明确系统边界；
6. 明确核心数据流；
7. 明确模块职责；
8. 明确后续版本演进方向；
9. 明确每个阶段的验收标准；
10. 防止项目在开发过程中逐渐演变成：

* 通用 EVM 交易分析系统；
* 通用区块链数据平台；
* 通用交易语义识别系统；
* ABI/Selector 数据库；
* 区块链浏览器；
* AI Agent 平台；
* 通用套利研究平台。

**任何后续需求，如果与本文档定义的核心目标冲突，应优先修改 PRD，而不是直接进入代码。**

---

# 2. 产品定义

## 2.1 一句话定义

> **EVM MEV Bot 是一个基于 Rust 构建的、面向多个 EVM 链的低延迟 MEV 机会发现、模拟、风险判断和交易执行系统。**

系统最终目标不是“理解区块链上的所有交易”。

系统最终目标是：

> **在尽可能短的时间内发现具有真实盈利可能的 MEV Opportunity，并通过模拟、风险控制和执行链路将 Opportunity 转化为真实交易。**

---

# 3. 核心目标

系统的最终核心链路：

```text
Blockchain
    ↓
Block / Transaction / Log / State Change
    ↓
Market State
    ↓
Pool State
    ↓
Liquidity Graph
    ↓
Opportunity Detection
    ↓
Simulation
    ↓
Profit Calculation
    ↓
Risk Control
    ↓
Execution
    ↓
On-chain Result
    ↓
Metrics / Replay
```

整个系统应该围绕这一条链路建设。

---

# 4. 产品北极星

## 4.1 North Star

```text
Chain Event
     ↓
State Change
     ↓
Market State
     ↓
Graph Change
     ↓
Opportunity
     ↓
Simulation
     ↓
Risk
     ↓
Execution
     ↓
Result
     ↓
Metrics
     ↓
Replay
     ↺
```

## 4.2 核心原则

系统必须优先保证：

```text
Correctness
    >
Completeness
    >
Latency
    >
Complexity
```

但是在进入 Live MEV 阶段后，延迟会成为与正确性同等级的重要指标。

---

# 5. 产品目标

## 5.1 长期目标

构建一个：

* Rust 原生；
* 多 EVM 链；
* 低延迟；
* 状态驱动；
* Opportunity-driven；
* Simulation-first；
* Risk-controlled；
* Execution-ready；
* 可 Replay；
* 可 Benchmark；
* 可扩展协议；

的 MEV Bot。

---

# 6. 第一阶段目标

v0.1 不追求真正赚钱。

v0.1 的目标是建立：

> **可靠的 Chain → State → Graph → Opportunity 基础设施。**

即：

```text
Chain
 ↓
Block
 ↓
Log
 ↓
Protocol Event
 ↓
Pool State
 ↓
Graph
 ↓
Arbitrage Opportunity
```

v0.1 完成以后，系统应该能够回答：

> “当前链上的某个区块变化后，哪些流动性池发生变化？这些变化形成了什么新的交易路径？这些路径是否存在理论套利机会？”

---

# 7. 最终产品能力

长期产品由以下能力组成：

```text
┌───────────────────────────────────────┐
│              EVM MEV Bot              │
├───────────────────────────────────────┤
│ Chain Layer                           │
│ Protocol Layer                        │
│ State Layer                           │
│ Graph Layer                           │
│ Opportunity Layer                     │
│ Simulation Layer                      │
│ Risk Layer                            │
│ Execution Layer                       │
│ Signer Layer                          │
│ Replay Layer                           │
│ Metrics Layer                         │
└───────────────────────────────────────┘
```

核心路径：

```text
Chain
  ↓
Protocol
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

Replay / Metrics 横向贯穿整个系统。

---

# 8. 明确不做的事情

这是本 PRD 最重要的章节之一。

## 8.1 不做通用交易语义系统

不构建：

```text
Transaction
 ↓
Semantic Classification
 ↓
Universal Meaning
```

系统不需要理解链上所有交易。

只需要理解：

> **与 MEV Opportunity 有关的交易、事件和状态变化。**

---

## 8.2 不做通用 ABI 数据库

不以：

* ABI 收集；
* Selector 收集；
* Function Signature 收集；
* Contract 分类；

作为项目主线。

这些数据只能服务于：

```text
Protocol Adapter
Pool Discovery
Event Decode
State Update
```

不能反过来让整个项目围绕 ABI 数据建设。

---

## 8.3 不做区块链浏览器

不做：

* 全链交易浏览器；
* 地址画像；
* Contract Explorer；
* Token Explorer；
* 通用交易搜索；
* 通用数据 API。

---

## 8.4 不做通用区块链数据平台

不构建：

```text
Raw Blockchain Data
      ↓
Data Warehouse
      ↓
Universal Analytics
```

数据只保存 MEV Bot 真正需要的数据。

---

## 8.5 不做 AI Agent

不做：

* AI Trading Agent；
* LLM Strategy；
* Prompt Strategy；
* AI 自动分析机会；
* AI 决策交易。

MEV Hot Path 不允许依赖 LLM。

---

## 8.6 不做 Sandwich

项目不以 Sandwich 为策略方向。

优先策略：

1. Arbitrage；
2. Backrun；
3. 后续再扩展其他 MEV Strategy。

---

## 8.7 不做一开始支持所有协议

初期只实现：

> 能形成稳定 Opportunity 的少量成熟 AMM 协议。

协议数量不是第一阶段目标。

---

## 8.8 不一开始支持所有 AMM 类型

第一阶段优先：

> Uniswap V2-style Constant Product AMM

暂不以：

* Uniswap V3；
* CLMM；
* StableSwap；
* Concentrated Liquidity；

作为 v0.1 的核心实现。

---

## 8.9 不做跨链套利

“多 EVM 链”意味着：

```text
Chain A
Chain B
Chain C
```

拥有统一架构。

并不意味着 v0.x 就实现：

```text
Chain A Token
    ↓
Bridge
    ↓
Chain B Token
```

跨链套利属于后续独立能力。

---

# 9. 产品策略范围

长期优先级：

```text
P0
DEX Arbitrage

P0
Backrun

P1
Multi-hop Arbitrage

P1
Private Transaction / Bundle

P1
Advanced Simulation

P2
Advanced Token Analysis

P2
Additional MEV Strategies

P3
Cross-chain MEV
```

---

# 10. v0.1 产品目标

v0.1 必须完成：

```text
EVM Chain Adapter
        ↓
Historical Blocks
        ↓
Event Decoder
        ↓
Pool Registry
        ↓
Pool State
        ↓
StateStore
        ↓
Graph
        ↓
Opportunity Detector
        ↓
Replay
```

最终可以：

```text
给定：

Chain
Block Range

得到：

Pool State
Graph
Opportunity
Expected Profit
```

---

# 11. v0.1 核心用户场景

虽然这是一个后端系统，但必须从真实使用场景定义需求。

## 场景 1：历史 Replay

输入：

```text
chain = xxx
from_block = X
to_block = Y
```

系统：

```text
读取 Block
 ↓
读取 Logs
 ↓
Decode
 ↓
更新 Pool State
 ↓
构建 Graph
 ↓
寻找 Opportunity
```

输出：

```text
Block
Pool Changes
Graph Changes
Opportunities
Expected Profit
```

---

# 12. 场景 2：Opportunity Replay

给定一个历史区块：

```text
Block 100
```

系统应该能够重新构造：

```text
Pool State @ Block 100
```

然后运行：

```text
Opportunity Detector
```

得到：

```text
Opportunity A
Opportunity B
Opportunity C
```

相同输入必须产生相同结果。

---

# 13. 场景 3：Live Chain

未来进入 Live 模式：

```text
WebSocket
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

Live Pipeline 必须和 Replay 使用相同的 State 更新逻辑。

即：

```text
Replay
   ↘
    State Engine
   ↗
Live
```

而不是：

```text
Replay → 一套逻辑

Live → 另一套逻辑
```

---

# 14. 场景 4：多 EVM 链

系统可以通过配置切换：

```text
Chain A
Chain B
Chain C
```

业务层不应该出现：

```rust
if chain == xxx
```

这样的链特化逻辑。

链差异必须进入：

```text
ChainAdapter
ChainProfile
ProtocolAdapter
```

---

# 15. 系统总体架构

```text
                       ┌───────────────┐
                       │   Blockchain  │
                       └───────┬───────┘
                               │
                         Chain Adapter
                               │
                               ▼
                       ┌───────────────┐
                       │ Block / Logs  │
                       └───────┬───────┘
                               │
                        Protocol Adapter
                               │
                               ▼
                       ┌───────────────┐
                       │ State Engine  │
                       └───────┬───────┘
                               │
                               ▼
                       ┌───────────────┐
                       │  StateStore   │
                       └───────┬───────┘
                               │
                         Graph Snapshot
                               │
                               ▼
                       ┌───────────────┐
                       │    Graph      │
                       └───────┬───────┘
                               │
                               ▼
                       ┌───────────────┐
                       │ Opportunity   │
                       └───────┬───────┘
                               │
                               ▼
                       ┌───────────────┐
                       │  Simulation   │
                       └───────┬───────┘
                               │
                               ▼
                       ┌───────────────┐
                       │     Risk      │
                       └───────┬───────┘
                               │
                               ▼
                       ┌───────────────┐
                       │   Execution   │
                       └───────────────┘
```

---

# 16. Chain Layer

## 16.1 职责

Chain Layer 负责：

* RPC；
* WebSocket；
* Block；
* Transaction；
* Receipt；
* Log；
* Chain ID；
* Block Number；
* Block Timestamp；
* Provider Failover。

不负责：

* DEX；
* Pool；
* Arbitrage；
* Opportunity。

---

# 17. ChainAdapter

核心接口概念：

```rust
trait ChainAdapter {
    fn chain_id(&self) -> ChainId;

    async fn latest_block(&self) -> Result<BlockNumber>;

    async fn get_block(
        &self,
        block: BlockNumber
    ) -> Result<BlockData>;

    async fn get_logs(
        &self,
        filter: LogFilter
    ) -> Result<Vec<Log>>;

    async fn subscribe_blocks(
        &self
    ) -> Result<BlockStream>;
}
```

实际 API 可以根据最终 Rust 技术选型调整。

原则不变：

> 上层业务不能依赖具体 RPC Provider。

---

# 18. ChainProfile

每条链拥有：

```text
ChainProfile
├── chain_id
├── name
├── rpc_http
├── rpc_ws
├── native_token
├── block_time
├── protocol_registry
├── execution_config
└── capability_flags
```

例如：

```text
GIWA
Ethereum
BSC
Base
Arbitrum
```

未来都应该通过 Profile 接入。

---

# 19. Provider Failover

生产系统不能依赖单 RPC。

至少设计：

```text
Provider A
Provider B
Provider C
```

能力：

* timeout；
* retry；
* health check；
* fallback；
* rate limit；
* provider latency metrics。

v0.1 可以先实现接口和基本 failover。

---

# 20. Protocol Layer

Protocol Layer 是整个项目非常重要的一层。

Chain：

> “这是哪个链？”

Protocol：

> “这个合约按照什么 DEX 协议工作？”

---

# 21. ProtocolAdapter

```rust
trait ProtocolAdapter {
    fn protocol_id(&self) -> ProtocolId;

    fn decode_log(
        &self,
        log: &Log
    ) -> Result<Option<ProtocolEvent>>;

    fn discover_pool(
        &self,
        address: Address
    ) -> Result<Option<PoolMeta>>;
}
```

第一阶段实现：

```text
V2-style AMM
```

---

# 22. V2-style AMM

第一阶段核心模型：

```text
x * y = k
```

池：

```text
Token A
   ↕
Pool
   ↕
Token B
```

State：

```text
reserve0
reserve1
fee
```

---

# 23. Pool Registry

Pool Registry 是 v0.1 的核心数据基础。

每个 Pool：

```text
Pool
├── chain_id
├── address
├── protocol
├── token0
├── token1
├── fee
└── pool_type
```

注意：

> Pool Registry 必须有明确来源和证据。

不能因为一个合约：

* 有 Transfer；
* 有 Swap；
* 有两个 Token；

就直接认定它是 Pool。

---

# 24. Pool Discovery

Pool Discovery 支持：

### 方式一：Factory Discovery

```text
Factory
 ↓
PairCreated
 ↓
Pool Registry
```

### 方式二：历史数据发现

从已经验证的数据中发现：

```text
Contract
 ↓
Event
 ↓
Pool relationship
```

### 方式三：配置注册

对于已知协议：

```toml
[[pools]]
address = "..."
token0 = "..."
token1 = "..."
```

---

# 25. 已验证数据输入

项目允许从：

```text
/Volumes/superfs/giwa-mev
```

提取已经验证的数据。

这些数据的角色是：

```text
Initial Facts
Fixtures
Registry Seed
Historical Validation Data
```

而不是：

```text
Runtime Semantic Engine
```

---

# 26. 数据导入原则

进入新项目的数据必须区分：

```text
Verified
Candidate
Unknown
```

只有：

```text
Verified
```

的数据才能作为：

* Pool Registry；
* Token Registry；
* Protocol Registry；
* Fixture；

的可信输入。

---

# 27. State Layer

State Layer 是 MEV Bot 的核心。

目标：

> **在内存中维护当前可用于 Opportunity Detection 的 Market State。**

---

# 28. StateStore

核心状态：

```text
StateStore
├── Pools
├── Reserves
├── Tokens
├── Block
└── Metadata
```

核心 API：

```rust
get_pool()
get_reserve()
update_pool()
snapshot()
```

---

# 29. Pool State

```text
PoolState
├── reserve0
├── reserve1
├── block_number
└── log_index
```

状态必须有时间点：

```text
Pool State @ Block N
```

而不是只有：

```text
Pool State
```

---

# 30. Reserve Update

对于 V2-style AMM：

```text
Sync
 ↓
reserve0
reserve1
```

优先使用协议定义的状态同步事件更新 Reserve。

不要：

```text
看到 Swap
↓
自己推算 reserve
```

作为唯一状态来源。

---

# 31. Token Metadata

Token：

```text
Token
├── chain_id
├── address
├── decimals
├── symbol?
└── status
```

其中：

```text
symbol
```

属于辅助信息。

核心计算不能依赖 symbol。

---

# 32. State Snapshot

Graph 不应该直接操作可变 StateStore。

推荐：

```text
StateStore
     ↓
Immutable Snapshot
     ↓
Graph
```

这样可以避免：

* 锁竞争；
* 状态读取不一致；
* Graph 计算过程中 State 被修改。

---

# 33. Graph Layer

Graph 是 Opportunity Detection 的基础。

模型：

```text
Token = Node
Pool  = Edge
```

例如：

```text
USDC
 ↓
WETH
 ↓
DAI
 ↓
USDC
```

---

# 34. Pool Graph

一个 Pool：

```text
USDC / WETH
```

形成：

```text
USDC → WETH
WETH → USDC
```

两条方向边。

---

# 35. Graph Edge

```text
GraphEdge
├── pool
├── token_in
├── token_out
├── fee
├── reserve_in
└── reserve_out
```

---

# 36. Graph 的职责

Graph 负责：

* 路径搜索；
* Cycle Detection；
* Candidate Generation。

Graph 不负责：

* 最终 Profit；
* 最终 Gas；
* 最终 Simulation；
* 最终 Execution。

---

# 37. Opportunity Layer

Opportunity 是整个系统的核心业务对象。

```text
Opportunity
├── chain
├── block
├── path
├── input_token
├── estimated_input
├── estimated_output
├── estimated_profit
├── gas_estimate
├── confidence
└── source
```

---

# 38. 第一种 Opportunity

v0.1：

> Two-Pool Arbitrage

结构：

```text
A
 ↓
Pool 1
 ↓
B
 ↓
Pool 2
 ↓
A
```

例如：

```text
USDC
 ↓
DEX A
 ↓
WETH
 ↓
DEX B
 ↓
USDC
```

---

# 39. Two-Pool Arbitrage

系统应该能够计算：

```text
Input
Output
Profit
```

并寻找：

```text
Optimal Input
```

不能只判断：

```text
price_a < price_b
```

就认为存在套利。

---

# 40. Multi-Hop Opportunity

v0.1 后半阶段支持：

```text
A
 ↓
B
 ↓
C
 ↓
A
```

初始限制：

```text
2 ~ 4 hops
```

避免无限路径搜索。

---

# 41. Graph 与 Opportunity 的关系

```text
Graph
 ↓
Candidate
 ↓
Analytical Calculation
 ↓
Opportunity
```

Graph 是：

> Candidate Generator

而不是：

> Profit Truth Engine

---

# 42. 数值计算原则

快速搜索可以使用：

```text
f64
```

但：

> 最终金额计算必须保留精确整数语义。

Token Amount：

```text
U256
```

不能因为方便直接全部转换成：

```text
f64
```

---

# 43. Simulation Layer

v0.1：

```text
Simulation Interface
```

可以存在。

但不要求完成完整 REVM。

接口应该提前固定：

```rust
trait Simulator {
    async fn simulate(
        &self,
        opportunity: &Opportunity
    ) -> Result<SimulationResult>;
}
```

---

# 44. Simulation 的职责

Simulation 最终负责验证：

```text
Opportunity
 ↓
Transaction
 ↓
EVM Execution
 ↓
Actual State Transition
 ↓
Actual Output
```

最终确认：

```text
Profit > 0
```

---

# 45. Risk Layer

风险控制必须独立。

```rust
trait RiskPolicy {
    fn evaluate(
        &self,
        opportunity: &Opportunity
    ) -> RiskDecision;
}
```

未来包括：

* minimum profit；
* maximum gas；
* maximum slippage；
* maximum loss；
* consecutive failure；
* token risk；
* liquidity risk；
* execution risk。

---

# 46. Execution Layer

Execution 最终负责：

```text
Opportunity
 ↓
Transaction
 ↓
Signer
 ↓
RPC / Private Relay
 ↓
Blockchain
```

---

# 47. v0.1 Execution

v0.1 不执行真实交易。

提供：

```text
NullExecutor
```

或者：

```text
DryRunExecutor
```

用于：

```text
Opportunity
 ↓
Execution Request
 ↓
Log
```

---

# 48. Signer Layer

Signer 独立于 Execution。

未来支持：

```text
Private Key
Hardware Wallet
Remote Signer
KMS
```

v0.1 只定义接口。

---

# 49. Replay Layer

Replay 是本项目的核心能力之一。

因为 MEV 系统必须能够回答：

> “为什么当时没有发现这个机会？”

或者：

> “为什么这个机会判断错了？”

---

# 50. Replay Architecture

```text
Historical Block
      ↓
Chain Adapter
      ↓
Protocol Decoder
      ↓
State Engine
      ↓
Graph
      ↓
Opportunity
      ↓
Simulation
      ↓
Result
```

Replay 和 Live 使用相同：

```text
State Engine
Protocol Decoder
Graph
Opportunity
```

---

# 51. Replay CLI

目标形式：

```bash
evm-mev replay \
  --chain xxx \
  --from-block X \
  --to-block Y
```

输出：

```text
blocks
pool_updates
graph_updates
opportunities
profit
latency
```

---

# 52. Deterministic Replay

相同：

```text
Chain
Block Range
Input Data
Configuration
```

必须产生相同：

```text
Pool State
Graph
Opportunity
```

这是核心验收条件。

---

# 53. Metrics

Metrics 至少记录：

```text
block_received_at
block_processed_at
state_updated_at
graph_updated_at
opportunity_detected_at
simulation_started_at
simulation_finished_at
execution_started_at
```

未来重点指标：

```text
block → opportunity latency
opportunity → simulation latency
simulation → execution latency
```

---

# 54. 项目 Workspace

推荐：

```text
evm-mev-bot/
│
├── Cargo.toml
│
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
│
├── contracts/
├── config/
├── fixtures/
├── data/
├── docs/
└── tests/
```

---

# 55. Crate 职责边界

## core

只放：

* 基础类型；
* Domain Model；
* Error；
* 基础 Traits。

不放业务实现。

---

## chain

负责：

* RPC；
* WS；
* Block；
* Receipt；
* Logs；
* Provider。

---

## protocol

负责：

* DEX；
* AMM；
* Event Decode；
* Pool Discovery。

---

## state

负责：

* Pool State；
* Token State；
* StateStore；
* Snapshot。

---

## graph

负责：

* Token Graph；
* Path；
* Cycle；
* Candidate。

---

## opportunity

负责：

* Arbitrage；
* Optimal Input；
* Profit Estimation；
* Opportunity。

---

## simulation

负责：

* EVM Simulation；
* REVM；
* State Override；
* Execution Result。

---

## risk

负责：

* Risk Policy；
* Limits；
* Circuit Breaker。

---

## execution

负责：

* Transaction Construction；
* Broadcast；
* Private Submission；
* Bundle。

---

## signer

负责：

* Signing。

---

## replay

负责：

* Historical Replay；
* Deterministic Replay；
* Regression。

---

## metrics

负责：

* Latency；
* Opportunity；
* Execution；
* Profit；
* Failure。

---

## pipeline

负责：

> 把所有模块连接起来。

它不应该成为新的业务逻辑黑盒。

---

## cli

负责：

```text
run
replay
inspect
benchmark
```

---

# 56. 配置体系

配置必须支持：

```toml
[chain]
name = "..."
chain_id = 123

[rpc]
http = "..."
ws = "..."

[protocol]
enabled = ["v2"]

[opportunity]
max_hops = 4
min_profit = "..."

[risk]
max_gas = "..."
```

业务代码不应该硬编码：

```text
RPC
Chain ID
Pool
Token
```

---

# 57. v0.1 功能范围

## P0

### Chain

* [x] Chain abstraction
* [x] Block fetching
* [x] Logs fetching
* [x] Basic WS abstraction
* [x] Provider abstraction

### Protocol

* [x] V2-style AMM
* [x] Event decode
* [x] Pool registry
* [x] Pool discovery

### State

* [x] Token
* [x] Pool
* [x] Reserve
* [x] StateStore
* [x] Snapshot

### Graph

* [x] Token node
* [x] Pool edge
* [x] Directed edge
* [x] Cycle detection

### Opportunity

* [x] Two-pool arbitrage
* [x] Optimal input
* [x] Profit estimation
* [x] 2~4 hop bounded search

### Replay

* [x] Historical replay
* [x] Deterministic replay
* [x] Fixture replay

---

# 58. v0.1 不纳入完成定义

以下即使存在代码，也不代表 v0.1 必须完成：

* REVM 完整交易模拟；
* 私有 Relay；
* Bundle；
* 真实签名；
* 真实广播；
* Flashbots；
* Flashblocks；
* V3；
* CLMM；
* 跨链；
* Sandwich；
* Liquidation；
* NFT MEV；
* Intent MEV；
* AI Strategy；
* Dashboard；
* Web UI。

---

# 59. v0.1 开发阶段

## Phase 1：Foundation

完成：

```text
Workspace
Core Types
Error
Traits
Config
CLI
```

验收：

```text
cargo check
cargo test
cargo fmt --check
cargo clippy
```

---

# 60. Phase 2：Chain

完成：

```text
ChainAdapter
Provider
Block
Log
Receipt
```

验收：

能够读取真实链历史区块。

---

# 61. Phase 3：Protocol

完成：

```text
V2 Adapter
Factory
Pair
Sync
Swap
```

验收：

能够从真实历史数据识别：

```text
Factory
Pool
Token0
Token1
Reserve
```

---

# 62. Phase 4：State

完成：

```text
StateStore
PoolState
TokenState
Snapshot
```

验收：

连续处理：

```text
Block N
Block N+1
Block N+2
```

状态正确更新。

---

# 63. Phase 5：Replay

完成：

```text
Block Range
 ↓
Event
 ↓
State
```

验收：

同一输入执行两次：

```text
Output A == Output B
```

---

# 64. Phase 6：Graph

完成：

```text
Pool → Edge
Token → Node
```

验收：

人工构造：

```text
A/B
B/C
C/A
```

能够发现：

```text
A → B → C → A
```

---

# 65. Phase 7：Two-Pool Arbitrage

完成：

```text
Pool A/B #1
Pool A/B #2
```

构造：

```text
price1 < price2
```

系统能够：

```text
发现机会
计算最佳输入
计算预期输出
计算理论利润
```

---

# 66. Phase 8：Multi-Hop

支持：

```text
A → B → C → A
```

最多：

```text
4 hops
```

必须限制：

```text
max_hops
max_paths
```

防止 Graph Search 爆炸。

---

# 67. v0.1 验收标准

## A. 编译

```text
cargo fmt --check
PASS
```

```text
cargo test
PASS
```

```text
cargo clippy
PASS
```

---

# 68. B. Chain

必须能够：

* 读取历史 Block；
* 获取 Logs；
* 获取 Receipt；
* 正确处理 Block Number；
* 正确处理 Chain ID。

---

# 69. C. Pool

必须能够：

* 识别 Pool；
* 识别 token0；
* 识别 token1；
* 获取 Reserve；
* 按 Block 保存 State。

---

# 70. D. State

必须：

```text
State(N)
→ Event
→ State(N+1)
```

结果确定。

不能：

* 随机；
* 依赖处理顺序；
* 状态污染；
* 隐式 RPC 查询。

---

# 71. E. Graph

必须能够：

```text
Pool
→ Edge
→ Path
→ Cycle
```

并支持：

```text
2-hop
3-hop
4-hop
```

---

# 72. F. Opportunity

必须能够通过 Fixture 验证：

### 无套利

```text
Pool A = 1:1
Pool B = 1:1
```

结果：

```text
No Opportunity
```

### 存在套利

```text
Pool A = 1:1
Pool B = 1:1.1
```

结果：

```text
Opportunity
```

### 最优输入

理论计算结果必须与：

```text
Brute Force
```

或：

```text
Local Search
```

处于允许误差范围内。

---

# 73. G. Replay

同一：

```text
Block Range
```

重复运行：

```text
Run A
Run B
```

必须：

```text
State A == State B
Graph A == Graph B
Opportunity A == Opportunity B
```

---

# 74. H. 数值安全

必须覆盖：

* U256；
* 大额 Token；
* 小额 Token；
* 0 Reserve；
* 极端 Decimal；
* Fee；
* Overflow；
* Underflow；
* Division by zero。

系统不得因为非法池状态：

```text
panic
```

---

# 75. I. 性能

v0.1 不以最终生产性能为目标。

但架构必须避免明显错误：

不能：

```text
每发现一个 Opportunity
    ↓
RPC
    ↓
RPC
    ↓
RPC
```

应该：

```text
Block
 ↓
State Update
 ↓
Memory State
 ↓
Opportunity
```

---

# 76. Hot Path 原则

最终 Hot Path：

```text
Block
 ↓
Event
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

不允许：

```text
Database
HTTP API
LLM
External Search
```

成为 Hot Path 必经依赖。

---

# 77. 内存优先

MEV Hot Path 数据：

```text
Pool
Reserve
Graph
Opportunity
```

优先保存在：

```text
RAM
```

数据库主要用于：

* 历史数据；
* Replay；
* Metrics；
* Debug；
* Research。

---

# 78. 正确性原则

系统必须区分：

```text
Observed
Derived
Estimated
Simulated
Executed
```

例如：

```text
Reserve
```

是：

```text
Observed
```

而：

```text
Expected Profit
```

属于：

```text
Estimated
```

REVM 结果属于：

```text
Simulated
```

链上 Receipt 才是：

```text
Executed Result
```

不能混淆。

---

# 79. Opportunity 生命周期

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
```

失败：

```text
Detected
   ↓
Rejected
```

或者：

```text
Submitted
   ↓
Failed
```

---

# 80. Profit 定义

必须明确：

```text
Gross Profit
```

与：

```text
Net Profit
```

的区别。

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

v0.1 主要计算：

```text
Theoretical Gross Profit
```

后续 Simulation 才逐步引入真实成本。

---

# 81. Gas

v0.1：

```text
Gas Estimate Interface
```

可以存在。

但不要因为没有完整 Gas Model 而阻塞：

```text
Graph
Opportunity
Replay
```

---

# 82. 数据质量等级

项目中的数据统一使用：

```text
Verified
Derived
Estimated
Unknown
```

例如：

| 数据                | 类型                 |
| ----------------- | ------------------ |
| Chain ID          | Verified           |
| Pool Address      | Verified           |
| Reserve           | Observed           |
| Token Decimals    | Verified / Unknown |
| Price             | Derived            |
| Arbitrage Profit  | Estimated          |
| Simulation Profit | Simulated          |
| Actual Profit     | Executed           |

---

# 83. Error Handling

禁止：

```rust
unwrap()
expect()
```

出现在不可控输入路径。

尤其是：

```text
RPC
Log
ABI Decode
Token
Pool
Amount
```

必须显式处理错误。

---

# 84. 可观测性

至少提供：

```text
blocks_processed
logs_processed
pools_updated
graph_updates
opportunities_detected
opportunities_rejected
simulation_count
execution_count
```

---

# 85. 日志原则

日志应该服务于：

```text
Debug
Replay
Performance
Incident
```

而不是输出大量无价值信息。

Hot Path 不应该因为日志产生严重性能损耗。

---

# 86. 测试策略

四层测试。

## Unit

测试：

* Math；
* Fee；
* Pool；
* State；
* Graph。

## Fixture

测试：

* Real Pool；
* Real Event；
* Real Block。

## Replay

测试：

```text
Historical Block Range
```

## Integration

测试：

```text
Chain
→ State
→ Graph
→ Opportunity
```

---

# 87. 生产前必须增加

后续版本必须增加：

```text
REVM
Private Relay
Signer
Execution
Live WS
Provider Failover
Latency Benchmark
```

---

# 88. 版本路线

版本不拆成大量微小审计版本。

每个版本代表一个完整能力域。

```text
v0.1
State + Graph + Opportunity

v0.2
EVM Simulation / REVM

v0.3
Risk + Transaction Construction + Execution

v0.4
Low-Latency Live Pipeline

v0.5
Multi-Protocol / Multi-Chain

v0.6
Backrun Strategy

v0.7
Private Submission / Bundle

v0.8
Advanced Token Risk / Tax / Honeypot

v0.9
Production Hardening

v1.0
Production EVM MEV Bot
```

---

# 89. v0.2

核心：

```text
Opportunity
 ↓
REVM
 ↓
Actual EVM Simulation
```

能力：

* State Override；
* Transaction Simulation；
* Gas；
* Slippage；
* Actual Output；
* Profit；
* Failure Reason。

---

# 90. v0.3

核心：

```text
Simulation
 ↓
Risk
 ↓
Transaction
 ↓
Execution
```

能力：

* Transaction Builder；
* Signer；
* Risk Policy；
* Gas Policy；
* Dry Run；
* RPC Broadcast。

---

# 91. v0.4

核心：

> Low Latency

从：

```text
Historical Replay
```

进入：

```text
Live Chain
```

重点：

* WS；
* Block Feed；
* Incremental State；
* Parallel Decode；
* Lock Reduction；
* Latency Benchmark。

---

# 92. v0.5

核心：

> Multi-Protocol / Multi-Chain

增加：

```text
V3
StableSwap
Additional DEX
Additional EVM Chains
```

但每个协议必须：

```text
Protocol Adapter
```

隔离。

---

# 93. v0.6

核心：

> Backrun

重点：

```text
Pending / Included Transaction
 ↓
State Change
 ↓
Opportunity
 ↓
Backrun
```

---

# 94. v0.7

核心：

> Private Execution

增加：

* Private RPC；
* Bundle；
* Relay；
* Bribe；
* Inclusion Strategy。

---

# 95. v0.8

核心：

> Token Risk

增加：

* Fee-on-transfer；
* Tax；
* Honeypot；
* Transfer restrictions；
* Dynamic token behavior。

注意：

这仍然是为了：

> 判断某个 Opportunity 是否安全执行。

不是重新建设一个通用 Token 分析平台。

---

# 96. v0.9

核心：

> Production Hardening

包括：

* 高可用；
* Provider Failover；
* Recovery；
* Circuit Breaker；
* Persistent State；
* Monitoring；
* Alert；
* Benchmark；
* Chaos Test。

---

# 97. v1.0

v1.0 的定义：

> 可以在真实 EVM 主网上持续运行，并在风险控制下自动发现、模拟和执行 MEV Opportunity。

---

# 98. 关键架构原则

## 原则 1：MEV First

所有功能都必须回答：

> 这个东西是否直接服务于 MEV Opportunity？

如果不能：

> 不进入核心系统。

---

## 原则 2：State First

不要：

```text
Opportunity
 ↓
RPC Query
 ↓
RPC Query
```

而是：

```text
Chain Event
 ↓
State
 ↓
Opportunity
```

---

## 原则 3：Protocol Adapter

协议差异必须隔离：

```text
Protocol
 ↓
Adapter
 ↓
Normalized State
```

上层不应该知道具体 DEX 实现细节。

---

## 原则 4：Graph 不是最终答案

Graph 负责：

```text
Candidate
```

最终答案来自：

```text
Simulation
```

---

## 原则 5：Replay First

任何生产策略都必须：

```text
Replay
 ↓
Validate
 ↓
Benchmark
 ↓
Live
```

---

## 原则 6：Live 和 Replay 同源

两者必须共享：

```text
Decoder
State
Graph
Opportunity
```

---

## 原则 7：Hot Path 极简

Hot Path 不允许引入：

```text
LLM
Database
External API
Web Search
```

等非必要依赖。

---

# 99. 防跑偏机制

以后任何新增需求必须回答以下问题：

### Q1

它是否直接服务：

```text
Chain
State
Graph
Opportunity
Simulation
Risk
Execution
```

之一？

如果不是：

> 默认拒绝进入核心项目。

---

### Q2

它是否属于 MEV Bot Hot Path？

如果不是：

> 应该放到 Research / Tooling / Replay / Metrics。

---

### Q3

它是否能提高：

```text
Opportunity Detection
Simulation Accuracy
Execution Success
Latency
```

之一？

如果都不能：

> 不应该成为当前阶段重点。

---

### Q4

它是否需要建立一个全新的“大系统”？

例如：

```text
Universal ABI Engine
Semantic Engine
Data Platform
AI Agent
```

如果是：

> 必须单独评估，不能直接并入核心项目。

---

# 100. 需求进入标准

所有新需求进入项目之前必须标记：

```text
Priority:
P0 / P1 / P2 / P3

Stage:
v0.1 / v0.2 / ...

Layer:
Chain / Protocol / State / Graph /
Opportunity / Simulation / Risk / Execution

Hot Path:
Yes / No
```

没有这些信息，不进入开发。

---

# 101. 需求冻结原则

v0.1 开始开发以后：

如果出现新的想法，例如：

```text
要不要支持 V3？
要不要做 Sandwich？
要不要做 AI？
要不要做 Token Scanner？
要不要做 Dashboard？
要不要做跨链？
```

默认：

> 不立即加入 v0.1。

统一进入：

```text
Future Backlog
```

等待版本评审。

---

# 102. v0.1 最终 Definition of Done

v0.1 不是：

> “代码写完了”。

而必须同时满足：

```text
                v0.1
                 │
      ┌──────────┼──────────┐
      ↓          ↓          ↓
   Chain       State      Protocol
      │          │          │
      └──────────┼──────────┘
                 ↓
               Graph
                 ↓
           Opportunity
                 ↓
              Replay
```

并满足：

1. 可以读取真实 EVM 历史区块；
2. 可以识别目标 AMM Pool；
3. 可以维护 Pool State；
4. 可以构建 Token/Pool Graph；
5. 可以发现 Two-Pool Arbitrage；
6. 可以计算理论最优输入；
7. 可以计算理论利润；
8. 可以发现有限 Multi-Hop Opportunity；
9. 可以 Replay；
10. Replay 是 deterministic；
11. 所有核心逻辑具有测试；
12. 数值计算不会产生未处理溢出；
13. Chain 与 Protocol 已经完成抽象；
14. 代码没有把具体链硬编码进核心业务；
15. v0.1 不需要真实交易执行。

---

# 103. 最重要的成功标准

v0.1 最重要的不是：

```text
代码量
```

不是：

```text
支持多少协议
```

也不是：

```text
支持多少链
```

而是：

> **能够从真实链历史状态中，可靠地恢复 Market State，并从 Market State 中稳定、可重复地发现真实存在的套利机会。**

即：

```text
真实链数据
      ↓
正确 State
      ↓
正确 Graph
      ↓
正确 Opportunity
```

---

# 104. 项目第一性原理

整个项目最终可以压缩成：

```text
Observe
   ↓
Understand Market State
   ↓
Find Price Inefficiency
   ↓
Prove It
   ↓
Execute It
   ↓
Measure It
   ↓
Improve It
```

其中：

```text
Observe
```

不是为了理解所有链上交易。

而是为了：

> **获得 MEV 所需要的市场状态。**

---

# 105. 最终架构边界

项目应该始终保持：

```text
                EVM MEV BOT
                     │
       ┌─────────────┼─────────────┐
       │             │             │
     Chain        Protocol       State
       │             │             │
       └─────────────┼─────────────┘
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

外围：

```text
Replay
Metrics
Fixtures
Research Data
```

服务于核心。

而不是反过来让：

```text
Research
Semantic
Data
AI
Dashboard
```

成为核心。

---

# 106. 项目开发铁律

以后整个项目开发过程中，以以下十条作为最高级工程约束：

### 1.

**MEV 是目的，不是顺便支持的功能。**

### 2.

**State 是核心资产。**

### 3.

**Protocol 必须 Adapter 化。**

### 4.

**Graph 只负责发现 Candidate。**

### 5.

**Simulation 才负责证明 Opportunity。**

### 6.

**Execution 必须建立在 Simulation + Risk 之上。**

### 7.

**Replay 必须和 Live 使用同一套核心逻辑。**

### 8.

**Hot Path 必须保持极简。**

### 9.

**不要为了“理解更多数据”而扩大系统边界。**

### 10.

**任何新功能如果不能明确提升 Opportunity Discovery、Simulation、Risk、Execution 或 Latency，就不能成为当前阶段核心工作。**

---

# 107. v0.1 开发起点

在真正开始写业务代码之前，首先完成：

```text
1. Repository 初始化
2. Workspace 初始化
3. Core Domain Model
4. Chain Trait
5. Protocol Trait
6. State Trait
7. Graph Trait
8. Opportunity Trait
9. Replay Trait
10. CLI
11. Fixture 体系
12. 测试体系
```

然后才进入：

```text
真实链
 ↓
真实 Pool
 ↓
真实 State
 ↓
真实 Opportunity
```

---

# 108. 第一条实际业务闭环

v0.1 第一个真正需要跑通的完整闭环：

```text
Historical Block
      ↓
Pool Event
      ↓
Pool State
      ↓
Two Pool
      ↓
Price Difference
      ↓
Optimal Input
      ↓
Expected Profit
      ↓
Opportunity
```

只要这条链路没有完全跑通：

> 不应该开始大量扩展其他协议、策略或复杂功能。

---

# 109. PRD 变更规则

本文档可以修改。

但是修改必须明确：

```text
Change
Reason
Impact
Version
```

禁止：

> 在代码实现过程中悄悄改变产品定义。

任何架构重大变化，都应该先修改 PRD，再修改代码。

---

# 110. 最终产品愿景

最终：

```text
                EVM MEV BOT
                     │
        ┌────────────┴────────────┐
        │                         │
      Multi-Chain             Low Latency
        │                         │
        └────────────┬────────────┘
                     │
               Market State
                     │
               Opportunity
                     │
                Simulation
                     │
                  Risk
                     │
                Execution
                     │
                Real Profit
```

目标不是做最大的区块链基础设施。

目标是做一个：

> **小而强、低延迟、状态准确、机会判断可靠、能够真正执行交易的 EVM MEV Bot。**

---

# 111. 当前阶段唯一核心问题

在 v0.1 开始之后，所有工程问题最终都应该回到一个问题：

> **我们能不能准确、低成本、可重复地知道“现在市场状态是什么，以及这个状态是否产生了可执行的 MEV Opportunity”？**

如果答案还是否定的：

```text
不要扩展功能。
不要增加复杂架构。
不要做 UI。
不要做 AI。
不要做通用数据系统。
```

继续把：

```text
Chain
 ↓
State
 ↓
Graph
 ↓
Opportunity
```

做好。

这就是 EVM MEV Bot v0.1 的核心。
