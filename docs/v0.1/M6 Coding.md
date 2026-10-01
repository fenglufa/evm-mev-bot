# M6 Coding Task — GIWA Execution：Transaction Builder → Signer → SequencerDirect

## 0. 任务目标

当前项目：

`evm-mev-bot`

目标：

**GIWA Testnet Arbitrage Bot**

M1～M5 已完成。

M6 的目标不是继续扩展套利发现能力，而是把 M5 已经产生的：

```text
Opportunity
    ↓
Simulation
    ↓
Risk
```

继续向下打通：

```text
Transaction Builder
    ↓
Signer
    ↓
GIWA SequencerDirect
    ↓
Submission
    ↓
Receipt
```

最终形成：

```text
Live / Replay
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
Signature
    ↓
GIWA Submission
    ↓
Receipt
```

但必须注意：

> M6 的完成不代表 M7 的“真实盈利套利”已经完成。

M6 证明的是：

**已经被 Simulation + Risk 接受的执行意图，可以被正确转换成 GIWA 交易，并完成签名、提交、Receipt 追踪。**

M7 才负责：

```text
Live Opportunity
→ Simulation
→ Risk
→ Execution
→ Included
→ Actual Profit
```

并至少完成一次真实套利闭环。

---

# 1. 当前基线

M1～M5 当前已经完成：

### M1

真实 GIWA 历史链数据：

```text
chain_id = 91342
```

已经验证：

* Block
* Log
* Sync
* Pool
* Reserve
* Protocol Decoder
* Attested emitter
* StateStore

并确定：

> Sync 是 reserve 的权威来源。

---

### M2

已经完成：

```text
Token
    ↕
Pool
    ↕
Pool
```

Graph：

* TokenNode
* PoolEdge
* DirectedEdge
* GraphSnapshot
* Registry
* Evidence merge/conflict detection

---

### M3

已经完成：

```text
Pool A
   ↓
Token X
   ↓
Pool B
```

两池套利：

```text
WETH → TTAX → WETH
```

使用：

* U256
* 精确 AMM 数学
* bounded integer search
* opportunity lifecycle

已经能够产生真实历史机会。

---

### M4

已经完成：

```text
Opportunity
    ↓
Simulation
    ↓
Risk
```

使用：

```text
revm 43.0.3
```

并且已经证明：

* block context pinned
* historical state pinned
* bytecode pinned
* storage pinned
* balance pinned
* transaction sequence 可执行
* revert 是合法 simulation result
* gas 可测量
* net profit 可以经过 Risk 判断

---

### M5

已经完成：

```text
GIWA Live Source
      ↓
Protocol
      ↓
EventPipeline
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
```

重要实现：

```text
HeadReader
WsHeadReader
HttpChainAdapter
RecordedChainAdapter
EventPipeline
ReplayEngine
```

Live / Replay 使用相同的：

```text
ProtocolEvent
    ↓
StateUpdate
    ↓
StateStore
```

当前 GIWA endpoint 的：

```text
eth_subscribe
```

实际不可用，因此当前 live source 使用：

```text
WebSocket connection
        +
eth_blockNumber polling
```

而不是假装存在稳定的 NewHeads subscription。

Flashblock 当前只能作为：

```text
candidate / observation
```

不能作为 canonical state。

M5 还已经证明：

* live ingestion 不被 simulation 阻塞
* simulation 可以在后台执行
* stale protection 已存在
* simulation 使用 opportunity block，而不是 latest
* no private key
* no signing
* no broadcast

---

# 2. M6 的核心原则

必须严格遵守下面这些边界。

## 2.1 Accept != Send

Risk：

```text
Accept
```

只能表示：

```text
当前机会满足风险规则
```

不能表示：

```text
立即发送交易
```

---

## 2.2 Signed != Submitted

必须分别记录：

```text
Built
Signed
Submitted
```

不能混成一个状态。

---

## 2.3 Submitted != Included

RPC 返回 transaction hash：

```text
Submitted
```

