# M11 — Multi-Hop Simulation / Execution Integration + Multi-Lane

## 1. Milestone 定义

### M11 的目标

建立完整的：

```text
CycleCandidate
      ↓
Multi-Hop Pricing
      ↓
Amount Optimization
      ↓
REVM Multi-Hop Simulation
      ↓
Risk Decision
      ↓
ExecutablePlan
      ↓
M10 Arbitrage Executor
      ↓
Multi-Lane Execution
      ↓
GIWA Receipt
      ↓
Realized Result
```

M11 完成后，系统第一次具备：

> **从 PathFinder 找到的多跳套利环，到经过精确金额优化、真实 EVM 模拟、风险判断，再生成 M10 可执行计划并进入真实执行生命周期的完整闭环。**

---

# 2. M11 的严格边界

M11 **必须实现**：

```text
M9.3 CycleCandidate
        ↓
MultiHopRoute
        ↓
MultiHopPricing
        ↓
AmountOptimizer
        ↓
SimulatedOpportunity
        ↓
Risk
        ↓
ExecutablePlan
        ↓
M10 Executor
        ↓
Multi-Lane
```

M11 **不得实现**：

```text
❌ V3
❌ Curve
❌ Balancer
❌ Flashloan
❌ Sandwich
❌ Cross-chain
❌ AI strategy
❌ LLM
❌ Private Sequencer
❌ Self-hosted GIWA node
❌ Flashblocks direct execution
❌ Bellman-Ford replacing PathFinder
❌ SPFA replacing PathFinder
❌ 新 Executor Contract
❌ 新 Signer framework
❌ 新 Receipt framework
```

尤其：

> **M11 不修改 M9.3 的 CycleCandidate 语义。**

---

# 3. 当前架构与目标架构

当前：

```text
GraphSnapshot
      ↓
PathFinder
      ↓
CycleCandidate

Opportunity
      ↓
旧 2-hop Optimizer

Simulation
      ↓
旧 2-hop Route

M10
      ↓
Executor
```

M11 后：

```text
                         GraphSnapshot
                               │
                               ↓
                         PathFinder
                               │
                               ↓
                       CycleCandidate
                               │
                               ↓
                       MultiHopRoute
                               │
                               ↓
                       MultiHopPricing
                               │
                               ↓
                      Amount Optimization
                               │
                               ↓
                    OptimizedCandidate
                               │
                               ↓
                         REVM Simulation
                               │
                               ↓
                    SimulatedOpportunity
                               │
                               ↓
                             Risk
                               │
                    ┌──────────┴──────────┐
                    │                     │
                  Reject                Accept
                                          │
                                          ↓
                                  ExecutablePlan
                                          │
                                          ↓
                              M10 ArbitrageExecutor
                                          │
                              ┌───────────┼───────────┐
                              ↓           ↓           ↓
                           Lane A      Lane B      Lane C
                              │           │           │
                              └───────────┼───────────┘
                                          ↓
                                       GIWA
                                          ↓
                                       Receipt
                                          ↓
                                   Realized Result
```

---

# 4. 第一原则：四种 Profit 必须严格分开

整个 M11 必须遵守：

```text
Estimated Opportunity
        ≠
Simulated Profit
        ≠
Executed Profit
        ≠
Realized Profit
```

具体：

### Estimated

来自：

```text
GraphSnapshot
+
MultiHopPricing
```

只是数学估计。

### Simulated

来自：

```text
REVM
+
Executor
+
canonical state
```

是真实 EVM 行为下的结果。

### Executed

来自：

```text
submitted / included transaction
```

描述实际交易。

### Realized

来自：

```text
receipt
+
balance delta
+
gas
+
fee
```

最终链上结果。

禁止任何层把另一个层的数据冒充自己的结果。

---

# 5. M11.1 — Architecture Integration

## 5.1 PathFinder 正式进入 workspace

当前：

```text
crates/pathfinder
```

存在，但 root workspace 没有正式纳入。

