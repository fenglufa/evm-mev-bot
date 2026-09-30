# evm-mev-bot v0.1 — M3 Coding Agent Task

## 1. 任务定位

项目：

`evm-mev-bot`

当前版本：

`v0.1`

当前里程碑：

```text
M1 Real Pool State        COMPLETE
M2 Real Market Graph      COMPLETE
M3 Real Arbitrage         ← 本任务
```

M1 已经证明：

```text
Real Block
 ↓
Real Log
 ↓
Real Sync
 ↓
Correct PoolState
```

M2 已经证明：

```text
Real PoolState
 ↓
Pool Registry
 ↓
Token Nodes
 ↓
Pool Edges
 ↓
GraphSnapshot
```

M3 现在需要继续向前：

```text
GraphSnapshot
 ↓
Candidate Path
 ↓
AMM Math
 ↓
Optimal Input
 ↓
Gross Profit
 ↓
Opportunity
```

M3 的目标不是实现完整 MEV 执行系统。

M3 的目标是：

> **在真实 EVM 市场状态上，能够发现并数学验证 two-pool arbitrage opportunity。**

---

# 2. M3 最终目标

M3 完成后，系统至少应该能够处理：

```text
Token A
   ↓
Pool 1
   ↓
Token B
   ↓
Pool 2
   ↓
Token A
```

然后计算：

```text
input A
 ↓
Pool 1 swap
 ↓
amount B
 ↓
Pool 2 swap
 ↓
amount A'
```

得到：

```text
gross_profit = amount_A' - input_A
```

并进一步寻找：

```text
optimal_input
```

使：

```text
profit(input)
```

在当前 GraphSnapshot 状态下达到最大或经过明确算法验证的近似最大值。

最终形成：

```text
Opportunity {
    chain_id,
    block_number,
    path,
    input_token,
    input_amount,
    output_amount,
    gross_profit,
}
```

注意：

M3 的 `Opportunity` 代表：

> 基于当前市场状态和 AMM 数学计算得到的理论毛利润机会。

它不是：

* 模拟执行结果
* gas 后利润
* bribe 后利润
* bundle 结果
* 实际执行结果
* realized profit

---

# 3. 严格禁止范围

M3 不得扩张到以下内容：

```text
REVM
Transaction Simulation
Gas Estimation
Gas Price
Priority Fee
Bribe
Bundle
Private Relay
Flashbots
Signer
Transaction Builder
Transaction Broadcast
Nonce Manager
Execution
Execution Risk
Token Tax
Honeypot Detection
V3
Concentrated Liquidity
Liquidation
Sandwich
NFT MEV
Intent
Cross-chain Arbitrage
AI Strategy
Machine Learning
Generic Semantic Classification
Universal DEX Support
```

这些都不属于 M3。

M3 只解决：

```text
Graph
 ↓
Path
 ↓
AMM Math
 ↓
Optimal Input
 ↓
Gross Profit
```

---

# 4. 第一原则：先阅读 M1/M2

开始编码前必须先阅读：

```text
M1 implementation
M1 Completion Report
M2 implementation
M2 Completion Report
M2 fixtures
M2 real-data evidence
```

尤其确认当前实际存在的：

```text
PoolMeta
PoolState
TokenId
PoolId
GraphSnapshot
GraphEdge
PoolRegistry
GraphBuilder
```

不要重新定义已有类型。

如果现有类型可以复用，应直接复用。

如果确实需要扩展，应说明：

```text
为什么现有模型不足
新增字段是什么
为什么属于 M3
```

不要为了实现方便复制一套平行数据模型。

---

# 5. M3 核心边界

M3 的边界必须保持：

```text
M2
GraphSnapshot
    ↓
M3
Opportunity Detector
```

不要让 Opportunity Detector 反向：

```text
RPC
StateStore
ProtocolDecoder
```

也就是说：

> M3 应该是纯市场计算层。

输入：

```text
GraphSnapshot
```

输出：

```text
Opportunity
```

RPC 不属于 Opportunity Detector。

---

# 6. Two-Pool Arbitrage

