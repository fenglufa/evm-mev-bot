# evm-mev-bot v0.1 — M2 Coding Agent Task

## 0. 任务定位

项目：

`evm-mev-bot`

版本：

`v0.1`

当前 Milestone：

`M2 — Real Market Graph`

M1 已经完成：

```text
Real Block
    ↓
Real Log
    ↓
Real Protocol Event
    ↓
Real Pool
    ↓
Real Sync
    ↓
Correct PoolState
```

M2 的任务是在 M1 已经验证的真实 Pool State 基础上建立：

```text
Pool State
    ↓
Pool Registry
    ↓
Token Nodes
    ↓
Pool Edges
    ↓
Graph Snapshot
    ↓
Real Market Graph
```

M2 **不实现套利机会检测**。

M2 完成后，系统应该能够回答：

> 在某一个确定的 block 上，系统知道哪些真实 Pool 存在、每个 Pool 连接哪些 Token、每条边对应什么 Pool，以及这些边使用什么真实状态。

---

# 1. M2 最终目标

最终形成：

```text
Token A
   │
   │ Pool 1
   │
Token B
   │
   │ Pool 2
   │
Token C
```

以及：

```text
Pool 1:
    token0 = A
    token1 = B
    reserve0 = ...
    reserve1 = ...

Pool 2:
    token0 = B
    token1 = C
    reserve0 = ...
    reserve1 = ...
```

Graph 必须能够从：

```text
A
```

找到：

```text
A → B
```

再找到：

```text
B → C
```

并且每条边都能够追溯到具体：

```text
Chain
Pool
Token pair
PoolState
Block
```

---

# 2. M2 最重要的原则

## 2.1 不允许为了制造 Graph 而制造 Pool

不能：

```text
Pool A
Pool B
Pool C
```

然后用 fixture 拼出一个漂亮的 Graph，就宣布 M2 完成。

必须区分：

```text
Synthetic Fixture
```

和：

```text
Real Market Graph
```

Fixtures 用于验证 Graph 算法。

Real Data 用于证明真实链上市场关系。

---

# 3. M2 首先调查 M1 的实现

开始工作前，必须先阅读当前项目。

执行：

```bash
pwd
find . -maxdepth 4 -type f | sort
```

重点阅读：

```text
crates/core/
crates/chain/
crates/protocol/
crates/state/
crates/replay/
data/protocols/
fixtures/
docs/v0.1/
PRD.md
```

尤其需要确认 M1 当前已经提供的：

```text
PoolMeta
PoolState
TokenId
PoolId
StateSnapshot
ProtocolId
ChainId
```

以及：

```text
StateStore
StateUpdate
Replay
```

**不要重新定义已有类型。**

如果 M1 已有类型不足，优先扩展已有类型。

---

# 4. M2 的核心数据模型

Graph 使用：

```text
Token = Node
Pool = Edge
```

即：

```text
Token A
    ↕
   Pool
    ↕
Token B
```

一个双 Token Pool 对 Graph 来说产生两个方向：

```text
A → B
B → A
```

但两个方向必须指向同一个：

```text
PoolId
```

不能复制成两个独立 Pool。

---

# 5. GraphSnapshot

Graph 必须绑定一个确定的 block。

建议：

```rust
struct GraphSnapshot {
    chain_id: ChainId,
    block_number: BlockNumber,
    nodes: ...,
    edges: ...,
}
```

不能存在一个没有 block identity 的“当前 Graph”。

原因：

```text
Block N
```

和：

```text
Block N+1
```

可能有完全不同的 reserve。

所以：

```text
GraphSnapshot(N)
```

必须与：

```text
GraphSnapshot(N+1)
```

明确区分。

---

# 6. GraphEdge

建议模型：

```rust
struct GraphEdge {
    pool: PoolId,

    token_in: TokenId,
    token_out: TokenId,

    reserve_in: U256,
    reserve_out: U256,

    fee: Fee,
}
```

具体字段可以根据 M1 实际模型调整。

关键要求：

```text
token_in != token_out
pool 必须存在
reserve 必须来自对应 PoolState
```

---

# 7. 双向 Edge

如果：

```text
Pool P
token0 = A
token1 = B
```

