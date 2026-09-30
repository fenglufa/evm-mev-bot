# evm-mev-bot v0.1 — M1 Coding Agent Task

## 0. 任务定位

你正在开发一个全新的 Rust 项目：

`evm-mev-bot`

当前版本：

`v0.1`

当前 Milestone：

`M1 — Real Pool State`

v0.1 的完整目标是建立：

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
```

但当前只允许实现到：

```text
Chain
  ↓
Protocol
  ↓
State
  ↓
PoolState
```

当前 M1 **不实现 Graph、Arbitrage Opportunity、Simulation、Execution**。

---

# 1. M1 最终目标

M1 必须证明：

```text
Real Block
    ↓
Real Transaction / Receipt
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

最终系统必须能够从真实 EVM 历史数据恢复至少一个真实 Pool 在指定 block 下的：

```text
chain_id
pool_address
token0
token1
reserve0
reserve1
block_number
log_index
```

并且该结果能够通过自动化测试和 replay 重现。

---

# 2. 最重要的原则

## 2.1 不允许预设协议事实

不要假设：

```text
GIWA = Uniswap V2
```

也不要假设：

```text
某地址 = Factory
某地址 = Pair
某地址 = Router
某地址 = Token
```

更不要根据名称、地址格式、单个 Transfer 或单个 Swap 推断协议身份。

所有协议事实必须通过实际数据验证。

---

# 3. 已有数据源

开发环境中可能存在一个已经积累了大量历史 EVM 数据的本地目录：

```text
/Volumes/superfs/giwa-mev
```

该目录是**数据调查和验证来源**。

不要将该项目整体复制进当前项目。

不要把其中的 semantic 系统迁移到当前项目。

不要建立对该项目代码的运行时依赖。

只允许读取其中的数据、配置、fixture、历史输出和必要的代码实现，以帮助当前项目确认真实链上事实。

---

# 4. Agent 开始工作前必须调查

在修改业务代码之前，先检查当前项目：

```bash
pwd
find . -maxdepth 3 -type f | sort
cat Cargo.toml
```

确认当前项目实际状态。

然后检查已有数据：

```bash
find /Volumes/superfs/giwa-mev/data \
     /Volumes/superfs/giwa-mev/config \
     /Volumes/superfs/giwa-mev/tests \
     -maxdepth 4 -type f | sort
```

重点调查：

```text
data/evidence/
data/raw/
data/raw/index.jsonl
config/
tests/
src/feed/
src/assembler/
src/rpc/
src/protocol/
src/replay/
```

不要一次性加载所有 `.bin` 文件。

先利用：

```text
index
evidence
summary
candidate
relationship
event
movement
```

等索引信息定位具体 block / tx / log 后，再读取对应 raw 数据。

---

# 5. 数据调查策略

采用以下优先级：

```text
已有 Evidence
      ↓
已有 Index
      ↓
已有 Raw Data
      ↓
RPC 验证
      ↓
必要时重新获取数据
```

不要一开始重新扫描整个链。

不要因为方便而下载大量新数据。

---

# 6. 需要建立的事实链

最终必须能够建立至少一条完整事实链：

```text
Factory
   ↓
Pool
   ├── token0
   ├── token1
   └── Pool Events
          ↓
         Sync
          ↓
   reserve0 / reserve1
```

但实际协议不一定是上述结构。

如果调查发现协议结构不同：

**以真实数据为准。**

不要为了符合本任务中的模型而修改事实。

---

# 7. Pool 身份确认规则

一个地址只有在有足够证据证明它是 AMM Pool 后，才能注册为 Pool。

至少需要综合多个证据来源，例如：

```text
Bytecode
Event signature
Function signature
Observed transactions
Token relationships
Protocol relationships
State-changing behavior
```

不能仅凭：

```text
Transfer
Swap-like event
两个 token
```

就认定为 Pool。

Agent 必须在最终报告中解释：

