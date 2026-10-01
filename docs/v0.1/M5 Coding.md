# M5 Coding Task — GIWA Live Market Pipeline

## 0. 任务定位

当前项目：

`evm-mev-bot`

目标：

> 在 M4 已经能够对真实 GIWA 历史链状态执行真实 EVM 字节码模拟的基础上，建立第一条真正运行在 GIWA Testnet 上的实时市场数据 → 状态 → Graph → Opportunity → Simulation → Risk Pipeline。

M5 不做交易执行。

M5 的核心问题只有一个：

> **当 GIWA 链上出现新的区块 / 市场状态变化时，Bot 能否在真实链数据上持续、正确、确定性地发现并验证套利机会？**

最终形成：

```text
GIWA WebSocket
      │
      ▼
NewHeads / Logs
      │
      ▼
Block / Log Ordering
      │
      ▼
Protocol Decode
      │
      ▼
State Update
      │
      ▼
StateStore
      │
      ▼
Graph Snapshot
      │
      ▼
Opportunity Detection
      │
      ▼
Simulation
      │
      ▼
Risk
      │
      ▼
Opportunity Decision
      │
      ▼
Metrics / Evidence
```

M5 完成后，系统应该已经具备：

> **Live Market Detection**

但仍然不具备：

> **Live Transaction Execution**

---

# 1. M4 是本任务的硬基线

不要重新设计 M1-M4。

M4 已经完成：

* 真实 GIWA Testnet 历史状态读取
* BlockContext
* historical RPC
* real bytecode
* REVM simulation
* exact U256
* Simulation
* Risk
* deterministic fixture
* execution facts
* simulation success/failure
* gas
* net profit
* historical opportunity validation

M4 已经证明：

```text
Analytical Opportunity
        ↓
Real EVM Simulation
        ↓
Executable / Non-executable
        ↓
Gas
        ↓
Net Profit
        ↓
Risk Decision
```

现在 M5 要解决的是：

```text
Live Chain
   ↓
Live State
   ↓
Live Opportunity
   ↓
M4 Simulation
```

因此：

**不要重写 Simulation。**

**不要重新实现 Opportunity。**

**不要重新实现 State。**

应该把已有模块组合成 Live Pipeline。

---

# 2. M5 成功定义

M5 不是：

> “WebSocket 能连接上 GIWA。”

也不是：

> “能够收到 NewHeads。”

真正的完成条件是：

```text
GIWA Live Event
      ↓
Block
      ↓
Logs
      ↓
Protocol Event
      ↓
State Update
      ↓
StateStore
      ↓
Graph
      ↓
Opportunity
      ↓
Simulation
      ↓
Risk
      ↓
Decision
```

至少有一条真实 GIWA Testnet 市场状态变化能够完整走完。

并且：

1. 没有使用 fake block
2. 没有使用 fake pool
3. 没有伪造 reserves
4. 没有伪造 opportunity
5. 没有用静态 fixture 冒充 live
6. Simulation 使用真实链状态
7. Replay 与 Live 使用相同 State Semantics
8. 能检测数据缺口
9. 能检测 stale opportunity
10. 能记录端到端 latency

---

# 3. 严格 Scope

## 3.1 本 M5 必须完成

### A. GIWA WebSocket

实现：

```text
WebSocketSource
```

至少支持：

* connection
* reconnect
* newHeads
* logs
* subscription lifecycle
* heartbeat / keepalive
* connection failure detection

---

### B. Live Block Pipeline

收到：

```text
newHeads
```

之后：

```text
Header
 ↓
Block Context
 ↓
Block Processing
```

必须记录：

* chain_id
* block_number
* block_hash
* parent_hash
* timestamp
* received_at
* processed_at

---

### C. Live Log Pipeline

实时接收：

```text
eth_subscribe logs
```

或者等价机制。

Log 必须至少携带：

```text
block_number
block_hash
transaction_hash
transaction_index
log_index
address
topics
data
```

不得丢失：

```text
transaction_index
log_index
```

因为 M1/M2/M3 已经确定：

> 同一区块中的状态变化必须按照 transaction_index → log_index 确定性排序。

---

# 4. 不要相信 WebSocket 消息顺序

这是 M5 的重要要求。

不能假设：

```text
WebSocket received order
=
Chain execution order
```

必须建立明确的排序机制：

```text
block_number
    ↓
transaction_index
    ↓
log_index
```

最终：

```text
ProtocolEvent
```

必须具有确定顺序。

---

# 5. Block Gap Detection

必须检测：

```text
current_block > previous_block + 1
```

例如：

```text
100
101
103
```

必须产生：

```text
BlockGapDetected
```