不代表：

```text
Included
```

必须等待 Receipt。

---

## 2.4 Included != Profitable

Receipt 成功也不能说明套利赚钱。

必须区分：

```text
simulation_profit
execution_gas
realized_token_delta
realized_native_delta
```

M7 才最终证明：

```text
realized_profit > 0
```

---

# 3. M6 的范围

M6 必须包含：

### A. Transaction Intent

把 Risk Approved 的机会转换为明确的执行意图。

### B. Transaction Builder

生成准确的 EVM transaction。

### C. Signer

使用隔离的私钥进行本地签名。

### D. Signed Transaction Validation

验证：

* chain ID
* nonce
* sender
* target
* value
* calldata
* gas
* fee
* transaction type

### E. GIWA SequencerDirect

实现 GIWA-specific submission adapter。

### F. Submission

发送 raw signed transaction。

### G. Receipt

轮询 / 获取 transaction receipt。

### H. Execution Lifecycle

完整记录：

```text
Detected
Simulated
RiskApproved
Built
Signed
Submitted
Included
Receipt
```

---

# 4. 明确不做

M6 不允许加入：

* Multi-chain
* BSC
* Base
* Ethereum mainnet
* Arbitrum
* Polygon
* Cross-chain arbitrage
* Flashloan
* V3
* Sandwich
* Liquidation
* Bundle
* Private relay
* MEV-Boost
* Builder marketplace
* AI
* LLM
* Agent
* 自动调参
* 自动修改 Risk threshold
* 自动修改 Opportunity
* 自动修改 Pool reserve
* 自动修改 fee
* 自动修改 transfer tax
* 自动修改 simulation state
* 新套利策略
* Multi-hop arbitrage
* 新 executor contract

---

# 5. 第一原则：先确认 GIWA 实际交易接口

不要假设 GIWA 支持某种 transaction submission 方式。

必须先从真实 GIWA endpoint / 官方文档 / RPC 行为确认：

```text
eth_sendRawTransaction
```

是否可用。

同时确认：

```text
chainId
transaction type
EIP-1559 support
gas price / fee fields
nonce behavior
receipt behavior
```

如果存在 GIWA 专用 sequencer endpoint / direct submission endpoint：

必须先确认其真实协议。

不能根据名称猜测。

不能因为 PRD 写了：

```text
SequencerDirect
```

就直接假设某个 HTTP API 存在。

---

# 6. Chain ID

必须真实验证：

```text
91342
```

是当前目标 GIWA Testnet chain ID。

不能仅仅依赖配置文件。

启动时应能够验证：

```text
configured_chain_id
        ==
rpc_chain_id
```

不一致时：

```text
Execution must refuse to proceed.
```

---

# 7. Transaction Intent

建议增加一个明确的数据结构：

```rust
TransactionIntent
```

至少包含：

```text
opportunity_id
simulation_id
risk_decision_id

chain_id

block_number
block_hash

sender

target

value

calldata

nonce

gas_limit

fee fields

transaction type

expected profit

minimum required profit

state fingerprint
```

其中：

```text
opportunity_id
simulation_id
risk_decision_id
```

必须能够一路追踪到最终 transaction hash。

---

# 8. State Binding

这是 M6 非常重要的一项。

TransactionIntent 不能只是：

```text
to
data
value
```

必须绑定它产生时的状态。

至少绑定：

```text
chain_id
block_number
block_hash
opportunity_id
simulation_id
```

建议增加：

```text
state_fingerprint
```

用于证明：

```text
Transaction
```

来自：

```text
Simulation
```

对应的状态。

---

# 9. Transaction Builder

新增：

```text
TransactionBuilder
```

职责只有：

> 将已经通过 Risk 的 TransactionIntent 转换成标准 EVM Transaction。

Builder 不允许：

* 修改套利路径
* 修改 input amount
* 修改 pool
* 修改 token
* 修改 opportunity
* 修改 fee
* 修改 minimum profit
* 重新寻找机会

