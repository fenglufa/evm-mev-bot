# M10 — Arbitrage Executor Contract

## 0. Milestone Identity

**Milestone:** M10
**Name:** Arbitrage Executor Contract
**Phase:** Phase 4 — Strategy Expansion
**Predecessors:** M9.1, M9.2, M9.3, M9.4
**Current baseline:** `66b421d`
**Repository:** `https://github.com/fenglufa/evm-mev-bot`

---

# 1. Mission

M10 的目标不是继续发现套利，也不是实现多跳优化。

M10 的唯一核心目标是：

> 建立一个最小、可验证、原子性的 Arbitrage Executor Contract，并将它安全地接入现有 `evm-execution` 生命周期，使一个已经确定的 `ExecutablePlan` 能够通过单笔链上交易执行多个 V2-style swap；任意步骤失败时整笔交易 revert；成功时最终输出必须满足预定义的 profit/min-output invariant。

M10 完成后，系统第一次具备：

```text
ExecutablePlan
      ↓
Arbitrage Executor Contract
      ↓
Swap A
      ↓
Swap B
      ↓
Final Profit Check
      ↓
Atomic Success
```

以及：

```text
任意步骤失败
      ↓
REVERT
      ↓
整个套利交易状态回滚
```

最终需要用：

1. REVM / simulation
2. 本地 deterministic fixture
3. GIWA Testnet controlled execution

共同证明这一点。

---

# 2. NON-GOALS

M10 明确禁止：

* V3
* Curve
* Balancer
* StableSwap
* Liquidation
* Sandwich
* Cross-chain arbitrage
* Flashloan
* Flashswap
* Private Sequencer
* Bundle
* Multi-relay
* Multi-chain
* AI / LLM
* PathFinder
* Bellman-Ford
* SPFA
* Amount Optimizer
* Incremental PathFinder
* Early Radar → direct execution
* Pending state → execution
* Prediction
* mempool strategy
* strategy discovery

尤其禁止：

```text
Executor Contract → 自己寻找套利
Executor Contract → 自己寻找路径
Executor Contract → 自己计算最优金额
Executor Contract → 自己读取外部市场状态
```

Contract 是：

> **Atomic Execution Primitive**

不是 Strategy Engine。

---

# 3. ARCHITECTURAL BOUNDARY

M10 必须严格遵守：

```text
M9.3
GraphSnapshot
    ↓
CycleCandidate

M11
CycleCandidate
    ↓
Pricing
    ↓
Optimization
    ↓
Simulation
    ↓
Risk
    ↓
ExecutablePlan

M10
ExecutablePlan
    ↓
ExecutionIntent
    ↓
TransactionBuilder
    ↓
Signer
    ↓
Executor Contract
    ↓
Atomic Execution
    ↓
Receipt
```

M10 不得反向依赖：

```text
PathFinder
Graph
Discovery
Early Radar
Pool Registry
```

M10 可以依赖现有：

```text
evm-execution
evm-simulation
evm-risk
evm-chain
evm-protocol
```

但必须先审计现有 dependency graph，避免为了一个接口引入循环依赖。

---

# 4. FIRST ACTION — REPOSITORY AUDIT

开始编码之前，必须先审计当前仓库。

至少检查：

```text
crates/execution/
crates/simulation/
crates/risk/
crates/protocol/
crates/chain/
crates/live/
crates/pathfinder/
crates/state/
crates/graph/
```

以及：

```text
Cargo.toml
Cargo.lock
README
docs/
scripts/
contracts/
deploy/
foundry.toml
hardhat.config.*
```

必须明确回答：

1. 当前是否已有 Solidity contract？
2. 当前是否已有 Solidity 编译工具链？
3. 当前是否已有部署脚本？
4. 当前是否已有 ABI 生成流程？
5. 当前是否已有 Executor/Router/Pairs fixture？
6. 当前 `evm-execution` 的 transaction builder 如何构造 `to/data/value`？
7. 当前 REVM simulation 如何注入 sender balance？
8. 当前 execution 的 receipt / profit / sequence 如何记录？
9. 当前 GIWA 的 gas / L1 fee 处理如何接入？
10. 是否可以复用已有测试 fixture？