M11.1 必须确认并修复：

```toml
"crates/pathfinder"
```

进入 workspace。

要求：

```bash
cargo metadata
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

全部通过。

---

# 6. CycleCandidate：禁止修改

M9.3：

```text
CycleCandidate
```

继续只表达：

```text
chain_id
target_block
ordered edges
cycle identity
hop count
```

禁止加入：

```text
❌ amount
❌ profit
❌ gas
❌ simulation
❌ risk
❌ calldata
❌ execution
```

M11 使用 adapter：

```text
CycleCandidate
        ↓
MultiHopRoute
```

而不是修改 CycleCandidate。

---

# 7. 新增 MultiHopRoute

建议放在：

```text
crates/opportunity/
```

或者一个独立的 M11 domain module。

推荐：

```rust
pub struct MultiHopRoute {
    pub chain_id: ChainId,
    pub target_block: BlockNumber,
    pub edges: Vec<GraphEdge>,
}
```

但必须进一步保证：

```text
edges.len() >= 2
```

并验证：

```text
edge[i].token_out == edge[i + 1].token_in
```

以及：

```text
edge[last].token_out == edge[0].token_in
```

并：

```text
no repeated pool
```

同时保留 M9.3 的：

```text
cycle identity
```

但不要把 presentation order 当成 semantic identity。

---

# 8. MultiHopRoute 必须是 immutable

构造成功之后：

```text
route
```

不能被后续 optimizer 修改。

Optimizer 修改的只能是：

```text
input_amount
```

而不是：

```text
route
```

这保证：

```text
Route Identity
```

和：

```text
Amount Identity
```

完全分开。

---

# 9. M11.2 — Multi-Hop Pricing

这是 M11 的第一个真正新增核心能力。

每个 hop：

```text
amount_out =
amount_in * fee_numerator * reserve_out
/
(
    reserve_in * fee_denominator
    +
    amount_in * fee_numerator
)
```

具体 fee 必须使用：

```text
GraphEdge.fee
```

而不是：

```text
997 / 1000
```

硬编码。

如果：

```text
fee == None
```

则：

```text
PricingResult = Unknown / Incomplete
```

不得猜测。

---

# 10. Pricing 必须使用 U256

禁止：

```rust
f64
f32
```

进入：

```text
financial calculation
```

包括：

```text
amount
reserve
fee
output
profit
```

全部使用：

```text
U256
```

或者已经存在的精确整数类型。

---

# 11. MultiHopQuote

建议建立：

```rust
pub struct MultiHopQuote {
    pub input: U256,
    pub output: U256,
    pub hops: Vec<HopQuote>,
}
```

其中：

```rust
pub struct HopQuote {
    pub amount_in: U256,
    pub amount_out: U256,
}
```

这样 M11 可以明确记录：

```text
A
 ↓ 100
B
 ↓ 97
C
 ↓ 95
A
```

而不是只保存最终结果。

---

# 12. Pricing 必须支持 deterministic replay

相同：

```text
route
+
GraphSnapshot
+
input_amount
```

必须产生完全一致：

```text
output
hop outputs
profit
```

不能依赖：

```text
RPC
clock
random
floating point
```

---

# 13. M11.3 — Amount Optimization

这是本次 M11 最容易做错的地方。

## 不允许直接复用 M3 ternary search

因为 M3 的理论基础是：

```text
two-hop fractional-linear
```

M11 是：

```text
multi-hop composition
+
integer rounding
```

不能直接宣称：

```text
global optimum
```

---

# 14. 第一版 optimizer

建议：

```text
BoundedDiscreteOptimizer
```

输入：

```text
route
search_min
search_max
```

输出：

```rust
pub struct OptimizationResult {
    pub best_input: U256,
    pub best_output: U256,
    pub best_profit: U256,
    pub evaluations: u64,
    pub domain_min: U256,
    pub domain_max: U256,
}
```

必须记录：

```text
search domain
evaluation count
best candidate
termination reason
```

---

# 15. Optimizer 第一版采用 bounded search

优先：

```text
coarse sampling
+
local refinement
```

或者：

```text
bounded discrete search
```

但必须：

> **保证候选空间有限。**

绝对禁止：

```text
while profit improves
```

这种无界循环。

---

# 16. 小范围必须拥有 brute-force oracle

这是 M11 的重要正确性测试。

例如：

```text
input ∈ [1, 1000]
```

对每一个输入：

```text
quote(input)
```

找到：

```text
oracle_best
```

再比较：

```text
optimizer_best == oracle_best
```

要求：

```text
best_input
best_output
best_profit
```

全部一致。

这证明 optimizer 没有漏掉小域中的真实最优点。

---

# 17. Optimizer 禁止修改 route

优化过程：

```text
route
   ↓
