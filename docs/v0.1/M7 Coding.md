# M7 Coding Task — GIWA Testnet Real Arbitrage

## 0. Milestone Definition

项目：

`evm-mev-bot`

目标：

**GIWA Testnet Arbitrage Bot**

M1～M6 已完成。

当前状态：

```text
M1 Historical Data Correctness     COMPLETE
M2 Market Graph                    COMPLETE
M3 Arbitrage Opportunity           COMPLETE
M4 EVM Simulation / Risk           COMPLETE
M5 Live Pipeline                   COMPLETE
M6 Execution                       COMPLETE
M7 Real Arbitrage                  CURRENT
M8 Hardening                       FUTURE
```

M7 是整个项目目前最重要的一次能力验证。

M7 不再以：

```text
transaction constructed
transaction signed
transaction submitted
transaction included
```

作为最终成功标准。

M7 的最终成功标准是：

> **在 GIWA Testnet 上，由系统真实发现或基于真实实时状态确认一个可执行套利机会，经过真实 Simulation + Risk，真实签名并提交交易，交易成功上链，并通过链上资产变化与完整交易成本计算证明该次套利的 realized profit > 0。**

---

# 1. M7 不允许“为了套利而制造套利”

这是整个 M7 最重要的约束。

禁止：

```text
修改 reserve
修改 fee
修改 transfer tax
修改 token balance
修改 pool balance
修改 opportunity threshold
修改 risk threshold
修改 simulation state
修改 gas cost
修改 L1 fee
修改历史 block
修改交易结果
```

也禁止：

```text
为了产生 Risk = Accept
而人为选择一个并不存在的机会。
```

允许：

```text
真实链上的机会
真实历史链状态
真实测试网流动性
真实交易
真实失败
```

不允许：

```text
fabricated opportunity
fabricated profit
fabricated receipt
fabricated balance delta
```

---

# 2. M7 最终事实链

必须形成：

```text
Live Chain
   ↓
Canonical State
   ↓
Graph
   ↓
Opportunity
   ↓
Simulation
   ↓
Risk
   ↓
TransactionIntent
   ↓
Build
   ↓
Sign
   ↓
Submit
   ↓
Included
   ↓
Receipt
   ↓
Actual Token / ETH Delta
   ↓
Actual L2 Gas
   ↓
Actual L1 Fee
   ↓
Realized Profit
```

其中任何一个环节不能由下一个环节“猜”。

---

# 3. M7 的第一阶段：重新寻找真实机会

M5 的真实窗口中：

```text
121 blocks
9958 logs
0 opportunities
```

因此不能假设：

```text
启动 bot
→ 很快出现套利
```

M7 必须先建立真实机会观测能力。

建议运行：

```text
Live source
+
持续 State
+
Graph
+
Opportunity
+
Simulation
+
Risk
```

持续运行。

---

# 4. Opportunity 必须来自真实市场

Opportunity 必须来源于：

```text
real PoolState
+
real fee evidence
+
real token behavior
+
real graph
```

不能直接读取：

```text
M3 fixture
```

然后说：

```text
Live opportunity
```

历史 fixture 可以用于：

```text
replay
debug
regression
execution validation
```

但不能伪装成 live opportunity。

---

# 5. Real Opportunity 的定义

至少必须满足：

```text
Pool A
Pool B
same token pair
```

并且：

```text
fee proven
```

并且：

```text
reserve state authoritative
```

并且：

```text
input amount
```

经过当前真实状态计算。

---

# 6. Simulation 必须使用真实执行状态

Simulation 必须绑定：

```text
chain_id
block_number
block_hash
```

并且：

```text
latest
```

不能替代：

```text
opportunity block
```

如果 opportunity 在：

```text
block N
```

产生：

Simulation 必须明确使用：

```text
state at N
```

或经过明确验证的：

```text
N + deterministic state transition
```

---

# 7. Opportunity 与 Simulation 的时间关系

必须记录：

```text
opportunity_detected_at
opportunity_block
simulation_started_at
simulation_completed_at
```

以及：

```text
current_head_at_simulation
```

用于判断机会是否已经过期。

---

# 8. M7 必须强化 Stale Protection

不能：

```text
Opportunity detected
      ↓
等待很久
      ↓
直接 send
```

