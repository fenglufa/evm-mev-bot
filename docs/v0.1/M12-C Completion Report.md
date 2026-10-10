# M12-C 完成报告

里程碑：M12-C（单节点部署与验证方案编写，文档交付）
任务书：`docs/v0.1/M12C Coding.md`（§1–§8）
基线 commit：`d34129d07be96d05d0ca58dc2f2d829b9e1d30a9`（`main` = `origin/main`，与任务 §2 预期一致）
交付判定：`M12_C_PLAN = COMPLETE` · `SELF_HOSTED_NODE = NOT_RUN` · `LOCAL_CANONICAL_RPC = NOT_VERIFIED` · `LOCAL_FLASHBLOCKS = NOT_VERIFIED` · `MULTI_NODE_HA = OUT_OF_SCOPE`（任务 §8）

---

## 1. 一句话结论

方案文档已按任务 §5 的 16 节结构写完并通过离线检查；**本轮没有部署节点、没有同步数据、没有探测任何真实 RPC**，所有验收矩阵的状态列一律是 `NOT_VERIFIED` / `NOT_RUN`，没有任何一处用 `PASS` 表示"只做了文档审查"。

## 2. 新增与修改的文件

| 文件 | 动作 | 字节 / 行数 / sha256 前 8 位 |
|---|---|---|
| `docs/v0.1/M12-C Single-Node Deployment and Verification Plan.md` | 新增 | 66 979 B / 811 行 / `48cbdffb` |
| `docs/v0.1/M12-C Completion Report.md` | 新增（本文件；不记自身摘要，否则一改就自相矛盾） | — |
| `docs/v0.1/M12C Coding.md` | 任务书，首次入库（此前未跟踪） | 16 699 B / 484 行 / `0df39c0f` |
| `crates/**` | **零改动**（任务 §3.2 禁止修改 M9–M12 生产代码） | `git status --porcelain crates/` 为空 |
| `data/**` | **零改动**（`git status --porcelain data/` 为空；`data/evidence/m12/c/` 未创建，`test -d` = NO） | — |

### 2.1 关于任务 §7.2（文档链接更新）

**不需要链接更新，且这不是遗漏而是核对结果。** 本仓库不存在统一里程碑索引：`docs/` 下只有 `v0.1/` 一个目录；`README.md` 为单行且不含里程碑字样；`docs/v0.1/v0.1 Technical Design.md` 检索 `M11` / `M12` / `里程碑` 无命中。因此没有可挂新文档的索引页。

## 3. 官方来源核验结果

### 3.1 `github.com/giwa-io/node`

| 项 | 实测结果 |
|---|---|
| `main` HEAD | `00985a5d…`，2026-07-13T04:50:49Z（`commits_main.atom`） |
| tag / release 数 | **10 个**（`v0.1.0`…`v0.6.0`，含 `v0.5.0`）；`node_tags.atom` 4 977 B `080bf780…`、`node_releases.atom` 35 944 B `32ea871b…` |
| 最新 release | `v0.6.0`，2026-07-07T04:38:18Z |
| tag → commit | **未取得**：GitHub REST `commits/v0.6.0` 返回 403（沙箱速率限制）→ `NEEDS_CONFIRMATION` |
| 全量差异面 | **未取得**：`trees/main?recursive=1` 返回 403；本轮只对 11 个关键路径做了 `v0.6.0` vs `main` 比对 → `NEEDS_CONFIRMATION` |
| 11 路径比对结果 | **4 个内容不同**（含 JWT 生成器判断由 `-f` 改 `-s`、peer 上限拼接）；`sepolia-genesis.json`（9 452 659 B `9431caa9…`）与 `sepolia-rollup.json`（1 544 B `2fe89131…`）两份一致 |

配置文件逐行取证（大小 / 换行数 / sha256 前 8 位）：`.env.sepolia` 3 204 B / 88 / `9cf425b9…`；`docker-compose.yaml` 1 296 B / 56 / `61278ffa…`；`README.md` 4 491 B / 136 / `a00b3e19…`；`reth/Dockerfile` 792 B / 26 / `337baaf1…`；`reth/entrypoint.sh` 1 486 B / 53 / `d71975ed…`；`node/Dockerfile` 997 B / 36 / `6575fbd4…`；`node/entrypoint.sh` 50 B / 4 / `ee16f672…`；`.env` 21 B / 1 / `e840dbd5…`。

### 3.2 `docs.giwa.io` 四篇 + 快照页