Graph 必须产生：

```text
A → B
B → A
```

即：

```text
GraphEdge {
    pool: P,
    token_in: A,
    token_out: B,
    reserve_in: reserve0,
    reserve_out: reserve1,
}

GraphEdge {
    pool: P,
    token_in: B,
    token_out: A,
    reserve_in: reserve1,
    reserve_out: reserve0,
}
```

两个 Edge：

```text
pool == P
```

---

# 8. Pool Registry

M2 需要明确 Pool Registry 的职责。

Registry 负责回答：

```text
PoolId
   ↓
PoolMeta
```

例如：

```text
Pool
 ├── protocol
 ├── token0
 ├── token1
 ├── fee
 └── pool_type
```

StateStore 负责：

```text
PoolId
   ↓
PoolState
```

Graph Builder 将两者组合：

```text
PoolMeta + PoolState
        ↓
GraphEdge
```

不要把：

```text
PoolMeta
PoolState
GraphEdge
```

混成一个对象。

---

# 9. Pool Discovery 是 M2 的重要部分，但不要擅自假设发现机制

M1 已经证明至少存在一个真实 Pool。

现在 M2 需要调查：

```text
如何发现更多真实 Pool？
```

Agent 必须自己调查现有数据。

重点检查：

```text
PoolCreated
Factory
Factory events
Protocol registry
Existing protocol profile
Historical evidence
```

如果存在可靠的 Factory / PoolCreated 证据：

可以实现：

```text
Factory
    ↓
PoolCreated
    ↓
Pool Registry
```

如果不存在：

必须使用当前项目已有的、能够被证明正确的 Pool discovery 机制。

---

# 10. 不允许为了 M2 强行加入 Factory

如果调查发现：

```text
Factory evidence 不足
```

不要为了“完整”而写：

```rust
FactoryAdapter
```

然后假设它存在。

此时可以：

```text
Known Pool Registry
        ↓
Graph
```

先建立 Graph。

M2 的目标是：

```text
Real Market Graph
```

不是：

```text
Universal Pool Discovery
```

---

# 11. Real Pool Registry 的证据要求

每个进入 Real Pool Registry 的 Pool 必须已经满足 M1 的 Pool identity 规则。

即：

```text
Pool
 ↓
Protocol evidence
 ↓
Token evidence
 ↓
State evidence
```

不能因为发现：

```text
Sync(...)
```

就自动添加 Pool。

M1 已经证明：

```text
Sync-shaped log ≠ Pool
```

M2 必须继承这一原则。

---

# 12. Unknown Pool

如果遇到：

```text
Sync-shaped log
```

但是没有充分 Pool identity evidence：

```text
不要进入 PoolRegistry
不要进入 Graph
不要进入 Real Market Graph
```

可以记录：

```text
UnattestedPool
```

或者直接忽略。

不要把未知地址当作真实市场节点。

---

# 13. Graph Builder

建立明确的 Graph Builder。

建议：

```rust
trait GraphBuilder {
    fn build(
        &self,
        snapshot: &StateSnapshot,
    ) -> Result<GraphSnapshot>;
}
```

实际接口可以根据当前代码调整。

Builder 应该执行：

```text
PoolRegistry
+
PoolState
+
Token identity
        ↓
GraphSnapshot
```

---

# 14. Graph Build 流程

标准流程：

```text
StateSnapshot
       ↓
iterate PoolMeta
       ↓
find PoolState
       ↓
validate Pool
       ↓
create Token Node
       ↓
create forward Edge
       ↓
create reverse Edge
       ↓
GraphSnapshot
```

---

# 15. 缺失 State 的 Pool

如果：

```text
PoolMeta
```

存在，但是：

```text
PoolState
```

不存在：

不要制造 reserve。

不能：

```text
reserve = 0
```

也不能：

```text
reserve = latest RPC value
```

除非当前明确执行的是 recovery 流程。

正常 Graph Snapshot：

```text
PoolMeta without valid PoolState
```

应该：

```text
skip
```

或者显式标记为：

```text
StateUnavailable
```

具体行为保持简单。

---

# 16. Invalid Pool State

以下情况不能进入有效 Graph Edge：