第一阶段只实现：

```text
A → Pool1 → B → Pool2 → A
```

例如：

```text
Token A
   │
   ▼
Pool 1
   │
   ▼
Token B
   │
   ▼
Pool 2
   │
   ▼
Token A
```

其中：

```text
Pool1 != Pool2
```

两个 Pool 必须是不同的 liquidity source。

---

# 7. Pool Direction

Graph 中一个 Pool 会产生两个方向：

```text
A → B
B → A
```

因此套利路径不能简单根据：

```text
token pair
```

判断。

必须使用：

```text
PoolId
+
token_in
+
token_out
```

确定具体 hop。

---

# 8. Hop 模型

如果现有模型不存在，可以新增：

```rust
struct Hop {
    pool_id: PoolId,
    token_in: TokenId,
    token_out: TokenId,
}
```

一个 two-pool path：

```rust
struct ArbitragePath {
    hops: Vec<Hop>,
}
```

M3 第一阶段必须严格：

```text
hops.len() == 2
```

并且：

```text
hop[0].token_out == hop[1].token_in
hop[1].token_out == hop[0].token_in
hop[0].pool_id != hop[1].pool_id
```

---

# 9. 不允许同池循环

必须拒绝：

```text
A → Pool1 → B → Pool1 → A
```

即：

```rust
hop0.pool_id == hop1.pool_id
```

直接 invalid。

这是非常重要的，因为：

```text
same pool round trip
```

不是我们要寻找的跨市场套利。

---

# 10. AMM 模型

M3 第一阶段只实现当前 M2 已经确认能够表示的：

```text
V2-style constant-product AMM
```

公式必须基于真实：

```text
reserve_in
reserve_out
fee
```

计算。

不能默认：

```text
fee = 0.003
```

不能在数学模块中写：

```rust
const FEE: ...
```

除非这个 fee 来自已经存在的 PoolMeta / GraphEdge。

---

# 11. Swap Math

对于：

```text
reserve_in
reserve_out
amount_in
fee_numerator
fee_denominator
```

使用整数数学。

概念公式：

```text
amount_in_with_fee =
    amount_in * fee_numerator

amount_out =
    amount_in_with_fee * reserve_out
    /
    (
        reserve_in * fee_denominator
        + amount_in_with_fee
    )
```

但必须根据当前项目已有 Fee 定义实现。

不要机械复制上述字段名称。

必须：

```text
检查现有 Fee 模型
 ↓
确认 numerator / denominator 语义
 ↓
实现统一 swap function
```

---

# 12. U256 是最终真值

所有最终金额计算必须使用：

```rust
U256
```

禁止：

```rust
f64
f32
```

作为最终计算类型。

尤其禁止：

```rust
reserve as f64
amount as f64
profit as f64
```

然后再转换回来。

因为 EVM token amount 是整数，而套利机会往往可能只有非常小的 margin。

---

# 13. Overflow

必须考虑：

```text
amount_in * fee_numerator
```

以及：

```text
amount_in_with_fee * reserve_out
```

可能产生的大整数。

使用当前 Rust/EVM 项目已经采用的安全 U256 运算方式。

不得：

```rust
unwrap()
expect()
panic!()
```

处理外部市场数据。

对于数学 overflow：

```text
返回 MathError
```

而不是 panic。

---

# 14. Zero / Invalid Reserves

以下状态必须拒绝：

```text
reserve_in == 0
reserve_out == 0
```

也必须拒绝：

```text
amount_in == 0
```

错误必须是明确的 domain error。

例如：

```text
InvalidReserve
InvalidAmount
InvalidFee
Overflow
```

具体命名根据现有项目 error 风格。

---

# 15. Single Hop Math

先实现：

```text
swap_exact_in(
    reserve_in,
    reserve_out,
    fee,
    amount_in
) -> amount_out
```

这个函数必须：

* deterministic
* pure
* no RPC
* no StateStore
* no Graph access
* U256
* tested

---

# 16. Two-Hop Math

然后实现：

```text
simulate_two_hop(
    path,
    input_amount,
    snapshot
)
```