而不能直接继续把 103 当成正常连续状态。

---

# 6. Gap Recovery

出现：

```text
100
101
103
```

之后，必须通过 RPC 补：

```text
102
```

至少补齐：

* block header
* relevant logs

然后：

```text
101
 ↓
102
 ↓
103
```

重新恢复连续状态。

不要直接：

```text
101
 ↓
103
```

---

# 7. Initial Synchronization

Live Pipeline 启动时不能直接从：

```text
最新 block
```

开始盲目监听。

必须建立：

```text
Initial Snapshot
        ↓
确定 starting block
        ↓
Replay missing blocks
        ↓
进入 WebSocket Live
```

要求避免：

```text
snapshot at block N
WebSocket starts at N-5
```

造成重复处理。

也要避免：

```text
snapshot at N
WebSocket starts at N+5
```

造成数据缺失。

---

# 8. 推荐 Live Bootstrap

建议采用：

```text
1. 获取 latest block N
2. 建立 snapshot at N
3. 建立 WS subscription
4. 记录 subscription start point
5. replay N+1 onward
6. 进入 live
```

但必须根据实际 RPC / WebSocket provider 行为验证。

不要假设 provider 一定提供无 gap subscription。

如果不能证明：

> subscription 建立时刻与 snapshot 之间没有缺口

则必须通过 block/log reconciliation 解决。

---

# 9. State Semantics 必须统一

这是 M5 最重要的架构要求之一。

不能出现：

```text
ReplayState
LiveState
```

两套不同逻辑。

必须：

```text
                ┌──────────────┐
Replay ────────►│              │
                │ State Engine │────► StateStore
Live ──────────►│              │
                └──────────────┘
```

也就是说：

```text
ProtocolEvent
       ↓
StateUpdate
       ↓
StateStore
```

Replay 和 Live 都走这一条路径。

---

# 10. State Update

如果 M1/M2 当前已有：

```text
ProtocolEvent
StateUpdate
```

继续使用。

如果目前存在：

```text
Live directly mutates StateStore
```

必须改掉。

正确：

```text
Raw Log
 ↓
Decode
 ↓
ProtocolEvent
 ↓
StateUpdate
 ↓
StateStore
```

---

# 11. State Update 必须可审计

每一个 live state change 至少能够追踪：

```text
block_number
transaction_hash
transaction_index
log_index
pool
event_type
before
after
```

例如：

```text
PoolSync
block=123
tx=0xabc
log=5

reserve0:
1000000 → 1200000

reserve1:
2000000 → 1700000
```

这样后面才能回答：

> “为什么这个机会是在这个 block 出现的？”

---

# 12. Sync 是 Pool Reserve 的权威来源

继续遵循 M1/M3 已经验证的规则。

对于 V2-style AMM：

```text
Sync
```

是 reserve state 的权威更新。

不要：

```text
Swap amountIn
+
Swap amountOut
```

自行推导最终 reserves 并当作 authoritative state。

可以解析 Swap。

但是：

```text
Swap = event information
Sync = reserve authority
```

必须保持。

---

# 13. Graph Live Update

M2 已经有：

```text
Graph
GraphSnapshot
PoolEdge
TokenNode
```

M5 不要重新设计 Graph。

只需要让：

```text
StateStore update
```

能够触发：

```text
Graph snapshot refresh
```

初期允许：

```text
StateStore
 ↓
Immutable Snapshot
 ↓
Graph rebuild
```

不要为了 latency 过早实现复杂 incremental graph mutation。

---

# 14. 正确性优先

M5 初期允许：

```text
new block
 ↓
state updates
 ↓
graph snapshot rebuild
 ↓
opportunity scan
```

即使：

```text
Graph rebuild = 10ms
```

也没关系。

现在最重要的是：

> 证明 Live State → Opportunity 的语义正确。

M8 再优化 latency。

---

# 15. Opportunity Trigger

不要每一个 log 都无条件重新计算整个市场。

至少应该建立：

```text
Changed Pool
      ↓
Affected Token Pair
      ↓
Affected Pool Pair
      ↓
Candidate Opportunity
```

初期可以采用 conservative strategy：

```text
任何相关 Pool reserve change
        ↓
重新扫描包含该 Pool 的双池组合
```

不要一开始做复杂 incremental dependency graph。

---

# 16. Stale Opportunity

这是 M5 必须新增的概念。

一个 Opportunity 必须绑定：

```text
observed_block
```

或者更严格：

```text
state_version
```

例如：

```text
Opportunity O1
observed_block = 100
```

之后：

```text
block 101
```

发生了相关 pool reserve 更新。

那么：

```text
O1
```