必须重新验证：

```text
current state
```

至少：

```text
block hash
pool reserves
relevant token state
```

仍然符合执行条件。

---

# 9. Risk = Accept 是必要条件

真实执行必须满足：

```text
RiskDecision::Accept
```

否则：

```text
NO SIGN
NO SUBMIT
```

特别是：

```text
Risk::Reject
Simulation::Revert
Simulation::Unknown
Stale
InsufficientBalance
```

任何一种：

```text
NO TRANSACTION
```

---

# 10. M7 需要重新审查 Risk 的 profitability model

M6 已经发现：

```text
execution gas
```

不能代表：

```text
total transaction cost
```

GIWA 是 OP Stack L2。

真实交易成本必须至少考虑：

```text
L2 execution gas
+
L1 data fee
```

M7 必须将：

```text
L1 fee
```

正式纳入 profitability。

不能继续使用：

```text
gas_limit × max_fee
```

作为完整交易成本。

---

# 11. Realized Profit

M7 必须建立明确的：

```rust
RealizedProfit
```

模型。

不要简单：

```text
balance_after - balance_before
```

就结束。

至少需要区分：

```text
input_asset_delta
output_asset_delta
native_balance_delta
gas_cost
l1_fee
```

---

# 12. Arbitrage Profit 的核心定义

对于：

```text
Token A
→
Pool A
→
Token B
→
Pool B
→
Token A
```

必须计算：

```text
gross_output
-
input_amount
-
all_execution_costs
=
realized_profit
```

如果 native ETH 同时用于 gas：

必须把：

```text
ETH spent on gas
```

从最终收益中扣除。

---

# 13. Token Profit

如果套利资产不是 native ETH：

例如：

```text
WETH
```

必须避免：

```text
WETH
profit
```

和：

```text
ETH gas
```

混为一谈。

应该明确记录：

```text
token_profit
native_gas_cost
native_l1_fee
```

然后根据明确的 profitability denomination 计算最终：

```text
realized_profit
```

---

# 14. 最终收益计价单位

M7 第一阶段只允许选择一个明确的计价单位。

优先：

```text
input token
```

例如：

```text
WETH
```

那么：

```text
realized_profit_weth
```

必须包含：

```text
WETH output
-
WETH input
-
WETH-equivalent execution cost
```

如果无法可靠地把 L1/L2 ETH cost 转换成 WETH：

不要伪造价格。

可以报告：

```text
gross token profit
+
native execution cost
```

并将：

```text
net profit in one denomination
```

标记为无法证明。

但 M7 最终 COMPLETE 必须能够证明：

```text
net realized profit > 0
```

---

# 15. 不允许使用外部价格制造利润

禁止：

```text
CoinGecko
Binance
OKX
external oracle
LLM
```

直接给：

```text
ETH/USD
WETH/USD
```

然后人为把不同资产收益换算。

M7 首选：

```text
same asset in
same asset out
```

这样可以避免外部价格依赖。

---

# 16. 最理想的真实套利

优先寻找：

```text
WETH
    ↓
Pool A
    ↓
Token X
    ↓
Pool B
    ↓
WETH
```

这样：

```text
input = WETH
output = WETH
```

天然可以计算：

```text
gross_profit_weth
```

然后扣：

```text
gas
L1 fee
```

得到：

```text
net_profit
```

---

# 17. Transfer Tax 必须纳入 Simulation

M4 已经证明：

```text
TTAX
```

存在：

```text
3%
5%
```

不同 transfer tax。

因此：

```text
AMM mathematical output
```

不能直接当：

```text
real token output
```

M7 必须确保：

```text
Simulation output
```

已经体现：

```text
actual token transfer behavior
```

---

# 18. Simulation 与 Realized Result 必须做 Delta Audit

交易完成后：

必须比较：

```text
simulation expected
```

和：

```text
actual receipt / balance result
```

例如：

```text
expected token output
actual token output
delta
```

以及：

```text
expected gas
actual gas
delta
```

如果差异超过明确 tolerance：

```text
ExecutionMismatch
```

---

# 19. Transaction Receipt 不够

Receipt：

```text
status = 1
```

只能证明：

```text
EVM transaction succeeded
```

不能证明：

```text
arbitrage profitable
```

必须继续读取：