```text
为什么这个地址是 Pool？
证据是什么？
来自哪个 block？
来自哪个 transaction？
哪个 log？
```

---

# 8. Token 身份确认

Pool 的 token0 / token1 必须从真实链上事实确认。

优先顺序：

```text
Protocol-defined token0/token1
        ↓
Pool contract call
        ↓
Observed event / transaction evidence
        ↓
Storage evidence
```

如果协议没有 `token0()` / `token1()`：

根据真实协议结构实现对应的恢复方式。

不要假设所有 AMM 都具有 Uniswap V2 的 ABI。

---

# 9. Reserve 恢复原则

这是 M1 最重要的规则。

如果协议存在类似：

```text
Sync(reserve0, reserve1)
```

的 authoritative state event：

```text
Sync
  ↓
PoolState
```

必须以该事件作为正常历史状态恢复的主要来源。

不要使用：

```text
Swap amount0In
Swap amount1In
```

自行推算 reserve 作为正常状态来源。

原因是：

```text
Swap ≠ authoritative final reserve state
```

如果真实协议没有 Sync：

必须根据真实协议设计确定 authoritative state source。

---

# 10. StateUpdate

Protocol Event 与 StateStore 之间必须存在明确的状态更新层：

```text
Raw Log
   ↓
Protocol Decoder
   ↓
ProtocolEvent
   ↓
StateUpdate
   ↓
StateStore
```

不要让 Protocol Decoder 直接修改 StateStore。

建议模型：

```rust
enum StateUpdate {
    PoolCreated(PoolMeta),

    PoolSynced {
        pool: PoolId,
        reserve0: U256,
        reserve1: U256,
        block_number: BlockNumber,
        log_index: u32,
    },
}
```

如果真实协议需要其他状态更新类型，可以增加。

但不要提前设计大量没有实际用途的状态类型。

---

# 11. PoolMeta / PoolState 必须分离

不要把 Pool 的静态身份和动态状态混在一个结构中。

建议：

```rust
struct PoolMeta {
    id: PoolId,
    protocol: ProtocolId,
    token0: TokenId,
    token1: TokenId,
    fee: Fee,
    pool_type: PoolType,
}
```

以及：

```rust
struct PoolState {
    pool: PoolId,
    reserve0: U256,
    reserve1: U256,
    block_number: BlockNumber,
    log_index: u32,
}
```

具体字段根据真实协议调整。

---

# 12. Identity

所有链相关对象必须显式包含 ChainId。

不要使用：

```text
Address
```

作为跨链全局身份。

至少：

```rust
TokenId = {
    chain_id,
    address,
}
```

以及：

```rust
PoolId = {
    chain_id,
    address,
}
```

---

# 13. Chain Layer

建立链无关的接口。

建议：

```rust
trait ChainAdapter {
    fn chain_id(&self) -> ChainId;

    async fn latest_block(&self) -> Result<BlockNumber>;

    async fn get_block(
        &self,
        number: BlockNumber,
    ) -> Result<Block>;

    async fn get_receipts(
        &self,
        block: BlockNumber,
    ) -> Result<Vec<TransactionReceipt>>;

    async fn get_logs(
        &self,
        filter: LogFilter,
    ) -> Result<Vec<ChainLog>>;
}
```

具体接口可以根据实际 Rust async trait / alloy 版本调整。

不要让上层直接依赖具体 RPC provider。

---

# 14. Raw RPC 类型不能泄漏到上层

例如：

```text
alloy provider response
```

不应该直接进入：

```text
state/
graph/
opportunity/
```

Chain Layer 负责：

```text
RPC
 ↓
Normalized Chain Types
```

例如：

```rust
struct ChainBlock { ... }

struct ChainTransaction { ... }

struct ChainReceipt { ... }

struct ChainLog { ... }
```

具体实现可以使用 alloy。

---

# 15. ChainEvent

协议层不应该知道 Flashblocks 的具体结构。

如果需要支持：

```text
Final RPC Block
Flashblocks
```