不要因为 M10 需要 Solidity 就自动引入 Foundry、Hardhat 或新的框架。

优先：

> 复用已有工具链。

只有确认不存在合约工具链后，才选择最小、可维护的方案。

---

# 5. CORE DATA MODEL

M10 必须引入明确的执行计划类型。

推荐：

```rust
ArbitrageExecutionPlan
```

或者项目现有命名体系下的等价名称。

它至少需要表达：

```text
chain_id
executor
sender
input_token
input_amount
legs[]
min_final_output
validity
simulation_context
profit_policy
```

其中每个 swap leg 至少需要：

```text
pool
token_in
token_out
amount_in / amount derivation
min_amount_out
```

但是：

> 不要在 M10 中设计 M11 的完整 optimizer 数据结构。

M10 只需要一个能够明确描述“已经确定的执行计划”的最小模型。

---

# 6. PLAN IMMUTABILITY

一旦进入执行阶段：

```text
ExecutablePlan
```

必须是 immutable / deterministic。

M10 不允许在：

```text
Signer
Builder
Contract
Submitter
```

阶段重新修改：

```text
route
input_amount
min_output
pool
token
```

如果计划发生变化：

```text
old plan
   ↓
INVALID

new plan
   ↓
new execution
```

不能偷偷 mutate。

---

# 7. CHAIN ID BINDING

所有执行计划必须绑定：

```text
chain_id
```

Contract execution / transaction construction 必须拒绝：

```text
plan.chain_id != runtime.chain_id
```

不能仅依赖：

```text
RPC endpoint
```

来判断 chain identity。

---

# 8. BLOCK / FRESHNESS BINDING

M10 不应该把：

```text
target_block
```

当作“交易一定在该 block 执行”的保证。

但它必须保留：

```text
simulation_block
```

或等价 execution context，用于证明：

> 这个 ExecutablePlan 是基于哪个 canonical state 模拟得到的。

交易提交前必须由现有 execution freshness / gate 体系决定：

```text
fresh
stale
reject
```

不要重新发明一套 freshness 规则。

---

# 9. EXECUTOR CONTRACT

实现最小 Arbitrage Executor Contract。

核心目标：

```text
execute(...)
```

接受一个已经确定的 route，并依次执行：

```text
swap A
swap B
[swap C ...]
```

但 M10 第一版至少需要证明：

```text
2-hop V2 arbitrage
```

3-hop 可以作为 contract capability 设计，但不是 M10 必须的真实交易验收项，除非现有 fixture 自然支持。

---

# 10. V2 EXECUTION SEMANTICS

M10 只支持：

```text
Uniswap V2-style Pair
```

不要依赖 universal router。

对于每一个 pair：

```text
tokenIn
tokenOut
pair
amountIn
amountOut
```

执行逻辑必须保证：

```text
tokenIn → Pair
Pair swap → tokenOut
```

以及下一腿：

```text
tokenOut → next Pair
```

---

# 11. DO NOT TRUST RETURN VALUES ALONE

Contract 不应该仅仅因为：

```text
pair.swap(...)
```

调用没有 revert，就认为套利成功。

必须检查最终资产状态。

核心 invariant：

```text
final_balance >= min_final_output
```

如果不满足：

```text
revert
```

---

# 12. PROFIT INVARIANT

M10 不负责发现 profit。

M10 只负责执行一个已经定义好的：

```text
min_final_output
```

或者等价 profit invariant。

最基本模型：

```text
final_balance >= required_final_balance
```

如果是同币种 round-trip：

```text
final_input_asset_balance_after
    >=
minimum_required_final_balance
```

如果：

```text
final < minimum
```

必须：

```text
REVERT
```

不能：

```text
emit failure
return false
```

然后让外部程序继续认为交易成功。

---

# 13. ATOMICITY

必须证明：

```text
swap A succeeds
swap B fails
```

最终链上状态：

```text
swap A changes
   ↓
ROLLBACK
```

不能留下：

```text
partial token transfer
partial swap
partial approval
partial profit
```

测试必须检查：

