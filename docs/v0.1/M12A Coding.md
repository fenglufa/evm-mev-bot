# M12-A Repo Audit：自建 GIWA RPC 与 Flashblocks 协议审计

## 1. 任务目标

为 `evm-mev-bot` 的 M12 低延迟基础设施阶段完成代码与官方协议审计，给出可以实施的单节点自建 GIWA RPC 方案。

本阶段只审计、核实、记录和制定实施方案，不进行生产代码重构，不要求实际部署节点，也不要求准备第二个 RPC。

最终需要回答：

1. 现有机器人究竟从哪些地方读取 RPC 地址？
2. 哪些模块使用 canonical RPC，哪些模块使用 pending / Flashblocks 数据？
3. 是否存在多个硬编码 endpoint，或者配置来源不统一？
4. 自建 GIWA 节点启动后，哪些地址和配置需要修改？
5. 官方节点如何启用 Flashblocks，机器人应该通过什么接口消费 Flashblocks 数据？
6. 现有 M9.4 Early Radar 和 M11 执行链需要怎样适配？
7. 如何在不部署真实节点的情况下，为后续实现编写可复现的测试？

## 2. 已冻结的项目决策

### 2.1 删除 HA 目标

M12 不再实现以下能力：

* 多 RPC endpoint 自动切换。
* 多节点健康管理。
* 多节点负载均衡。
* 跨节点故障转移。
* 备用节点部署与真实切换演练。

本阶段仍需保留单节点运行所必需的：

* 连接超时。
* 现有请求重试语义。
* 连接断开后的安全恢复。
* 节点不可用时的安全停机或暂停执行。
* 基础健康检查和可观测性。

不要因为取消 HA 而移除现有必要的错误处理、重试、连接恢复或安全停机机制。

### 2.2 M12 的核心方向

优先支持单个自建 GIWA 节点，减少对公共 GIWA RPC 的依赖，验证：

* 本地 canonical HTTP JSON-RPC。
* 本地 WebSocket 能力（只有实际需要且节点支持时才使用）。
* 本地 Flashblocks 数据能力。
* RPC 响应与现有 Rust 数据模型的兼容性。
* 单节点的同步、重启、断连和恢复行为。

不要求本阶段启动自建节点。

### 2.3 保留的系统边界

* Rust，单进程架构。
* 不重写 M9.3 PathFinder。
* 不重写 M10 Executor、Signer、ReceiptTracker。
* 不重写 M11 Multi-Lane。
* 不允许 Flashblocks 数据直接覆盖 canonical StateStore。
* 不允许未验证的 pending 状态直接驱动交易提交。
* 不增加未经证明的 RPC 调用。
* 不修改套利算法以规避基础设施问题。

## 3. 官方资料核验

必须阅读并记录当前版本的官方资料：

1. GIWA 节点仓库：
   https://github.com/giwa-io/node

2. 官方 Sepolia 环境文件：
   https://github.com/giwa-io/node/blob/main/.env.sepolia

3. 官方节点部署文档：
   https://docs.giwa.io/giwa-chain/en/node-operators/get-started

4. 官方 Flashblocks 文档：
   https://docs.giwa.io/giwa-chain/en/network-information/flashblocks

5. 官方 GIWA 网络连接信息：
   https://docs.giwa.io/giwa-chain/en/get-started/connect-to-giwa

6. 官方节点 Release：
   https://github.com/giwa-io/node/releases

所有版本相关结论必须记录审计日期、Release/tag 或 commit SHA。

不得把其他 OP Stack 链的部署配置当作 GIWA 已经验证的事实。

### 3.1 节点部署要求

核实并记录：

* 当前 GIWA Sepolia 节点所需的执行客户端和共识组件。
* 官方推荐的节点启动方式。
* `.env.sepolia` 中所有必须配置的字段。
* `OP_NODE_L1_ETH_RPC` 和 `OP_NODE_L1_BEACON` 的用途。
* `RETH_HTTP_PORT`、`RETH_WS_PORT`、`RETH_AUTHRPC_PORT`、`OP_NODE_RPC_PORT` 的用途与访问边界。
* 数据目录、持久化方式、同步模式和节点同步状态检查方式。
* 当前 Release 的 Rust、Go、Docker、构建工具和其他版本要求。
* 最低和推荐硬件需求。
* 当前官方 Release 与仓库 README、配置文件之间是否存在版本差异。

特别核实 `RETH_AUTHRPC_PORT` 等内部 Engine API 是否只应由节点内部组件访问。