应该统一进入：

```rust
enum ChainEvent {
    Block(BlockEvent),
    // Future variants can be added.
}
```

Flashblocks 必须在 Chain Layer 内完成组装。

Protocol Layer 只看到已经规范化的数据。

---

# 16. Flashblocks

如果当前 M1 需要使用已有 Flashblocks 数据进行历史恢复：

必须先确认：

```text
payload_id
index
base
diff
```

之间的关系。

不要把单个 Flashblock frame 当成完整 block。

必须处理：

```text
index 0
index 1
index 2
index 3
index 4
```

等 frame 的组合。

如果 payload 不完整：

```text
不要伪造完整 block
```

可以：

```text
discard incomplete payload
```

或者：

```text
fallback to finalized RPC block
```

具体选择必须保持数据正确性优先。

---

# 17. 同 Block 排序

状态恢复必须保证 deterministic ordering。

至少：

```text
transaction_index
    ↓
log_index
```

必须按照链上的实际执行顺序处理。

不要按照：

```text
文件名
网络返回顺序
Hash 字典序
```

排序。

---

# 18. StateStore

实现一个进程内状态存储。

M1 不需要数据库。

建议：

```rust
trait StateStore {
    fn pool_meta(&self, pool: PoolId) -> Option<&PoolMeta>;

    fn pool_state(&self, pool: PoolId) -> Option<&PoolState>;

    fn apply(&mut self, update: StateUpdate);

    fn snapshot(&self, block: BlockNumber) -> StateSnapshot;
}
```

可以使用：

```text
HashMap
```

等简单结构。

不要为了 M1 引入：

```text
PostgreSQL
Redis
Kafka
ClickHouse
```

等基础设施。

---

# 19. StateStore 正确性

必须验证：

```text
Block N
  Sync(pool=A, reserve=100/200)

Block N+1
  Sync(pool=A, reserve=120/180)

        ↓

Current State

pool A
reserve0 = 120
reserve1 = 180
```

状态必须按照事件顺序演进。

---

# 20. Replay

M1 必须具备最基本的历史 replay 能力：

```text
BlockRange
    ↓
Block
    ↓
Receipt
    ↓
Log
    ↓
ProtocolEvent
    ↓
StateUpdate
    ↓
StateStore
```

同样的数据执行两次：

```text
Run A
Run B
```

必须得到完全一致的 PoolState。

---

# 21. RPC eth_call 的使用规则

允许使用：

```text
eth_call
```

但不能把它作为正常历史热路径中的 reserve 来源。

允许用于：

```text
Pool initialization
Validation
Recovery
Cross-check
```

例如：

```text
Sync says:
reserve0 = 100
reserve1 = 200

eth_call getReserves():
100 / 200
```

可以用于验证。

但正常 replay：

```text
Log
 ↓
Sync
 ↓
StateStore
```

不应该每个 block 都：

```text
eth_call getReserves()
```

---

# 22. 数值类型

所有 reserve、amount、token amount 必须使用：

```text
U256
```

禁止使用：

```text
f64
```

作为最终状态真值。

如果后续 Graph 使用浮点权重进行快速筛选，可以单独转换。

但：

```text
PoolState
StateUpdate
Reserve
Amount
```

必须保持精确整数。

---

# 23. Protocol Adapter

建立：

```rust
trait ProtocolAdapter {
    fn protocol_id(&self) -> ProtocolId;

    fn decode_log(
        &self,
        log: &ChainLog,
    ) -> Result<Option<ProtocolEvent>>;

    fn is_pool(
        &self,
        address: Address,
    ) -> bool;
}
```

不要假设所有协议共享同一个 ABI。

协议-specific decoder 应该放在：

```text
protocol/
```

中。

---

# 24. v0.1 第一协议

v0.1 的第一条真实协议路径是：

```text
真实 GIWA 数据
        ↓
真实协议
        ↓
真实 Pool
        ↓
真实 State
```