必须变成：

```text
STALE
```

不能继续把 O1 送给 Simulation。

---

# 17. Opportunity Identity

建议建立类似：

```text
OpportunityId
```

包含：

```text
chain_id
observed_block
pool_a
pool_b
direction
```

必要时加入：

```text
state_version
```

不要只使用：

```text
pool_a + pool_b
```

因为同一个套利路径在不同 block 中是不同机会。

---

# 18. Simulation Integration

M4 Simulation 已经存在。

M5 要做：

```text
Live Opportunity
       ↓
Build Simulation Request
       ↓
Historical/Current BlockContext
       ↓
Real State
       ↓
REVM
       ↓
SimulationResult
       ↓
Risk
```

Simulation 必须使用：

> Opportunity 所对应的状态版本。

不能：

```text
Opportunity at block 100
Simulation against block 105
```

否则结果没有意义。

---

# 19. Live Simulation 的 State Source

这里要特别注意 M4 当前实现。

M4 的 simulation fixture 使用：

```text
historical RPC
```

M5 不要简单地把：

```text
latest
```

塞进去。

需要明确：

```text
SimulationStateSource
```

至少支持：

```text
at block N
```

读取：

* block context
* account balance
* code
* storage
* nonce
* required state

---

# 20. 不允许使用 “latest” 模糊状态

Simulation request 必须明确：

```text
block_number
```

例如：

```text
SimulationRequest {
    chain_id,
    block_number,
    opportunity,
    ...
}
```

不能：

```text
eth_getBalance(..., "latest")
eth_getCode(..., "latest")
eth_getStorageAt(..., "latest")
```

然后模拟一个来自历史 block 的 opportunity。

---

# 21. Simulation Queue

M5 需要开始考虑并发。

因为：

```text
Live
 ↓
Opportunity
 ↓
Simulation
```

未来可能出现：

```text
10 opportunities/block
```

不能让 WebSocket ingestion 被 Simulation 阻塞。

建议：

```text
Live Ingestion
       │
       ▼
State Engine
       │
       ▼
Opportunity
       │
       ▼
Simulation Queue
       │
       ├── Worker
       ├── Worker
       └── Worker
```

---

# 22. M4 Simulator `Send` 问题必须处理

M4 当前存在：

> `Simulator` future 不是 `Send`

M5 如果需要 Tokio multi-thread worker：

```rust
tokio::spawn(...)
```

必须解决这个边界。

不要：

```text
unsafe impl Send
```

不要通过不安全方式绕过。

必须明确选择：

### Option A

让 Simulator / provider / future 具备：

```text
Send + Sync
```

如果 REVM / RPC abstraction 可以安全支持。

### Option B

使用 dedicated single-thread simulation runtime。

### Option C

使用 blocking worker：

```text
spawn_blocking
```

或者专用 worker thread。

但必须：

* benchmark
* document
* test

最终选择以：

> correctness + deterministic behavior

为优先。

---

# 23. Simulation 不得阻塞 Live Ingestion

无论最终采用哪种 worker 方案，都必须保证：

```text
WebSocket
 ↓
Block ingestion
```

不会因为：

```text
REVM simulation
```

长时间阻塞。

例如：

```text
Block 100
 ↓
20 opportunities
 ↓
simulation starts
```

仍然应该可以继续接收：

```text
Block 101
Block 102
```

---

# 24. Stale Simulation Cancellation

如果：

```text
Opportunity O1
block=100
```

进入 simulation queue。

之后：

```text
block=101
```

发现：

```text
pool A
```

已经发生变化。

那么 O1 即使 simulation 最终完成，也不能直接变成 executable opportunity。

必须检查：

```text
Opportunity state version
==
Current relevant state version
```

否则：

```text
STALE
```

---

# 25. Risk Integration

M4 Risk 继续使用。

M5 只负责：

```text
Opportunity
 ↓
Simulation
 ↓
Risk
```

最终输出：

```text
Unknown
Reject
Accept
```

但是：

> `Accept` 仅代表 Simulation + Risk 层面的接受。

不代表：

```text
send
```

更不代表：

```text
profit realized
```

---

# 26. M5 明确禁止交易执行

本任务禁止：

* private key
* signer
* transaction signing
* raw transaction broadcast
* SequencerDirect
* Flashblock submission
* relay
* bundle
* private mempool
* gas bidding
* nonce manager for live sending
* executor contract deployment

不要因为：

> “已经发现机会了”

就顺手实现发送。

M6 才处理 Execution。

---

# 27. FlashblockSource

GIWA 的低延迟数据能力非常重要，因此 M5 必须为 Flashblock 留出正确的位置。

