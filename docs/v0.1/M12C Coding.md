# M12-C Coding Agent Task — 单节点部署与验证方案编写

## 1. 任务目标

为 `evm-mev-bot` 项目编写一份完整的 GIWA 单节点部署与验证方案，供未来真实部署时直接执行。

本任务只交付方案文档和必要的文档索引更新，不执行任何节点部署、数据同步或真实环境验证。

### 本任务完成后应达到的状态

* 部署方案完整，可以由工程师按步骤执行。
* 节点版本、依赖、配置来源和待确认事项明确。
* Canonical RPC、Flashblocks、WebSocket、同步状态、故障恢复均有明确测试步骤。
* 每个验证项都有可执行命令、预期结果、失败处理和证据要求。
* 明确区分代码审计、离线测试、真实节点验证。
* 未执行的真实环境验证继续保持 `NOT_VERIFIED` 或 `NOT_RUN`。

## 2. 当前项目基线

开始前必须确认：

* 仓库：`https://github.com/fenglufa/evm-mev-bot`
* 当前预期基线：`d34129d`，以实际 `origin/main` 为准。
* 前置审计：`docs/v0.1/M12-A Repo Audit.md`
* 前置完成报告：`docs/v0.1/M12-B Completion Report.md`
* M12-B 证据目录：`data/evidence/m12/b/`
* GIWA 官方节点仓库：`https://github.com/giwa-io/node`

开始时记录：

1. 当前分支、HEAD、`origin/main`。
2. 工作树是否干净。
3. M12-A、M12-B 文档和证据文件是否存在。
4. 当前 GIWA 节点仓库的 Release、配置文件、启动脚本和 `.env.sepolia` 的实际内容。

如果基线与预期不一致，先记录差异，不要擅自重置分支或覆盖用户工作。

## 3. 硬性范围限制

### 3.1 允许执行

* 阅读仓库、官方文档和配置文件。
* 对现有实现进行只读审查。
* 整理部署架构、依赖关系和执行步骤。
* 编写 RPC 验证脚本示例、测试命令和验收标准。
* 运行不连接真实 GIWA 节点的文档检查或纯离线测试。
* 编写风险清单、回滚方案和未来部署检查表。

### 3.2 禁止执行

* 不部署 GIWA 节点。
* 不启动节点容器或节点进程。
* 不执行 `docker compose up`、节点构建或快照恢复。
* 不下载、同步或导入链上数据。
* 不购买服务器，不创建云资源。
* 不执行真实节点 RPC 探测。
* 不接入真实资金，不签名、不广播交易。
* 不修改 M9-M12 生产代码。
* 不为方便部署而新增 HA、多节点、负载均衡或自动故障转移。
* 不将公共 RPC、公共 Flashblocks 服务描述为本地节点验证结果。
* 不把尚未验证的行为写成已验证事实。

如果文档编写过程中发现代码缺陷，只记录问题及对应文件和行号，不在本任务中修复。

## 4. 官方资料核对要求

至少核对以下来源：

1. GIWA 节点仓库：`https://github.com/giwa-io/node`
2. GIWA `.env.sepolia`：`https://github.com/giwa-io/node/blob/main/.env.sepolia`
3. GIWA 节点启动文档：`https://docs.giwa.io/giwa-chain/en/node-operators/get-started`
4. GIWA Flashblocks 文档：`https://docs.giwa.io/giwa-chain/en/network-information/flashblocks`
5. GIWA 连接说明：`https://docs.giwa.io/giwa-chain/en/get-started/connect-to-giwa`

对每项关键配置，注明：

* 来源 URL。
* 对应 Release、版本或 commit（能够确认时）。
* 配置变量名称。
* 默认值及其含义。
* 是否为必需配置。
* 是否已在当前代码中核实。
* 是否仍需真实节点验证。

### 特别要求

官方文档与当前节点仓库可能存在版本差异。不得只复制文档中的旧配置。

重点核对：

* op-geth 与 op-reth 的客户端差异。
* 当前版本对应的环境变量名称。
* `.env.sepolia` 与启动脚本实际使用的变量。
* Flashblocks 上游 URL 是否为空，以及空值时是否真的启用 Flashblocks。
* 节点 RPC、WebSocket、Engine API、op-node 自身 RPC 的端口与用途。
* 官方推荐的同步方式、磁盘占用和历史状态能力。
* L1 Ethereum RPC 和 Beacon 端点是否都需要配置。

无法确认的配置必须标为 `NEEDS_CONFIRMATION`，不能自行推断。

## 5. 方案文档结构