逻辑：

```text
amount0 = input_amount

amount1 =
    swap(
        pool1,
        amount0
    )

amount2 =
    swap(
        pool2,
        amount1
    )

profit =
    amount2 - amount0
```

但是必须防止：

```text
amount2 < amount0
```

发生 U256 underflow。

应明确返回：

```text
profit = 0
```

或者使用 signed/domain result 表示亏损。

建议最终 Opportunity 只接受：

```text
gross_profit > 0
```

---

# 17. Profit Representation

不要强行把亏损表示成 U256 负数。

推荐区分：

```rust
SwapResult {
    amount_out: U256,
}

ProfitResult {
    input: U256,
    output: U256,
    profit: ...
}
```

如果当前项目已有适合表示：

```text
output < input
```

的金额差类型，可以复用。

最终：

```text
Opportunity
```

只进入：

```text
gross_profit > 0
```

的结果。

---

# 18. Candidate Path Discovery

第一阶段不要实现复杂图算法。

M3 只需要从：

```text
GraphSnapshot
```

枚举：

```text
Pool1
Pool2
```

形成：

```text
A → B
B → A
```

即可。

逻辑：

```text
for each token A:
    for each edge A → B:
        for each edge B → A:
            if pool1 != pool2:
                candidate path
```

必须去重。

例如：

```text
Pool1 → Pool2
```

与：

```text
Pool2 → Pool1
```

在路径方向上可能分别对应不同 input token。

不要简单用无向 pair 去重。

---

# 19. Multiple Pools

必须支持：

```text
A/B:

Pool1
Pool2
Pool3
```

产生：

```text
Pool1 → Pool2
Pool1 → Pool3
Pool2 → Pool1
Pool2 → Pool3
Pool3 → Pool1
Pool3 → Pool2
```

但：

```text
Pool1 → Pool1
Pool2 → Pool2
Pool3 → Pool3
```

全部禁止。

---

# 20. Optimal Input

这是 M3 的核心。

不能只测试：

```text
input = 1 token
```

因为：

```text
profit(input)
```

并不是固定线性函数。

必须寻找：

```text
argmax profit(input)
```

---

# 21. Optimal Input 第一版算法

不要为了“数学完美”引入复杂优化库。

M3 第一阶段允许使用：

```text
bounded search
```

或者：

```text
coarse-to-fine search
```

例如：

```text
确定 input 上限
 ↓
粗粒度搜索
 ↓
找到最优区间
 ↓
缩小区间
 ↓
再次搜索
 ↓
直到达到精度/迭代限制
```

但必须先解决：

> 如何确定合理的 input upper bound。

不能直接：

```text
U256::MAX
```

进行搜索。

---

# 22. Input Upper Bound

至少必须受：

```text
reserve_in
```

约束。

不能允许：

```text
amount_in
```

远大于池子的流动性。

可以采用项目内明确的 bounded policy。

例如：

```text
max_input = reserve_in * ratio
```

但 ratio 必须：

* 可配置
* 有文档说明
* 不作为最终数学真理
* 不能隐藏在代码里

如果能推导更严格的 bound，优先使用数学推导。

---

# 23. Search Precision

必须明确记录：

```text
search_iterations
search_strategy
input_lower_bound
input_upper_bound
```

如果最终采用近似搜索：

`Opportunity` 不应声称：

> “这是绝对数学全局最优。”

应该表达为：

> “在指定搜索范围和搜索策略下得到的最优输入。”

除非代码和数学确实能够证明全局最优。

---

# 24. Math Validation

这是 M3 最重要的测试之一。

对于 fixture：

```text
reserveA
reserveB
fee
```

使用：

```text
brute force / exhaustive local search
```

验证优化器结果。

例如：

```text
input = 1..N
```

在较小 fixture 上穷举。

然后：

```text
optimizer_result
```

必须与：

```text
brute_force_best
```

一致，或者满足明确的误差/搜索策略条件。

---

# 25. Fixture 设计

至少需要：

### Fixture 1：No Arbitrage