* token balance
* pair reserve
* executor balance
* sender balance

确认整个交易状态恢复。

---

# 14. SLIPPAGE INVARIANT

每一个 swap leg 都应该有最小输出保护。

例如：

```text
amountOut >= minAmountOut
```

否则：

```text
revert
```

不要接受：

```text
actual output < min output
```

然后只在 off-chain 记录失败。

---

# 15. ROUTE VALIDATION

Executor contract 必须拒绝明显非法 route。

至少包括：

```text
tokenIn == tokenOut
zero address
zero amount
zero pair
same pair where prohibited
broken token continuity
```

例如：

```text
A → B
B → C
```

合法。

但：

```text
A → B
C → D
```

必须拒绝。

---

# 16. TOKEN CONTINUITY

多跳 route 必须满足：

```text
leg[i].token_out
    ==
leg[i+1].token_in
```

否则：

```text
revert
```

不能依赖 off-chain builder 永远正确。

Contract 是最后一道防线。

---

# 17. FINAL ASSET INVARIANT

对于 round-trip arbitrage：

```text
input token
      ↓
A
      ↓
B
      ↓
input token
```

最终资产必须回到：

```text
input token
```

并且：

```text
final >= min_final
```

M10 不支持：

```text
A → B → C
```

最后停在 C 的“套利完成”。

---

# 18. APPROVAL / TOKEN TRANSFER MODEL

必须先审计现有 GIWA 测试 token 和 pool。

不要假设：

```text
ERC20
```

一定完全标准。

需要确认：

* `approve`
* `transfer`
* `transferFrom`
* return value
* decimals
* fee-on-transfer
* non-standard behavior

M10 第一版：

> 不支持 fee-on-transfer token。

如果发现现有测试 token / pool 存在 transfer tax：

```text
unsupported
```

必须显式拒绝，而不是静默计算错误。

---

# 19. REENTRANCY / CALLBACK

第一版 Executor 不引入 flashloan，因此不需要设计复杂 callback protocol。

但 contract 仍应具备基本的：

```text
nonReentrant
```

或者等价安全边界。

尤其：

```text
external execute()
```

执行过程中不能被任意 token/pair callback 重入。

如果现有依赖不适合引入 OpenZeppelin：

> 实现最小、明确的 reentrancy guard。

不要为了一个 modifier 引入整个大型依赖树。

---

# 20. ACCESS CONTROL

Executor 不应该允许任意地址提交任意 route。

至少需要：

```text
authorized operator
```

或者：

```text
owner/operator
```

但必须保持简单。

推荐：

```text
operator
```

只有 operator 可以：

```text
execute()
```

非 operator：

```text
revert
```

M10 不做复杂 role system。

---

# 21. EXECUTOR BALANCE SAFETY

必须避免 contract 成为永久资金黑洞。

至少明确：

```text
input funds source
final recipient
```

正常成功：

```text
profit / remaining funds
→ recipient
```

失败：

```text
revert
```

不能出现：

```text
成功交易后资金永久留在 executor
```

除非这是明确设计且有独立提款机制。

如果实现 rescue/withdraw：

* 必须限制 operator/owner
* 必须有独立测试
* 不得影响正常套利执行
* 不得成为 M10 的核心功能扩张

---

# 22. CONTRACT SHOULD NOT DISCOVER STATE

Contract 不应该：

```text
查询池列表
查询套利路径
寻找最优池
计算最佳 amount
```

它只执行：

```text
given route
```

---

# 23. OFF-CHAIN BUILDER

M10 Rust side 增加：

```text
ArbitrageExecutionPlan
→ calldata
```

必须 deterministic。

相同：

```text
plan
+
executor address
```

必须产生相同：

```text
calldata
```

测试必须验证 byte-for-byte equality。

---

# 24. EXISTING EXECUTION INTEGRATION

必须复用现有：

```text
TransactionIntent
TransactionBuilder
Signer
TransactionSubmitter
ReceiptTracker
ExecutionRecord
ExecutionStage
```

不得另造：

```text
ArbitrageSigner
ArbitrageSubmitter
ArbitrageReceipt
```

除非当前接口确实无法表达套利执行，并且必须提供架构理由。