但不要把 Flashblock 和普通 NewHeads 的逻辑写成两套 Pipeline。

必须抽象：

```text
MarketDataSource
```

例如：

```rust
trait MarketDataSource {
    async fn next_event(&mut self) -> Result<ChainEvent>;
}
```

或者符合项目现有架构的等价 abstraction。

然后：

```text
NewHeadsSource
FlashblockSource
ReplaySource
```

都进入：

```text
Unified Event Pipeline
```

---

# 28. Flashblock 与普通 Block 的关系

不要把 Flashblock 当作普通 block。

它可能代表：

```text
partial / intermediate state
```

因此必须定义明确语义：

```text
Flashblock
    ↓
candidate state
```

而：

```text
Final Block
    ↓
canonical state
```

必须避免：

```text
Flashblock state
直接永久覆盖
canonical block state
```

除非能够证明两者状态语义一致。

---

# 29. M5 Flashblock 实现要求

如果当前 GIWA Testnet endpoint 可以稳定验证 Flashblock：

必须实现：

```text
FlashblockSource
```

并完成至少：

```text
connect
subscribe
sequence
stale detection
gap detection
fallback
```

推荐：

```text
Flashblock
     ↓
low-latency candidate state
     ↓
Opportunity
     ↓
Simulation
```

最终：

```text
Canonical Block
     ↓
reconciliation
```

---

# 30. Flashblock 不可用时

必须能够：

```text
Flashblock
   ↓
failure / stale / gap
   ↓
NewHeads fallback
```

不能：

```text
Flashblock unavailable
=
Bot stops forever
```

---

# 31. 如果当前无法证明 Flashblock 数据语义

不要伪造支持。

如果 GIWA Testnet 当前 endpoint：

* 文档不足
* provider 不支持
* 返回结构无法验证
* 无法证明 sequence semantics

那么：

**实现 interface + adapter boundary + tests**

即可。

同时写清：

```text
Flashblock implementation blocked by verified provider capability.
```

不能用 mock 数据宣布 Flashblock 完成。

---

# 32. Live / Replay Parity Test

这是 M5 的核心 Acceptance。

准备一组真实 GIWA blocks：

```text
N
N+1
N+2
...
N+K
```

分别走：

```text
ReplaySource
```

和：

```text
LiveSource
```

最终比较：

```text
State
Graph
Opportunity
```

必须一致。

允许：

```text
timestamp
latency
transport metadata
```

不同。

但核心状态结果必须一致。

---

# 33. Determinism

同一批 live events：

```text
E1
E2
E3
...
En
```

重复运行两次：

```text
Run A
Run B
```

必须得到相同：

```text
State hash
Graph hash
Opportunity set
```

不能因为：

```text
async scheduling
```

产生不同结果。

---

# 34. Event Ordering Test

构造真实或者录制的：

```text
block
transaction
logs
```

验证：

```text
transaction_index
```

优先于：

```text
log_index
```

例如：

```text
tx=3 log=8
tx=4 log=1
```

正确顺序：

```text
tx=3 log=8
tx=4 log=1
```

而不是：

```text
log=1
log=8
```

---

# 35. Duplicate Event

Live provider 可能重复发送消息。

必须支持：

```text
duplicate block
duplicate log
duplicate reconnect event
```

不能导致：

```text
State double-apply
```

至少需要明确：

```text
event identity
```

例如：

```text
chain_id
block_hash
transaction_hash
log_index
```

---

# 36. Reconnection

必须测试：

```text
WS connected
 ↓
events
 ↓
connection lost
 ↓
reconnect
 ↓
reconcile
 ↓
continue
```

不能：

```text
reconnect
 ↓
直接继续
```

必须确认中间是否出现：

```text
block gap
```

---

# 37. Reorg / Canonicality

GIWA Testnet 当前链语义需要实际验证。

如果 provider 能提供：

```text
removed=true
```

必须正确处理。

如果无法观察到 reorg，也必须在 architecture 中保留：

```text
Canonicality
```

不要把：

```text
WebSocket event received
```

直接等同：

```text
永久 canonical
```

---

# 38. Metrics

M5 必须增加 latency metrics。

至少：

### Block latency

```text
chain_timestamp
→
WS_received
```

### Processing latency

```text
WS_received
→
StateUpdated
```

### Opportunity latency

```text
StateUpdated
→
OpportunityDetected
```

### Simulation latency

```text
SimulationStart
→
SimulationEnd
```

### End-to-end

```text
Chain Event
→
Risk Decision
```

---

# 39. 建议统一 Event Timing

例如：