```text
Pool1
Pool2
```

价格一致。

结果：

```text
no opportunity
```

---

### Fixture 2：Obvious Arbitrage

两个池价格明显不同。

结果：

```text
opportunity exists
gross_profit > 0
```

---

### Fixture 3：Fee Erases Profit

价格差存在，但加入 fee 后：

```text
gross_profit <= 0
```

必须：

```text
no opportunity
```

---

### Fixture 4：Optimal Input

构造一个：

```text
profit(input)
```

明显存在最佳输入的案例。

验证：

```text
optimizer
```

确实找到合理峰值。

---

### Fixture 5：Zero Reserve

必须拒绝。

---

### Fixture 6：Extreme Reserve

测试：

```text
U256
```

大数计算。

---

### Fixture 7：Same Pool Twice

```text
A → Pool1 → B → Pool1 → A
```

必须拒绝。

---

### Fixture 8：Multiple Pools Same Pair

至少：

```text
A/B Pool1
A/B Pool2
A/B Pool3
```

验证候选路径数量和去重。

---

# 26. Real Data Investigation

这是 M3 与普通数学库最大的区别。

不要先让人手工告诉你：

```text
哪两个池子可以套利
```

由 Coding Agent 自己调查。

数据优先级：

```text
1. M2 已验证 evidence
2. M2 Pool Registry
3. M2 real GraphSnapshot / fixtures
4. 现有 historical data
5. RPC validation
6. 必要时重新抓取
```

禁止：

```text
猜测池子关系
```

禁止：

```text
为了得到盈利结果而修改真实 reserve
```

禁止：

```text
人为制造真实机会
```

---

# 27. Real Market Opportunity

Agent 必须在真实历史市场中搜索：

```text
GraphSnapshot
 ↓
Two-pool candidate
 ↓
AMM calculation
 ↓
optimal input
 ↓
gross profit
```

如果找到真实机会：

必须记录：

```text
chain
block
pool1
pool2
token A
token B
reserve state
fees
input
output
gross profit
```

并能够重新运行得到相同结果。

---

# 28. 如果真实数据没有套利

不能制造。

必须按照：

```text
当前真实数据
 ↓
搜索多个历史 block
 ↓
扩大真实数据范围
 ↓
重新计算
```

如果仍然不存在：

报告：

```text
No profitable opportunity found
```

同时提供：

```text
searched block range
number of snapshots
number of candidate paths
number of evaluated paths
best gross profit
best candidate
```

这样：

```text
“No opportunity found”
```

本身也是可验证结果。

---

# 29. Real Opportunity Fixture

如果找到真实套利，必须建立一个可复现 fixture。

fixture 至少包含：

```text
chain_id
block_number
pool metadata
pool state
fees
candidate path
expected input
expected output
expected gross profit
```

并验证：

```text
fixture replay
```

与真实计算一致。

但注意：

fixture 是真实数据的快照，不是人工构造的“真实机会”。

---

# 30. Block Consistency

M3 必须严格使用：

```text
GraphSnapshot.block_number
```

对应的状态。

不能：

```text
Pool1 block N
Pool2 block N+1
```

混合计算。

如果 GraphSnapshot 已经保证这一点，M3 必须直接依赖这个保证，而不是重新查询 RPC。

---

# 31. Determinism

同一个：

```text
GraphSnapshot
```

执行：

```text
detect_opportunities(snapshot)
```

两次。

结果必须完全一致。

包括：

```text
path ordering
input
output
profit
```

不能因为：

```text
HashMap iteration order
```

导致结果变化。

必要时：

```text
sort
```

所有 candidate。

---

# 32. Opportunity Ordering

如果最终返回多个 Opportunity：

必须定义 deterministic ordering。

例如：

```text
gross_profit descending
```

然后：

```text
path identity
```

作为 tie-breaker。

具体排序规则必须写入文档和测试。

不要依赖：

```text
HashMap
BTreeMap
```

的偶然 iteration 行为来形成业务语义。

---

# 33. Opportunity Identity

必须能够稳定标识：