Builder 是：

```text
transformation
```

不是：

```text
strategy
```

---

# 10. Transaction Builder 必须验证

Builder 在构造交易前至少验证：

```text
chain_id == 91342

sender != zero

target != zero

nonce valid

gas_limit > 0

calldata valid

value valid
```

并验证：

```text
intent.block_hash
```

与其来源 Simulation 一致。

如果 Simulation 和 Intent 不一致：

```text
BuildRejected
```

---

# 11. Nonce

M6 必须明确 nonce policy。

至少区分：

```text
Pending nonce
Latest confirmed nonce
```

不能简单永久使用：

```text
latest
```

也不能多个并发 execution 随便获取 nonce。

必须解决：

```text
two execution attempts
        ↓
same nonce
```

的问题。

M6 第一阶段建议：

```text
Single execution lane
```

即：

```text
one sender
one nonce allocator
one outstanding transaction
```

先保证正确。

不要为了性能提前实现复杂 nonce manager。

---

# 12. Fee

必须从 GIWA 实际 RPC 验证：

```text
legacy
```

还是：

```text
EIP-1559
```

还是其他 transaction type。

不要硬编码：

```text
EIP1559
```

也不要硬编码：

```text
gasPrice
```

如果 GIWA 支持 EIP-1559：

必须明确：

```text
max_fee_per_gas
max_priority_fee_per_gas
```

来源。

如果只支持 legacy：

则使用：

```text
gas_price
```

所有 fee 计算必须使用：

```text
U256
```

或项目已有精确整数类型。

禁止：

```text
f64
f32
```

进入最终交易构造。

---

# 13. Gas Limit

Gas limit 必须来自明确来源。

优先：

```text
simulation gas used
```

再经过明确 margin policy。

例如：

```text
gas_limit =
simulation_gas_used + configured_margin
```

但不能直接凭感觉：

```text
30_000_000
```

作为最终 live transaction gas limit。

测试 fixture 可以继续使用较大 gas limit。

真实 transaction 必须使用合理值。

---

# 14. Transaction Builder 与 M4 Simulation 的一致性

这是 M6 的核心验收项。

必须建立：

```text
M4 Simulation
       ↓
TransactionIntent
       ↓
TransactionBuilder
       ↓
Raw Transaction
```

然后验证：

```text
Raw Transaction
```

重新解码之后：

```text
sender
target
value
calldata
nonce
gas_limit
fee
chain_id
```

与：

```text
TransactionIntent
```

完全一致。

不能只比较：

```text
to
data
```

---

# 15. Builder → Simulation Round Trip

必须增加测试：

```text
Opportunity
    ↓
Simulation
    ↓
Risk
    ↓
TransactionIntent
    ↓
TransactionBuilder
    ↓
BuiltTransaction
```

然后将 BuiltTransaction 再用于：

```text
simulation / validation
```

验证：

```text
builder did not change execution semantics
```

特别检查：

```text
calldata
value
target
gas
sender
```

---

# 16. Signer

新增：

```text
Signer
```

Signer 只负责：

```text
unsigned transaction
        ↓
signature
        ↓
signed raw transaction
```

Signer 不负责：

* Opportunity
* Simulation
* Risk
* RPC
* Strategy
* Nonce discovery
* Gas discovery

---

# 17. Private Key 安全

M6 不允许：

```text
private key
```

进入：

* Git
* source code
* fixture
* logs
* report
* test output
* error message
* metrics
* JSON evidence

必须使用：

```text
environment variable
```

或者其他本地 secret mechanism。

例如：

```text
GIWA_EXECUTION_PRIVATE_KEY
```

但具体名称可以根据现有项目规范决定。

必须：

```text
.gitignore
```

检查。

---

# 18. Signer 测试

必须使用测试私钥验证：

```text
unsigned tx
    ↓
sign
    ↓
recover sender
```

最终：

```text
recovered_sender == expected_sender
```

必须通过。

同时验证：

```text
chain_id
```

