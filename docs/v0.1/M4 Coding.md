# evm-mev-bot v0.1 — M4 Coding Agent Task

## 1. 任务定位

项目：

`evm-mev-bot`

当前：

```text
M1 Real Pool State          COMPLETE
M2 Real Market Graph        COMPLETE
M3 Real Arbitrage           COMPLETE
M4 Simulation & Profit      ← 本任务
```

M3 已经建立：

```text
GraphSnapshot
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

M4 继续：

```text
Opportunity
    ↓
Simulation Request
    ↓
EVM Transaction
    ↓
Execution Simulation
    ↓
Actual Output
    ↓
Gas Used
    ↓
Net Profit
    ↓
Risk Result
```

---

# 2. M4 的真正目标

M4 不是实现真实交易。

M4 的目标是：

> **证明 M3 的理论 Opportunity 在 EVM 执行语义下是否仍然成立，并计算执行后的真实净利润。**

最终希望得到：

```text
gross_profit
    ↓
simulation
    ↓
actual_output
    ↓
gas_cost
    ↓
net_profit
```

并明确区分：

```text
Estimated / Analytical
Simulated
Executed
Realized
```

M4 只能进入：

```text
Simulated
```

不能声称：

```text
Executed
Realized
```

---

# 3. 必须先阅读

开始编码前必须完整阅读：

```text
PRD.md
M1 Completion Report
M2 Completion Report
M3 Completion Report
M3 Coding Task
```

尤其阅读 M3：

```text
Opportunity
PricedHop
SearchRecord
GraphSnapshot
PoolState
PoolMeta
Fee
```

不得重新创建平行模型。

---

# 4. 当前仓库必须保持

M3 已经是：

```text
GraphSnapshot → Opportunity
```

完整闭环。

M4 不允许修改 M3 的数学语义。

特别是：

```text
gross_profit
```

必须保持原定义：

```text
analytical AMM output - input
```

M4 新增：

```text
simulated_output
gas_used
gas_cost
net_profit
```

两者必须同时存在。

---

# 5. M4 的第一原则

M4 必须避免一个常见错误：

```text
M3 Opportunity
    ↓
重新用一套“模拟公式”
    ↓
说这就是 simulation
```

这不算 EVM Simulation。

真正的 Simulation 必须尽可能执行：

```text
真实 EVM bytecode
+
真实 calldata
+
真实 state
```

得到：

```text
execution result
```

因此 M4 应优先研究当前项目是否已经可以引入：

```text
REVM
```

或其它真正的 EVM execution engine。

---

# 6. Simulation crate

优先建立：

```text
crates/simulation
```

职责：

```text
EVM execution
State loading
State override
Transaction execution
Execution result
Gas measurement
```

不要让：

```text
opportunity
```

依赖：

```text
simulation
```

依赖方向应该是：

```text
core
 ↓
opportunity
 ↓
simulation
```

或者：

```text
core
 ↓
opportunity

core
 ↓
simulation
```

由 pipeline 负责连接。

不要形成：

```text
simulation → opportunity
```

这种循环依赖。

---

# 7. Simulator Trait

根据 PRD，定义类似：

```rust
trait Simulator {
    async fn simulate(
        &self,
        request: &SimulationRequest,
    ) -> Result<SimulationResult>;
}
```

具体 async / sync 根据当前 crate 架构决定。

重点：

> 上层不应该知道 REVM 的具体类型。

---

# 8. SimulationRequest

需要表达：

```text
chain_id
block_number
opportunity
transaction
state source
```

至少需要：

```text
from
to
value
calldata
gas_limit
block context
```

如果 M4 第一阶段不需要所有字段，不要为了“完整”而虚构。

---

# 9. SimulationResult

至少需要能够表示：

```text
success
revert_reason
gas_used
output
logs
state_changes
```

具体字段根据 REVM 能提供什么来设计。

尤其需要区分：

```text
simulation failed
```

和：

```text
simulation succeeded but not profitable
```

---

# 10. 不要直接支持真实 Signer

M4 不需要：

```text
Private Key
KMS
Hardware Wallet
Remote Signer
```

模拟不应该接触真实私钥。

---

# 11. 不要 Broadcast

绝对禁止：

```text
eth_sendRawTransaction
```

以及：

```text
private relay
bundle submission
```

M4：

```text
NO BROADCAST
```

---

# 12. Transaction Construction

M4 必须第一次回答：

> Opportunity 应该如何变成一笔 transaction？

对于当前 two-pool arbitrage：

```text
Token A
 ↓