但协议身份必须由 Agent 调查得到。

如果最终确认是 V2-style：

实现：

```text
V2-like Pool Adapter
```

如果发现不是：

实现真实协议对应 Adapter。

不要为了复用模板而强行套 V2。

---

# 25. Evidence

M1 必须建立最小的事实证据结构。

例如：

```rust
struct EvidenceRef {
    source: EvidenceSource,
    block_number: Option<BlockNumber>,
    transaction_hash: Option<TransactionHash>,
    log_index: Option<u32>,
}
```

可以根据实际情况调整。

每一个最终确认的 Pool 至少应该能够追溯到：

```text
Pool identity evidence
Token evidence
State evidence
```

---

# 26. Fixtures

必须创建小型 deterministic fixtures。

至少包含：

### Fixture 1：No Sync

```text
Pool
 ↓
Swap
```

确认不能错误生成新的 authoritative reserve。

### Fixture 2：Single Sync

```text
Sync(100, 200)
```

预期：

```text
reserve0 = 100
reserve1 = 200
```

### Fixture 3：Multiple Sync

```text
Block 100
Sync(100, 200)

Block 101
Sync(120, 180)
```

最终：

```text
120 / 180
```

### Fixture 4：Same Block Ordering

同一个 block：

```text
tx_index=1 log_index=2
tx_index=1 log_index=3
tx_index=2 log_index=0
```

必须按照真实顺序应用。

### Fixture 5：Invalid Data

例如：

```text
reserve0 = 0
reserve1 = 0
```

或者无法解析的 log。

系统不能 panic。

---

# 27. Real Data Acceptance Test

这是 M1 最重要的测试。

必须选择一个真实历史 block：

```text
Real Block N
```

然后完成：

```text
Block N
 ↓
Receipt
 ↓
Log
 ↓
Protocol Event
 ↓
Pool
 ↓
Sync
 ↓
PoolState
```

测试至少验证：

```text
chain_id
pool
token0
token1
reserve0
reserve1
block
```

如果条件允许，再通过 RPC `eth_call` 对同一 block 的 reserve 做交叉验证。

注意：

RPC validation 只是 validation，不改变 replay 的 authoritative source。

---

# 28. 失败时的行为

如果 Agent 无法确认真实协议：

不要：

```text
猜
```

不要：

```text
写死一个地址
```

不要：

```text
制造 fixture 假装真实验证通过
```

应该输出：

```text
BLOCKED

Reason:
无法通过现有数据确认 Protocol / Pool identity。

Evidence inspected:
...

Missing evidence:
...

Next required data:
...
```

然后停止扩展到 M2。

---

# 29. 不允许做的事情

当前 M1 明确禁止：

```text
Graph
Arbitrage detection
Bellman-Ford
Cycle search
Opportunity ranking
REVM
Transaction simulation
Bundle
Private relay
Bribe
Signer
Nonce manager
Execution
Mempool strategy
V3
V4
Liquidation
Sandwich
NFT MEV
Intent MEV
AI strategy
Dashboard
Web UI
Microservices
Database infrastructure
```

尤其不要因为“未来需要”而提前实现。

---

# 30. 不允许复制旧项目架构

不要复制：

```text
semantic/
selector/
target/
ABI census
code census
generic event classification
generic semantic graph
```

这些不是当前项目的核心。

当前项目只需要：

```text
Chain
Protocol
State
```

---

# 31. Crate 结构

如果项目尚未建立 workspace，可以建立：

```text
evm-mev-bot/
├── Cargo.toml
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
├── contracts/
├── config/
├── fixtures/
├── data/
├── docs/
└── tests/
```

但是：

**M1 只需要真正实现：**

```text
core
chain
protocol
state
replay
```

其他 crate 可以只建立最小接口或暂时为空。

不要为了目录完整而制造大量代码。

---

# 32. 依赖方向

必须保持：

```text
core
 ↑
chain
 ↑
protocol
 ↑
state
 ↑
replay
```