```text
reserve0 == 0 && reserve1 == 0
```

或者其他协议定义的无效状态。

但注意：

**不要因为 reserve 很小就认为无效。**

例如：

```text
reserve0 = 1
reserve1 = 1000000000000
```

仍然可能是真实状态。

只验证明确非法条件。

---

# 17. Token Node

Token Node 至少应该由：

```text
TokenId
```

标识。

由于：

```text
TokenId = ChainId + Address
```

不同链的同地址 Token 不允许合并。

例如：

```text
Chain A + 0x123
```

与：

```text
Chain B + 0x123
```

必须是不同 Node。

---

# 18. Pool Edge 的状态一致性

如果 Pool：

```text
token0 = A
token1 = B
```

并且：

```text
reserve0 = 100
reserve1 = 200
```

则：

```text
A → B
```

必须使用：

```text
100 → 200
```

而：

```text
B → A
```

必须使用：

```text
200 → 100
```

不能交换错误。

---

# 19. Fee

如果 Pool 有真实 fee：

必须使用：

```text
PoolMeta.fee
```

或对应真实协议状态。

不要在 Graph 中默认：

```text
0.3%
```

不要写死：

```text
30 / 10000
```

除非真实协议证据确认。

M2 只需要保存 fee。

**暂时不要用 fee 做套利计算。**

---

# 20. Graph 查询能力

M2 至少需要支持：

```text
neighbors(token)
```

例如：

```text
A
 ↓
[B, C, D]
```

还需要：

```text
edges(token_in, token_out)
```

能够找到：

```text
A → B
```

对应的全部 Pool。

这是后续套利搜索必须依赖的能力。

---

# 21. 一个 Token 对多个 Pool

必须正确支持：

```text
Token A
   │
   ├── Pool 1 ── Token B
   │
   ├── Pool 2 ── Token B
   │
   └── Pool 3 ── Token C
```

尤其：

```text
A ↔ B
```

可能存在多个不同 Pool。

不能使用：

```text
HashMap<(A,B), SinglePool>
```

然后覆盖其他 Pool。

应该支持：

```text
(A,B) → [Pool1, Pool2, ...]
```

这是 M3 两池套利的基础。

---

# 22. Edge Identity

一个 Graph Edge 的身份不能只由：

```text
token_in
token_out
```

决定。

因为：

```text
A → B via Pool1
```

和：

```text
A → B via Pool2
```

是两个不同市场。

因此至少：

```text
EdgeId = PoolId + token_in + token_out
```

或者等价的唯一结构。

---

# 23. Graph Snapshot 的不可变性

构建完成后：

```text
GraphSnapshot
```

应该视为 immutable snapshot。

不要在 Opportunity Search 中直接修改 Graph。

后续如果 PoolState 更新：

```text
StateStore
 ↓
new snapshot
 ↓
GraphSnapshot(N+1)
```

而不是：

```text
GraphSnapshot(N)
```

被偷偷修改。

---

# 24. Graph 与 StateStore 的关系

不要让 Graph Builder 自己重新查询 RPC。

正确：

```text
Chain
 ↓
Protocol
 ↓
StateStore
 ↓
StateSnapshot
 ↓
GraphBuilder
 ↓
GraphSnapshot
```

禁止：

```text
GraphBuilder
 ↓
RPC
```

Graph 应该是 State 的纯消费方。

---

# 25. 同 Block 一致性

GraphSnapshot 必须保证所有 Pool State 来自同一个：

```text
block_number
```

不能：

```text
Pool1 → Block N
Pool2 → Block N+1
Pool3 → Block N-1
```

然后称之为：

```text
GraphSnapshot(N)
```

如果 StateStore 中存在不同 block 的状态：

必须明确选择：

```text
target block
```

并获取该 block 对应的 state snapshot。

---

# 26. Real Data 调查

Agent 必须再次利用：

```text
/Volumes/superfs/giwa-mev
```

调查更多真实 Pool。

不要要求用户人工提供 Pool 地址。

调查顺序：

```text
Existing protocol evidence
        ↓
Pool-related relationships
        ↓
Pool creation evidence
        ↓
Pool state evidence
        ↓
Confirmed Pool Registry
```