新建：

`docs/v0.1/M12-C Single-Node Deployment and Verification Plan.md`

文档必须至少包含以下章节。

### §1 执行摘要

明确说明：

* 本文是未来部署操作手册。
* 本轮没有部署节点。
* 当前仍处于单节点路线。
* 真实 Canonical RPC 和 Flashblocks 验证尚未完成。
* 部署与同步被推迟到代码和离线测试稳定之后。

### §2 当前架构与数据路径

画出或用 Mermaid 描述：

* GIWA op-reth 执行客户端。
* op-node 共识客户端。
* Ethereum L1 Execution RPC。
* Ethereum L1 Beacon API。
* Bot Canonical RPC。
* Bot WebSocket。
* Flashblocks 上游与 Bot Pending/Radar 数据路径。
* 交易提交与回执读取路径。

明确区分：

1. Canonical 链上数据。
2. Flashblocks/Pending 早期观察数据。
3. 交易提交端点。
4. 回执与最终执行结果。

不得假设本地读取意味着本地交易提交。必须核实 `RETH_ROLLUP_SEQUENCERHTTP` 等实际提交路径的配置及默认值，并明确标注潜在的公共端点依赖。

### §3 版本与依赖锁定

列出未来部署前必须锁定的项目：

* GIWA node Git commit 或 Release。
* op-reth 版本。
* op-node 版本。
* Docker / Docker Compose 版本。
* 操作系统和架构。
* L1 RPC 服务及 Beacon 服务。
* `.env.sepolia` 的配置版本。
* 快照来源及其可信度（如果最终选择使用快照）。

不得在本轮擅自认定某个版本已经通过 Bot 验证。

### §4 硬件与同步策略

依据官方资料列出最低与推荐配置，并说明这是官方参考值，不是本项目实测值。

比较：

* Snap Sync。
* Archive Sync。
* Consensus-Driven Sync。

分析每种模式对以下需求的影响：

* 初次同步时间。
* 磁盘空间。
* 历史状态查询能力。
* 日常 RPC 服务能力。
* 故障恢复成本。

针对本 Bot 的实际需求给出建议，但不要仅凭理论就宣称某种模式一定满足所有查询需求。

单独列出：

* 数据目录与磁盘监控。
* 磁盘空间不足时的停止条件。
* 同步过程的日志和进度检查。
* 是否需要快照，以及使用快照前的校验要求。

### §5 网络与安全配置

列出未来部署时需要检查的网络接口和端口。

原则：

* Bot 使用的 Canonical HTTP RPC 默认优先评估 `127.0.0.1:8545`。
* WebSocket 默认优先评估 `127.0.0.1:8546`。
* 必须以当前节点配置和实际监听行为为准，不能只根据常见默认值判定。
* Engine API 等内部接口不得无必要暴露到公网。
* 不应把 Docker 容器内部地址与宿主机可访问地址混为一谈。
* L1 端点凭证必须脱敏，不得进入提交的证据文件。
* 如果节点与 Bot 不在同一台机器，必须单独设计受限网络访问，不能直接将 RPC 暴露到公网。

文档应提供部署前的端口核查清单，但本轮不得真正启动服务或探测端口。

### §6 Bot 配置映射

根据当前仓库真实 CLI、环境变量及 `PipelineConfig` 实现，制作配置映射表。

至少核对：

* `GIWA_RPC_URL` / `--rpc-url`
* `GIWA_WS_URL` / `--ws-url`
* `GIWA_FLASHBLOCKS_URL` / `--flashblocks-url`
* `--rpc-endpoint-purpose`
* `--flashblocks-endpoint-purpose`
* 交易提交端点的实际配置来源
* 相关轮询间隔和超时配置

未来本地部署配置的目标示例可以是：

* Canonical RPC：`http://127.0.0.1:8545`
* WebSocket：`ws://127.0.0.1:8546`
* Flashblocks URL：默认不设置，直至其实际行为经过验证。
* 端点用途标签：必须与真实端点用途一致。

以上仅为待部署时使用的配置示例，不表示当前节点已监听这些地址。

不得通过 URL 是 localhost 就自动认定端点为本地节点。用途标签必须依据实际部署事实填写。

### §7 部署前检查表

形成可逐项勾选的检查表，至少包括：

* 版本已锁定。
* L1 RPC 可用性与限额已确认。
* L1 Beacon 端点已准备。
* CPU、内存、磁盘和网络预算已确认。
* 数据目录与权限已确认。
* 环境变量名称已核实。
* 端口暴露范围已确认。
* 日志与磁盘空间监控已准备。
* 配置备份和恢复方法已准备。
* 费用上限及停止条件已确定。
* Bot 仍处于不签名、不广播的验证模式。