quote(x)
   ↓
profit(x)
```

route 始终 immutable。

---

# 18. 优化结果不是 Opportunity

不要叫：

```text
ArbitrageOpportunity
```

因为还没有经过 simulation。

建议：

```text
OptimizedCandidate
```

表达：

```text
CycleCandidate
+
optimized amount
+
analytical quote
```

---

# 19. M11.4 — Multi-Hop REVM Simulation

这里必须最大化复用 M10。

不要重新写：

```text
MultiHopEvmSimulator
```

而是：

```text
OptimizedCandidate
        ↓
ArbitrageExecutionPlan
        ↓
M10 ExecutorCall
        ↓
existing ExecutorRun
        ↓
REVM
```

---

# 20. MultiHop ExecutorCall

M11 要负责把：

```text
MultiHopRoute
+
optimized amount
+
min outputs
+
final guard
```

变成 M10 所理解的：

```text
legs[]
```

例如：

```text
A → B
B → C
C → A
```

对应：

```text
Leg 0
tokenIn  = A
tokenOut = B

Leg 1
tokenIn  = B
tokenOut = C

Leg 2
tokenIn  = C
tokenOut = A
```

---

# 21. 每一腿 amount 必须来自前一腿

例如：

```text
amount0 = optimized_input
```

然后：

```text
amount1 = quote0.amount_out
amount2 = quote1.amount_out
```

不能出现：

```text
leg2.amount_in
```

独立于：

```text
leg1.amount_out
```

否则 plan 与实际 calldata 会不一致。

---

# 22. REVM 必须验证真实 Executor

M11 的 simulation 必须：

```text
real Executor bytecode
+
real Pair bytecode
+
real ERC20 bytecode
```

不能使用：

```text
mock executor
```

作为最终 acceptance。

---

# 23. Simulation 必须 pinned

必须明确：

```text
chain_id
block_number
block_hash
```

至少：

```text
block_number
```

必须成为 simulation identity 的一部分。

如果 RPC provider 支持：

```text
eth_call(..., block_number)
```

必须使用 pinned block。

---

# 24. Simulation Result 新模型

建议：

```rust
pub struct SimulatedOpportunity {
    pub candidate: OptimizedCandidate,
    pub simulation: SimulationResult,
    pub input_amount: U256,
    pub final_amount: U256,
    pub gross_profit: U256,
    pub gas_used: U256,
    pub simulation_block: BlockIdentity,
}
```

但必须注意：

```text
gross_profit
```

只能在：

```text
same denomination
```

下成立。

如果 gas 是 native token，而 input 是 ERC20：

```text
do not subtract
```

---

# 25. Simulation 失败必须成为明确状态

不能：

```text
profit = 0
```

代表 simulation failed。

必须：

```text
SimulationStatus::Reverted
```

或者：

```text
SimulationStatus::Failed
```

并保留：

```text
revert reason
```

---

# 26. M11.4 — Risk Integration

Risk 输入：

```text
SimulatedOpportunity
```

输出：

```text
RiskDecision
```

例如：

```text
Accept
Reject
```

Reject 必须有：

```text
RiskRejectReason
```

---

# 27. Risk 第一版至少检查

```text
simulation success
input > 0
output > input
min profit
max gas
simulation freshness
state freshness
route validity
executor validity
```

如果：

```text
simulation_block
```

过旧：

```text
Reject
```

---

# 28. Risk 不负责发送交易

严格禁止：

```text
Risk → Signer
Risk → RPC send
Risk → Submitter
```

必须：

```text
Risk
 ↓