如果已有 M1 数据不足：

允许使用 RPC 做验证。

---

# 27. Real Market Graph 的最低要求

M2 最终至少应该达到：

```text
真实 Pool A
    ↕
Token X / Token Y

真实 Pool B
    ↕
Token Y / Token Z
```

从而形成：

```text
X
│
Pool A
│
Y
│
Pool B
│
Z
```

如果真实数据中确实不存在第二个可以证明的 Pool：

**不能制造第二个 Pool。**

此时 M2 可以在：

```text
Real Graph with one confirmed Pool
```

基础能力上完成，但必须明确：

```text
Multi-pool real validation BLOCKED
```

并说明缺失证据。

不要为了让验收通过制造假数据。

---

# 28. Fixtures

必须建立最小 Graph fixtures。

## Fixture A：One Pool

```text
A ↔ B
```

验证：

```text
nodes = 2
edges = 2
```

---

## Fixture B：Two Pools Same Pair

```text
Pool1: A ↔ B
Pool2: A ↔ B
```

验证：

```text
edges(A,B) = 2
```

不能覆盖。

---

## Fixture C：Three Token Chain

```text
Pool1: A ↔ B
Pool2: B ↔ C
```

验证：

```text
neighbors(A) = B
neighbors(B) = A,C
neighbors(C) = B
```

---

## Fixture D：Same Token Address Different Chain

```text
Chain1 + 0x123
Chain2 + 0x123
```

验证：

```text
node != node
```

---

## Fixture E：Missing State

```text
PoolMeta exists
PoolState missing
```

验证：

```text
no valid GraphEdge
```

---

## Fixture F：Invalid State

验证：

```text
invalid reserve
```

不会进入有效 Graph。

---

# 29. Real Data Acceptance Test

M2 必须加入真实数据测试。

至少证明：

```text
Real Pool
    ↓
PoolMeta
    ↓
PoolState
    ↓
GraphEdge
```

并验证：

```text
pool
token0
token1
reserve0
reserve1
block
```

完全对应。

---

# 30. 如果发现多个真实 Pool

如果 Agent 调查发现多个真实 Pool：

必须全部建立证据链：

```text
Pool1
Pool2
Pool3
...
```

并验证：

```text
Pool Registry
      ↓
Graph
```

没有地址被重复、覆盖或者错误合并。

---

# 31. Graph Determinism

同样的：

```text
StateSnapshot
```

执行：

```text
Build A
Build B
```

必须得到完全一致的 Graph。

不能依赖：

```text
HashMap iteration order
filesystem order
RPC response order
```

影响 Graph 的最终序列化结果。

如果需要 deterministic output：

使用：

```text
sorted IDs
```

或明确的数据结构。

---

# 32. Graph Serialization

M2 应该提供一种 deterministic representation。

例如：

```text
GraphSnapshot
```

能够序列化成：

```json
{
  "chain_id": ...,
  "block_number": ...,
  "nodes": [...],
  "edges": [...]
}
```

具体格式根据现有项目风格决定。

要求：

同一输入：

```text
JSON A == JSON B
```

---

# 33. 性能要求

M2 不需要过早优化。

优先：

```text
correctness
determinism
simple lookup
```

不要现在实现：

```text
lock-free graph
incremental graph mutation
SIMD
parallel graph algorithms
custom allocator
```

除非实际测试证明必要。

---

# 34. 不允许实现的内容

当前 M2 禁止：

```text
Arbitrage detection
Bellman-Ford
Cycle profitability
Optimal input amount
Profit calculation
Gas estimation
REVM
Transaction simulation
Execution
Bundle
Private relay
Signer
Nonce manager
```

尤其不要因为 Graph 已经存在就顺便写：

```text
find_arbitrage()
```

M3 再做。

---

# 35. 不要把 Graph 做成“万能数据结构”

不要设计：

```text
GenericGraph<TEverything>
```

然后加入大量：

```text
metadata
semantic labels
AI tags
protocol classification
risk score
execution state
```

M2 Graph 只表达：

```text
Token
Pool
PoolState
```

足够。

---

# 36. Chain-neutral

Graph 层不得写：