```rust
PipelineTiming {
    chain_time,
    received_at,
    decoded_at,
    state_updated_at,
    graph_updated_at,
    opportunity_detected_at,
    simulation_started_at,
    simulation_finished_at,
    risk_decided_at,
}
```

使用：

```text
Instant
```

计算本机 latency。

使用：

```text
SystemTime / block timestamp
```

记录链上时间。

不要混淆。

---

# 40. Metrics 不要只输出平均值

至少记录：

```text
count
min
max
p50
p95
p99
```

特别是：

```text
block → opportunity
block → risk
```

---

# 41. Live Evidence

M5 必须保存真实运行证据。

建议：

```text
data/evidence/m5/
```

至少包括：

```text
live-session.json
blocks.jsonl
events.jsonl
state-updates.jsonl
opportunities.jsonl
simulation-results.jsonl
risk-decisions.jsonl
metrics.json
```

具体格式可以按当前 repo 约定调整。

---

# 42. Evidence 必须能够回答这些问题

至少能够回答：

### Q1

哪个 GIWA block 被收到？

### Q2

什么时候收到？

### Q3

收到哪些 logs？

### Q4

哪些 logs 改变了 pool state？

### Q5

reserve 如何变化？

### Q6

哪个 opportunity 被发现？

### Q7

opportunity 对应哪个 state/block？

### Q8

simulation 使用哪个 block？

### Q9

simulation 是否成功？

### Q10

gas 是多少？

### Q11

risk decision 是什么？

### Q12

整个过程耗时多少？

---

# 43. Real GIWA Data Requirement

必须使用真实：

```text
GIWA Testnet
chain_id = 91342
```

数据。

不能使用：

```text
mock pool
mock reserve
mock opportunity
mock block
```

进行最终 acceptance。

Unit tests 可以使用 fixtures。

Integration acceptance 必须使用真实链数据。

---

# 44. Provider 配置

不要把：

```text
GIWA RPC URL
WebSocket URL
```

hardcode 在业务代码。

放到：

```text
config
environment
CLI
```

并且：

```text
ChainAdapter
```

负责访问。

---

# 45. 不要破坏 Chain Abstraction

虽然当前只有：

```text
GIWA Testnet
```

但不要在：

```text
State
Graph
Opportunity
Simulation
Risk
```

写：

```rust
if chain_id == 91342
```

GIWA 特殊逻辑只能进入：

```text
chain adapter
```

或者：

```text
GIWA-specific data source
```

---

# 46. Chain-specific boundary

推荐：

```text
crates/chain/
    src/
        adapter/
        websocket/
        giwa/
            flashblock.rs
            ...
```

具体结构根据当前代码调整。

不要为了抽象而抽象。

---

# 47. CLI

建议新增：

```bash
evm-mev-bot live
```

或者符合当前 CLI 风格的命令。

至少支持：

```text
--rpc-url
--ws-url
--start-block
--duration
```

如果当前 CLI 已经存在配置体系，优先复用。

---

# 48. Live Session

一次 live run 应有：

```text
session_id
chain_id
start_block
end_block
started_at
ended_at
source
```

例如：

```text
source = websocket
```

或者：

```text
source = flashblock
```

---

# 49. Graceful Shutdown

Live process 收到：

```text
SIGINT
SIGTERM
```

必须：

```text
stop subscriptions
stop ingestion
finish in-flight state transition
finish / cancel simulation safely
flush metrics
write session evidence
exit
```

不能留下：

```text
corrupted evidence
```

---

# 50. Backpressure

必须明确：

```text
WebSocket ingestion
→
event queue
→
state processing
→
opportunity queue
→
simulation queue
```

每一级是否：

```text
bounded
unbounded
drop
block
coalesce
```

都必须有设计。

特别注意：

> 不能 silently drop market events。

如果真的发生 backlog：

```text
BackpressureDetected
```

必须记录。

---

# 51. 不允许 silently drop

以下情况不能静默忽略：

* decode failure
* unknown pool
* unknown token
* missing block
* missing log
* duplicate event
* state conflict
* simulation failure
* provider reconnect
* queue overflow

必须：

```text
metric
log
evidence
```

至少记录一种结构化信息。

---

# 52. Unknown Protocol Event

不要因为：

```text
Unknown event
```

直接 panic。

应该：

```text
UnknownEvent
```

并保留：

```text
address
topics
data
block
tx
log_index
```

便于后续协议扩展。

---

# 53. Error Classification

不要全部：

```text
anyhow!("failed")
```

建议至少区分：

```text
TransportError
DecodeError
OrderingError
GapError
StateError
GraphError
OpportunityError
SimulationError
RiskError
```

具体类型按照现有 error architecture 调整。

---

# 54. Test Layers