目标：

```text
ExecutablePlan
    ↓
existing Execution lifecycle
```

---

# 25. SIMULATION INTEGRATION

M10 必须让 REVM 能模拟：

```text
EOA
 ↓
Executor
 ↓
Pair A
 ↓
Pair B
 ↓
EOA / recipient
```

Simulation 必须使用：

```text
real deployed bytecode
real pair bytecode
real token bytecode
real canonical state
```

不能只模拟 Rust-level formulas。

当前 simulation crate 已明确把 REVM 作为“真实 bytecode + 真实 block state”的最终执行判断层，因此 M10 应复用这个边界，而不是重新实现第二套 EVM 模拟器。

---

# 26. SIMULATION NEGATIVE CASES

至少实现：

### NC1 — wrong chain

```text
plan.chain_id != chain_id
```

→ reject

### NC2 — stale plan

计划 freshness 不满足现有 gate

→ reject

### NC3 — unauthorized operator

→ contract revert

### NC4 — broken token continuity

→ contract revert

### NC5 — wrong token

→ contract revert

### NC6 — insufficient input balance

→ revert

### NC7 — insufficient allowance

→ revert

### NC8 — min output violated

→ revert

### NC9 — final profit invariant violated

→ revert

### NC10 — pair swap revert

→ entire transaction revert

### NC11 — malformed calldata

→ reject/revert

### NC12 — zero amount

→ reject/revert

---

# 27. MOST IMPORTANT TEST — PARTIAL EXECUTION ROLLBACK

必须有一个确定性 fixture：

```text
Executor
  ↓
Pair A
  ↓
successful swap
  ↓
Pair B
  ↓
forced failure
```

然后验证：

```text
Pair A reserves
before == after

Token balances
before == after

Executor balances
before == after
```

这条是 M10 的核心验收之一。

不能只验证：

```text
tx reverted
```

因为：

> revert 本身不是充分证据。

必须证明状态确实没有残留。

---

# 28. SUCCESS E2E FIXTURE

建立 deterministic fixture：

```text
Token A
Token B

Pair A/B #1
Pair A/B #2
```

使：

```text
A
 ↓
B
 ↓
A
```

存在确定性的价格差。

然后：

```text
input = X
```

执行：

```text
swap #1
swap #2
```

验证：

```text
final A > initial A
```

并且：

```text
final A >= min_final
```

---

# 29. DO NOT FABRICATE PROFIT

测试 fixture 中可以人为构造 profitable state。

但 evidence 必须明确标记：

```text
CONTROLLED_FIXTURE
```

不能写成：

```text
REAL_MARKET_OPPORTUNITY
```

真实 GIWA testnet 没有发现盈利套利时：

```text
UNKNOWN
```

或：

```text
NO_REAL_OPPORTUNITY_OBSERVED
```

不能伪造。

---

# 30. REAL GIWA EXECUTION

M10 必须尝试真实 GIWA Testnet controlled execution。

但真实交易必须满足：

```text
controlled
bounded
reversible-risk-aware
```

优先使用：

```text
self-funded
small amount
known test token
known test pair
```

如果当前 GIWA 没有合适的双池价格差：

> 不允许为了“完成 M10”人为宣称真实套利成功。

可以完成：

```text
contract deployment
contract call
revert evidence
controlled non-profitable execution
```

但：

```text
REAL_PROFITABLE_ARBITRAGE
```

只能在真实 receipt + balance delta 证明后才能标记。

---

# 31. REAL EXECUTION EVIDENCE

真实执行至少记录：

```text
chain_id
executor_address
operator_address
tx_hash
block_number
receipt_status
gas_used
effective_gas_price
input_asset
input_amount
output_asset
output_amount
before_balance
after_balance
balance_delta
```

如果存在 L1 fee：

```text
l1_fee
```

必须记录。

最终：

```text
realized_profit
```

必须由实际余额/receipt evidence 推导。

不能使用 simulation result 代替。

---

# 32. SIMULATION VS REAL EXECUTION RECONCILIATION

如果有成功真实执行：

必须比较：

```text
simulation
vs
receipt
vs
actual balance delta
```