确实进入 signature domain。

---

# 19. 禁止 Private Key 默认加载

非常重要：

默认：

```text
cargo run
```

不能因为发现环境变量存在，就自动开始执行真实交易。

建议 execution mode 明确区分：

```text
BuildOnly
SignOnly
Submit
```

默认：

```text
BuildOnly
```

或者：

```text
SignOnly
```

但不能默认 Submit。

---

# 20. Execution Mode

建议实现：

```rust
enum ExecutionMode {
    BuildOnly,
    SignOnly,
    Submit,
}
```

如果项目已有 execution mode 设计，可以沿用。

必须满足：

### BuildOnly

允许：

```text
Opportunity
→
Risk
→
Build
```

但：

```text
No private key required
No network submission
```

---

### SignOnly

允许：

```text
Build
→
Sign
```

但：

```text
No broadcast
```

---

### Submit

才允许：

```text
Build
→
Sign
→
Submit
```

必须显式指定。

---

# 21. Submission Adapter

新增抽象：

```rust
trait TransactionSubmitter
```

职责：

```text
submit(raw_transaction)
    -> transaction_hash
```

以及：

```text
get_receipt(transaction_hash)
```

如果需要：

```text
get_transaction()
```

也可以加入。

---

# 22. GIWA SequencerDirect

实现：

```text
GiwaSequencerDirect
```

这是 GIWA-specific adapter。

不要把 GIWA-specific RPC 写入：

```text
TransactionBuilder
Signer
Opportunity
Simulation
Risk
```

必须隔离：

```text
execution/
    builder
    signer
    submitter
    receipt
    giwa/
        sequencer_direct
```

具体目录以当前项目结构为准，不要求机械照抄。

---

# 23. eth_sendRawTransaction

如果真实 GIWA RPC 支持：

```text
eth_sendRawTransaction
```

必须真实验证。

验证至少包括：

```text
raw tx accepted
tx hash returned
```

然后：

```text
getTransactionReceipt(tx_hash)
```

确认 Receipt。

---

# 24. 如果 GIWA 有 Sequencer Direct

如果 GIWA 实际存在专用 sequencer endpoint：

必须：

1. 验证 endpoint
2. 验证 request format
3. 验证 authentication
4. 验证 response
5. 验证 tx hash
6. 验证 receipt

并实现：

```text
GiwaSequencerDirect
```

但不要为了满足接口而伪造。

如果真实接口无法验证：

```text
SequencerDirect = BLOCKED
```

不能写 mock 假装完成。

---

# 25. Submission 不允许自动重试导致重复交易

必须考虑：

```text
submit
    ↓
timeout
```

此时不能立即：

```text
retry
```

否则可能：

```text
same signed transaction
```

重复发送。

必须先判断：

```text
transaction hash
```

或者：

```text
nonce
```

状态。

原则：

> Submission timeout != submission failure.

必须能够区分：

```text
Rejected
Unknown
Accepted
Included
```

---

# 26. Receipt Tracking

增加：

```text
ReceiptTracker
```

状态至少：

```text
Submitted
Pending
Included
Reverted
NotFound
Timeout
```

Receipt 必须记录：

```text
transaction_hash
block_number
block_hash
transaction_index
status
gas_used
effective_gas_price
```

如果 receipt 有：

```text
logs
```

也保留必要信息。

---

# 27. Receipt 与原始交易绑定

必须验证：

```text
receipt.transaction_hash
==
submitted_transaction_hash
```

并验证：

```text
receipt.block_hash
```

与链上的 block 一致。

不能只因为 RPC 返回 receipt 就认为交易成功。

---

# 28. Transaction Status

建议生命周期：

```rust
enum ExecutionStatus {
    Detected,
    Simulated,
    RiskApproved,
    Built,
    Signed,
    Submitted,
    Included,
    Reverted,
    Failed,
}
```

不要把所有状态都塞进：

```text
bool success
```

---

# 29. Execution Record

增加统一：

```text
ExecutionRecord
```