ExecutablePlan
 ↓
Execution
```

---

# 29. ExecutablePlan 必须复用 M10

M11 不新增：

```text
FinalExecutionPlan
```

直接生成：

```text
ArbitrageExecutionPlan
        ↓
ExecutablePlan
```

使用 M10 已经定义好的：

```text
chain_id
executor
sender
recipient
input_token
input_amount
legs
min_final_output
validity
simulation
profit_policy
```

---

# 30. Simulation → Execution 必须 hash-bind

ExecutablePlan 必须能够证明：

```text
simulation calldata hash
==
execution calldata hash
```

并且：

```text
route identity
==
simulation route identity
==
execution route identity
```

否则拒绝执行。

---

# 31. M11.5 — Multi-Lane

这是新的基础设施。

建议：

```text
ExecutionLane
```

包含：

```text
lane_id
candidate_id
plan_hash
simulation_id
state
nonce_reservation
capital_reservation
```

---

# 32. Lane 状态机

建议：

```text
Created
   ↓
Simulating
   ↓
RiskChecking
   ↓
Ready
   ↓
Reserved
   ↓
Submitting
   ↓
Submitted
   ↓
Included
   ↓
Settled
```

失败：

```text
Rejected
Cancelled
Expired
Failed
```

---

# 33. Multi-Lane 的关键原则

允许：

```text
A simulation
B simulation
C simulation
```

并行。

允许：

```text
A risk
B risk
C risk
```

并行。

但：

```text
same signer
+
same nonce
```

不能由多个 lane 同时拥有。

---

# 34. Nonce Reservation

需要：

```text
NonceManager
```

至少支持：

```text
reserve()
commit()
release()
```

生命周期：

```text
reserve
   ↓
sign
   ↓
submit
   ↓
receipt
   ↓
commit
```

如果：

```text
simulation fails
risk rejects
plan expires
```

必须：

```text
release
```

---

# 35. Capital Reservation

M11 第一版不做完整 capital management。

但是必须防止：

```text
Lane A
input = 100

Lane B
input = 100

wallet balance = 100
```

两个 lane 都认为自己能执行。

所以至少要有：

```text
available_capital
reserved_capital
```

的概念。

第一版可以：

```text
one capital domain
```

而不做复杂 portfolio management。

---

# 36. Multi-Lane 第一版禁止“自动抢跑”

不要：

```text
Lane A profit 100
Lane B profit 90
Lane C profit 80

全部 broadcast
```

正确：

```text
Candidate ranking
       ↓
winner selection
       ↓
reserve nonce/capital
       ↓
submit winner
```

---

# 37. M11.6 — End-to-End

必须至少拥有：

## Controlled 2-hop

```text
A → B → A
```

验证：

```text
pricing
optimizer
simulation
risk
ExecutablePlan
M10
receipt
balance
```

---

## Controlled 3-hop

```text
A → B → C → A
```

这是 M11 最重要的新增 fixture。

必须完整走：

```text
CycleCandidate
→
Pricing
→
Optimizer
→
REVM
→
Risk
→
Executor
```

---

# 38. 3-hop 必须真实进入 M10 Executor

不能：

```text
3-hop pricing
3-hop simulation

但是执行时：
2-hop
```

必须：

```text
Leg A
Leg B
Leg C
```

真实 calldata。

---

# 39. 3-hop Atomic Rollback

必须有：

```text
leg A succeeds
leg B succeeds
leg C fails
```

然后验证：

```text
Executor balances unchanged
Token balances unchanged
Pair reserves unchanged
```

这是 M10 atomicity 在多跳场景下的第一次真正扩展。

---

# 40. Real GIWA

如果市场条件允许：

```text
M9.3 real CycleCandidate
        ↓