| 页面 | 取到的关键事实 | 页面自述更新时间 |
|---|---|---|
| Node Operators / Get Started | 硬件最低 4 cores / 8 GB / 500 GB NVMe，推荐 8+ / 16+ / 1+ TB；`eth_syncing` 的 `curl` 检查命令 | 1 year ago |
| Get Started / Connect to GIWA | chain id 91342；`https://sepolia-rpc.giwa.io`；`https://sepolia-rpc-flashblocks.giwa.io`（两者标注 rate-limited、不建议生产）；explorer | 10 months ago |
| Network Information / Flashblocks | 预确认 ~200 ms；Flashblocks-aware 方法 9 个（`eth_call`、`eth_estimateGas`、`eth_getBalance`、`eth_getBlockByNumber`、`eth_getLogs`、`eth_getTransactionCount`、`eth_getTransactionByHash`、`eth_getTransactionReceipt`、`eth_simulateV1`） | — |
| Notices / op-geth 支持终止 | 公告 2026-04-16；op-geth 支持至 2026-05-31；Karst 起新功能只在 op-reth；fault proof `op-program`→`kona-client`；op-geth 不支持 L1 Glamsterdam | 5 months ago |
| Node Operators / Snapshots | 周更；**不含链 tip，需 catch-up**；步骤 1 为 `docker compose down && rm -rf ./reth_data`；下载为 `curl -sL https://sepolia-snapshot.giwa.io/download.sh \| sh -s -- -c reth -p full -o ./reth_data` | 8 months ago |

官方页脚更新时间全部已在方案 §4 与 §7 检查表 #15 标注为"可能过期，部署前重取"。

## 4. 从本仓库代码确认的事实（不是推断）

1. **RPC 方法清单来自代码**：生产调用点 **29 处 / 16 个方法**（`crates/*/src`），测试专用 **4 处**。与 M12-A 记的 27 处差额**恰好两项**：`eth_syncing`（`crates/chain/src/readiness.rs:364`，M12-B 新增）与 `eth_getBlockByNumber(["pending", true])`（`crates/chain/src/head.rs:167`，M12-B 的 D5 工作）；另有 12 处调用点行号因 M12-B 改动而漂移，故方案 §8.1 明确声明行号以 `d34129d` 为准。
2. **`eth_getTransactionByHash` 在生产路径 0 调用点**，仅出现在 `crates/execution/tests/live_reads_probe.rs:279` → 方案 §8 把它与 `eth_estimateGas`、`eth_feeHistory` 归为"能力探测项/零调用项"，不写成机器人必需方法。
3. **端点用途只能声明、不能从 URL 推断**：`endpoint_purpose()`（`crates/cli/src/lib.rs:506`）缺省 `unknown`、词表外字面量拒绝、角色不符拒绝、无 URL 拒绝；`EndpointPurpose`（`crates/chain/src/endpoint.rs:30`）无任何 URL 派生函数。任务 §6 的要求在代码里已成立。
4. **就绪闸门确实挂在被执行的那条路径上**：`gate_readiness()`（`crates/pipeline/src/runner.rs:1104`）在 WS 路径（:970）与 HTTP 路径（:1005）各调一次，拒绝时 `PipelineError::NodeNotReady`（:1120）；预算 `DEFAULT_CHECK_BUDGET = 8`（`readiness.rs:275`）；指标 `readiness.eth_syncing_asks`（`runner.rs:1347`）。
5. **执行车道与 canonical 读共用同一个 URL**：`ExecutionStage::connect(...)` 在 `runner.rs:1325`，其入参来自 `config.rpc_url` → 支撑 §2.3 的核心判定"本地读 ≠ 本地提交"。
6. **`--flashblocks-url` 走 HTTP pending 轮询**：`runner.rs:1385` 用 `HttpChainAdapter::connect` + `FlashblockSource`（`crates/live/src/flashblocks.rs:209,435` 调 `pending_raw()`，即 `["pending", false]`）。
7. **M9.4 的 WS preconf 雷达没有生产构造点**：`PreconfProvider` / `EarlyRadar` 的构造只出现在 `crates/live/**`（含其测试），runner/CLI 均不构造 → §9.3 的推论依据。
8. **fail-closed 的 pending 重量形状**：trait 默认 `pending_full_transactions()` 返回 `Err`（`head.rs:86`），`WsHeadReader` 故意不覆盖（:247 及 :243-246 注释）。
9. **参数面**：WS 默认 `10 000 ms / 15 000 / 45 000 / 8 次 / 250→4 000 ms`（`crates/chain/src/ws.rs:70-79`）、HTTP 超时 20 s（`crates/chain/src/rpc.rs:94`）、`SourceConfig` 与 `FlashblockConfig` 默认值（`crates/live/src/source.rs:49`、`crates/live/src/flashblocks.rs:58-65`）；间隔默认值单一来源在 `crates/cli/src/lib.rs:414-431`（D3 之后现状）。
10. **D4 残留仍有两处硬编码**：`crates/execution/src/stage.rs:348`、`crates/execution/src/sequence.rs:1489`；可用修复函数 `receipt_provenance(endpoint_url)` 在 `sequencer_direct.rs:535-539`。**按任务 §3.2 只记录，未修复。**