```text
balance
token balance
Transfer logs
```

---

# 20. Realized Balance Snapshot

真实交易前：

记录：

```text
sender native balance
input token balance
output token balance
```

真实交易后：

再次读取：

```text
sender native balance
input token balance
output token balance
```

必须使用：

```text
block before
block after
```

明确绑定。

---

# 21. Token Balance Delta

如果使用 ERC-20：

必须读取：

```text
balanceOf(sender)
```

交易前后。

不要只依赖 Transfer event。

原因：

```text
Transfer logs
```

是 execution evidence。

而：

```text
balanceOf
```

是最终资产状态。

二者应该互相验证。

---

# 22. Transfer Log Audit

Receipt 中的：

```text
Transfer
```

事件必须能够解析。

至少记录：

```text
token
from
to
amount
log_index
```

用于：

```text
expected flow
vs
actual flow
```

验证。

---

# 23. Arbitrage Route Audit

如果交易：

```text
Pool A swap
Pool B swap
```

必须从 receipt logs 证明：

```text
Pool A
```

确实执行了第一腿。

并且：

```text
Pool B
```

确实执行了第二腿。

不能仅凭：

```text
status = 1
```

说套利完成。

---

# 24. Actual Profit Formula

最终建议形成：

```text
gross_token_profit
    =
final_input_token_balance
-
initial_input_token_balance
```

然后：

```text
realized_profit
    =
gross_token_profit
-
converted_total_execution_cost
```

其中：

```text
total_execution_cost
=
L2_execution_fee
+
L1_data_fee
```

具体 conversion 必须使用交易本身可证明的数据。

---

# 25. 如果无法可靠换算

如果：

```text
WETH profit
```

但：

```text
ETH cost
```

无法在不依赖外部价格的情况下换算：

不要给一个虚假的：

```text
net_profit_weth
```

可以报告：

```text
WETH gross profit
ETH execution cost
```

并将：

```text
Net Profit Proof
```

标记为：

```text
INCOMPLETE
```

这种情况下：

```text
M7 ≠ COMPLETE
```

---

# 26. 真实机会执行前最后一道 Gate

在 Sign 之前增加：

```text
ExecutionPreflight
```

至少检查：

```text
chain_id
current_head
opportunity_block
opportunity_block_hash
pool reserves
simulation result
risk decision
nonce
native balance
input token balance
gas estimate
fee estimate
l1 fee estimate
```

全部通过才允许：

```text
Sign
```

---

# 27. Preflight 不重新寻找机会

Preflight 的职责不是：

```text
find new opportunity
```

而是：

```text
verify this exact opportunity
```

如果发现机会变化：

```text
Reject
```

不要自动切换到另一个 opportunity。

---

# 28. Submission Path

M6 当前已经验证：

```text
eth_sendRawTransaction
```

可以真实发送。

M7 首先继续使用：

```text
eth_sendRawTransaction
```

保证最简单可靠。

---

# 29. SequencerDirect 重新探测

M6 报告中：

```text
SequencerDirect
```

仍然 BLOCKED。

但 GIWA 官方 node 配置目前公开了：

```text
RETH_ROLLUP_SEQUENCERHTTP=https://sepolia-sequencer.giwa.io
```

M7 必须重新研究这个 endpoint。

但：

**不要假设它就是 raw transaction API。**

必须实际验证：

```text
HTTP method
request format
response format
authentication
transaction submission semantics
```

如果无法验证：

```text
SequencerDirect = BLOCKED
```

继续使用：

```text
eth_sendRawTransaction
```

即可。

---

# 30. Flashblocks

GIWA 官方 node 配置目前也提供：

```text
wss://sepolia-flashblocks.giwa.io/ws
```

作为 Flashblocks 配置项。

M7 可以重新验证：

```text
Flashblocks observation
```

但不要因此修改 canonical state 规则。

仍然保持：

```text
Flashblock
    ↓
candidate / early signal
    ↓
canonical reconciliation
```

而不是：

```text
Flashblock
    ↓
直接当最终 state
```

---

# 31. M7 第一优先级不是 Flashblocks

不要因为 Flashblocks 存在，就把 M7 变成：

```text
Flashblocks optimization project
```

M7 最重要的是：

```text
真实套利
```