不要建议将 Engine API、JWT 认证接口或其他内部端口直接暴露到公网。

### 3.2 Flashblocks 核验

必须明确区分以下三个概念：

A. Flashblocks 上游数据源。

B. GIWA 节点自身的 Flashblocks 接入能力。

C. 机器人对外发起查询时使用的 Flashblocks-aware RPC 接口。

核实官方环境文件中的 `FLASHBLOCKS_WEBSOCKET_URL`：

* 它由哪个组件读取？
* 连接失败时节点如何处理？
* 是否需要额外的启动参数或构建功能？
* 节点如何把收到的数据暴露给 JSON-RPC 层？
* 自建节点的普通 HTTP JSON-RPC 是否天然支持 Flashblocks pending 语义？
* 本地 Flashblocks-aware RPC 是否与 canonical RPC 使用相同端口？
* 如果不相同，分别对应什么接口和配置？

以上问题必须依据当前官方代码和配置回答。

不能仅凭环境变量存在就宣称 Flashblocks 已启用，也不能把“WebSocket 连接成功”当作“机器人能够读取正确的 Flashblocks 状态”。

## 4. 仓库审计范围

审计 `main` 分支当前代码，先记录当前 HEAD SHA 和工作树状态。

重点检查以下模块；如果文件名或结构已经变化，以实际代码为准：

* `crates/chain/src/rpc.rs`
* `crates/chain/src/head.rs`
* `crates/chain/src/`
* `crates/live/src/preconf_provider.rs`
* `crates/live/src/preconf_decode.rs`
* `crates/live/src/preconf_loop.rs`
* `crates/live/src/preconf_radar.rs`
* `crates/live/src/lib.rs`
* `crates/simulation/`
* `crates/execution/`
* `crates/cli/`
* 所有配置文件、环境变量读取代码和 RPC endpoint 构造位置。

### 4.1 RPC 地址清点

使用全仓库搜索，列出：

* 所有公共 GIWA RPC URL。
* 所有 Flashblocks RPC URL。
* 所有 WebSocket URL。
* 所有配置字段和环境变量。
* 所有默认值。
* 所有从命令行参数、配置文件或环境变量读取 endpoint 的入口。
* 所有直接构造 HTTP/WebSocket 客户端的地方。
* 所有绕过统一 ChainAdapter、HeadReader 或 FrameSource 的网络访问。

每一条记录至少包括：

| 字段      | 要求                                     |
| ------- | -------------------------------------- |
| 文件路径与行号 | 必须精确到实际代码                              |
| 变量或配置名称 | 使用代码中的真实名称                             |
| 当前来源    | 默认值、环境变量、CLI、构造参数等                     |
| 使用者     | 哪个模块读取该地址                              |
| 协议      | HTTP、WebSocket 或其他                     |
| 用途      | canonical、pending、receipt、submission 等 |
| 是否需要修改  | 是、否、待验证                                |
| 建议目标    | 本地 endpoint、保留公共 endpoint 或待实测         |

不能仅通过搜索字符串就判定某个 endpoint 正在生产路径中使用；需要追踪其调用关系。

### 4.2 现有 ChainAdapter 审计

检查 `HttpChainAdapter`：

* endpoint 是否在创建时固定。
* `eth_chainId` 的读取和校验方式。
* HTTP timeout。
* 现有重试次数和重试条件。
* JSON-RPC error 的处理。
* RPC trace 是否会记录 endpoint 或敏感信息。
* 哪些方法由该 adapter 实际提供。
* 是否存在额外的 RPC 客户端绕过该 adapter。

当前目标是继续复用现有单端点 adapter。

本阶段不新增多 endpoint router，不增加 HA，也不改写重试机制。

### 4.3 HeadReader 与 pending 数据审计

核实：

* `pending_raw()` 实际发送什么 JSON-RPC method 和 params。
* 当前 endpoint 是从哪里注入的。
* pending 读取与 canonical 读取是否使用同一个 endpoint。
* 当前实现是否为 HTTP polling。
* pending 返回 `null`、请求失败、JSON 解析失败时分别如何处理。
* 是否有任何代码将 pending 状态直接写入 canonical StateStore。
* 是否存在把 pending 视图的 hash 当成最终 canonical block hash 的风险。

### 4.4 M9.4 Early Radar 审计

保持 M9.4 的既有协议发现：