```text
chain
block
input token
hop0 pool
hop1 pool
direction
```

例如可以形成：

```text
OpportunityId
```

但如果现有项目没有必要新增 ID 类型，不要为了类型数量而增加。

核心要求是：

> 相同 snapshot + 相同 path 必须得到相同机会身份。

---

# 34. Graph 与 Opportunity 的职责边界

Graph：

```text
回答：
哪些 Token 与哪些 Token 通过哪些 Pool 相连？
```

Opportunity：

```text
回答：
这些连接形成的路径是否存在理论盈利？
```

Graph 不应该：

```text
计算 profit
```

Opportunity 也不应该：

```text
修改 GraphSnapshot
```

---

# 35. 不修改 Market State

M3 的模拟：

```text
PoolState
```

只是输入。

计算 swap 时：

```text
不要修改 PoolState
```

也不要把：

```text
simulated reserve
```

写回：

```text
StateStore
```

M3 只是：

```text
pure calculation
```

---

# 36. AMM Math API

推荐最终形成类似：

```rust
swap_exact_in(...)
```

以及：

```rust
simulate_path(...)
```

然后：

```rust
find_optimal_input(...)
```

最后：

```rust
detect_opportunities(...)
```

形成：

```text
Opportunity Detection
    ↓
Path Enumeration
    ↓
Optimal Input
    ↓
Path Simulation
    ↓
AMM Math
```

具体模块名称按照现有 workspace 风格决定。

不要机械照搬上述 crate/module 名称。

---

# 37. Crate 设计

优先考虑：

```text
crates/opportunity
```

如果数学逻辑足够独立，可以：

```text
crates/opportunity/
├── path.rs
├── math.rs
├── optimizer.rs
├── detector.rs
├── error.rs
└── tests/
```

但必须先阅读当前 workspace。

如果已有：

```text
core
graph
```

可以合理复用。

不要创建重复的：

```text
Token
Pool
Fee
GraphEdge
```

---

# 38. Error Design

外部数据和数学错误必须可处理。

至少覆盖：

```text
InvalidPath
SamePool
InvalidReserve
InvalidAmount
InvalidFee
Overflow
MissingPool
MissingState
TokenMismatch
NoLiquidity
```

具体错误枚举按照项目已有 error convention 调整。

禁止：

```rust
unwrap()
expect()
panic!()
```

处理来自：

```text
GraphSnapshot
PoolState
PoolMeta
```

的数据。

---

# 39. 性能

M3 不需要为了性能过早优化。

优先：

```text
correctness
determinism
auditability
```

然后再考虑：

```text
candidate enumeration
```

优化。

第一阶段允许：

```text
O(E²)
```

级别的 two-pool candidate enumeration。

只要真实数据规模下可接受。

不要为了避免 O(E²) 提前实现复杂索引系统。

---

# 40. Multi-hop 暂不作为 M3 核心

M3 第一阶段只要求：

```text
2-hop / 2-pool cycle
```

即：

```text
A → B → A
```

不要因为 Graph 已经支持多 hop，就立刻实现：

```text
A → B → C → A
A → B → C → D → A
```

多 hop 可以作为 M3 后续扩展，但不是 M3 COMPLETE 的必要条件。

如果实现 3-hop：

必须保证不会影响 two-pool 核心验收。

---

# 41. Gas / Execution Boundary

M3 的：

```text
gross_profit
```

是：

```text
token amount
```

不允许表达：

```text
net_profit_after_gas
```

因为 M3 没有：

```text
gas
```

也没有：

```text
execution simulation
```

所以不要写：

```text
profitable
```

作为：

> 实际可执行盈利。

应该明确：

> 理论 AMM 毛利润为正。

---

# 42. Token Decimal

M3 的数学计算必须使用：

```text
raw integer amount
```

即：

```text
U256
```

不要把：

```text
decimals
```

混入 AMM 原始数学。

例如：

```text
1 ETH
```

在数学层面应该是：

```text
1000000000000000000
```

而不是：

```text
1.0
```

展示层以后再转换。

---

# 43. Different Token Decimals