M11 optimizer
        ↓
M11 simulation
        ↓
M11 risk
        ↓
M10 executor
        ↓
GIWA
```

必须记录：

```text
chain_id
block
route_id
candidate_id
input
output
simulation result
tx hash
receipt
gas
fee
balance delta
```

---

# 41. 如果没有真实盈利机会

必须：

```text
REAL_PROFITABLE_ARBITRAGE = UNKNOWN
```

而不是：

```text
0
```

更不能：

```text
controlled fixture
=
real arbitrage
```

必须严格区分：

```text
CONTROLLED_FIXTURE
REAL_MARKET
```

---

# 42. Evidence 目录

建议：

```text
data/evidence/m11/
```

结构：

```text
m11/
├── pricing/
├── optimizer/
├── simulation/
├── risk/
├── lanes/
├── controlled/
│   ├── 2hop/
│   └── 3hop/
├── real/
│   ├── execution.json
│   ├── failure.json
│   └── reconciliation.json
├── manifest.json
└── README.md
```

---

# 43. Evidence 必须能够独立重算

至少：

```text
pricing
optimizer
simulation summary
route identity
plan hash
calldata hash
```

都应该能独立验证。

不能只：

```text
println!("PASS")
```

---

# 44. 新增 RPC 必须单独统计

M11 的 instrumentation：

```text
不得增加 hot-path RPC
```

尤其：

```text
metrics
logging
evidence
debug
```

不能偷偷增加：

```text
eth_call
eth_getBalance
eth_getCode
eth_getLogs
```

如果需要 RPC：

```text
必须明确属于 simulation 本身
```

并记录：

```text
rpc_count
```

---

# 45. 测试矩阵

M11 至少需要：

### Pricing

```text
2-hop
3-hop
4-hop
zero input
zero reserve
missing fee
broken route
integer rounding
U256 overflow boundary
```

### Optimizer

```text
small brute-force oracle
boundary optimum
interior optimum
all-negative
zero-profit
large U256
determinism
```

### Simulation

```text
2-hop
3-hop
4-hop
revert
stale block
wrong chain
wrong executor
min output
final guard
```

### Risk

```text
profitable
unprofitable
gas too high
simulation reverted
stale
invalid route
```

### Lane

```text
parallel simulation
nonce conflict
capital conflict
release
commit
expiration
duplicate plan
winner selection
```

---

# 46. Critical Negative Controls

M11 必须至少包含：

```text
NC1 wrong chain
NC2 stale simulation
NC3 broken route
NC4 missing fee
NC5 insufficient liquidity
NC6 optimizer boundary violation
NC7 simulation revert
NC8 min output failure
NC9 final profit failure
NC10 calldata mismatch
NC11 plan hash mismatch
NC12 nonce collision
NC13 capital collision
NC14 duplicate lane
NC15 expired plan
```

---

# 47. Determinism

相同：

```text
GraphSnapshot
CycleCandidate
```

必须得到：

```text
same route
same quote
same optimizer result
same plan hash
same calldata hash
```

至少连续两次运行。

---

# 48. 不能用 `f64` 绕过 U256

最终检查：

```bash
rg "f64|f32" crates/opportunity crates/simulation crates/risk crates/execution
```

发现任何 financial-core 使用，都必须解释。

---

# 49. Panic / unwrap

继续执行已有安全门：

```text
production panic hard = 0
```

新的：

```text
unwrap()
expect()
unreachable!()
panic!()
```

必须全部审计。

---

# 50. Secret Scan

继续：

```text
private key literal = 0
```

同时保留 positive control。

---

# 51. Cargo Gate

严格：

```text
cargo fmt --check
```

然后：

```text
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

然后：

```text
cargo test --workspace
```

并且：

> **所有 Cargo gates serial execution。**

继续禁止多个共享：

```text
target/pipeline-tests/
```

的测试并发执行。

---