至少包括：

```text
route
input
output
gas
status
profit
```

如果不同：

必须解释：

```text
state drift
gas pricing
L1 fee
rounding
external state change
```

不能为了让数字相同而修改 evidence。

---

# 33. GAS / FEE

复用现有 `evm-execution` fee infrastructure。

不得在 M10 创建第二套：

```text
gas oracle
fee oracle
L1 fee calculator
```

GIWA 特有逻辑继续留在：

```text
execution::giwa
```

chain-agnostic executor logic 不得硬编码 GIWA。

当前 execution crate 已明确把 GIWA-specific RPC knowledge 隔离在 `giwa`，而 builder/signer/intent/lifecycle 保持 chain-agnostic；M10 必须保持这个原则。

---

# 34. TRANSACTION VALUE

必须明确：

```text
msg.value
```

是否需要。

对于第一版 ERC20 V2 arbitrage：

```text
value = 0
```

如果 route 涉及 native asset：

不要直接扩大 M10 scope。

优先：

```text
ERC20 only
```

native wrapping 可以作为后续扩展。

---

# 35. SIGNING

继续复用现有 signer。

M10 不得：

```text
log private key
persist private key
embed private key
```

测试中如果需要 private key：

必须：

```text
test-only deterministic key
```

并使用项目现有 secret scan 机制。

---

# 36. CONTRACT ADDRESS BINDING

Execution plan 必须明确：

```text
executor_address
```

不能：

```text
runtime default executor
```

默默替换。

如果：

```text
plan.executor != configured executor
```

必须：

```text
reject
```

避免签名和发送时发生目标漂移。

---

# 37. CALldata DETERMINISM

对于：

```text
same plan
```

必须：

```text
same calldata
```

必须测试：

```text
build(plan) == build(plan)
```

包括：

```text
selector
encoding
address order
amount order
min output
route order
```

---

# 38. ROUTE IDENTITY

执行记录必须保留 route identity。

推荐：

```text
route_id
```

由：

```text
chain_id
+
ordered pool addresses
+
ordered token transitions
```

确定性生成。

但：

> 不要重新定义 M9.3 CycleCandidate identity。

M10 的 `route_id` 是执行层 identity，不得替代 M9.3 candidate identity。

---

# 39. EXECUTION RECORD

现有：

```text
ExecutionRecord
```

继续作为最终生命周期记录。

至少能够关联：

```text
candidate/correlation id
plan id
execution id
tx hash
receipt
```

但不要把 M9.3 candidate 直接塞进 execution crate dependency。

优先使用：

```text
opaque IDs
```

而不是跨 crate 强类型依赖。

---

# 40. ERROR SEMANTICS

必须区分：

```text
PlanRejected
ContractReverted
SubmissionFailed
ReceiptTimeout
IncludedReverted
IncludedSucceeded
```

不要把所有情况变成：

```text
ExecutionError::Failed
```

尤其：

```text
submitted
!=
included
```

```text
included
!=
successful
```

```text
successful
!=
profitable
```

继续保持项目现有语义。

---

# 41. NO EARLY RADAR DEPENDENCY

M10 不得 import：

```text
evm_live::preconf*
```

不得：

```text
RadarEvent → execute
```

不得：

```text
pending → Executor
```

正确路径仍然是：

```text
Early Radar
 ↓
Canonical Verification
 ↓
Canonical State
 ↓
PathFinder
 ↓
M11
 ↓
ExecutablePlan
 ↓
M10
```

---

# 42. CONTRACT SECURITY CHECKLIST

至少检查：

* reentrancy
* access control
* arbitrary call
* arbitrary target
* arbitrary token
* arbitrary recipient
* zero amount
* malformed route
* token continuity
* output invariant
* final profit invariant
* allowance abuse
* stuck funds
* unauthorized withdrawal
* delegatecall
* selfdestruct
* unchecked external call
* return-value handling
* integer overflow/underflow
* calldata bounds

禁止：

```text
delegatecall(user supplied target)
```

禁止：

```text
arbitrary external call
```

除非该行为是 M10 核心设计并经过单独安全审计。