任何必需项未确认，不进入实际部署。

### §8 Canonical RPC 验收矩阵

为每个测试列出：目的、命令、预期结果、失败判据、证据文件和安全影响。

至少覆盖：

1. `eth_chainId`，预期应与 GIWA 测试网 `91342` 一致。
2. `eth_blockNumber`。
3. `eth_syncing` 的 `false`、同步进度对象及错误响应。
4. `eth_getBlockByNumber` 的 `latest`。
5. `eth_getBlockByNumber` 的 `pending`。
6. `eth_getTransactionByHash`。
7. `eth_getTransactionReceipt`。
8. Bot 当前实际调用的其他必要 RPC 方法。

所有方法清单必须从当前代码提取，不得只凭经验罗列。

测试结果要区分：

* RPC 方法受支持。
* 返回结构符合 Bot 的解析器预期。
* 数据在时间上足够新鲜。
* 对 Bot 的完整业务链路确实可用。

仅返回 HTTP 200 不算通过。

### §9 Pending 与 Flashblocks 验收矩阵

分别测试：

A. 不配置 Flashblocks 上游的普通节点模式。

B. 配置官方 Flashblocks 上游的节点模式。

对比以下内容：

* `eth_getBlockByNumber("pending", false)` 的交易字段形状。
* `eth_getBlockByNumber("pending", true)` 的完整交易对象。
* Pending 区块号、hash、transactions、timestamp、gasUsed、stateRoot。
* 同一高度下交易集合是否持续变化。
* 同高度 hash 是否可能变化。
* Pending 数据如何转变为 Canonical 区块。
* Flashblocks 上游断连或返回异常时的行为。
* 本地节点是否实际提供 Bot 需要的 Pending/Flashblocks 语义。

必须提醒：

* Pending 的 hash 不可默认当成 Canonical 区块身份。
* `stateRoot` 不可在未核实语义时当作可用于 Canonical 状态校验的根。
* 不得将普通 `pending` 响应自动等同于 Flashblocks。
* 不得把公共 Flashblocks RPC 的测试结果写成本地 Flashblocks 验证结果。

若某项无法通过静态资料确认，写入真实部署时的待验证清单。

### §10 WebSocket 验收矩阵

未来验证：

* 是否能够建立连接。
* 是否支持 Bot 实际使用的订阅类型。
* 是否能收到连续通知。
* 断开后能否重连。
* 重连后是否存在事件缺口或重复。
* HTTP 轮询回退是否正常。
* WS 与 HTTP 对同一 Canonical 高度的判断是否一致。

所有异常必须有清晰处理策略。不得把 WebSocket 连接成功等同于事件完整性已验证。

### §11 Readiness 与执行安全验收

结合 M12-B 当前实现核实：

* `eth_syncing` 不为 `false` 时是否拒绝启动。
* 超时、断连、无效 JSON-RPC 响应是否 fail-closed。
* 节点高度明显落后时是否有足够的信息识别风险。
* Canonical RPC 故障时是否可能误判为无套利机会。
* 是否存在静默回退到公共 RPC 的路径。
* 节点重启后，旧 block-pinned candidate 和 simulation 是否失效。
* 已提交交易的追踪信息是否得到保留。
* Nonce 与资金预留是否会因为重启而重复或丢失。
* 未通过 readiness gate 时，Signer 和 Submitter 是否不会被调用。

如果当前代码无法满足某条要求，只记录文件、函数、证据及后续修复建议，不在本任务修改代码。

### §12 故障与恢复测试

制定未来真实环境中的故障注入步骤：

* 节点未同步。
* RPC 超时。
* RPC 断连。
* WebSocket 断连。
* Flashblocks 上游断连。
* 节点进程重启。
* 节点数据目录磁盘空间不足。
* 节点短暂落后后恢复。
* Bot 重启。
* 已提交交易在 Bot 重启后仍未确认。

每项均明确：

* 预期状态转移。
* 是否暂停机会评估或执行。
* 是否允许恢复。
* 恢复时需要重新验证哪些状态。
* 哪些行为绝不允许自动发生。
* 需要采集哪些日志与指标。

不得设计成节点故障后自动切换公共 RPC 的多端点容灾方案。

### §13 最终端到端验证顺序

真实部署时按阶段执行：