必须至少有：

## Unit

测试：

* event ordering
* event identity
* duplicate detection
* gap detection
* stale opportunity
* state version
* timing
* metrics
* queue behavior

---

## Integration

真实：

```text
RPC
WebSocket
```

测试：

```text
connect
receive
decode
state
graph
opportunity
```

---

## Replay

真实历史 blocks：

```text
Replay → State → Graph → Opportunity
```

---

## Live

真实 GIWA Testnet：

```text
WebSocket → State → Graph → Opportunity
```

---

## Live + Simulation

至少验证：

```text
Live Opportunity
→
M4 Simulation
→
Risk
```

---

# 55. Acceptance Test A

### A1 — Workspace

```bash
cargo fmt --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

全部通过。

---

# 56. Acceptance B — Real GIWA Connection

真实：

```text
GIWA Testnet
```

能够：

```text
connect WebSocket
subscribe
receive blocks
```

并保存 evidence。

---

# 57. Acceptance C — Continuous Blocks

连续收到：

```text
N
N+1
N+2
...
```

无误判 gap。

---

# 58. Acceptance D — Gap Recovery

人工构造或通过受控 fixture：

```text
N
N+1
N+3
```

必须：

```text
detect
recover N+2
continue
```

---

# 59. Acceptance E — Event Ordering

相同 block：

```text
tx_index
log_index
```

排序确定。

---

# 60. Acceptance F — Duplicate

重复 event：

```text
State
```

只能 apply 一次。

---

# 61. Acceptance G — State

真实 GIWA logs：

```text
ProtocolEvent
→
StateUpdate
→
StateStore
```

并且 state 与 RPC historical readback 一致。

---

# 62. Acceptance H — Graph

State 更新之后：

```text
GraphSnapshot
```

包含正确 pool state。

---

# 63. Acceptance I — Opportunity

至少有真实历史 / live state：

```text
Pool A
Pool B
```

能够生成 candidate opportunity。

如果 live session 期间没有真实可接受套利机会：

> 不得伪造机会。

可以使用：

```text
recorded live events
```

做 replay acceptance。

但必须明确标记：

```text
replay
```

---

# 64. Acceptance J — Simulation

至少一个真实 opportunity：

```text
Opportunity
→
Simulation
```

Simulation 使用：

```text
exact block state
```

而不是 latest。

---

# 65. Acceptance K — Risk

Simulation result 必须进入：

```text
Risk
```

输出：

```text
Accept / Reject / Unknown
```

不能直接：

```text
Opportunity → Accept
```

---

# 66. Acceptance L — Stale

验证：

```text
Opportunity at N
```

之后：

```text
relevant pool changed at N+1
```

则：

```text
Opportunity(N) = STALE
```

不能被执行层继续消费。

---

# 67. Acceptance M — Replay / Live Parity

同一段真实 GIWA blocks：

```text
Replay
```

和：

```text
Live recording
```

最终：

```text
State
Graph
Opportunity
```

一致。

---

# 68. Acceptance N — Determinism

相同 event stream：

```text
Run A
Run B
```

结果：

```text
state_hash_A == state_hash_B
graph_hash_A == graph_hash_B
opportunity_A == opportunity_B
```

---

# 69. Acceptance O — Reconnect

测试：

```text
disconnect
reconnect
reconcile
continue
```

不得：

```text
duplicate state
miss blocks
```

---

# 70. Acceptance P — Latency

真实 live run 至少记录：

```text
block_received_latency
state_update_latency
opportunity_latency
simulation_latency
risk_latency
end_to_end_latency
```

输出：

```text
min
p50
p95
p99
max
```

---

# 71. Acceptance Q — No Execution

代码审查必须证明 M5 没有：

```text
private key
signing
broadcast
SequencerDirect
relay
bundle
```

---

# 72. Acceptance R — No Fake Data

最终 report 必须明确：

```text
real chain evidence
fixture evidence
replay evidence
live evidence
```

不能混淆。

---

# 73. Acceptance S — Flashblock

如果 GIWA Testnet Flashblock 可以被真实验证：

必须：

```text
FlashblockSource
```

真实运行。

至少验证：

```text
sequence
stale
gap
fallback
```

如果当前 endpoint 无法可靠验证：

必须：

1. 实现 abstraction
2. 保留 adapter boundary
3. 写测试
4. 写明 blocker
5. 不得伪造“Flashblock 已完成”

---

# 74. Acceptance T — Simulation Concurrency

必须证明：

```text
Live ingestion
```

不会因为：

```text
Simulation
```

阻塞。

至少需要一个 integration test 或 benchmark。

---

# 75. M5 最终运行模式

完成后应该能够运行：

```bash
evm-mev-bot live
```

看到类似：

```text
connected to GIWA
chain_id=91342