## 5. 仍未确认（`NEEDS_CONFIRMATION`，方案附录 B.4 共 9 项）

`v0.6.0` 对应 commit；main 相对 tag 的全量差异面；`FLASHBLOCKS_WEBSOCKET_URL` 空值时官方意图（README 与 entrypoint 互斥）；`wss://sepolia-flashblocks.giwa.io/ws` 与 `https://sepolia-rpc-flashblocks.giwa.io` 是否同一服务；本地 op-reth 是否接受 `eth_sendRawTransaction`；Archive 模式磁盘与同步时长 / Full 可服务历史深度；快照校验值或签名；`consensus-layer` 同步的实际表现；`RETH_GCMODE=archive` 是否需额外参数。

## 6. 文档检查结果（全部离线，未连接任何端点）

| 检查 | 结果 |
|---|---|
| 16 个必需章节齐全（`^## 1.` … `^## 16.`） | **PASS**（16/16） |
| 围栏 `bash` 块 `bash -n` 语法检查 | **PASS**（4 个块，0 报错） |
| 内嵌 JSON-RPC `--data` 载荷可解析 | **PASS**（1 个载荷） |
| `rpc <method> '<params>'` 参数数组可解析 | **PASS**（13 个，全部解析为数组；首轮脚本报 `False` 是**检查脚本的缺陷**——`all(json.loads(p))` 把空数组 `[]` 当假值，已改为按解析成功与否判定后复跑通过） |
| `file:line` 引用格式与范围（越界即失败） | **PASS**（方案 43 处 / 报告 14 处，共 57 处，涉及 20+ 个文件，0 越界） |
| 关键行号内容断言（防止按名字猜行） | **PASS**（首轮 67 条断言暴露 6 处错引，已改为真实行号并复验 11 条全过） |
| 密钥扫描（≥64 位十六进制、PEM、内联 JWT、`sk-` 令牌、带凭据 URL） | **PASS**（0 命中） |
| L1 占位符保持未填充（`<your-preferred-l1-eth-rpc>` / `<your-preferred-l1-beacon>` 均出现） | **PASS** |
| `data/evidence/m12/c/` 未创建 | **PASS**（不存在） |
| `data/` 下无改动（历史证据未重印、未修改） | **PASS**（`git status --porcelain data/` 为空） |
| 生产代码 diff | **PASS**（`crates/**` 零改动） |

⚠️ 这里所有 `PASS` 的含义是**"文档与静态检查通过"**，一律不代表节点侧功能已验证。任务 §14 的词表规则（"不得用 PASS 表示仅完成了文档审查"）在方案文档内部通过把验收矩阵状态列统一写为 `NOT_VERIFIED` / `NOT_RUN` 来遵守。

## 7. 本轮明确未做的事（任务 §3.2 逐条对账）

未部署 GIWA 节点、未启动节点容器或进程、未执行 `docker compose up`、未构建镜像、未做快照恢复、未下载/同步/导入链上数据、未购买服务器或创建云资源、未执行任何真实节点 RPC 探测、未接入真实资金、未签名、未广播交易、未修改 M9–M12 生产代码、未新增 HA / 多节点 / 负载均衡 / 自动故障转移、未把公共 RPC 或公共 Flashblocks 服务的描述当作本地节点验证结果、未把未验证行为写成已验证事实、未伪造测试日志 / 时间戳 / 响应 / 验收结果、未重印或修改 M10 及其它历史证据文件。

## 8. 两处"顺手发现"的记录方式

1. **M12-A 事实性少记**：`data/evidence/m12/audit_manifest.json` 的 `official_sources.releases.count = 9`，本轮实测 10 个 tag（多出 `v0.5.0`）。按里程碑纪律**未回填修改已提交证据**，差异记录在新文档附录 A.4。
2. **官方 README 与 entrypoint 互斥**：README 教"uncomment `FLASHBLOCKS_WEBSOCKET_URL=`"（空值），而 `reth/entrypoint.sh:23` 的判断是 `[[ -n … ]]` —— 空值走 `else` 分支打印 `Running in vanilla mode`。方案 §9.1 把它写成"照抄即静默不开"的部署陷阱，并要求未来以启动日志那一行作为验收证据。

## 9. 停止点

方案已交付，按任务 §8 停在这里：不开始部署、不开始真实环境验证。未来要推进 `SELF_HOSTED_NODE`，需要用户点名授权一次真实部署轮（含 §13 阶段 9 的交易授权，是另一件事）。