至少包含：

```text
execution_id

opportunity_id
simulation_id
risk_decision_id

chain_id

opportunity_block
opportunity_block_hash

sender
target

nonce

transaction_hash

status

created_at
built_at
signed_at
submitted_at
included_at

gas_limit
gas_used

fee fields

simulation_profit
estimated_execution_cost
```

M6 暂时不要求完整 realized profit。

M7 再增加：

```text
realized_profit
```

---

# 30. Idempotency

Execution 必须有：

```text
execution_id
```

并且一个：

```text
RiskApproved opportunity
```

不能因为 event loop 重复触发而生成无限交易。

至少必须防止：

```text
same opportunity
+
same simulation
+
same state
```

被重复执行。

建议 key：

```text
(opportunity_id, simulation_id, state_fingerprint)
```

---

# 31. Stale Opportunity

M5 已经实现 stale protection。

M6 必须继续遵守。

如果：

```text
opportunity.block_hash
```

已经不是当前允许执行的状态：

```text
ExecutionRejected
```

不能：

```text
force send
```

---

# 32. Simulation → Execution Gate

提交之前必须再次确认：

```text
simulation.success == true

risk.decision == Accept

opportunity not stale

chain_id matches

block binding valid

sender balance sufficient

nonce valid
```

任何一个不满足：

```text
No submission.
```

---

# 33. Balance Check

至少检查：

```text
native balance
```

能够覆盖：

```text
gas_limit * max_fee
```

以及 transaction value。

如果套利本身需要 token balance：

也必须验证。

不要只依赖 M4 simulation 中的：

```text
state override
```

因为 M4 使用过：

```text
sender native balance override
```

M6 的真实执行不能继续依赖这个 override。

---

# 34. Real Execution 与 M4 Override 的边界

这是一个关键问题。

M4：

```text
state override
```

只是为了证明：

```text
execution semantics
```

M6：

```text
real execution
```

必须使用真实链上账户状态。

禁止：

```text
override balance
override reserve
override token balance
override fee
override tax
```

来制造可以发送的交易。

---

# 35. Safe Submission Strategy

M6 的第一次真实 submission 必须非常谨慎。

不要为了测试直接发送：

```text
高价值套利
```

也不要：

```text
人为修改 opportunity
```

来产生利润。

优先使用：

```text
low-value / controlled test transaction
```

验证：

```text
Build
→ Sign
→ Submit
→ Receipt
```

如果项目已有安全的测试交易方案，可以采用。

如果无法在不破坏套利语义的情况下找到安全测试交易：

```text
real submission = BLOCKED
```

可以接受。

但：

```text
Builder
Signer
Submission interface
Receipt tracking
```

仍然必须完成并测试。

---

# 36. 不允许伪造 M7

如果 M6 最终只有：

```text
Build PASS
Sign PASS
Submit BLOCKED
```

必须如实报告：

```text
M6 partial
```

不能写：

```text
Execution complete
```

更不能写：

```text
Real arbitrage complete
```

---

# 37. Logging

日志必须包含：

```text
execution_id
opportunity_id
simulation_id
risk_decision_id
```

如果已经有：

```text
transaction_hash
```

再记录：

```text
transaction_hash
```

但是：

```text
private key
```

绝对不能出现。

---

# 38. Metrics

至少增加：

```text
execution_build_latency
sign_latency
submission_latency
receipt_latency
```

以及：

```text
execution_build_success
execution_sign_success
execution_submit_success
execution_receipt_success
execution_revert
```

M8 再做 percentile：

```text
p50
p95
p99
```

M6 不需要过早做复杂 telemetry 系统。

---

# 39. Error Taxonomy

不要把所有错误都变成：

```text
ExecutionError
```

至少区分：

```text
ChainMismatch
InvalidIntent
StaleOpportunity
InsufficientBalance
NonceUnavailable
BuildFailed
SigningFailed
SubmissionRejected
SubmissionUnknown
ReceiptTimeout
TransactionReverted
```