第一版建议：

> **只允许白名单 V2 Pair。**

---

# 43. PAIR ALLOWLIST

Executor 最好具备：

```text
allowed_pair[pair] = true
```

只有已注册 pair 才能执行。

这样：

```text
arbitrary target
```

不会直接变成：

```text
arbitrary contract call
```

M10 不做动态 protocol discovery。

---

# 44. TOKEN ALLOWLIST

同理：

```text
allowed_token[token] = true
```

但不要让 allowlist 系统变成复杂治理系统。

简单即可。

---

# 45. OPERATOR POLICY

至少：

```text
operator
```

只有 operator 可以执行。

测试：

```text
authorized → success
unauthorized → revert
```

---

# 46. DEPLOYMENT

如果仓库当前没有 contract deployment tooling：

先做：

```text
toolchain audit
```

然后选择最小方案。

部署必须产生：

```text
contract address
deployment tx
deployment block
bytecode hash
ABI version
chain id
```

并进入 evidence。

---

# 47. BYTECODE / ABI EVIDENCE

真实部署必须保存：

```text
ABI
bytecode hash
runtime bytecode hash
deployment address
deployment transaction
```

避免未来出现：

```text
Rust calldata
≠
实际部署 contract ABI
```

---

# 48. REPLAYABILITY

M10 evidence 必须允许：

```text
fixture
→ rebuild
→ simulate
→ compare
```

至少可以重建：

```text
plan hash
calldata hash
simulation result
```

真实链 evidence 不要求完全离线重放整个链，但必须能够重新验证关键字段。

---

# 49. DETERMINISM GATES

至少：

### D1

same plan → same plan hash

### D2

same plan → same calldata

### D3

same fixture → same simulation result

### D4

same failure fixture → same revert classification

### D5

same route → same route_id

---

# 50. NEGATIVE CONTROL EVIDENCE

Evidence 必须包含 planted negative controls。

至少：

```text
wrong chain
wrong executor
wrong operator
wrong token continuity
zero amount
min output violation
final profit violation
pair not allowed
token not allowed
forced second-leg revert
```

每个都必须得到预期拒绝。

---

# 51. RPC SAFETY

M10 instrumentation 不得增加新的 hot-path RPC。

特别禁止为了：

```text
metrics
logging
profit verification
debug
```

额外发：

```text
eth_call
eth_getBalance
eth_getTransactionReceipt
eth_getLogs
```

如果 execution lifecycle 本身需要 RPC：

> 使用现有 execution pipeline。

Instrumentation 必须读取已有结果。

---

# 52. NO FABRICATED SUCCESS

以下任何一项都不能单独证明套利成功：

```text
simulation success
tx hash returned
receipt exists
receipt.status = 1
```

最终真实套利成功需要：

```text
receipt.status = 1
+
expected route observed
+
actual balance delta verified
+
profit denomination proven
```

如果没有真实 profit：

```text
REAL_PROFIT = UNKNOWN
```

不能写：

```text
REAL_PROFIT = 0
```

除非确实测量到了 0。

---

# 53. TEST LAYERS

测试至少分成：

```text
Unit
Integration
Contract
REVM
Execution lifecycle
GIWA controlled
Evidence
```

不要把所有测试塞进一个 integration test。

---

# 54. CONTRACT UNIT TESTS

至少：

1. authorized operator
2. unauthorized operator
3. zero amount
4. invalid pair
5. invalid token
6. broken token continuity
7. min output
8. final output
9. successful 2-hop
10. forced second-leg revert
11. reentrancy attempt
12. withdrawal authorization
13. allowlist rejection
14. deterministic calldata

---

# 55. REVM TESTS

必须至少覆盖：

```text
successful 2-hop
failed 2-hop
slippage violation
profit violation
insufficient balance
insufficient allowance
```

并检查：

```text
balances
reserves
status
logs
gas
```

---

# 56. EXECUTION LIFECYCLE TEST

证明：

```text
ExecutablePlan
 ↓
TransactionIntent
 ↓
Build
 ↓
Sign
 ↓
Submit
 ↓
Receipt
 ↓
ExecutionRecord
```