Pool1
 ↓
Token B
 ↓
Pool2
 ↓
Token A
```

必须有一个可执行入口。

优先检查真实协议现有：

```text
Router
Pair
Swap contract
```

哪一个最适合作为 execution target。

不要假设一定存在标准：

```text
UniswapV2Router02
```

必须通过真实数据和代码证据确认。

---

# 13. Arbitrage Executor Contract

如果当前真实环境没有合适的套利执行 contract：

可以新增一个极小的测试 contract：

```text
ArbitrageExecutor
```

职责仅仅是：

```text
transfer / approve
    ↓
swap pool1
    ↓
swap pool2
    ↓
return profit
```

但必须明确：

> 这是 simulation harness，不是生产交易合约。

生产 execution contract 不属于 M4。

---

# 14. Contract Scope

如果新增 Solidity contract：

只允许包含 M4 所需最小功能。

不要实现：

```text
Universal Router
Multi-DEX Router
Flash Loan System
Bundle System
MEV Vault
Strategy Engine
```

M4 只需要：

```text
two-pool V2-style swap
```

---

# 15. State Loading

Simulation 必须使用：

```text
block N
```

对应状态。

不要：

```text
GraphSnapshot @ block N
RPC state @ latest
```

混合。

这是 M4 最危险的错误之一。

必须保证：

```text
simulation_state_block
==
opportunity.block_number
```

或者如果 EVM state provider 明确支持：

```text
block hash
```

优先使用 block hash。

---

# 16. Historical Simulation First

M4 第一阶段只做：

```text
Historical Block
```

即：

```text
Opportunity @ block N
        ↓
simulate @ block N
```

不要一开始接 Live。

原因：

```text
historical state
```

可以：

```text
reproduce
debug
compare
```

---

# 17. Real State

真实 simulation 必须尽可能加载：

```text
Pool contract bytecode
Token contract bytecode
Storage
Balance
Allowance
Code
```

不能只把：

```text
reserve0
reserve1
```

塞给数学函数然后称为 EVM simulation。

---

# 18. REVM State

如果使用 REVM：

优先设计：

```text
Database / StateDB
```

能够按：

```text
address
```

加载：

```text
AccountInfo
Storage
Bytecode
```

并允许：

```text
state override
```

仅用于：

```text
test executor balance
approval
```

不要修改真实历史 state 的核心池状态。

---

# 19. RPC State Source

如果使用 RPC：

需要读取：

```text
eth_getCode
eth_getBalance
eth_getStorageAt
```

或项目已有等价能力。

优先复用：

```text
ChainAdapter
```

不要让：

```text
simulation
```

直接依赖 Alloy Provider 具体实现。

---

# 20. Snapshot Consistency

Simulation Request 必须携带：

```text
block_number
```

最好同时：

```text
block_hash
```

State Loader 必须验证：

```text
requested block
```

与：

```text
loaded state
```

一致。

如果无法保证：

```text
return SimulationError::StateMismatch
```

不要继续模拟。

---

# 21. Transaction Calldata

必须从：

```text
Opportunity
```

得到：

```text
amount_in
minimum_amount_out
path
```

其中：

```text
amount_in
```

来自 M3 的 optimal input。

但：

```text
minimum_amount_out
```

必须明确是：

```text
simulation slippage bound
```

而不是 M3 的 gross output 本身。

---

# 22. Slippage

M4 第一次引入：

```text
minimum_amount_out
```

但不要把它做成复杂风险系统。

第一阶段允许：

```text
min_amount_out = simulated expected output
```

用于：

```text
deterministic historical simulation
```

或者使用明确配置的：

```text
slippage_bps
```

生成：

```text
minimum_amount_out
```

必须记录：

```text
expected_output
slippage_policy
minimum_output
```

---

# 23. Token Tax

M3 已经发现：

```text
TTAX
```

存在实际 transfer tax 行为。

因此 M4 不得继续假设：

```text
token transfer amount
==
receiver amount
```

必须让：

```text
EVM execution
```

决定实际结果。

这正是 M4 相对于 M3 的核心价值之一。

---

# 24. Real Historical Opportunity

优先使用 M3 已经确认的真实机会：

```text
chain 91342
block 37191169
WETH / TTAX
Pool A
Pool B
```

但不要直接假设 M3 的：

```text
gross_profit
```

就是 simulation result。

必须实际执行。

---

# 25. 真实机会 Simulation Acceptance

至少实现：

```text
M3 Opportunity
 ↓