尤其注意：

```text
Token A decimals = 18
Token B decimals = 6
```

不能因为：

```text
A/B
```

直接比较：

```text
reserveA / reserveB
```

得到错误价格。

M3 的数学应该通过：

```text
actual amount
```

进行 swap。

如果需要计算人类可读价格：

必须明确 decimals adjustment。

但：

> Opportunity 的最终金额计算不能依赖浮点价格。

---

# 44. Real Price Difference

如果 Agent 为了候选筛选需要计算：

```text
price
```

允许使用：

```text
approximate numeric representation
```

但：

```text
candidate screening
```

只能用于性能优化。

最终是否盈利必须回到：

```text
U256 exact swap calculation
```

不能：

```text
price difference > threshold
→ directly call it opportunity
```

---

# 45. Test Matrix

至少覆盖：

```text
single-hop math
two-hop math
no arbitrage
profitable arbitrage
fee erases profit
zero reserve
zero input
overflow
same pool
token mismatch
multiple pools
optimal input
determinism
ordering
```

---

# 46. Mathematical Cross-check

必须增加一个独立验证路径。

不要：

```text
optimizer
```

调用：

```text
same optimizer
```

验证自己。

应该：

```text
optimizer
        ↓
result A

independent brute-force/local search
        ↓
result B

A == B
```

至少在小规模 fixture 上做到这一点。

---

# 47. Real-data Acceptance Test

必须新增至少一个真实数据 acceptance test。

要求：

```text
real block
 ↓
real GraphSnapshot
 ↓
real candidate
 ↓
real AMM math
```

不能只：

```text
fixture → opportunity
```

---

# 48. Real Opportunity Acceptance

优先目标：

```text
真实历史数据
 ↓
找到 gross_profit > 0
```

如果找到：

必须保存证据。

如果找不到：

不能修改数据。

必须报告：

```text
真实搜索范围
candidate count
evaluated count
best candidate
best gross profit
```

并明确：

```text
No profitable opportunity found in searched real market states
```

---

# 49. Evidence

真实结果必须能够追溯到：

```text
block
pool
token
reserve
fee
path
```

不要只输出：

```text
profit = 123
```

必须能够回答：

> 这个利润是从哪个真实市场状态算出来的？

---

# 50. Evidence 优先级

调查真实数据时继续遵循：

```text
Existing verified evidence
        ↓
Existing index
        ↓
Existing raw data
        ↓
RPC validation
        ↓
Refetch
```

不要首先重新抓取所有数据。

特别是：

```text
/Volumes/superfs/giwa-mev
```

如果仍然存在，Coding Agent 应自行读取需要的数据。

不要要求用户手工分析：

```text
thousands of .bin
```

---

# 51. 不得猜测协议

不要因为：

```text
V2-style AMM
```

就认为所有：

```text
Sync
Pool
Factory
```

都是标准 Uniswap V2。

M2 已经建立真实证据链。

M3 必须直接使用：

```text
M2 confirmed PoolMeta
PoolState
GraphEdge
Fee
```

---

# 52. Real Data Search Strategy

可以优先搜索：

```text
多个真实池子
+
相同 Token Pair
```

因为 two-pool arbitrage 最容易从：

```text
same token pair
```

产生。

如果当前 4 个池子不足以形成：

```text
same pair
```

则继续调查历史数据中是否存在其他已经能够被证据确认的池子。

但：

> 不得为了满足 M3 而降低 M2 的证据标准。

---

# 53. 真实数据不足怎么办

如果真实数据只有：

```text
Pool1 A/B
Pool2 C/D
```

无法形成套利。

不要制造：

```text
Pool2 A/B
```

正确做法：

```text
继续调查真实历史数据
```

或者：

```text
M3 数学部分 COMPLETE
Real Market Opportunity 搜索未发现可验证机会
```

最终报告必须如实说明。

---

# 54. CLI

如果当前 CLI 已经存在：

建议增加：

```bash
evm-mev opportunity ...
```

例如：

```bash
evm-mev opportunity \
  --chain <chain> \
  --block <block>
```