# 52. M11 Commit Strategy

建议仍然采用四阶段：

### Commit 1

```text
feat(m11): add multi-hop pricing and optimization
```

包含：

```text
workspace/pathfinder
MultiHopRoute
MultiHopPricing
MultiHopQuote
MultiHopOptimizer
```

---

### Commit 2

```text
test(m11): add multi-hop simulation and execution integration
```

包含：

```text
REVM
3-hop
Risk
ExecutablePlan
Lane
Nonce
Capital
```

---

### Commit 3

```text
evidence(m11): add multi-hop execution evidence
```

包含：

```text
controlled
real
reconciliation
manifest
```

---

### Commit 4

```text
docs(m11): add multi-hop execution completion report
```

包含：

```text
M11 Completion Report
semantic audit
architecture
acceptance matrix
```

---

# 53. M11 Completion Criteria

只有全部满足：

```text
M11.1 Architecture Integration       ✅
M11.2 Multi-Hop Pricing              ✅
M11.3 Amount Optimization            ✅
M11.4 REVM + Risk                    ✅
M11.5 Multi-Lane                     ✅
M11.6 End-to-End                     ✅
```

才能：

```text
M11 = COMPLETE
```

---

# 54. 但有一个非常重要的例外

即使：

```text
REAL_PROFITABLE_ARBITRAGE
```

没有出现，也可以：

```text
M11 = COMPLETE
```

前提是：

```text
controlled 2-hop = PASS
controlled 3-hop = PASS
REVM = PASS
Risk = PASS
Executor = PASS
Lane = PASS
real attempt = documented UNKNOWN
```

这和 M10 的验收哲学保持一致。

---

# 55. M11 完成后系统将真正变成什么

到 M10 为止：

```text
M9.3
找到了套利环

M10
证明了一条执行路线可以原子执行
```

M11 完成后：

```text
M9.3
      ↓
“发现一个环”
      ↓
M11
      ↓
“计算应该投入多少钱”
      ↓
“用真实 EVM 模拟”
      ↓
“判断是否值得执行”
      ↓
“形成 ExecutablePlan”
      ↓
M10
      ↓
“原子执行”
      ↓
“确认链上结果”
```

这时候我们的系统才第一次真正具备：

> **Arbitrage Decision Loop**

---

# 56. M11 之后

完成 M11 后，下一阶段就非常清晰了：

```text
M9    Market Discovery / Graph / PathFinder
M10   Atomic Executor
M11   Strategy Decision + Multi-Hop Execution
M12   Self-hosted GIWA Node + Low-Latency Infrastructure
M13   Production Operations
M14   Production Validation
```

也就是说：

**M11 是策略层闭环。**

**M12 才是我们真正开始解决 GIWA MEV 延迟的问题。**

这点非常重要——在 M11 完成以前，不建议再花时间去优化：

```text
RPC latency
Rust microseconds
Flashblocks transport
sequencer connection
```

因为如果：

```text
发现 → 定价 → 优化 → 模拟 → 风险 → 执行
```

还没有形成完整闭环，那么继续压 10ms、50ms 没有意义。

---

## 最终冻结的 M11 目标

```text id="g6bd2h"
                 M9.3
            CycleCandidate
                   │
                   ↓
            MultiHopRoute
                   │
                   ↓
          Exact Multi-Hop Quote
                   │
                   ↓
          Bounded Optimization
                   │
                   ↓
          OptimizedCandidate
                   │
                   ↓
            Real REVM State
                   │
                   ↓
        SimulatedOpportunity
                   │
                   ↓
                 Risk
                   │
             ┌─────┴─────┐
             │           │
           Reject       Accept
                           │
                           ↓
                    ExecutablePlan
                           │
                           ↓
                    M10 Executor
                           │
                           ↓
                    Multi-Lane
                           │
                           ↓
                         GIWA
                           │
                           ↓
                       Receipt
                           │
                           ↓
                  Realized Result
```

**这就是 M11 的完整任务边界。**