整个生命周期使用现有 execution types。

---

# 57. REAL GIWA CONTROLLED TEST

如果条件允许：

```text
deploy executor
 ↓
configure operator/pairs/tokens
 ↓
fund test wallet/executor
 ↓
approve
 ↓
execute
 ↓
receipt
 ↓
balance reconciliation
```

所有金额保持极小。

---

# 58. REAL FAILURE TEST

至少尝试一次真实可控 failure：

```text
min_output deliberately too high
```

预期：

```text
tx included
receipt.status = 0
```

并验证：

```text
no partial state
```

如果 GIWA 测试环境无法安全构造真实失败：

```text
UNKNOWN
```

不要伪造。

---

# 59. REAL SUCCESS TEST

如果真实 GIWA 上存在可控、确定性的 two-pool test fixture：

必须完成：

```text
real tx
+
receipt.status=1
+
actual balance delta
```

如果没有：

```text
M10 code/tests COMPLETE
real profitable arbitrage = NOT_PROVEN
```

这两者必须分开。

---

# 60. EVIDENCE STRUCTURE

建议：

```text
data/evidence/m10/
```

至少：

```text
README.md

manifest.json

contract/
  deployment.json
  abi.json
  bytecode_hash.json

fixtures/
  success.json
  revert.json
  slippage.json
  profit_guard.json

simulation/
  success.json
  failure.json

execution/
  lifecycle.json

negative_controls/
  wrong_chain.json
  wrong_operator.json
  broken_route.json
  invalid_pair.json
  min_output.json

real/
  giwa_execution.json
  giwa_failure.json

recompute/
  deterministic.json
```

如果真实交易不存在：

不要创建伪造的 `giwa_success.json`。

---

# 61. EVIDENCE MANIFEST

Manifest 至少包含：

```text
git_commit
chain_id
executor_address
deployment_tx
deployment_block
contract_hash
test_fixture_hash
plan_hash
calldata_hash
simulation_hash
real_tx_hash
```

不存在的字段：

```text
null
```

而不是：

```text
0
```

更不能填写虚构值。

---

# 62. STATIC SECURITY SCANS

继续使用项目既有安全扫描。

至少：

```text
private key literal scan
secret scan
URL scan
panic scan
unsafe scan
```

Contract 额外：

```text
arbitrary-call scan
delegatecall scan
selfdestruct scan
```

---

# 63. PRODUCTION DIFF AUDIT

最终必须明确：

M10 修改了哪些 production crate。

特别检查：

```text
M9.1 discovery
M9.2 state
M9.3 pathfinder
M9.4 live
```

原则：

> M10 不应该修改这些 milestone 的核心语义。

如果修改：

必须在 completion report 中逐项说明原因。

---

# 64. CARGO GATES

必须串行执行：

```text
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
```

如果仓库存在共享：

```text
target/pipeline-tests/
```

必须继续串行运行。

不能并发跑多个 workspace cargo test。

---

# 65. FULL REGRESSION

M10 完成前必须证明：

```text
M1-M9.4
```

全部 regression green。

特别：

```text
M9.1 discovery
M9.2 reconstruction
M9.3 pathfinder
M9.4 radar
```

不能因为 M10 引入 execution dependency 导致这些语义变化。

---

# 66. FOUR-COMMIT DISCIPLINE

严格四个 commit：

### Commit 1 — code

```text
feat(m10): add arbitrage executor contract
```

只放：

* contract
* Rust execution integration
* production code

---

### Commit 2 — tests

```text
test(m10): add executor contract and lifecycle coverage
```

只放：

* unit tests
* integration tests
* REVM tests
* negative controls

---

### Commit 3 — evidence

```text
evidence(m10): add executor execution evidence
```

只放：

* evidence
* fixtures
* manifests
* recomputation results
* real execution evidence

---

### Commit 4 — docs

```text
docs(m10): add arbitrage executor completion report
```

只放：

* completion report
* architecture notes
* task updates
* evidence README

---

# 67. COMPLETION REPORT REQUIRED SECTIONS

创建：

```text
docs/v0.1/M10 Completion Report.md
```

至少包含：