Transaction
 ↓
EVM Simulation
```

然后得到：

```text
simulation_success
gas_used
actual_output
```

比较：

```text
analytical_output
vs
simulated_output
```

---

# 26. Analytical vs Simulation

结果必须明确：

```text
analytical_output
simulated_output
delta
```

例如：

```text
analytical:
744486240802

simulated:
XXXX

delta:
XXXX
```

如果不同：

必须解释原因。

可能包括：

```text
token tax
transfer fee
rounding
router behavior
additional transfer
```

不能简单说：

```text
simulation is wrong
```

---

# 27. Gross Profit

M3：

```text
gross_profit
```

保持不变。

M4 新增：

```text
simulated_gross_profit
```

即：

```text
simulated_output - input
```

仍然不扣 gas。

---

# 28. Gas Used

Simulation 必须取得：

```text
gas_used
```

不要只使用：

```text
gas_limit
```

作为实际 gas。

必须区分：

```text
gas_limit
gas_used
```

---

# 29. Gas Price

M4 必须开始引入：

```text
gas_price
```

但必须说明：

```text
historical block
```

的 gas pricing semantics。

对于 EIP-1559 链，需要考虑：

```text
base_fee
priority_fee
```

对于 legacy gas：

```text
gas_price
```

具体由：

```text
ChainProfile
```

或 transaction model 决定。

不要统一硬编码。

---

# 30. Gas Cost

至少计算：

```text
gas_cost
```

使用：

```text
gas_used × effective_gas_price
```

具体遵循该链 transaction semantics。

必须使用：

```text
U256
```

或项目适合的精确整数类型。

禁止：

```text
f64
```

作为最终成本计算。

---

# 31. Net Profit

M4 第一次定义：

```text
net_profit
```

例如：

```text
net_profit =
    simulated_output
    - input
    - gas_cost
```

注意：

如果 input/output 是 token A：

```text
gas_cost
```

可能是 native token。

不能直接：

```text
U256 subtraction
```

除非：

```text
native token == input token
```

所以必须定义：

```text
profit denomination
```

---

# 32. Profit Denomination

M4 必须明确：

```text
profit_token
```

例如：

```text
WETH
```

如果 gas 是 ETH：

需要：

```text
ETH → WETH
```

的价值转换。

但如果历史状态中没有可靠的价格来源：

不要伪造 conversion rate。

这种情况下：

```text
gross profit
```

可以计算。

```text
net profit
```

必须：

```text
NotComputable
```

而不是假设：

```text
1 ETH = 1 WETH
```

除非链上语义本身证明两者是 wrapped/native 1:1 且执行路径中可确定。

---

# 33. Simulation Result

建议最终有：

```text
SimulationResult {
    success,
    block,
    gas_used,
    effective_gas_price,
    gas_cost,
    output_amount,
    analytical_output,
    simulated_output,
    output_delta,
    ...
}
```

具体字段根据实际代码调整。

---

# 34. Simulation Failure

至少区分：

```text
Reverted
OutOfGas
MissingCode
MissingState
StateMismatch
InvalidTransaction
ProviderError
UnsupportedTransaction
```

不要统一：

```text
SimulationFailed
```

否则以后无法判断风险。

---

# 35. Revert Reason

如果 EVM 能提供：

```text
revert data
```

应该保留。

不要强行 decode 所有 revert。

可以先：

```text
raw revert bytes
```

后续再扩展。

---

# 36. State Changes

M4 建议记录：

```text
state_changes
```

但第一阶段不需要做完整链级 diff。

至少能够审计：

```text
pool reserve changes
token balances
```

如果 REVM 能直接获得完整 journal：

可以保留。

否则不要为了实现“漂亮的 diff”引入大量复杂代码。

---

# 37. Simulation Determinism

同一个：

```text
Opportunity
+
block
+
transaction
+
state
```

运行两次：

必须得到：

```text
same success
same output
same gas
same revert
```

如果不同：

M4 不能 COMPLETE。

---

# 38. Analytical Cross-check

必须比较：

```text
M3 analytical
vs
M4 simulated
```

至少在：

```text
no tax fixture
```

中：

```text
analytical_output == simulated_output
```

或者只存在明确记录的执行层差异。

---

# 39. Tax Fixture

必须增加：

```text
transfer-tax token
```

测试。

验证：

```text
AMM analytical formula
```

无法反映：

```text
actual receiver amount
```

而：

```text
EVM simulation
```

能够得到真实结果。

这会成为 M4 最重要的测试之一。

---

# 40. Revert Fixture

构造：

```text
invalid minAmountOut
```

导致：

```text
swap revert
```

Simulation 必须：

```text
success = false
```

并保留：

```text
revert data
```

---

# 41. Out-of-Gas Fixture

至少有一个：

```text
gas_limit too low
```

测试。

必须得到：

```text
OutOfGas
```

而不是：

```text
ProviderError
```

---

# 42. State Mismatch Fixture

故意：

```text
Opportunity @ block N
Simulation @ block N+1
```

必须拒绝。

这是 M4 的核心 correctness gate。

---

# 43. Historical Gas Fixture

真实历史 block：

必须读取：

```text
block base fee
transaction gas price
```

如果目标 transaction 是新构造的：

必须明确：

```text
gas price policy
```

不能偷偷拿：

```text
latest gas price
```

模拟历史 opportunity。

---

# 44. Do Not Overbuild Gas Strategy

M4 暂时不要实现：

```text
dynamic gas bidding
priority fee optimization
bribe optimization
bundle pricing
private relay
```

只计算：

```text
given gas policy
+
simulation gas used
=
gas cost
```

---

# 45. Risk Layer

M4 可以建立：

```text
crates/risk
```

但第一阶段只需要最基本：

```text
minimum_net_profit
maximum_gas
simulation_success
```

例如：

```rust
trait RiskPolicy {
    fn evaluate(
        &self,
        opportunity: &Opportunity,
        simulation: &SimulationResult,
    ) -> RiskDecision;
}
```

---

# 46. Risk Decision

至少：

```text
Accept
Reject
Unknown
```

例如：

```text
simulation reverted
→ Reject