Flashblocks latency optimization 属于：

```text
M8
```

M7 只需要证明：

```text
Flashblocks 不破坏正确性
```

即可。

---

# 32. 真实套利执行必须具备 Kill Switch

加入：

```text
ExecutionEnabled
```

或等价配置。

默认：

```text
false
```

必须显式：

```text
--execution-mode submit
```

才能真实发送。

Live bot 默认：

```text
BuildOnly
```

或者：

```text
ObserveOnly
```

不能启动程序就自动交易。

---

# 33. Single Outstanding Arbitrage

M7 第一阶段继续保持：

```text
one execution lane
```

即：

```text
one wallet
one nonce lane
one outstanding arbitrage
```

不要现在做并发 nonce pool。

原因：

M7 目标是：

```text
correct profitable execution
```

不是：

```text
maximum throughput
```

---

# 34. Failed Arbitrage 也必须完整记录

如果真实交易：

```text
status = 0
```

也必须保存：

```text
opportunity
simulation
risk
transaction
receipt
reason
```

不能只记录：

```text
tx failed
```

---

# 35. Gas / L1 Fee 必须来自真实交易

真实交易后：

必须拿到：

```text
gas_used
effective_gas_price
```

计算：

```text
L2 fee
```

同时计算：

```text
L1 data fee
```

不能继续假设：

```text
L1 fee = 0
```

---

# 36. OP Stack L1 Fee

M7 必须调查 GIWA 当前实际 RPC 对：

```text
eth_getTransactionReceipt
```

或：

```text
debug / fee APIs
```

提供的 L1 fee 信息。

如果 RPC receipt 没有直接字段：

必须研究 GIWA / OP Stack 当前可验证的计算方式。

最终报告必须明确：

```text
L1 fee source
```

不能只写：

```text
L1 fee included
```

---

# 37. Actual Cost Evidence

最终每一笔真实套利必须形成：

```text
ExecutionCostEvidence
```

至少：

```text
transaction_hash

gas_used
effective_gas_price
l2_fee

l1_fee

total_execution_cost
```

并有：

```text
source
```

证明这些数字来自真实链。

---

# 38. Realized Profit Evidence

必须形成：

```text
ProfitEvidence
```

至少：

```text
initial_balance
final_balance

initial_token_balance
final_token_balance

gross_profit

l2_fee
l1_fee

net_profit
```

以及：

```text
block_before
block_after
```

---

# 39. Profit 必须能独立重算

非常重要。

报告里不能只有：

```text
realized_profit = 123
```

必须提供：

```text
A
+
B
-
C
-
D
=
E
```

例如：

```text
initial WETH
+
WETH received
-
WETH spent
-
ETH execution cost equivalent
=
net result
```

任何人拿 evidence 都应该能重新计算。

---

# 40. 不允许使用程序内部缓存作为最终证据

最终 realized profit 不得来自：

```text
StateStore
GraphSnapshot
OpportunityLedger
```

这些只能帮助：

```text
strategy
```

最终事实必须来自：

```text
chain
```

---

# 41. M7 最低成功条件

至少完成：

```text
1 个真实机会
```

并且：

```text
Risk = Accept
```

然后：

```text
真实 transaction
```

成功上链。

然后：

```text
actual asset delta
```

可验证。

最终：

```text
realized_profit > 0
```

---

# 42. 不要求连续盈利

M7 不要求：

```text
10 次套利
100 次套利
稳定盈利
```

只要求：

```text
至少 1 次
```

但这 1 次必须是完整可验证的真实套利。

---

# 43. 如果始终没有真实机会

这是允许的。

例如运行：

```text
24h
```

仍然：

```text
0 RiskApproved opportunity
```

不能人为制造。

此时：

```text
M7 = BLOCKED / INCOMPLETE
```

并报告：

```text
observation window
blocks
pools
opportunities
simulation count
risk accept count
```

---

# 44. 如果有机会但都不盈利

也不能修改 threshold。

例如：

```text
100 opportunities
0 Risk Accept
```

则：

```text
M7 = INCOMPLETE
```

这是有效的真实结果。

---

# 45. 如果 Risk Accept 但真实交易失败

例如：

```text
Risk Accept
→ Sign
→ Submit
→ Revert
```