1. 部署后先确认节点进程和基础资源状态。
2. 等待节点完成同步；在未确认同步完成前不连接真实执行流程。
3. 验证 Canonical RPC。
4. 验证 WebSocket。
5. 验证普通 Pending 行为。
6. 在独立检查阶段验证 Flashblocks 上游配置与行为。
7. 验证 Bot readiness gate 与端点用途标签。
8. 在无真实资金、无签名、无广播条件下验证完整观察与模拟链路。
9. 通过上述门槛后，才制定下一阶段受控交易验证方案；本任务不授权执行真实交易。
10. 记录测试网节点运行表现、延迟、资源消耗和已知限制。

每阶段失败均停止向下一阶段推进。

### §14 证据目录与状态定义

建议未来真实验证使用独立目录：

`data/evidence/m12/c/`

规划证据文件，例如：

* `manifest.json`
* `node-version.json`
* `node-config-redacted.json`
* `sync-status.json`
* `canonical-rpc-results.json`
* `pending-shape-results.json`
* `flashblocks-results.json`
* `websocket-results.json`
* `failure-recovery-results.json`

这些是未来计划的文件，不要求本轮生成虚假的测试结果文件。

每项证据记录：

* 测试 ID。
* 运行时间。
* 节点版本和 commit。
* Bot commit。
* 配置摘要。
* 命令与返回结果。
* 通过/失败/未运行状态。
* 失败原因。
* 原始响应的安全脱敏版本。

状态必须至少区分：

* `PASS`
* `FAIL`
* `NOT_RUN`
* `NOT_VERIFIED`
* `BLOCKED`
* `OUT_OF_SCOPE`

不得用 `PASS` 表示仅完成了文档审查。

### §15 停止条件与回滚方案

定义以下停止条件：

* 链 ID 不符。
* 同步状态不明确。
* Canonical RPC 响应形状与 Bot 预期不一致。
* 节点高度新鲜度无法判断。
* Flashblocks 模式未生效或行为不明。
* 发现公共端点回退。
* 证据可能泄露凭证。
* 节点或 Bot 故障导致交易执行状态不确定。
* 资源使用超过事先确认的预算。

明确如何停止 Bot、保留诊断信息、恢复上一份配置，以及如何避免清理仍需要用于调查的数据。

禁止将删除节点数据目录作为默认回滚方式。

### §16 未来部署验收标准

只有全部必要条件通过，才能宣称本地节点验证完成：

* 节点版本与配置已锁定。
* 节点同步状态与数据新鲜度有证据支持。
* Canonical RPC 必要方法通过。
* Pending 响应形状符合 Bot 需求。
* WebSocket 与回退机制通过。
* Flashblocks 行为有单独证据。
* Readiness gate 与执行安全通过。
* 重启、断连及恢复行为通过。
* 没有未解释的公共端点回退。
* 证据与报告统计一致。

未验证的功能保持未验证，不能因其他项目通过而整体放行。

## 6. 文档与证据质量要求

* 结论必须能追溯到官方资料、仓库代码或实际测试。
* 官方资料中的值必须注明来源，不能将硬件建议写成实测需求。
* 所有命令必须经过语法和参数核查；本轮不执行会启动节点、同步数据或访问真实 RPC 的命令。
* 文档必须明确区分事实、推断、建议和待验证项。
* 不得伪造测试日志、时间戳、响应或验收结果。
* 不得重印或修改 M10 及其他历史证据文件。
* 不得为了生成新证据而运行节点或联网探测。

## 7. 完成后的交付

至少交付：

1. `docs/v0.1/M12-C Single-Node Deployment and Verification Plan.md`
2. 如仓库存在统一里程碑索引，只做必要的文档链接更新。
3. 一份简短的完成报告，记录：

   * 新增或修改的文件。
   * 核实过的官方来源。
   * 已从代码确认的事实。
   * 尚未确认的事项。
   * 文档检查结果。
   * 明确声明本轮没有部署、同步或进行真实 RPC 验证。

## 8. 最终状态约束

本任务完成后，仍必须保持：

* `M12_C_PLAN = COMPLETE`
* `SELF_HOSTED_NODE = NOT_RUN`
* `LOCAL_CANONICAL_RPC = NOT_VERIFIED`
* `LOCAL_FLASHBLOCKS = NOT_VERIFIED`
* `MULTI_NODE_HA = OUT_OF_SCOPE`

这里的 `M12_C_PLAN = COMPLETE` 仅代表方案文档完成，不代表 M12-C 的真实环境验证完成。

完成后停止，等待下一条任务，不要自行开始部署节点或进入真实环境验证。