net profit known and > minimum
→ Accept

net profit cannot be denominated
→ Unknown
```

不要实现复杂 token risk。

---

# 47. No Execution

即使：

```text
RiskDecision::Accept
```

也：

```text
NO BROADCAST
```

M4 的 Accept 只是：

> 模拟结果满足当前风险规则。

不是：

> 发送交易。

---

# 48. Execution Interface

如果需要，可以提前定义：

```rust
trait Executor {
    async fn execute(
        &self,
        request: &ExecutionRequest,
    ) -> Result<ExecutionResult>;
}
```

但只实现：

```text
NullExecutor
DryRunExecutor
```

真实 executor 不属于 M4。

---

# 49. M4 与 Live

不要在 M4 中强行实现完整：

```text
WebSocket → Live Opportunity
```

但要保证架构不阻碍未来 Live。

Simulation 的输入应该是：

```text
explicit block/state
```

而不是：

```text
latest
```

这样未来：

```text
Live Opportunity
 ↓
capture current block
 ↓
Simulation @ exact block
```

自然成立。

---

# 50. Replay

M4 必须复用已有：

```text
Replay
```

不能重新实现。

最终应该能够：

```text
Historical block
 ↓
M1 state
 ↓
M2 graph
 ↓
M3 opportunity
 ↓
M4 simulation
```

完整 replay。

---

# 51. Real Historical Replay

至少使用：

```text
block 37191169
```

进行一次完整：

```text
Replay
 ↓
Opportunity
 ↓