更准确地说：

```text
core
  ↑
  ├── chain
  ├── protocol
  └── state

replay
  ├── chain
  ├── protocol
  └── state
```

禁止：

```text
state → concrete RPC
protocol → replay
core → protocol implementation
core → GIWA
```

---

# 33. GIWA-specific code 的位置

如果最终确认当前真实数据对应某个 GIWA-specific protocol：

可以实现：

```text
protocol/src/giwa/
```

或者：

```text
crates/protocol/src/adapters/giwa.rs
```

但 GIWA-specific 内容不能污染：

```text
core
chain
state
```

未来添加：

```text
Ethereum
Base
Arbitrum
BSC
```

时，核心 State 模型不应该被迫修改。

---

# 34. M1 开发顺序

严格按照：

```text
Step 1
建立 workspace / core types

↓

Step 2
Chain normalized types

↓

Step 3
ChainAdapter

↓

Step 4
Replay input abstraction

↓

Step 5
调查真实历史数据

↓

Step 6
确认真实 Protocol

↓

Step 7
实现 Protocol Adapter

↓

Step 8
ProtocolEvent

↓

Step 9
StateUpdate

↓

Step 10
StateStore

↓

Step 11
真实 Pool State replay

↓

Step 12
Fixture tests

↓

Step 13
Real Data Acceptance Test

↓

Step 14
cargo fmt
cargo check
cargo test

↓

Step 15
M1 Evidence Report
```

---

# 35. 每完成一个阶段必须验证

不要最后才测试。

每一个重要阶段都执行：

```bash
cargo fmt --check
cargo check
cargo test
```

如果存在 clippy：

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

根据项目实际依赖决定是否执行。

---

# 36. M1 完成定义

只有同时满足以下条件，才能宣布：

```text
M1 COMPLETE
```

### A. Chain

可以读取真实历史 block / receipt / logs。

### B. Protocol

至少确认一个真实协议的真实 Pool。

### C. Pool

至少确认：

```text
pool
token0
token1
```

### D. State

可以从 authoritative state event 恢复：

```text
reserve0
reserve1
```

### E. Ordering

同 block 内事件按照：

```text
transaction_index
log_index
```

确定性处理。

### F. Replay

相同输入可以得到相同结果。

### G. Tests

所有 unit / integration tests 通过。

### H. Real Data

至少一个真实历史 block 的 PoolState 验证成功。

### I. Evidence

最终结果可以追溯：

```text
Pool
 ↓
Evidence
 ↓
Block
 ↓
Transaction
 ↓
Log
 ↓
State
```

---

# 37. M1 最终输出

完成后不要只说：

```text
Implemented M1.
```

必须输出报告：

```text
# M1 Completion Report

## 1. Implementation

新增：
- ...

修改：
- ...

## 2. Protocol Fact

Protocol:
...

Pool:
...

Token0:
...

Token1:
...

## 3. Evidence

Pool evidence:
...

Token evidence:
...

State evidence:
...

## 4. Real Replay

Chain:
...

Block:
...

Transaction:
...

Log:
...

## 5. Recovered State

reserve0:
...

reserve1:
...

## 6. Validation

RPC cross-check:
PASS / FAIL / NOT AVAILABLE

## 7. Tests

cargo fmt:
PASS

cargo check:
PASS

cargo test:
PASS

## 8. Known Limitations

...

## 9. M1 Status

COMPLETE / BLOCKED
```

---

# 38. 最终原则

牢记：

```text
事实优先于设计
真实数据优先于假设
正确性优先于性能
Evidence 优先于猜测
Replay 优先于手工验证
最小实现优先于提前抽象
```

当前不是要“做一个看起来完整的 MEV Bot”。

当前只需要把：

```text
Real EVM Data
      ↓
Real Protocol
      ↓
Real Pool
      ↓
Real State
```

真正跑通。

**M1 完成以后，才允许进入 M2。**