1. Executive Summary
2. Scope
3. Non-goals
4. Repository Audit
5. Existing Execution Reuse
6. Contract Architecture
7. Plan Model
8. Route Model
9. Access Control
10. Token/Pair Allowlist
11. Atomicity
12. Slippage Guard
13. Profit Guard
14. REVM Simulation
15. Negative Controls
16. Execution Lifecycle
17. Deployment
18. GIWA Real Test
19. Simulation vs Receipt
20. Balance Reconciliation
21. Gas / L1 Fee
22. Determinism
23. Security
24. RPC Safety
25. Regression
26. Production Diff
27. Limitations
28. UNKNOWN / N/A
29. Four-Commit Record
30. Final Verdict

---

# 68. COMPLETION VERDICT

M10 只有在以下条件全部满足时才可以：

```text
COMPLETE
```

## Contract

* [ ] Executor contract exists
* [ ] ABI deterministic
* [ ] bytecode deterministic
* [ ] deployment proven
* [ ] operator control proven
* [ ] pair allowlist proven
* [ ] token allowlist proven
* [ ] route validation proven
* [ ] atomicity proven
* [ ] min-output proven
* [ ] final-profit guard proven
* [ ] no arbitrary call
* [ ] no delegatecall
* [ ] no flashloan

## Rust

* [ ] ArbitrageExecutionPlan exists
* [ ] deterministic calldata builder
* [ ] chain binding
* [ ] executor binding
* [ ] existing execution lifecycle reused
* [ ] no duplicate signer
* [ ] no duplicate submitter
* [ ] no duplicate receipt system

## Simulation

* [ ] REVM successful path
* [ ] REVM revert path
* [ ] min-output rejection
* [ ] profit rejection
* [ ] insufficient funds rejection
* [ ] deterministic simulation

## Real GIWA

* [ ] deployment evidence
* [ ] controlled call attempted
* [ ] receipt evidence
* [ ] balance reconciliation

If successful real arbitrage cannot be established:

```text
REAL_PROFITABLE_ARBITRAGE = UNKNOWN
```

does not block code completion if all other M10 acceptance criteria are proven.

## Quality

* [ ] fmt
* [ ] clippy
* [ ] workspace tests
* [ ] regression
* [ ] secret scan
* [ ] panic scan
* [ ] evidence URL scan
* [ ] RPC safety
* [ ] clean worktree
* [ ] four commits
* [ ] completion report

---

# 69. M10 MUST NOT CLAIM

Do not write any of the following unless independently proven:

```text
Flashblocks execution
Private execution
MEV protection
Guaranteed profit
Stable profitability
Production readiness
24/7 readiness
Competitive latency
Mainnet readiness
```

M10 proves:

> **Atomic execution primitive.**

It does not prove:

> **Profitable MEV production system.**

---

# 70. FINAL ARCHITECTURE AFTER M10

Completion should leave the system conceptually at:

```text
M9.1
Pool Discovery
       ↓
M9.2
Canonical State
       ↓
M9.3
Cycle Candidate
       ↓
M9.4
Early Radar
       │
       ↓
Canonical Verification
       ↓
Canonical Graph
       ↓
M11
Optimize
       ↓
REVM
       ↓
Risk
       ↓
ExecutablePlan
       ↓
M10
Arbitrage Executor
       ↓
Atomic Transaction
       ↓
Receipt
       ↓
Realized Result
```

M10 must not reverse this dependency.

---

# 71. FINAL PRINCIPLE

The most important rule of M10:

```text
Simulation says:
    “This transaction should execute.”

Execution says:
    “This transaction was submitted.”

Receipt says:
    “This transaction was included.”

Balance reconciliation says:
    “This transaction actually produced this result.”
```

These four statements must never be collapsed into one.

M10 的成功标准不是“交易发出去了”。

M10 的成功标准是：

> **一个确定性的套利执行计划，可以经过现有 execution lifecycle 构造成交易，通过 Executor Contract 原子执行；成功时整个 route 完整执行并满足最终资产 invariant，失败时整笔交易回滚；所有结果能够通过 simulation、receipt 和实际 balance evidence 分层验证。**