Simulation
```

如果 M3 opportunity 因为历史交易已经改变 state 而无法直接作为同一区块交易重新执行：

必须准确解释：

```text
Opportunity 是 block-end state 上的 hypothetical transaction
```

而不是原历史交易本身。

不要把两者混淆。

---

# 52. Important Historical Semantics

M3 的 Opportunity：

```text
Pool state @ end of block N
```

如果模拟：

```text
transaction
```

它代表：

> 在 block N 的最终状态之后再执行这笔 hypothetical transaction。

因此它不是：

```text
block N 中真实交易之前
```

也不是：

```text
block N 中真实交易之后某一时刻
```

必须明确 simulation state semantics。

---

# 53. Simulation State Selection

建议 M4 第一阶段统一：

```text
Opportunity snapshot @ block N
+
simulate hypothetical tx @ state N
```

也就是：

```text
post-block state
```

这样最容易 deterministic。

如果以后需要：

```text
pre-state
```

再独立增加：

```text
StatePosition::BeforeBlock
StatePosition::AfterBlock
```

不要现在混在一起。

---

# 54. Real Token Tax

对于：

```text
TTAX
```

必须重点观察：

```text
transfer
balanceOf
fee deduction
```

如果 simulation 发现：

```text
analytical output != actual output
```

记录：

```text
difference
```

并解释。

不要为了让两个数字一致而修改 M3 数学。

---

# 55. Transaction Target

如果使用真实 Router：

必须记录：

```text
router address
router bytecode hash
router ABI/evidence
```

如果使用自建 executor：

记录：

```text
contract bytecode hash
deployment address
source
```

真实交易 simulation 必须可以审计 target。

---

# 56. Calldata Evidence

最终 simulation request 必须能输出：

```text
to
value
calldata
```

最好还能显示：

```text
selector
arguments
path
amountIn
amountOutMin
deadline
```

如果 decoder 无法确定：

保留：

```text
raw calldata
```

不要猜 ABI。

---

# 57. Approval

Simulation 中如果需要 ERC20 approval：

可以通过：

```text
state override
```

提供测试账户 allowance。

但必须明确：

```text
approval is simulation setup
```

不是历史链上事实。

不能把 override 后的 allowance 当成真实状态。

---

# 58. Sender

M4 必须有：

```text
simulation sender
```

可以使用：

```text
deterministic test address
```

但必须保证：

```text
balance
allowance
nonce
```

足够。

这个账户不能拥有真实私钥。

---

# 59. Native Token

如果套利输入是：

```text
WETH
```

simulation 可能不需要 native value。

如果使用：

```text
ETH
```

则：

```text
msg.value
```

必须正确。

不要默认：

```text
value = 0
```

---

# 60. Contract Code

任何参与 simulation 的：

```text
Pool
Token
Router
Executor
```

必须确保：

```text
code exists
```

如果：

```text
eth_getCode == empty
```

拒绝：

```text
MissingCode
```

不能用“猜测 bytecode”。

---

# 61. Real State Loading Performance

M4 第一阶段：

```text
correctness > latency
```

可以：

```text
RPC state load
```

逐地址读取。

不要现在做：

```text
high-performance state cache
```

但接口设计必须允许未来 cache。

---

# 62. Cache Boundary

可以设计：

```text
StateProvider
```

例如：

```rust
trait StateProvider {
    fn account(...)
    fn storage(...)
    fn code(...)
}
```

未来可以：

```text
RPCStateProvider
CachedStateProvider
RecordedStateProvider
```

但不要为了抽象而实现三个 provider。

先实现：

```text
一个真实 provider
+
一个 fixture provider
```

即可。

---

# 63. Real Code Cache

如果真实历史 simulation 需要大量：

```text
eth_getCode
```

可以缓存：

```text
address → bytecode
```

但 cache 必须与：

```text
chain_id
```

绑定。

不能：

```text
address
```

单独作为全局 key。

---

# 64. Storage Cache

同理：

```text
chain_id
block
address
slot
```

至少需要能够表达完整 state identity。

不要：

```text
address + slot
```

全局缓存。

---

# 65. M4 Acceptance

M4 只有满足以下条件才能：

```text
COMPLETE
```

### A

存在：

```text
crates/simulation
```

并有清晰职责。

### B

存在真正的 EVM execution engine。

### C

可以把 M3 Opportunity 转换成：

```text
SimulationRequest
```

### D

可以执行真实：

```text
Pool + Token + Router/Executor
```

bytecode。

### E

simulation state 与 Opportunity block 一致。

### F

能够得到：

```text
success
output
gas_used
```

### G

能够区分：

```text
revert
out of gas
missing state
state mismatch
```

### H

无税 fixture：

```text
analytical output
≈
simulated output
```

最好 exact。

### I

tax fixture：

```text
analytical
!=
simulated
```

并能证明差异来自 token execution semantics。

### J

能够计算：

```text
simulated gross profit
```

### K

能够计算 gas cost。

### L

如果 profit denomination 可确定：

能够计算：

```text
net profit
```

否则明确：

```text
Unknown / NotComputable
```

### M

Risk Policy 至少能够处理：

```text
simulation success
minimum net profit
maximum gas
```

### N

真实历史 opportunity：

```text
M3 Opportunity
 ↓