block=372xxxxx
received=...
state_updates=...
graph_updated=...
opportunities=...

opportunity=...
observed_block=...
pool_a=...
pool_b=...

simulation=...
gas=...
gross_profit=...
net_profit=...

risk=REJECT
reason=...

latency:
block_to_state=...
block_to_opportunity=...
block_to_simulation=...
block_to_risk=...
```

---

# 76. 不要为了演示伪造 Accept

尤其禁止：

```text
为了让 live demo 出现 Accept
```

而修改：

* reserve
* fee
* tax
* gas
* token balance
* risk threshold

如果真实链上没有 Accept：

> 就记录 Reject。

这比制造一个假的 Accept 更有价值。

---

# 77. M4 已发现的 TTAX 问题必须保留

不要在 M5 为了让机会看起来更漂亮而忽略：

```text
TTAX transfer tax
```

M4 已经证明：

```text
AMM analytical profit
≠
Executable profit
```

因此 M5 必须继续：

```text
Opportunity
→
Simulation
→
Risk
```

而不是：

```text
Opportunity
→
Profit > 0
→
Accept
```

---

# 78. L1 Fee

M4 已明确：

> 当前 net profit 计算只覆盖 EVM execution gas，不覆盖 OP-stack L1 data fee。

M5 不需要因此重构整个 Risk。

但必须：

```text
明确记录当前 cost model limitation
```

不要在 report 中把：

```text
net profit
```

描述成：

```text
最终真实可实现利润
```

---

# 79. Opportunity / Simulation / Execution 三层必须继续区分

严格保持：

```text
Estimated Opportunity
        ↓
Simulation Result
        ↓
Risk Decision
        ↓
Execution Result
        ↓
Realized Profit
```

M5 最多做到：

```text
Risk Decision
```

不能跨到：

```text
Execution Result
```

---

# 80. Architecture Review

完成代码后，Coding Agent 必须重新审查：

### 是否存在：

```text
Live-specific state logic
```

绕过 StateEngine？

### 是否存在：

```text
Replay-specific state logic
```

绕过 StateEngine？

### 是否存在：

```text
latest
```

读取混入 historical opportunity simulation？

### 是否存在：

```text
async ordering dependency
```

导致 nondeterminism？

### 是否存在：

```text
WebSocket received order
```

被错误当作 chain order？

### 是否存在：

```text
simulation blocks ingestion
```

？

### 是否存在：

```text
stale opportunity
```

进入 simulation/risk？

必须全部检查。

---

# 81. 性能目标

M5 不要求最终 MEV latency。

但是必须测量。

建议目标：

```text
Block received
→ State updated
```

尽量：

```text
< 100ms
```

普通：

```text
State
→ Opportunity
```

尽量：

```text
< 200ms
```

但这些是 engineering targets，不是硬性 correctness acceptance。

不要为了达到数字牺牲：

* correctness
* determinism
* state integrity

---

# 82. M5 不做的事情

明确禁止：

## 不做 Multi-chain

不做：

```text
BSC
Base
Ethereum
Arbitrum
```

---

## 不做 Execution

不做：

```text
Signer
SequencerDirect
Broadcast
```

---

## 不做 Flashloan

---

## 不做 Multi-hop

---

## 不做 V3

---

## 不做 Sandwich

---

## 不做 Liquidation

---

## 不做 AI

---

## 不做 Agent

---

## 不做 Dashboard

---

## 不做 Web Frontend

---

## 不做 Explorer

---

## 不做通用 Indexer

---

# 83. 推荐实施顺序

虽然 M5 是一个 milestone，但 Coding Agent 应按以下顺序实现。

### Phase 1

```text
Live Event abstraction
```

### Phase 2

```text
GIWA WebSocket
NewHeads
Logs
```

### Phase 3

```text
Ordering
dedup
gap detection
reconnect
```

### Phase 4

```text
State Engine
```

### Phase 5

```text
Graph update
```

### Phase 6

```text
Opportunity trigger
```

### Phase 7

```text
Live → Simulation
```

### Phase 8

```text
Simulation queue
```

### Phase 9

```text
Risk
```

### Phase 10

```text
metrics
evidence
```

### Phase 11

```text
Replay / Live parity
```

### Phase 12

```text
Flashblock
```

如果 Flashblock 当前真实 endpoint 无法验证，则停在 abstraction + blocker，而不是造 mock completion。

---

# 84. Git Commit 要求

建议：

```text
feat(chain): add GIWA websocket market source
```

```text
feat(pipeline): add live state pipeline
```

```text
feat(opportunity): connect live state to opportunity detection
```

```text
feat(simulation): connect live opportunities to simulation
```

```text
feat(metrics): add live pipeline latency metrics
```

最终：

```text
feat(m5): complete GIWA live market pipeline
```

不要产生大量：

```text
fix v0.5.1
fix v0.5.2
fix v0.5.3
```

这种碎片化版本。

---

# 85. M5 Completion Report

最终必须创建：

```text
docs/v0.1/M5 Completion Report.md
```

报告必须包含：

## 1. Summary

M5 做了什么。

## 2. Architecture

Live pipeline。

## 3. Real GIWA Evidence

真实：

```text
chain
blocks
logs
pools
state
```

## 4. State Semantics

Replay / Live 是否统一。

## 5. Gap Recovery

如何实现。

## 6. Duplicate / Ordering

如何实现。

## 7. Opportunity

真实 opportunity evidence。

## 8. Simulation

使用哪个 block。

## 9. Risk

Decision。

## 10. Latency

至少：

```text
p50
p95
p99
```

## 11. Flashblock

实际状态：

```text
PASS
PARTIAL
BLOCKED
```

必须说明原因。

## 12. Determinism

重复运行结果。

## 13. Replay / Live Parity

结果对比。

## 14. Limitations

尤其：

```text
L1 fee
testnet liquidity
execution absent
```

## 15. Tests

完整：

```text
cargo fmt --check
cargo check
cargo test
cargo clippy
```

## 16. Git Commits

列出：

```text
code commit
docs commit
```

---

# 86. 最终验收标准

M5 只有在下面这个闭环真实成立时才能标记 COMPLETE：

```text
        GIWA Testnet
             │
             ▼
      WebSocket / Source
             │
             ▼
        Block / Logs
             │
             ▼
       Protocol Decode
             │
             ▼
         StateUpdate
             │
             ▼
         StateStore
             │
             ▼
       Graph Snapshot
             │
             ▼
     Opportunity Detection
             │
             ▼
        Simulation
             │
             ▼
           Risk
             │
             ▼
       Decision + Metrics