错误必须能够帮助定位：

```text
为什么没有执行
```

---

# 40. 测试要求

必须增加单元测试和 integration tests。

至少覆盖：

### Transaction Builder

```text
valid intent
invalid chain
invalid target
invalid nonce
invalid gas
invalid calldata
stale state
```

---

### Signer

```text
sign
recover sender
chain id
invalid key
```

---

### Submission

```text
mock submit success
mock submit rejection
mock timeout
mock unknown
```

---

### Receipt

```text
success receipt
reverted receipt
missing receipt
timeout
```

---

### Execution lifecycle

必须测试：

```text
Detected
→ Simulated
→ RiskApproved
→ Built
→ Signed
→ Submitted
→ Included
```

以及：

```text
RiskRejected
→ no build
```

```text
Stale
→ no sign
```

```text
InsufficientBalance
→ no submit
```

---

# 41. 真实链验证

必须使用真实 GIWA Testnet。

至少验证：

```text
chain_id
latest block
gas / fee behavior
nonce
balance
transaction submission method
receipt
```

如果进行真实 transaction submission：

必须记录：

```text
tx hash
block number
receipt
```

并提供 evidence。

---

# 42. 真实交易验证原则

如果真实发送交易：

必须明确：

```text
这是 M6 execution validation transaction
```

或者：

```text
这是 M7 real arbitrage transaction
```

二者不能混淆。

如果只是：

```text
ordinary test transaction
```

不能把它算成：

```text
arbitrage execution
```

---

# 43. M6 不需要真实盈利

M6 可以完成：

```text
Transaction construction
+
Signing
+
Submission
+
Receipt
```

但：

```text
realized arbitrage profit
```

属于 M7。

因此：

```text
M6 COMPLETE
```

并不等于：

```text
M7 COMPLETE
```

---

# 44. 与现有 M4 Simulator 的集成

不要重写 Simulator。

必须复用：

```text
crates/simulation
```

已经存在的能力。

Execution 只能消费：

```text
SimulationResult
```

而不是重新读取：

```text
PoolState
```

自己重新计算。

---

# 45. 与 Risk 的集成

不要重新实现 Risk。

复用：

```text
crates/risk
```

已经存在的规则。

Execution 输入必须来自：

```text
RiskDecision
```

而不是：

```text
Opportunity
```

直接 Send。

---

# 46. 建议执行架构

最终结构可以接近：

```text
crates/
    execution/
        intent
        builder
        signer
        submitter
        receipt
        lifecycle
        error
        giwa/
            sequencer_direct
```

但如果当前 workspace 已经有合适结构，也可以整合。

原则：

> 不为了目录形式而增加无意义 crate。

---

# 47. 不要增加独立服务

M6 仍然保持：

```text
single process
```

不要：

```text
execution-service
signer-service
submission-service
```

这些都应该在同一进程内部作为模块存在。

---

# 48. Execution 不应该反向依赖 Strategy

依赖方向：

```text
Opportunity
    ↓
Simulation
    ↓
Risk
    ↓
Execution
```

而不是：

```text
Execution
    ↓
Opportunity strategy
```

Execution 是基础设施。

---

# 49. Replay / Live 都必须能产生 Execution Input

最终：

```text
Replay
    ↓
Opportunity
    ↓
Simulation
    ↓
Risk
    ↓
ExecutionIntent
```

和：

```text
Live
    ↓
Opportunity
    ↓
Simulation
    ↓
Risk
    ↓
ExecutionIntent
```

必须使用相同 Execution API。

这样 M6 可以用历史真实机会验证执行链路。

---

# 50. Historical Execution Fixture

必须利用 M3/M4 已有真实历史机会。

特别是：

```text
WETH / TTAX
Pool A
Pool B
block 37191169
```

但必须注意：

M4 已经证明：

```text
analytical ask
```

会 revert。

所以不能因为 M3 有 gross profit，就直接把它当成可发送套利。

M6 可以使用：

```text
RiskRejected
```