如果需要：

```bash
evm-mev opportunity \
  --chain <chain> \
  --from-block <N> \
  --to-block <M>
```

可以支持历史搜索。

但 CLI 不是 M3 的核心。

不要为了 CLI 重构 pipeline。

---

# 55. Output

CLI / debug output 至少能够显示：

```text
Chain
Block
Input Token
Input Amount
Output Amount
Gross Profit
```

Path：

```text
TokenA
  -> Pool1
  -> TokenB
  -> Pool2
  -> TokenA
```

并显示：

```text
Pool1 reserves
Pool2 reserves
Pool1 fee
Pool2 fee
```

方便审计。

---

# 56. Deterministic Serialization

如果 Opportunity / Path 需要序列化：

必须确保：

```text
same snapshot
same input
same code
```

得到相同 serialized representation。

如果新增 Serialize：

不要无意义扩大：

```text
Serialize/Deserialize
```

范围。

根据实际需求决定。

---

# 57. M3 不负责 Replay Framework

M1/M2 已经有 deterministic foundation。

M3 只需要：

```text
GraphSnapshot → Opportunity
```

可重放。

不要重新实现：

```text
Chain Replay
State Replay
```

---

# 58. M3 Completion Report

新增：

```text
docs/v0.1/M3 Completion Report.md
```

至少包含：

## 1. Status

```text
COMPLETE
```

或者明确：

```text
PARTIAL
```

不得为了版本完成强行写 COMPLETE。

## 2. Implemented Components

列出：

```text
Path
AMM Math
Optimizer
Detector
```

## 3. Mathematical Model

说明：

```text
formula
fee
integer arithmetic
overflow handling
```

## 4. Candidate Enumeration

说明：

```text
how paths are generated
how duplicates are removed
how same-pool paths are rejected
```

## 5. Optimal Input

说明：

```text
search algorithm
bounds
iterations
precision
```

## 6. Test Results

例如：

```text
cargo fmt --check
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
```

## 7. Mathematical Cross-check

说明：

```text
optimizer vs brute force
```

## 8. Real-data Evidence

必须说明：

```text
searched blocks
snapshots
pools
candidate paths
```

## 9. Real Opportunity

如果找到：

完整记录。

如果没找到：

明确：

```text
No profitable opportunity found
```

不要模糊表达。

## 10. Known Limitations

明确：

```text
No gas
No simulation
No execution
No multi-hop optimization
```

## 11. Scope Boundary

说明 M4 / 后续阶段。

---

# 59. Commit Discipline

继续保持 M1/M2 的方式：

代码：

```text
M3 code
```

一个独立 commit。

文档：

```text
M3 documentation
```

一个独立 commit。

提交前：

```text
git status
```

必须干净。

---

# 60. Validation Gates

完成前必须实跑：

```bash
cargo fmt --check

cargo check --workspace --all-targets

cargo test --workspace

cargo clippy --all-targets --all-features -- -D warnings
```

全部通过。

不得：

```text
只检查新 crate
```

必须检查：

```text
整个 workspace
```

---

# 61. 不要修改 M1/M2 的已验证事实

如果 M3 发现：

```text
M1/M2 报告存在事实错误
```

可以修正，但必须：

```text
明确记录
```

不要为了 M3 悄悄修改历史结论。

尤其不要修改：

```text
real evidence
```

来适配算法。

---

# 62. Code Quality

继续保持 M1/M2 已建立的约束：

非测试代码尽量避免：

```rust
unwrap()
expect()
panic!()
```

不要：

```text
hardcode chain id
hardcode pool address
hardcode token address
```

不要：

```text
f64/f32
```

进入最终金额计算。

不要：

```text
GIWA-specific logic
```

进入：

```text
opportunity
graph
math
```

核心逻辑必须保持 multi-EVM。

---

# 63. Chain Neutrality

M3 不允许出现：

```text
if chain_id == 91342
```

或者：

```text
if pool == 0x...
```

之类业务逻辑。

真实数据的特殊性应该存在于：

```text
evidence
fixture
config
data
```