* 当前实测 endpoint 使用 HTTP polling 读取 `eth_getBlockByNumber(["pending", true])`。
* 同一高度可能出现多次不断增长的交易列表。
* 不能把该行为直接称为推送式 Flashblocks stream。
* pending 视图的 hash 不能直接当作最终 canonical 身份。
* canonical reconciliation 仍然是最终裁决。
* 现有生产实现不应为采集额外增加 RPC 调用。

检查 `FrameSource` 抽象是否足以支持将来新增一个本地数据源，而无需修改 `preconf_decode`、`EarlyRadar` 的核心语义。

如果官方自建节点的接口行为与现有 endpoint 不同，应提出单独的 provider adapter 方案，不得直接修改已有协议假设。

## 5. 配置目标

本阶段只提出配置方案，不立即修改生产配置。

建议设计为明确区分 canonical RPC 与 Flashblocks 数据入口的配置模型。

概念示例：

```toml
[giwa]
chain_id = 91342

[rpc]
canonical_http_url = "http://127.0.0.1:8545"

[flashblocks]
enabled = false
# 只有官方实现和实际接口确认后才添加本地入口。
# 不预先假设本地 Flashblocks 与 canonical RPC 共用地址。
```

以上只是配置设计示意。必须检查当前仓库的真实配置结构，再决定是否沿用现有字段或引入新字段。

具体要求：

1. 不在多个模块中散落重复的 endpoint 默认值。
2. 如果当前项目已有配置体系，优先复用。
3. 不因为示意配置而强制新增 TOML 依赖。
4. 公共 RPC 地址只允许作为明确的开发/测试配置，不应在生产模式中静默回退。
5. 本地 RPC 不可用时，应明确报错或暂停，不允许静默切回未知公共 endpoint。
6. 启动时核实 chain ID 为 91342。
7. 配置日志不得打印 API key、认证 token 或敏感 URL 参数。
8. HTTP 和 WebSocket 地址必须分别定义其语义，不能混用。
9. 如果未来同一地址支持多种能力，必须通过实际探测或明确配置确定，不能靠端口号猜测。

## 6. 自建 RPC 的兼容性清单

请对现有代码实际使用的 JSON-RPC methods 进行汇总，至少检查：

* `eth_chainId`
* `eth_blockNumber`
* `eth_getBlockByNumber`
* `eth_getLogs`
* `eth_call`
* `eth_getCode`
* `eth_getBalance`
* `eth_getTransactionCount`
* `eth_getTransactionReceipt`
* `eth_sendRawTransaction`
* `eth_estimateGas`
* `eth_gasPrice`
* `eth_feeHistory`
* `eth_getTransactionByHash`
* `eth_syncing`

以上列表是审计候选项，不代表每个方法都一定被当前生产代码使用。必须逐项确认实际调用情况。

同时确认：

* 当前 op-reth 节点是否支持这些方法。
* 哪些方法依赖 archive/history 状态。
* 哪些方法要求特定 block tag。
* 哪些方法需要 WebSocket。
* 哪些方法可能受节点同步状态影响。
* 哪些方法与 Flashblocks pending 语义有关。
* 哪些方法只用于测试、证据生成或非热路径。
* 是否有 GIWA 特定扩展方法。
* Private / Direct Sequencer 方法继续保持条件能力，不作为本阶段依赖。

不能仅凭标准 Ethereum JSON-RPC 规范就宣称 GIWA 当前部署全部支持。

## 7. 安全与状态一致性

必须审计以下边界：

1. Canonical state 的来源仍然权威。
2. Early Radar 不直接修改 canonical StateStore。
3. 同一交易在多个 pending 视图中重复出现时，不应重复派生不安全的执行动作。
4. 节点重启、重连或 pending 视图重置后，需要明确哪些状态失效、哪些可以保留。
5. 链 ID 不匹配时，机器人必须拒绝继续运行。
6. canonical 读取不可用时，不得把过期 GraphSnapshot 当作新状态使用。
7. 节点尚未同步完成时，不得把缺失数据解释成零储备或零利润。
8. RPC 失败不能被转换成“无套利机会”。
9. 交易提交失败与交易结果未知必须区分。
10. 本阶段不新增签名或广播测试。

## 8. 测试方案设计

本阶段不要求真实节点，但需要为后续开发给出具体测试用例。

### 8.1 配置测试

* 本地 canonical URL 正确注入。
* 缺失必需 URL 时明确失败。
* 错误 chain ID 时拒绝启动。
* 公共 RPC 与本地 RPC 配置切换不会产生意外回退。
* HTTP / WebSocket 配置不会被误用。

### 8.2 RPC 兼容性测试