M7 仍然不能 COMPLETE。

但这将产生非常重要的：

```text
execution mismatch evidence
```

必须记录：

```text
simulation
vs
real execution
```

差异。

---

# 46. 如果真实交易成功但利润 <= 0

同样：

```text
M7 != COMPLETE
```

必须记录：

```text
gross profit
gas
l1 fee
net profit
```

并分析：

```text
why simulation overestimated profitability
```

---

# 47. 最重要的 Simulation → Reality Audit

M7 必须回答：

> Simulation 为什么相信这笔交易能赚钱，而真实结果为什么真的赚钱？

必须比较：

```text
simulated output
actual output

simulated gas
actual gas

simulated profit
actual gross profit

estimated fee
actual fee

estimated net profit
actual net profit
```

形成：

```text
SimulationRealityDelta
```

---

# 48. M7 第一笔成功交易不允许追求最大利润

第一笔真实套利：

优先：

```text
small input
small exposure
```

但必须：

```text
profit > all costs
```

不要为了证明能力：

```text
放大 input
```

增加不必要风险。

---

# 49. 不允许人为注入流动性

如果 GIWA Testnet 当前没有自然形成可套利市场：

不能：

```text
deploy pool
add liquidity
modify reserve
```

然后把自己制造的价格差当成：

```text
real market arbitrage
```

除非明确将其标记为：

```text
controlled test market
```

这种测试可以用于：

```text
execution validation
```

但：

**不能计入 M7 的 Real Arbitrage COMPLETE。**

---

# 50. 可以建立 Controlled Arbitrage Fixture

如果真实市场始终没有机会，可以建立：

```text
Controlled Execution Fixture
```

验证：

```text
Builder
Signer
Simulation
Submission
Receipt
Profit accounting
```

但它只能证明：

```text
execution system works
```

不能证明：

```text
real market arbitrage works
```

---

# 51. Real Arbitrage 与 Controlled Arbitrage 必须完全分开

报告必须明确：

```text
REAL_MARKET
```

和：

```text
CONTROLLED_FIXTURE
```

不能混淆。

---

# 52. M7 Metrics

至少记录：

```text
opportunity_count
simulation_count
risk_accept_count
risk_reject_count

build_count
sign_count
submit_count
included_count
revert_count

profitable_count
unprofitable_count
```

以及：

```text
gross_profit
l2_cost
l1_cost
net_profit
```

---

# 53. M7 Latency

至少记录：

```text
opportunity_detection_latency
simulation_latency
preflight_latency
sign_latency
submission_latency
inclusion_latency
```

但：

```text
p50/p95/p99
```

仍属于 M8。

M7 只要求保存单次事实。

---

# 54. Execution Lifecycle

必须最终支持：

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

最后新增：

```text
ProfitVerified
```

这是 M7 相对于 M6 最重要的新增状态。

---

# 55. Execution Record

M7 应扩展 M6 的 ExecutionRecord：

```text
execution_id

opportunity_id
simulation_id
risk_decision_id

transaction_hash

opportunity_block
opportunity_block_hash

execution_block
execution_block_hash

sender

input_asset
input_amount

gross_output

gross_profit

gas_used
l2_fee
l1_fee
total_fee

realized_profit

status
```

---

# 56. Profit Verification Status

建议：

```rust
enum ProfitVerificationStatus {
    Pending,
    VerifiedPositive,
    VerifiedNegative,
    Inconclusive,
}
```

只有：

```text
VerifiedPositive
```

才可以计入：

```text
successful_real_arbitrage
```

---

# 57. M7 Acceptance Criteria

## A — Real Opportunity

至少发现：

```text
1 real market opportunity
```

PASS / BLOCKED。

---

## B — Real Simulation

Opportunity 使用真实链状态完成 simulation。

PASS。

---

## C — Risk Accept

至少一次：

```text
Risk = Accept
```

PASS / BLOCKED。

---

## D — Preflight

真实 execution 前通过：

```text
balance
nonce
state
stale
fee
```

PASS。

---

## E — Build

Transaction 正确构造。

PASS。

---

## F — Sign

真实 signer 完成。

PASS。

---

## G — Submit

真实：

```text
eth_sendRawTransaction
```

或经验证的 GIWA sequencer path。

PASS。

---

## H — Inclusion