而不是：

```text
core opportunity logic
```

---

# 64. No Hidden Constants

尤其检查：

```text
fee
input bound
profit threshold
search iterations
```

不能偷偷写死。

如果是：

```text
algorithmic constant
```

必须说明：

```text
why
```

如果是：

```text
business threshold
```

应该：

```text
configurable
```

---

# 65. Minimum M3 Acceptance Criteria

M3 只有全部满足以下条件才可以标记：

```text
COMPLETE
```

### A

能够从：

```text
GraphSnapshot
```

生成 two-pool candidate。

### B

能够正确拒绝：

```text
same pool twice
```

### C

能够正确处理：

```text
multiple pools same pair
```

### D

AMM swap 使用：

```text
U256
```

精确计算。

### E

Fee 正确参与计算。

### F

能够计算：

```text
two-hop output
gross profit
```

### G

能够寻找：

```text
optimal input
```

并有独立 brute-force/local-search 验证。

### H

No-arbitrage fixture：

```text
no opportunity
```

### I

Profitable fixture：

```text
gross_profit > 0
```

### J

Fee-erased fixture：

```text
no opportunity
```

### K

Zero reserve / invalid path：

```text
correctly rejected
```

### L

真实 GraphSnapshot 能进入：

```text
candidate detection
```

### M

真实历史数据完成搜索。

### N

如果找到真实机会：

```text
real opportunity reproducible
```

如果没有：

```text
真实搜索结果可审计
```

### O

结果 deterministic。

### P

workspace 全量：

```text
fmt PASS
check PASS
test PASS
clippy PASS
```

### Q

M3 Completion Report 完成。

---

# 66. 最终验收链路

M3 最终必须能够证明：

```text
Real Block
    ↓
Real Logs
    ↓
Real Pool State
    ↓
Real Market Graph
    ↓
Two-Pool Candidate
    ↓
Exact AMM Math
    ↓
Optimal Input
    ↓
Gross Profit
    ↓
Opportunity
```

这是 M3 真正的完成标准。

---

# 67. 最重要的工程原则

不要为了：

```text
M3 COMPLETE
```

人为制造：

```text
profitable opportunity
```

真正应该证明的是：

```text
系统能正确判断盈利和不盈利。
```

所以：

```text
Profitable fixture
```

证明：

> 数学实现可以发现机会。

而：

```text
No-arbitrage fixture
```

证明：

> 数学实现不会凭空制造机会。

最终：

```text
Real historical data
```

证明：

> 这套逻辑已经真正接到了现实市场状态。

---

# 68. M3 完成后的状态

如果全部通过：

```text
v0.1

M1 Real Pool State
        ↓
M2 Real Market Graph
        ↓
M3 Real Arbitrage Opportunity
```

系统第一次拥有：

```text
真实市场状态
+
真实市场图
+
真实套利计算
```

但此时仍然：

```text
NO EXECUTION
```

下一阶段才进入：

```text
Simulation
Risk
Execution
```

不要提前实现。

---

# 69. 给 Coding Agent 的最后要求

你不是只负责写代码。

你同时负责：

```text
Developer
+
Data Investigator
+
Mathematical Validator
+
Evidence Collector
```

所以：

1. 先读 M1/M2。
2. 自己调查真实数据。
3. 不要要求用户提前告诉你套利池子。
4. 不要猜测真实协议。
5. 不要制造真实数据。
6. 不要用 fixture 冒充 real-data evidence。
7. 不要用浮点数作为最终金额计算。
8. 不要把 gross profit 描述成 executable profit。
9. 不要扩张到 simulation/execution。
10. 所有结论都必须能通过代码、fixture 或真实证据复现。

最终交付：

```text
Code commit
Documentation commit
Clean working tree
Full validation results
M3 Completion Report
```

如果真实市场数据没有发现机会，也可以完成 M3 的数学与检测能力，但报告必须如实写明：

```text
No profitable opportunity found in the searched real market states.
```

绝对不要为了让报告出现 `Opportunity` 而篡改数据或降低证据标准。