M4 Simulation
```

至少跑通一次。

### O

simulation deterministic。

### P

workspace：

```text
fmt PASS
check PASS
test PASS
clippy PASS
```

### Q

M4 Completion Report 完成。

---

# 66. M4 Completion Report

新增：

```text
docs/v0.1/M4 Completion Report.md
```

必须记录：

## 1. Status

```text
COMPLETE
```

或：

```text
PARTIAL
```

不能为了版本完成而强行 COMPLETE。

## 2. Simulation Engine

记录：

```text
engine
version
configuration
```

## 3. State Source

记录：

```text
chain
block
state source
```

## 4. Transaction

记录：

```text
from
to
value
calldata
gas limit
```

## 5. Execution

记录：

```text
success
output
gas used
logs
revert
```

## 6. Analytical vs Simulation

表格：

```text
analytical output
simulated output
delta
```

## 7. Gas

记录：

```text
gas used
gas price
gas cost
```

## 8. Profit

记录：

```text
gross profit
gas cost
net profit
profit denomination
```

## 9. Risk

记录：

```text
decision
reason
```

## 10. Real Historical Evidence

必须能够追溯：

```text
block
pool
token
transaction target
bytecode
state
```

## 11. Fixtures

至少：

```text
normal V2
tax token
revert
out of gas
state mismatch
```

## 12. Limitations

明确：

```text
no broadcast
no private relay
no bundle
no signer
no dynamic gas bidding
no live pipeline
```

---

# 67. M4 最重要的验收链路

最终必须形成：

```text
Real Historical Block
        ↓
Real Pool State
        ↓
Real Graph
        ↓
M3 Opportunity
        ↓
Transaction Construction
        ↓
EVM Simulation
        ↓
Actual Output
        ↓
Gas Used
        ↓
Gas Cost
        ↓
Net Profit
        ↓
Risk Decision
```

这是 M4 的真正完成标准。

---

# 68. 最重要的原则

M3 证明：

```text
理论上能赚
```

M4 要证明：

```text
把它变成交易后
EVM 实际执行会发生什么
```

所以绝对不能：

```text
M3 gross_profit
        ↓
直接减一个估算 gas
        ↓
net_profit
```

这不算 M4。

必须：

```text
真实 EVM execution
        ↓
真实 output
        ↓
真实 gas_used
```

再进行：

```text
profit calculation
```

---

# 69. 不要因为真实交易复杂而修改 M3

如果出现：

```text
M3 analytical output
≠
M4 simulated output
```

不要修改 M3 来“对齐”。

正确流程：

```text
Difference
 ↓
Investigate
 ↓
Identify execution semantics
 ↓
Record evidence
```

可能是：

```text
token tax
router fee
rounding
transfer behavior
state mismatch
```

只有在证明 M3 本身数学错误时，才修改 M3。

---

# 70. 不要把历史真实交易当成 Simulation Ground Truth

尤其注意：

```text
真实交易 tx
```

和：

```text
M3 hypothetical opportunity
```

不是一回事。

真实 tx 可以用于：

```text
验证 token behavior
验证 fee
验证 bytecode
验证 state
```

但不能直接说：

> “真实交易成功，所以 M3 opportunity 模拟正确。”

两者必须分开。

---

# 71. 最终交付

保持 M1/M2/M3 的交付习惯：

```text
Code commit
Documentation commit
Clean working tree
Full validation
M4 Completion Report
```

如果过程中发现：

```text
PRD 与真实实现冲突
```

不要偷偷改变范围。

先记录：

```text
Conflict
Evidence
Recommended interpretation
```

然后再决定是否需要修改 PRD。

---

# 72. M4 完成后的项目状态

目标：

```text
v0.1

M1
Real Pool State
        ↓
M2
Real Market Graph
        ↓
M3
Real Arbitrage
        ↓
M4
EVM Simulation + Net Profit
```

此时系统才第一次能够严谨地区分：

```text
Gross Profit
        ≠
Simulated Profit
        ≠
Net Profit
        ≠
Executed Profit
        ≠
Realized Profit
```

后续再进入：

```text
Risk
 ↓
Execution
 ↓
Live
 ↓
Metrics
 ↓
Replay
```

不要在 M4 中提前跨过去。