真实 transaction：

```text
receipt.status = 1
```

PASS。

---

## I — Route Verification

Receipt logs 证明套利两腿真实执行。

PASS。

---

## J — Asset Delta

真实账户资产变化可证明。

PASS。

---

## K — L2 Fee

真实 gas cost 可证明。

PASS。

---

## L — L1 Fee

真实 L1 data fee 可证明。

PASS。

---

## M — Gross Profit

真实资产变化证明：

```text
gross_profit > 0
```

PASS。

---

## N — Net Profit

扣除：

```text
L2 fee
L1 fee
```

之后：

```text
realized_profit > 0
```

PASS。

---

## O — Simulation Reality Delta

真实结果与 Simulation 可对账。

PASS。

---

## P — Evidence Reproducibility

第三方仅使用 evidence：

能够重新计算：

```text
realized_profit
```

PASS。

---

## Q — No Fabrication

确认没有：

```text
state override
reserve modification
fee modification
threshold modification
fake opportunity
fake receipt
```

PASS。

---

# 58. M7 COMPLETE 定义

只有同时满足：

```text
A
B
C
D
E
F
G
H
I
J
K
L
M
N
O
P
Q
```

才可以：

```text
M7 = COMPLETE
```

其中最关键的是：

```text
N — Net Profit > 0
```

---

# 59. M7 如果卡住怎么办

不要为了 COMPLETE 修改事实。

允许：

```text
PARTIAL
```

或：

```text
BLOCKED
```

例如：

```text
Real Opportunity = 0
```

则：

```text
M7 BLOCKED
```

这是正确结果。

---

# 60. M7 不做 M8 的事情

不要在 M7 大规模加入：

```text
Flashblocks optimization
lock-free state
parallel decode
parallel simulation
memory optimization
allocation optimization
high-frequency RPC
advanced retry
circuit breaker
multi-wallet
parallel nonce
bundle
private relay
```

M8 再做。

---

# 61. M7 最终报告

生成：

```text
docs/v0.1/M7 Completion Report.md
```

报告必须包含：

## 1. Execution Summary

```text
observation window
blocks
opportunities
simulations
risk accepts
transactions
```

---

## 2. Real Opportunity

完整记录：

```text
block
pool A
pool B
token pair
fee
reserve
input amount
expected output
expected profit
```

---

## 3. Simulation

记录：

```text
simulation block
simulation state
gas
output
profit
```

---

## 4. Risk

记录：

```text
RiskDecision
```

以及所有 gate。

---

## 5. Transaction

记录：

```text
tx hash
from
to
value
nonce
gas
fee
calldata hash
```

---

## 6. Receipt

记录：

```text
receipt block
status
gas used
effective gas price
logs
```

---

## 7. Actual Asset Delta

记录：

```text
before balance
after balance
delta
```

---

## 8. L2 Cost

记录：

```text
gas used
effective gas price
L2 fee
```

---

## 9. L1 Cost

记录：

```text
L1 fee
source
calculation
```

---

## 10. Profit

必须明确：

```text
gross profit
L2 cost
L1 cost
net realized profit
```

并给出可独立重算公式。

---

## 11. Simulation vs Reality

表格：

```text
metric
simulation
actual
delta
```

至少：

```text
output
gas
profit
fee
net profit
```

---

## 12. Evidence

所有关键事实都必须有：

```text
RPC evidence
transaction hash
receipt
balance query
token balance query
```

---

## 13. Acceptance Matrix

逐项：

```text
A PASS
B PASS
C PASS
...
Q PASS
```

---

## 14. Known Limitations

如实记录：

```text
no real opportunity
sequencer direct unavailable
L1 fee unavailable
```

等等。

---

# 62. 最终原则

M7 的目标不是：

> “让机器人发出一笔交易。”

而是：

> **证明这个机器人能够在真实 GIWA Testnet 市场中，从真实状态发现真实套利机会，并最终证明这笔真实交易扣除所有真实成本后确实赚钱。**

所以最终证据不是：

```text
tx hash
```

而是：

```text
Opportunity
+
Simulation
+
Risk
+
Transaction
+
Receipt
+
Asset Delta
+
L2 Fee
+
L1 Fee
=
Realized Profit > 0
```

这才是 M7。