使用 mock JSON-RPC server 或现有 fixture 验证：

* 正常 JSON-RPC 响应。
* `result: null`。
* JSON-RPC error。
* HTTP 5xx。
* 请求超时。
* 断连。
* 非 JSON 响应。
* 缺少字段或错误数据类型。
* `eth_syncing` 的不同响应形式。
* 不同 block tag 的处理。

不要通过 mock 测试宣称官方节点实际支持某个方法。

### 8.3 Flashblocks 测试

复用 M9.4 已有录制数据和 `ReplayFrameSource`，覆盖：

* 同高度重复视图。
* 同高度交易列表增长。
* pending 视图 reset。
* canonical reconciliation。
* canonical 与 pending 内容不一致。
* pending 不可用。
* provider 数据形状改变时 fail-closed。

如果新增测试需要额外 RPC，应记录调用次数，并说明为什么现有路径无法覆盖。

## 9. 明确禁止的操作

本阶段禁止：

* 修改 `HttpChainAdapter` 的生产逻辑。
* 重写 `HeadReader`。
* 修改 M9.4 Early Radar 的核心状态语义。
* 新增 HA、多个 RPC 管理器或负载均衡器。
* 新增真实节点部署依赖。
* 将公共 Flashblocks endpoint 当作本地 RPC。
* 在未经验证的情况下假定本地 HTTP 与 WebSocket 使用相同端口。
* 修改 M10 Executor、Signer、ReceiptTracker。
* 修改 M11 Multi-Lane。
* 进行真实资金交易。
* 新增签名、广播或生产 RPC 调用。
* 为完成审计而大规模整理现有代码。
* 把未验证事项标记为支持或完成。

## 10. 交付物

新增一份报告：

`docs/v0.1/M12-A Repo Audit.md`

报告至少包括：

1. 审计范围、审计日期、仓库 HEAD SHA。
2. 官方节点版本和相关资料。
3. 节点部署配置核验结果。
4. Flashblocks 上游、节点能力、客户端 RPC 三者的关系。
5. 全部 RPC endpoint 和配置来源清单。
6. 现有 RPC 调用关系图。
7. 当前 ChainAdapter、HeadReader、FrameSource 的职责边界。
8. M9.4 Early Radar 的兼容性分析。
9. 现有 JSON-RPC methods 的实际使用矩阵。
10. 本地节点接入所需的最小修改清单。
11. 配置模型建议。
12. 需要实现的单节点恢复能力。
13. mock / replay 测试矩阵。
14. 未验证事项和真实节点启动后的验收清单。
15. M12-B/C 后续任务拆分。

附上一份可供后续实现使用的结构化清单，例如：

`data/evidence/m12/audit_manifest.json`

清单只记录已经核实的信息，不要将推测写成事实。

如果新增证据文件，必须确保它能从已记录的来源独立复核，不得引用即将删除的临时日志作为唯一依据。

## 11. 验收标准

M12-A 只有满足以下条件才能判定 COMPLETE：

* 已记录当前仓库 HEAD SHA。
* 已核实官方 GIWA 节点当前版本和配置要求。
* 已核实 Flashblocks 配置的实际用途。
* 已列出所有生产 RPC endpoint 的来源和调用关系。
* 已区分 canonical RPC、pending polling 与 Flashblocks-aware RPC。
* 已确认现有配置切换的最小实现范围。
* 已形成后续测试矩阵。
* 已列出所有必须等真实节点才能验证的事项。
* 没有修改生产代码。
* 没有新增 RPC 调用。
* 没有新增签名或广播。
* 相关报告和清单可以复核。
* 当前工作树和测试状态已记录。

本阶段的结论只能是：

* `M12_AUDIT = COMPLETE`：审计资料和实施方案完整。
* `SELF_HOSTED_NODE = NOT_RUN`：尚未启动自建节点。
* `LOCAL_CANONICAL_RPC = NOT_VERIFIED`：尚未完成真实节点验证。
* `LOCAL_FLASHBLOCKS = NOT_VERIFIED`：尚未完成真实节点验证。
* `MULTI_NODE_HA = OUT_OF_SCOPE`：已按项目决策删除。

## 12. 后续工作

M12-A 完成后，先审阅报告，再单独下发 M12-B 代码任务。

M12-B 优先完成最小必要的单节点配置和恢复能力。

M12-C 完成现有链路的延迟基准和观测完善。

真实节点部署与 Flashblocks 实测保留为基础设施就绪后的验收项。

不要在 M12-A 中提前实现这些功能。