的机会验证：

```text
Execution must stop.
```

然后另外使用一个：

```text
RiskApproved
```

fixture 验证：

```text
Builder → Signer
```

如果没有真实 RiskApproved opportunity：

可以建立**完全基于真实历史状态的数据 fixture**，但不得伪造 profit 或 reserve。

---

# 51. 不允许修改真实历史数据

禁止：

```text
修改 reserve
修改 fee
修改 tax
修改 balance
修改 block
修改 receipt
```

来制造：

```text
RiskApproved
```

机会。

---

# 52. Signed Transaction Evidence

至少输出一份 evidence：

```text
chain_id
nonce
type
to
value
gas_limit
fee
data_hash
signed_tx_hash
recovered_sender
```

绝不输出：

```text
private key
```

---

# 53. Submission Evidence

如果真实提交：

至少记录：

```text
transaction_hash
submission_endpoint_type
submitted_at
receipt_block
receipt_status
gas_used
effective_gas_price
```

如果 endpoint 不支持提交：

必须记录：

```text
BLOCKED
```

和真实 RPC response。

---

# 54. Acceptance Criteria

## A — Workspace

```text
cargo fmt --check
```

PASS。

---

## B — Check

```text
cargo check --workspace
```

PASS。

---

## C — Tests

```text
cargo test --workspace
```

PASS。

---

## D — Clippy

```text
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

PASS。

---

## E — Chain ID

真实 RPC：

```text
91342
```

验证 PASS。

---

## F — Transaction Type

真实验证 GIWA：

```text
legacy / EIP-1559 / actual supported type
```

PASS。

---

## G — Builder

RiskApproved intent 能生成：

```text
unsigned transaction
```

PASS。

---

## H — Builder Round Trip

Builder 输出重新 decode 后：

```text
sender
target
value
calldata
nonce
gas
fee
chain_id
```

全部一致。

PASS。

---

## I — Signer

```text
sign
→ recover sender
```

PASS。

---

## J — Private Key Isolation

证明：

```text
private key
```

不会进入：

```text
repo
logs
fixtures
reports
```

PASS。

---

## K — Risk Gate

RiskRejected：

```text
no build
```

PASS。

---

## L — Stale Gate

Stale：

```text
no sign
no submit
```

PASS。

---

## M — Balance Gate

insufficient balance：

```text
no submit
```

PASS。

---

## N — Submission

如果 GIWA RPC 支持：

```text
eth_sendRawTransaction
```

必须真实验证。

PASS / BLOCKED。

---

## O — Receipt

如果 transaction submitted：

必须真实获取：

```text
receipt
```

PASS / BLOCKED。

---

## P — Revert

如果交易 receipt：

```text
status = reverted
```

系统必须明确记录：

```text
Reverted
```

而不是：

```text
Success
```

---

## Q — Lifecycle

至少验证：

```text
Detected
→
Simulated
→
RiskApproved
→
Built
→
Signed
→
Submitted
→
Included
```

PASS。

---

## R — Idempotency

相同：

```text
opportunity
+
simulation
+
state
```

不能产生重复 execution。

PASS。

---

## S — State Binding

TransactionIntent 必须绑定：

```text
block_number
block_hash
simulation_id
```

PASS。

---

## T — No Override

真实 execution：

```text
no state override
```

PASS。

---

## U — No Automatic Send

默认运行模式：

```text
no automatic submission
```

PASS。

---

## V — Real Evidence

所有真实链结论必须有：

```text
RPC evidence
```

或：

```text
transaction receipt
```

PASS。

---

## W — No Fake Completion

任何无法验证的 GIWA submission capability：

必须：

```text
BLOCKED
```

不能：

```text
PASS
```

---

# 55. M6 完成标准

M6 只有在下面链路真实打通后，才可以标记 COMPLETE：

```text
RiskApproved
      ↓
TransactionIntent
      ↓
TransactionBuilder
      ↓
Unsigned Tx
      ↓
Signer
      ↓
