## M5 — GIWA Flashblocks 已知 Endpoint

项目已经提供一个已知的 GIWA Sepolia Flashblocks endpoint：

```text
https://sepolia-rpc-flashblocks.giwa.io/
```

这是 **M5 实现 FlashblockSource 时必须优先验证的真实 endpoint**。

### 重要要求

不要假设这个 endpoint 的协议能力。

Coding Agent 必须首先对该 endpoint 做真实能力探测，并记录实际结果。

至少验证：

1. HTTP JSON-RPC 是否可用
2. WebSocket 是否可用
3. 支持哪些 JSON-RPC methods
4. 是否存在 Flashblock-specific subscription
5. 是否存在 block / flashblock sequence
6. 返回数据结构
7. 是否包含 block number
8. 是否包含 block hash / parent hash
9. 是否包含 transactions
10. 是否包含 logs / receipts / state changes
11. 如何判断一个 Flashblock 是否已经过期
12. 如何判断 Flashblock 是否缺失
13. 普通 canonical block 与 Flashblock 如何对应
14. Flashblock 与 NewHeads 如何进行 reconciliation
15. 连接断开后如何恢复

### 禁止猜测

不要根据：

* endpoint 名称
* 第三方文章
* 旧版本代码
* 类似 OP Stack 项目
* 猜测的 RPC method
* 猜测的数据结构

直接实现协议。

必须优先通过真实 endpoint、GIWA 官方资料或当前节点实现验证。

### Endpoint 配置

不要把 endpoint hardcode 到业务逻辑。

建议配置：

```text
GIWA_FLASHBLOCKS_URL
```

或者使用项目现有 chain configuration。

默认值可以设置为：

```text
https://sepolia-rpc-flashblocks.giwa.io/
```

但业务代码不得直接引用这个字符串。

### 最重要的架构要求

Flashblocks 不得形成第二套 Pipeline。

必须：

```text
                 ┌─ NewHeadsSource
                 │
MarketDataSource ├─ FlashblockSource
                 │
                 └─ ReplaySource
                         │
                         ▼
                  Unified Event
                         │
                         ▼
                  State Engine
                         │
                         ▼
                     Graph
                         │
                         ▼
                   Opportunity
                         │
                         ▼
                    Simulation
                         │
                         ▼
                       Risk
```

Flashblocks 与普通 NewHeads 的区别应该存在于：

```text
MarketDataSource / Chain-specific adapter
```

而不是：

```text
State
Graph
Opportunity
Simulation
Risk
```

中。

### Flashblock 验证结果

最终 M5 Completion Report 必须明确给出：

```text
FLASHBLOCK_ENDPOINT
https://sepolia-rpc-flashblocks.giwa.io/

CONNECT:
PASS / FAIL

SUBSCRIPTION:
PASS / FAIL / BLOCKED

DECODE:
PASS / FAIL / BLOCKED

SEQUENCE:
PASS / FAIL / BLOCKED

STATE INTEGRATION:
PASS / FAIL / BLOCKED

NEWHEADS RECONCILIATION:
PASS / FAIL / BLOCKED

FALLBACK:
PASS / FAIL

END-TO-END:
PASS / FAIL / BLOCKED
```

如果 endpoint 实际能力与预期不同，以**真实验证结果**为准。

不得为了满足 M5 Acceptance 而 mock Flashblock 数据。

如果当前 endpoint 无法完成某项能力，明确记录 `BLOCKED`，同时保留正确的 abstraction boundary。