```

并且：

```text
Replay == Live
```

在核心 state semantics 上成立。

---

# 87. M5 完成后的项目状态

M1：

```text
Protocol
```

↓

M2：

```text
Graph
```

↓

M3：

```text
Opportunity
```

↓

M4：

```text
Simulation + Risk
```

↓

M5：

```text
Live Market Pipeline
```

到这里，系统才真正从：

> “历史数据上的套利研究系统”

进入：

> **“能够实时观察 GIWA Testnet 市场并判断实时套利机会的系统”**

但仍然：

```text
NOT EXECUTING
```

下一阶段 M6 才是：

```text
Opportunity
 ↓
Simulation
 ↓
Risk
 ↓
Transaction Build
 ↓
Signer
 ↓
SequencerDirect
```

M6 的前提必须是 M5 已经能够稳定产生**带有明确 block/state version 的实时机会**。

---

# 88. 给 Coding Agent 的最后要求

开始编码之前：

1. 阅读当前 `PRD.md`
2. 阅读 M1-M4 Completion Reports
3. 阅读当前 workspace 全部 crate
4. 不假设上述结构已经存在
5. 先确认当前 M4 实际 API
6. 检查当前 ChainAdapter
7. 检查当前 StateStore / StateUpdate
8. 检查 Graph API
9. 检查 Opportunity API
10. 检查 Simulation API
11. 检查 Risk API
12. 检查当前 CLI
13. 检查真实 GIWA RPC / WebSocket 能力
14. 优先使用已有 abstraction
15. 不重复造轮子
16. 不修改 dsh / 其他外部项目——本项目与它们无关
17. 不使用 fake chain data 作为最终证据
18. 不为了通过 acceptance 修改真实业务语义
19. 不为了性能过早复杂化 Graph
20. 不跨入 M6 Execution

如果现有代码与本任务描述存在冲突：

> **以当前仓库真实代码和 M1-M4 已验证语义为准。**

如果发现架构问题：

> 优先修正根因，而不是增加临时兼容层。

如果某项无法真实验证：

> 明确记录 BLOCKED / PARTIAL，不得伪造 PASS。

最终提交：

```text
1. code
2. tests
3. real GIWA evidence
4. M5 Completion Report
5. git commit
```

并在最终报告中明确回答：

> **“M5 是否已经证明：GIWA Testnet 的实时链上状态，可以经过统一 State Semantics，进入实时 Opportunity → Simulation → Risk 闭环？”**

只有证据完整时，才能宣布：

```text
M5 COMPLETE
```