Signed Raw Tx
      ↓
GIWA Submission
      ↓
Transaction Hash
      ↓
Receipt
```

如果：

```text
Builder + Signer
```

完成，但：

```text
Submission
```

因为 GIWA provider 限制无法验证：

则必须明确：

```text
M6 PARTIAL / BLOCKED
```

不能把它写成 COMPLETE。

---

# 56. M6 与 M7 的边界

M6：

```text
Execution infrastructure
```

M7：

```text
Real profitable arbitrage
```

因此 M6 不要求：

```text
realized_profit > 0
```

M7 才要求：

```text
real opportunity
+
simulation
+
risk approval
+
real transaction
+
inclusion
+
receipt
+
realized profit verification
```

---

# 57. M6 与 M8 的边界

M6 暂时不做：

```text
latency optimization
lock optimization
memory allocation optimization
parallel nonce manager
high-throughput execution
advanced retry
circuit breaker
Flashblock ultra-low-latency execution
```

这些进入：

```text
M8
```

---

# 58. 最终代码检查

完成后必须检查：

```bash
git diff
git status
```

确认：

* 没有 private key
* 没有 fake endpoint
* 没有 fake transaction hash
* 没有 fake receipt
* 没有 fake opportunity
* 没有硬编码测试余额
* 没有生产代码 unwrap/expect/panic
* 没有 f64/f32 进入金额/fee/gas 核心计算
* 没有修改 M1-M5 correctness semantics

---

# 59. 最终 Completion Report

完成后必须生成：

```text
docs/m6-completion-report.md
```

报告必须包含：

## 1. Summary

一句话说明 M6 最终状态：

```text
COMPLETE
```

或者：

```text
PARTIAL
```

或者：

```text
BLOCKED
```

---

## 2. Code Changes

列出：

```text
新增 crate / module
新增 trait
新增 struct
新增 adapter
新增 tests
```

---

## 3. Transaction Construction

说明：

```text
transaction type
chain id
nonce
gas
fee
value
calldata
```

实际来源。

---

## 4. Signer

说明：

```text
signing
sender recovery
private key handling
```

---

## 5. Submission

说明：

```text
RPC / SequencerDirect
```

是否真实验证。

---

## 6. Receipt

说明：

```text
tx hash
block
status
gas
```

如果没有真实交易：

明确：

```text
BLOCKED
```

---

## 7. Real Evidence

必须列出：

```text
RPC endpoint
block
transaction hash
receipt
```

如果有。

---

## 8. Tests

必须列出：

```text
cargo fmt --check
cargo check --workspace
cargo test --workspace
cargo clippy ...
```

真实结果。

---

## 9. Acceptance Matrix

逐项：

```text
A PASS
B PASS
C PASS
...
```

不得只写：

```text
all passed
```

---

## 10. Known Limitations

必须诚实记录：

例如：

```text
GIWA eth_sendRawTransaction unavailable
SequencerDirect undocumented
No safe real submission target
```

等等。

---

# 60. 最终要求

M6 最重要的不是代码量。

真正目标是：

```text
M5:
Opportunity
    ↓
Simulation
    ↓
Risk

M6:
Risk
    ↓
Transaction
    ↓
Signature
    ↓
GIWA
    ↓
Receipt
```

并且整个过程中：

```text
不伪造机会
不伪造利润
不伪造交易
不伪造 receipt
不伪造 GIWA API
不修改历史事实
不绕过 Risk
不使用 state override 进行真实执行
```

如果真实 GIWA 环境限制导致某一步无法完成：

**如实标记 BLOCKED，并提供实际 RPC 证据。**

不要通过 mock、假 endpoint、修改测试数据来制造“完成”。

M6 的最终目的，是让 M7 可以在这个基础上直接进入：

```text
Real Opportunity
        ↓
Risk
        ↓
Real Transaction
        ↓
GIWA
        ↓
Actual Arbitrage
        ↓
Realized Profit
```

而不需要重新设计 Execution 层。