```text
GIWA
91342
某个 GIWA 地址
```

Graph 只依赖：

```text
ChainId
TokenId
PoolId
PoolMeta
PoolState
```

链特定数据继续留在：

```text
data/protocols/
```

以及测试数据中。

---

# 37. M2 开发顺序

严格按照：

```text
Step 1
阅读 M1 实现
        ↓
Step 2
确认 PoolMeta / PoolState / TokenId
        ↓
Step 3
调查真实 Pool Discovery
        ↓
Step 4
建立 / 扩展 Pool Registry
        ↓
Step 5
实现 GraphSnapshot
        ↓
Step 6
实现 GraphEdge
        ↓
Step 7
实现双向 Edge
        ↓
Step 8
实现 neighbors / edges 查询
        ↓
Step 9
实现 deterministic serialization
        ↓
Step 10
Fixtures
        ↓
Step 11
Real Market Graph Test
        ↓
Step 12
完整测试
        ↓
Step 13
M2 Completion Report
```

---

# 38. M2 完成验收

只有满足以下条件才能宣布：

```text
M2 COMPLETE
```

## A. Pool Registry

能够保存经过验证的真实 Pool。

## B. Token Nodes

真实 Token 能够成为 Graph Node。

## C. Pool Edges

Pool 能够成为：

```text
Token A ↔ Token B
```

双向 Edge。

## D. Multiple Pools

如果真实数据存在多个同 Token Pair Pool：

必须全部保留。

## E. State Consistency

Edge 的 reserve 必须来自对应：

```text
PoolState
```

## F. Block Consistency

一个 GraphSnapshot 必须对应一个明确 block。

## G. Query

至少支持：

```text
neighbors(token)
edges(token_in, token_out)
```

## H. Determinism

同一 StateSnapshot：

```text
Graph A == Graph B
```

## I. Real Data

至少一个真实 Pool 进入真实 Graph。

如果真实数据已经能够证明多个 Pool，则必须进行多 Pool 真实验证。

## J. Tests

必须通过：

```bash
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

具体命令根据项目实际配置调整。

---

# 39. M2 Completion Report

完成后必须生成：

```text
docs/v0.1/M2 Completion Report.md
```

报告至少包含：

## 1. Implementation

新增 / 修改的模块。

## 2. Pool Discovery

实际使用什么方式发现 Pool。

## 3. Confirmed Pools

列出真实确认的 Pool 数量。

## 4. Token Graph

节点数量。

## 5. Edge Graph

边数量。

## 6. Real Block

真实 Graph 对应的 block。

## 7. Real Pool Evidence

每个真实 Pool 的证据。

## 8. Determinism

同输入构建两次的结果。

## 9. Tests

fmt / check / test / clippy。

## 10. Known Limitations

特别说明：

```text
Factory evidence
Pool discovery coverage
Number of confirmed pools
```

## 11. M2 Status

```text
COMPLETE
```

或者：

```text
BLOCKED
```

不得使用模糊状态。

---

# 40. 最终原则

M2 的核心不是：

> “我们实现了 Graph。”

而是证明：

```text
真实链上状态
      ↓
真实 Pool
      ↓
真实 Token relationship
      ↓
真实 Market Graph
```

Graph 是 State 的确定性投影：

```text
StateSnapshot
      ↓
GraphSnapshot
```

不要让 Graph 自己创造事实。

不要让 Graph 自己查询链。

不要让 Graph 猜测 Pool。

不要让 Graph 覆盖多个市场。

不要在 M2 偷渡套利逻辑。

---

# 41. M2 最终目标

完成后，我们应该能够得到：

```text
                Real Market Graph

                    Token A
                   /       \
                  /         \
               Pool 1      Pool 2
                /             \
               /               \
           Token B ----------- Token C
                  \
                   \
                   Pool 3
                     \
                    Token D
```

每一条边都能追溯：

```text
Edge
 ↓
Pool
 ↓
PoolState
 ↓
Block
 ↓
Real Chain Evidence
```

到了这里：

```text
Chain
 ↓
Protocol
 ↓
State
 ↓
Graph
```

才真正完整。

**然后 M3 才开始承担 Opportunity。**
