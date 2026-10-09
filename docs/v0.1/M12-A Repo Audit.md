# M12-A Repo Audit — 自建 GIWA RPC 与 Flashblocks 协议审计

审计日期：2026-10-09（Asia/Shanghai，UTC+8）
审计对象：`evm-mev-bot` `main` 分支，HEAD `a1a6952ff960d723c2736d5ed2e5bc42043dc845`
性质：只审计、只核实、只制定方案。**本里程碑未修改任何生产代码，未新增任何 RPC 调用，未新增签名或广播，未启动任何节点。**

---

## 0. 一页结论（白话版）

1. **把机器人指向自建节点，改的是"配置值"，不是代码。**
   全仓 16 个 crate 的 `src/` 里没有任何硬编码的 URL，也没有任何硬编码的链 id `91342`；这条不是靠"我觉得"，是靠 `crates/cli/tests/no_execution.rs:300` 那个测试守着的——它遍历所有 `crates/*/src`，只要生产代码里出现 `https://`、`http://`、`wss://`、`ws://` 或 `91342` 就红。地址只有一个来源：命令行参数或环境变量。
   所以「自建节点接入」这件事的最小修改清单（§10）是 **0 行代码 + 3 个配置值**。

2. **但有三件事只有真节点能回答，本阶段一律记为未验证。**
   - 自建节点的 HTTP 口（8545）会不会因为开了 Flashblocks 就变成 Flashblocks-aware；
   - 本地 `eth_getBlockByNumber("pending", …)` 返回的形状，是否与公共 Flashblocks 端点一致；
   - 节点还在同步时，机器人的表现是不是"安全地停下来"。

3. **本阶段查出来的最实际缺口不是配置，是"机器人完全不看同步状态"。**
   `eth_syncing` 在整个仓库（含测试）里命中 0 次。节点没同步完时，机器人不会说"节点还没好"，而是把"查不到"当成"没有"：`get_block` 返回 `MissingData`、`pending_raw()` 返回 `None`。一个还没同步到的块高，会被解读成"这里没有数据"。这就是 §7 第 7 条要防的事，目前代码里没有防线。

4. **官方仓库里 Flashblocks 只有一个开关，而且默认是关的。**
   GIWA 官方节点仓库把 Flashblocks 唯一地实现为 op-reth 的启动参数 `--flashblocks-url`，值来自环境变量 `FLASHBLOCKS_WEBSOCKET_URL`；而 `.env.sepolia` 里这一行是**注释掉的**，README 的启用步骤又示范写成空值——字面照做，节点会跑在 vanilla 模式且不会报错。共识侧（op-node）没有任何 Flashblocks 配置项。

5. **顺带登记 4 个既有缺陷（本阶段只登记，不修）：**
   Flashblocks 链校验的报错把两个字段的角色写反了（`runner.rs:1236`）；测试用的 Flashblocks 环境变量名与生产的差一个词（`GIWA_FLASHBLOCKS_RPC_URL` vs `GIWA_FLASHBLOCKS_URL`）；执行链路把端点类型写死成"公共 HTTP RPC"（2 处），自建节点会被证据表标成公共节点；以及 Radar 的解码器只吃"完整交易对象"，而生产轮询发的是 `false`（不带完整对象）——详见 §16。

---

## 1. 审计范围、基线与工作树状态

### 1.1 仓库基线（审计当时实测）

| 项 | 值 |
| --- | --- |
| HEAD SHA | `a1a6952ff960d723c2736d5ed2e5bc42043dc845` |
| HEAD 提交时间 | 2026-10-09T15:42:27+08:00 |
| 分支 | `main`（与 `origin/main` 同步于 `250cd26..a1a6952`） |
| `git status --porcelain` | 仅 `?? docs/v0.1/M12A Coding.md`（任务书本身，未跟踪） |
| crate 数 | 16（chain cli core discovery execution graph live metrics opportunity pathfinder pipeline protocol replay risk simulation state） |
| 可执行文件 | 1 个（`crates/cli/src/main.rs:6`，二进制名 `evm-mev-bot`） |
| `data/evidence/` | m5 m6 m7 m8 m9 m10 m11（本里程碑新增 m12） |
| 配置文件 | 仓库内**不存在**任何 `.env` / `.yaml` / `.ini` / TOML 运行时配置；只有 `Cargo.toml` 与 `rust-toolchain.toml` |

### 1.2 本里程碑未改动生产代码（可复核）

`git status --porcelain` 全程只有本文件与 `data/evidence/m12/`；`crates/*/src`、`crates/*/tests` 零 diff。复核命令：

```sh
git diff --stat HEAD -- crates            # 期望：空
git status --porcelain                    # 期望：仅 docs/ 与 data/evidence/m12/
```

### 1.3 关卡状态（串行执行，`--test-threads=1`）

| 关卡 | 命令 | 结果 |
| --- | --- | --- |
| 格式 | `cargo fmt --all -- --check` | 通过（输出 0 字节） |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | 通过（4m48s 完成，无 warning） |
| 测试 | `cargo test --workspace -- --test-threads=1` | **通过**：123 个测试目标，1611 passed / 0 failed / 29 ignored；整条命令 wall 3975.39 s（其中编译 6m 35s，取自日志第 21 行的 `Finished … in`）。日志 `target/m12-test.txt`（临时件，判据可由 §18 命令重跑复现） |

---

## 2. 官方节点版本与相关资料

### 2.1 已读取的官方来源与指纹

抓取日期 2026-10-09，全部 HTTP 200，均以 `curl` 取原始文件后本地解析（未使用二手转述）。

| 来源 | 版本标识 | 大小（字节） | sha256 |
| --- | --- | --- | --- |
| `giwa-io/node` 仓库 main 分支 HEAD | commit `00985a5dba4ecf594d120072fd3557f095d3c51f`（实测自 `https://github.com/giwa-io/node/commits/main.atom`，feed 内最新一条；该页自标更新时间 `2026-07-13T04:50:49Z`） | — | — |
| 仓库根目录清单 | main 分支 HTML 根目录载荷实测：6 个文件（`.env`、`.env.sepolia`、`.gitignore`、`LICENSE`、`README.md`、`docker-compose.yaml`）+ 5 个目录（`.github`、`genesis`、`node`、`resources`、`reth`） | — | — |
| `.env`（根，只写 `DATA_DIR` 一类默认） | main 分支当前 | 21 | `e840dbd5a924edd5512622cbf74077022065e1ec69bffbe219d29a19ea50edc5` |
| `.env.sepolia` | main 分支当前 | 3204 | `9cf425b98ceee8f2882c7805c5b0ac79c7424bc572b1a29d136f863d596a2108` |
| `reth/entrypoint.sh` | main 分支当前 | 1486 | `d71975edf31f84c9531761eeacb77989cc4a11dfcb0181c31842b467b88dac03` |
| `docker-compose.yaml` | main 分支当前 | 1296 | `61278ffae361aa98674cc6ab0d06a5cd1e69940d01e9fd84e42e0e86f810f2f1` |
| `README.md` | main 分支当前 | 4491 | `a00b3e19611cc8f0dc099e9569b35287222d48bb0214b500bbc3358eee1e6356` |
| `genesis/execution/sepolia-genesis.json` | main 分支当前 | 9452659 | `9431caa96d9a9658a94a1678aaf8469e4aad601d1ab965115adfe407effaeece` |
| `genesis/consensus/sepolia-rollup.json` | main 分支当前 | 1544 | `2fe89131e4fa91324af61080c1845c6c79597629318137f2dd7e3f21525f543f` |
| `node/entrypoint.sh` | main 分支当前（全文 3 行，有效语句只有 `exec op-node "$@"`） | 50 | `ee16f67270ad6eff130284d58c02fa82b2f51c5e0c1ce99e26c18e6207aa8051` |
| docs `get-started/connect-to-giwa` | 页面自标 "Last updated 10 months ago" | 702328 | — |
| docs `network-information/flashblocks` | 页面自标 10 months ago | — | — |
| docs `node-operators/get-started` | 页面自标 "Last updated 1 year ago" | — | — |
| `giwa-io/node` Releases | 共 9 个 | — | — |

### 2.2 Release 时间线（版本相关结论全部挂在这里）

| Release | 时间（UTC） | 与本审计相关的要点 |
| --- | --- | --- |
| **v0.6.0（最新）** | 2026-07-07T04:38:18Z | 移除 op-geth；**破坏性：`GETH_*` → `RETH_*`**；op-reth **v2.3.3**、op-node **v1.19.1**；单一 PR #39 |
| v0.5.1 | 2026-07-01 | 引入 Karst 激活时间 `1783317600`；工具链 Rust 1.94 / Go 1.26.4 |
| v0.4.0 | 2026-04-27 | 默认客户端切换 |
| v0.3.0 | prerelease | 自标 "DO NOT USE THIS RELEASE" |

**结论：一切以 v0.6.0 的字段名为准。** GIWA 部署文档页面（10–12 个月前更新）仍在写 `GETH_*`，与 v0.6.0 的 `.env.sepolia` 不一致——照文档抄配置会得到节点读不懂的变量名。这是 §3.1 最后一项「官方 Release 与 README/配置文件之间是否存在版本差异」的实测答案：**存在，且方向是"文档滞后于 Release"。**

### 2.3 链身份（官方文件互证）

| 字段 | 值 | 出处 |
| --- | --- | --- |
| L2 chain id | `91342` | `sepolia-rollup.json.l2_chain_id`、`sepolia-genesis.json.config.chainId`、docs connect 页 |
| L1 chain id | `11155111`（Sepolia） | `sepolia-rollup.json.l1_chain_id` |
| 出块时间 | 1 秒 | `sepolia-rollup.json.block_time` |
| Karst 硬分叉 | `1783317600` | rollup.json `karst_time` + genesis `karstTime` + Release v0.5.1 |
| Jovian / Isthmus | `1767070800` / `1753349700` | rollup.json |
| genesis alloc 条目 | 2339 | sepolia-genesis.json |
| 公共 RPC | `https://sepolia-rpc.giwa.io`（官方注明 rate-limited、不建议生产使用） | docs connect 页 |
| 公共 Flashblocks RPC | `https://sepolia-rpc-flashblocks.giwa.io`（同一条 rate-limited 说明） | docs connect 页 |
| 浏览器 | `https://sepolia-explorer.giwa.io` | docs connect 页 |
| 主网 | 🚧 未提供 | docs connect 页 |

### 2.4 未取到的官方证据（如实记录）

- **GitHub REST API（`api.github.com` 的 commits / trees / 代码搜索端点）在本沙箱一律返回 HTTP 403/401，且无 `gh` CLI。** 因此官方仓库的**递归文件总数**与 op-reth 上游 `--flashblocks-url` 的内部行为（连接失败后节点如何表现、pending 视图如何构造）**本阶段未验证**，记入 §14，不得当作事实；能实测到的只有：单个 raw 文件（下表）、根目录清单、atom feed 里的最新 commit。 因此 op-reth 上游 `--flashblocks-url` 的内部行为（连接失败后节点如何表现、pending 视图如何构造）**本阶段未验证**，记入 §14，不得当作事实。
- 本审计只证明"官方仓库/配置/文档这样写"，不证明"照做之后节点行为如何"。

---

## 3. 节点部署配置核验

### 3.1 组件与启动方式（实测自 v0.6.0 当前 main）

| 角色 | 组件 | 版本 | 镜像构建 |
| --- | --- | --- | --- |
| 执行客户端 | **op-reth** | v2.3.3 | `reth/Dockerfile`：`FROM rust:1.94-bookworm`，`libclang-dev pkg-config build-essential`，`cargo install just`，`git clone --branch op-reth/$RETH_VERSION`，构建 `just update-superchain-registry-submodule && just maxperf` |
| 共识组件 | **op-node** | v1.19.1 | `node/Dockerfile`：`rust:1.94-bookworm` + `golang:1.26.4-bookworm`，jq/zip/yq，`just build-superchain-go && just op-node`；`node/entrypoint.sh` 全文只有一句 `exec op-node "$@"` |
| 编排 | docker compose | — | `docker-compose.yaml` 三个服务：`jwt-generator`（alpine + openssl 写 `/shared/jwtsecret.key`）、`execution`、`consensus` |
| 启动命令 | README | — | `docker compose build --parallel` → `NETWORK_ENV=<.env.{network}> docker compose up -d` |

### 3.2 `.env.sepolia` 逐字段核验（3204 字节 / 上表 sha256）

**必填（README 明确点名 2 个）：**

| 字段 | 值 / 状态 | 用途与边界 |
| --- | --- | --- |
| `OP_NODE_L1_ETH_RPC` | `<your-preferred-l1-eth-rpc>` 占位 | L1（Sepolia）信标/数据获取源。**op-node 自己不做 L1 同步，必须另有一个可用的 Sepolia L1 RPC——这是自建 GIWA 节点的隐性前置成本。** |
| `OP_NODE_L1_BEACON` | `<your-preferred-l1-beacon>` 占位 | L1 信标节点，用于确认 L1 侧数据 |
| `OP_NODE_L1_RPC_KIND` | `debug_geth` | 取 L1 数据的方式 |
| `OP_NODE_L1_TRUST_RPC` | `false` | 不信任单一 L1 RPC |

**端口与数据（本审计最关心的一组）：**

| 字段 | 值 | 谁读它 | 访问边界 |
| --- | --- | --- | --- |
| `RETH_HTTP_PORT` | 8545 | op-reth `--http.addr=0.0.0.0` | 对外（compose 已发布） |
| `RETH_WS_PORT` | 8546 | op-reth `--ws.addr=0.0.0.0` | 对外（compose 已发布） |
| `RETH_AUTHRPC_PORT` | **8551** | op-node ↔ op-reth 的 Engine API（JWT） | **内部。`docker-compose.yaml` 未发布 8551**；op-node 走 `OP_NODE_L2_ENGINE_RPC=ws://execution:8551`（容器网络内） |
| `RETH_AUTHRPC_JWTSECRET` | `/shared/jwtsecret.key` | 同上 | 由 `jwt-generator` 服务生成，共享卷 |
| `OP_NODE_RPC_PORT` | 9545（compose 发布） | op-node 自身 RPC | 非 EVM JSON-RPC，机器人不使用 |
| `RETH_PORT` / `RETH_DISCOVERY_PORT` | 30303 tcp/udp | P2P | 对外 |
| `OP_NODE_P2P_PORT` | 9222 tcp/udp；`OP_NODE_P2P_NAT=false` | P2P | 对外 |
| `RETH_METRICS_PORT` | 6060，compose 映射 `7301:6060` | Prometheus | 运维 |
| op-node metrics | `0.0.0.0:7300` | Prometheus | 运维 |
| `RETH_DATADIR` | `/app/data`，卷 `${DATA_DIR}:/app/data`；根 `.env` 默认 `DATA_DIR=./reth_data` | 持久化 | — |
| `RETH_MAXPEERS` | 100 | — | — |
| `OP_NODE_ROLLUP_HALT` | `major` | 遇到严重版本分歧时停下来 | — |
| `OP_NODE_L1_CACHE_SIZE` / `OP_NODE_VERIFIER_L1_CONFS` | 1500 / 4 | L1 确认深度 | — |

> **§3.1 特别核实项的答案：Engine API（8551）+ JWT 只应由节点内部组件访问，官方 compose 本身就没有发布这个端口。** 本报告不建议、也没有任何配置把 8551 或 JWT 接口暴露到公网。

**同步模式（三选一，OPTION 1 为当前生效项）：**

| 方案 | 字段 | 当前状态 |
| --- | --- | --- |
| OPTION 1 | `RETH_GCMODE=full` + `OP_NODE_SYNCMODE=execution-layer` | **生效中**（full 节点，非 archive） |
| OPTION 2 | archive 相关字段 | 注释状态 |
| OPTION 3 | consensus-layer sync | 注释状态 |

### 3.3 Flashblocks 相关字段（本里程碑核心）

| 字段 | `.env.sepolia` 里的真实状态 |
| --- | --- |
| `FLASHBLOCKS_WEBSOCKET_URL` | **默认注释掉**，第 14–15 行是原文注释：`# OPTIONAL - if you want to use the flashblocks feature, uncomment this line.` / `#FLASHBLOCKS_WEBSOCKET_URL=wss://sepolia-flashblocks.giwa.io/ws` |
| 逐文件出现次数（两种口径，实测） | 精确变量名 `FLASHBLOCKS_WEBSOCKET_URL`：`.env` 0、`.env.sepolia` **1**（第 15 行，注释态）、`README.md` **1**（第 86 行，示范写入空值）、`docker-compose.yaml` 0、`reth/entrypoint.sh` **2**（第 23、24 行）、`node/entrypoint.sh` 0、两个 genesis 各 0。<br>不区分大小写的词根 `flashblocks`：`.env` 0、`.env.sepolia` 3、`README.md` 3、`docker-compose.yaml` 0、`reth/entrypoint.sh` 4、`node/entrypoint.sh` 0、genesis 各 0。<br>启动开关 `--flashblocks-url` 全仓只在 `reth/entrypoint.sh` 出现 **1** 次 |

**README 的启用步骤实测写的是 `FLASHBLOCKS_WEBSOCKET_URL=`（等号后为空）**，而 `reth/entrypoint.sh` 的判断是：

```bash
if [[ -n "${FLASHBLOCKS_WEBSOCKET_URL:-}" ]]; then
    ADDITIONAL_ARGS="$ADDITIONAL_ARGS --flashblocks-url=$FLASHBLOCKS_WEBSOCKET_URL"
    echo "Running in flashblocks support mode"
else
    echo "Running in vanilla mode"
fi
```

⇒ **字面照 README 做（设为空值）＝ 走 else 分支 ＝ vanilla 模式，节点照常启动、不报错。** 「环境变量存在」不等于「Flashblocks 已启用」，这条正是任务书 §3.2 禁止的推断，官方脚本亲自示范了它为什么不成立。

`reth/entrypoint.sh` 其余启动参数（决定本地节点暴露什么能力）：`exec op-reth node` + `--ws.addr=0.0.0.0 --ws.api=web3,debug,eth,net,txpool` + `--http.addr=0.0.0.0 --http.api=web3,debug,eth,net,txpool,miner` + `--authrpc.addr=0.0.0.0` + `--rollup.sequencer-http=$RETH_ROLLUP_SEQUENCERHTTP` + `--rollup.disable-tx-pool-gossip` + `--chain=$GENESIS_FILE` + （GCMODE=full 时）`--full`。
其中 `RETH_ROLLUP_SEQUENCERHTTP=https://sepolia-sequencer.giwa.io`——**默认值仍是公共端点，自建节点并不会自带一条私有 sequencer 通道。**

### 3.4 硬件与同步检查

| 项 | 官方数值（docs `node-operators/get-started`，页面自标 1 年前更新） |
| --- | --- |
| Testnet 最低 | 4 核 / 8 GB RAM / 500 GB NVMe |
| Testnet 推荐 | 8+ 核 / 16+ GB / 1 TB NVMe |
| 同步状态检查 | `curl http://localhost:8545 -X POST -H "Content-Type: application/json" --data '{"jsonrpc":"2.0","method":"eth_syncing","params":[],"id":0}'` |

注意两处版本相关风险：硬件表来自 1 年前的页面，未经 v0.6.0（op-reth）重测；官方给的检查方法就是 `eth_syncing`，而机器人从未调用它（§9）。

---

## 4. Flashblocks：上游、节点能力、客户端 RPC 三者的关系

任务书 §3.2 要求严格区分三个概念。本审计的结论如下（每一条都标了出处，出处不是官方代码的写"未验证"）。

```
A. Flashblocks 上游数据源
   预确认数据服务（官方 Flashblocks WS 端点，形如 wss://sepolia-flashblocks.giwa.io/ws）
        │  仅当 FLASHBLOCKS_WEBSOCKET_URL 非空时，op-reth 以 --flashblocks-url=<该值> 连接
        ▼
B. GIWA 节点自身的 Flashblocks 接入能力
   op-reth 进程（v2.3.3）内部把收到的预确认数据接入其 pending 视图 / engine 侧
   —— op-node（v1.19.1）侧没有任何 Flashblocks 配置字段（官方仓库逐文件计数为 0）
        │  对外通过节点自身的 JSON-RPC 口暴露（本审计未验证其形状，见 §14）
        ▼
C. 机器人消费用的 Flashblocks-aware RPC 接口
   公共端点实测：https://sepolia-rpc-flashblocks.giwa.io（标准 JSON-RPC 语义，pending 可读）
   本地对应物：自建节点的 8545（HTTP）/ 8546（WS）是否等价 —— 未验证
```

逐条回答任务书列出的 7 个问题：

| 问题 | 答案 | 依据 |
| --- | --- | --- |
| `FLASHBLOCKS_WEBSOCKET_URL` 由哪个组件读取？ | **只有 op-reth**（经 `reth/entrypoint.sh` 转成 `--flashblocks-url`）。`node/entrypoint.sh` 只有 `exec op-node "$@"`，`docker-compose.yaml` 里 0 次出现 | 官方仓库逐文件关键词计数 + 脚本原文 |
| 连接失败时节点如何处理？ | **未验证**（沙箱内 401，无法检索 op-reth 上游实现；不猜测） | §2.4 |
| 是否需要额外启动参数或构建功能？ | 需要且只需要一个启动参数 `--flashblocks-url`；构建侧无 feature flag（`reth/Dockerfile` 只有 `just maxperf`） | 官方脚本 |
| 节点如何把收到的数据暴露给 JSON-RPC 层？ | **未验证。** 官方仓库层面看不到任何"新增 RPC 方法/新端口"的配置；从 `--http.api/--ws.api` 列表（web3,debug,eth,net,txpool[,miner]）看，没有 Flashblocks 专用命名空间 | 官方 entrypoint 参数表 |
| 自建节点的普通 HTTP JSON-RPC 是否天然支持 Flashblocks pending 语义？ | **不能宣称支持。** 无官方文档或配置证明这一点；公共文档只说「Flashblocks RPC 用起来就是标准 JSON-RPC，但必须由支持该模式的 provider 提供（这类端点叫 Flashblocks-aware）」 | docs flashblocks 页原文 + 缺乏反证 |
| 本地 Flashblocks-aware RPC 是否与 canonical RPC 同端口？ | **未验证，且本阶段禁止假设。** 已知：canonical 走 8545、WS 走 8546，是两个不同端口；Flashblocks-aware 若存在会落在哪个端口没有任何官方证据 | §9 禁止项 + compose 端口表 |
| 如果不同，分别对应什么接口和配置？ | 现阶段无法回答；M12-B 的验收方式见 §14（实测探测，不许按端口号猜） | — |

**另有一条重要区分（来自 §4 的 A→C 链）：官方 Flashblocks 文档把 "provider 必须支持该模式" 写成使用前提，并明确建议「运行你自己的 Flashblocks-aware RPC 节点」。** 但"自建节点 = Flashblocks-aware"这一步在官方仓库里**没有被证明**：仓库只给了把上游数据接进 op-reth 的那一个参数，没给端点侧的能力说明。这正是本阶段判 `LOCAL_FLASHBLOCKS = NOT_VERIFIED` 的原因。

---

## 5. 全部 RPC endpoint 与配置来源清单

### 5.1 生产路径上的端点入口（机器人真正用的）

| # | 文件:行 | 变量/配置名 | 当前来源 | 使用者 | 协议 | 用途 | 是否需要修改 | 建议目标 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | `crates/cli/src/lib.rs:150` | `--rpc-url` / `GIWA_RPC_URL`（`LiveArgs`） | CLI 或环境变量，**无默认值** | `pipeline::runner`（canonical HeadReader + ChainAdapter） | HTTP（WS 另有其口） | canonical 读状态、pending 读候选、receipt | **是（值）** | `http://127.0.0.1:8545` |
| 2 | `crates/cli/src/lib.rs:155` | `--ws-url` / `GIWA_WS_URL`（`LiveArgs`） | CLI 或环境变量，无默认值 | `chain::ws` + WS HeadReader | WebSocket | canonical heads 订阅；不支持则自动回落轮询 | 可选（值） | `ws://127.0.0.1:8546`，**须先实测订阅是否可用** |
| 3 | `crates/cli/src/lib.rs:159` | `--flashblocks-url` / `GIWA_FLASHBLOCKS_URL`（`LiveArgs`） | CLI 或环境变量，无默认值 | `pipeline::runner:1229-1265` → `FlashblockSource` | HTTP 轮询 `pending` | pending 候选视图（不推进状态） | **待验证** | 只有 §14 的本地探测通过后才填本地值；否则保持不配 |
| 4 | `crates/cli/src/lib.rs:402` | `--rpc-url` / `GIWA_RPC_URL`（`ValidateArgs`） | 同上，无默认值 | 执行 lane（fee/nonce/chain/submit 四槽同一 adapter） | HTTP | 计价、nonce、提交、receipt | **是（值）** | `http://127.0.0.1:8545` |
| 5 | `crates/cli/src/lib.rs:828` | `--rpc-url` / `GIWA_RPC_URL`（`ArbitrageArgs`） | 同上，无默认值 | 套利 route run | HTTP | 读、计价、提交 | **是（值）** | 同上 |
| 6 | `crates/chain/src/rpc.rs:77`（`connect`）→ `:92`（`connect_with_trace`） | `HttpChainAdapter::connect(url)` | 构造参数（来自上表 1/4/5） | 全仓唯一 HTTP 出口 | HTTP | 全部 HTTP JSON-RPC | 否 | — |
| 7 | `crates/chain/src/ws.rs:153/188` | `WsRpcClient::request/subscribe` | 构造参数（来自上表 2） | 唯一 WS 出口 | WebSocket | canonical heads | 否 | — |
| 8 | `crates/execution/src/giwa/sequencer_direct.rs`（`http: HttpChainAdapter` 字段） | 复用 #6 | 构造参数 | 执行 lane | HTTP | 读 + 提交 | 否 | — |

环境变量名全量（`"…"` 字面量计数，含测试）：`GIWA_RPC_URL` 29、`GIWA_EXECUTION_PRIVATE_KEY` 6、`GIWA_WS_URL` 5、`GIWA_FLASHBLOCKS_URL` 5、`GIWA_FLASHBLOCKS_RPC_URL` 2、`RETH_ROLLUP_SEQUENCERHTTP` 1。
生产代码里读环境变量的只有两处：`crates/execution/src/signer.rs:202` 与 `:388`（`PRIVATE_KEY_ENV = "GIWA_EXECUTION_PRIVATE_KEY"`，定义在 `signer.rs:24`）；其余端点一律经 clap 的 `env = "…"` 声明。

### 5.2 硬编码 URL 字面量（全部不在生产路径）

| 位置类型 | 内容 | 数量 | 是否生产路径 |
| --- | --- | --- | --- |
| `crates/*/src`（生产代码） | 任何 `http(s)://`、`ws(s)://`、`91342` | **0** | — （由 `crates/cli/tests/no_execution.rs:300` 强制，该测试遍历全部 16 个 crate 的 `src`，并在 `#[cfg(test)]` 处截断、剔除注释行） |
| `crates/*/tests` doc/prose | `https://sepolia-rpc.giwa.io` 出现在运行说明里：`simulation/tests/real_market_fee.rs:7`、`simulation/tests/multihop_capture.rs:8`、`execution/tests/sequencer_direct_probe.rs:16`、`execution/tests/live_reads_probe.rs:6` 与 `:470` | 5 | 否 |
| `crates/*/tests` 代码 | `https://sepolia-rpc.giwa.io` 作为 `#[ignore]` 探针的默认值：`replay/tests/m1_replay.rs:551`（`#[ignore = "reads the live RPC"]`）、`replay/tests/record_fixtures.rs:393`（`#[ignore = "reads the live RPC; run deliberately to refresh the real fixtures"]`） | 2 | 否（需 `--ignored` 显式触发） |
| `crates/*/tests` doc/prose | `https://sepolia-sequencer.giwa.io` | 1 | 否 |
| `crates/*/tests` | Flashblocks URL 字面量 | **0** | — |
| `crates/*/tests` | `http://127.0.0.1:*`（本地 mock/TcpListener） | 23 | 否 |

### 5.3 录制数据里的端点（历史证据，不参与运行）

`data/` 目录实测：

| URL 形态 | 出现次数 | 文件数 |
| --- | --- | --- |
| `https://sepolia-rpc.giwa.io` | 183 | 103 |
| `wss://sepolia-rpc.giwa.io` | 5 | 5 |
| `https://sepolia-rpc-flashblocks.giwa.io` | 17 | 9 |
| `wss://sepolia-rpc-flashblocks.giwa.io/ws` | 3 | 3 |
| `https://sepolia-sequencer.giwa.io` | 2 | 2 |
| `wss://sepolia-flashblocks.giwa.io`（官方 `.env.sepolia` 示范的那个主机名） | **0** | 0 |

> 最后一行值得注意：**仓库里从未记录过官方 `.env.sepolia` 示范的 Flashblocks WS 主机名**；机器人实测的是 `sepolia-rpc-flashblocks` 这个 RPC 形态端点。两者是否是同一服务，属于 §14 未验证项。
>
> 证据表内部一律用端点摘要（`rpc-…`）而非 URL 标识端点（`crates/chain/src/rpc_trace.rs:448`：「the digest identity … Emitted in place of the URL」，keccak256 在 `:934`，`with_endpoint` 在 `:439` 只把原始 URL 留作脱敏键）；`data/evidence/m9/m9.4/protocol.json` 里两个摘要分别是 canonical `rpc-faa716cada04a9ef`、flashblocks `rpc-a9eb0c34d5519e0a`。

### 5.4 是否存在"绕过统一 adapter"的网络访问

不存在。全仓 HTTP/WS 客户端只在两个咽喉点建立：`crates/chain/src/rpc.rs:189`（`request_with`）与 `crates/chain/src/ws.rs:153`（`request`）；生产 JSON-RPC 请求点共 **27** 处，全部落在这两个咽喉点之内（逐点清单见 §9.1）。`reqwest::Client` 只在 `rpc.rs:93` 构造一次；`TcpListener` 只出现在测试里（`crates/chain/tests/rpc_trace_safety.rs`、`crates/simulation/tests/support/stub.rs`）。

---

## 6. 现有 RPC 调用关系图

```
                          ┌───────────────────────────────────────────────┐
  CLI 参数 / 环境变量      │  crates/cli/src/lib.rs                        │
  GIWA_RPC_URL ──────────►│   :150 LiveArgs.rpc_url                       │
  GIWA_WS_URL ───────────►│   :155 LiveArgs.ws_url                        │
  GIWA_FLASHBLOCKS_URL ──►│   :159 LiveArgs.flashblocks_url               │
  (--execution-mode 需 :325-329 同时给 rpc_url；replay 例外 :310-315)      │
                          └───────┬───────────────────┬───────────────────┘
                                  │ PipelineConfig     │
                     crates/pipeline/src/config.rs :152-158（Option<String>，无默认）
                                  │
        ┌─────────────────────────┼──────────────────────────────┐
        ▼                         ▼                              ▼
  canonical 源               candidate 源                  执行 lane
  runner.rs build_canonical  runner.rs:1229-1265           arbitrage.rs:340 / cli:556
        │                         │                              │
        ▼                         ▼                              ▼
  HeadReader 实现选择        HttpChainAdapter::connect      GiwaSequencerDirect::connect
  http → head.rs:106           (:1229)                      (stage.rs:338 / sequence.rs:1477)
  ws   → head.rs:159              │                              │
        │                    FlashblockSource                4 个能力槽共用同一 adapter
  PollingFrameSource         live/src/flashblocks.rs        (submitter/fees/nonces/chain)
  （M9.4 Radar，未接线）        :209/:435 pending_raw()            │
        │                                                       ▼
        ▼                                              may_submit() && may_broadcast()
  WsRpcClient / 同一 HttpChainAdapter                   sequencer_direct.rs:336/:347
        │                                                       │
        └──────────────┬────────────────────────────────────────┘
                       ▼
              两个咽喉点（唯一出口）
              chain/src/rpc.rs:189  request_with   （HTTP，20s 超时，1 次重试）
              chain/src/ws.rs:153   request        （WS）+ :188 subscribe
                       │
                       ▼
              RpcTraceSink：端点只落摘要 rpc-…，URL 作为脱敏键

  状态推进的唯一闸门：live/src/event.rs:46  SourceKind::is_canonical()
      Canonical   → 唯一能推进 StateStore 的变体
      Candidate   → 永不推进（§27）
      候选与 canonical 的裁决：CandidateResolved{candidate_hashes, matched_hash, canonical_hash, …}
```

依赖方向补充：`evm-discovery`、`evm-pathfinder` 均**不是** `evm-cli` / `evm-pipeline` 的依赖，所以 `eth_getLogs` 这个唯一站点（`rpc.rs:710`）虽然在库代码里，热路径上当前不经过它（详见 §9.2）。

---

## 7. ChainAdapter / HeadReader / FrameSource 的职责边界

### 7.1 `HttpChainAdapter`（`crates/chain/src/rpc.rs`）— 本阶段结论：继续复用，不改

| 审计项（§4.2） | 实测答案 | 出处 |
| --- | --- | --- |
| endpoint 是否在创建时固定 | **是**。`connect_with_trace(url, sink)` 把 `url` 存进结构体（`self.url = url.to_owned()`），之后所有请求都用它；换端点只能重建 | `rpc.rs:92`、`:109` |
| `eth_chainId` 读取与校验 | 建连时发一次 `eth_chainId`（`:102`）学到链 id 并存下来；校验发生在调用方：`pipeline::runner`（registry ↔ 端点）、`arbitrage.rs:345`、`cli/lib.rs:559`、`GiwaSequencerDirect::connect_with_trace:94` | 四处闸门 |
| HTTP 超时 | `reqwest::Client::builder().timeout(20s)`，整个进程一个 client | `rpc.rs:93-94` |
| 重试次数与条件 | **最多 1 次重试**（`for _ in 0..2`）。只对"传输层失败"重试（`CLASS_SEND_FAILED`、`CLASS_NON_JSON`、`CLASS_HTTP_STATUS` → `terminal:false`）；节点明确拒绝（`CLASS_NODE_REJECTED`）与解码失败（`CLASS_DECODE_FAILED`）标记 `terminal:true` 不重试 | `rpc.rs:211-212`、`one_attempt :304` |
| JSON-RPC error 处理 | `error` 字段 → `CLASS_NODE_REJECTED` → `ChainError::RpcRejected`（`rpc.rs:70`）；有 `result`（即使是 `null`）算作 Answered；缺 `result` → `CLASS_DECODE_FAILED` | `rpc.rs:70-71`、`one_attempt` |
| RPC trace 是否记录端点或敏感信息 | 表里只记摘要 `rpc-…`（`rpc_trace.rs:448`）；原始 URL 只作脱敏键（`:439`）；脱敏行为有测试（`:1255`）。**符合 §5 第 7 条** | 同左 |
| 实际提供的方法 | 12 个站点对应 12 种方法（见 §9.1） | `rpc.rs` |
| 是否有额外 RPC 客户端绕过 | **没有**（§5.4） | 全仓实测 |

`get_block` 在 `rpc.rs:546`：端点答 `null` 时返回 `ChainError::MissingData("block {n} not available")`。**这是"节点还没同步"与"链上确实没这个块"在代码层无法区分的根因**（§12、§16）。

### 7.2 `HeadReader`（`crates/chain/src/head.rs`）

- 契约在 `:35/:47/:52/:62`；HTTP 实现 `transport "http"`（`:106`），WS 实现 `:159+`。
- **`pending_raw()` 实际发的是 `eth_getBlockByNumber(["pending", false])`**（`:133`，`request_raw`）；返回 `null` 时给 `Ok(None)`（`:135`；`:62` 是 trait 的默认空实现，`:131` 才是 HTTP 实现），请求失败/解码失败则向上抛 `ChainError`（走 §7.1 的分类）。
- 候选块回查 `candidate_read` 用 `eth_getBlockByHash [hash, false]`（`:139`）。
- **pending 读取与 canonical 读取用同一个 endpoint、同一个 adapter 实例**——这是"本地只有一个 HTTP 口"能成立的前提，也是"公共 Flashblocks 端点 ≠ 本地 RPC"这条禁令在代码层的体现（`:1229` 的 Flashblocks 源同样是 `HttpChainAdapter::connect(config.flashblocks_url)`，只是 URL 不同）。
- 当前实现是 HTTP 轮询；WS 侧的 `pending` 分支（`:213`）也发同样的参数。
- **没有任何代码把 pending 状态写进 canonical StateStore**：闸门是 `live/src/event.rs:46` 的 `is_canonical()`，`Candidate` 变体的注释直接写明"Never advances state (§27)"；这条边界另有 7 道结构性测试守着（§8.3）。
- **pending hash 被当最终身份的风险**：不存在于状态路径（`CandidateResolved` 同时带 `candidate_hashes / matched_hash / canonical_hash`，裁决用后者），但存在于一处展示——`flashblocks.rs:251/262/270` 的 capability 行把 `probe_method` 记为 `eth_getBlockByHash` 并在探测里用 pending 自身 hash 回查，注释已声明"reading *state* at it is still unproven, so no candidate is applied to the store"（`:264`）。

### 7.3 `FrameSource`（`crates/live/src/preconf_provider.rs`）

- 契约 `:34`：`next_view`，加上两个默认方法 `supports_pending_receipts()`（默认 `false`）与 `next_receipts()`（默认 `Ok(None)`），后两者的注释写明「§52: `UNKNOWN`，不是 `SUPPORTED`」。
- 现有实现：`PollingFrameSource`（`:79`，`transport "http-poll-pending"`，`next_view` → `reader.pending_raw()` @ `:105`）、`ReplayFrameSource`（`:125`，喂 `RecordedFrame` `:118`）。
- 传输循环 `PreconfLink<F>`（`preconf_loop.rs:234`）**对 `FrameSource` 泛型**：⇒ 将来加一个本地数据源（例如直连节点的另一形态接口）只需新增一个 `impl FrameSource`，**不必改 `preconf_decode`、不必改 `EarlyRadar` 核心语义**。这是 §4.4 要的答案：**抽象够用。**
- 唯一真实障碍不在抽象，在数据形状：见 §8.2。

---

## 8. M9.4 Early Radar 兼容性分析

### 8.1 隔离性（实测，可复核）

| 断言 | 实测 |
| --- | --- |
| Radar 相关文件（`preconf_provider.rs` / `preconf_radar.rs` / `preconf_loop.rs` / `preconf_decode.rs` / `preconf.rs`）在 `crates/live` 之外的生产代码里被引用 | **0 处**。`grep -rn "PollingFrameSource\|EarlyRadar\|PreconfLink\|note_canonical" crates/*/src` 除 `crates/live/` 外无命中 |
| 结构隔离测试 | `crates/live/tests/preconf_isolation.rs` 7 道：`:301` 以构建图本身为隔离、`:355` 无 radar 源文件命名 canonical writer、`:374` 反向、`:410` pool state 无 source 字段且 core 无 flashblocks 变体、`:458` radar 类型不携带储备/价格、`:497` canonical 桥是"仅身份、单向"（断言 `pub fn note_canonical(` 存在）、`:523` 闸门要有暴露面 |
| `flashblocks.rs:13-17` 的定位 | 原文："它做不到的事是触及状态引擎"（What it cannot do is reach the state engine） |

⇒ 本阶段**不需要**为 Radar 做任何适配，也没有修改它的理由；`§2.3` 的三条禁令（不覆盖 canonical StateStore、不用未验证 pending 驱动提交、不为采集加 RPC）当前均由测试而非约定保证。

### 8.2 数据形状差异（本审计发现的主要兼容性问题）

M12-A 任务书 §4.4 第一条前提写的是「当前实测 endpoint 使用 HTTP polling 读取 `eth_getBlockByNumber(["pending", true])`」。**这条前提与生产代码不一致，实测如下：**

| 角色 | 请求形状 | 出处 |
| --- | --- | --- |
| 生产轮询（M5 候选源、Radar 的 `PollingFrameSource` 共用的 `HeadReader::pending_raw()`） | `["pending", **false**]` | `crates/chain/src/head.rs:133`（HTTP）、`:213`（WS 分支） |
| M9.4 协议取证（`#[ignore]` 的实端点探针） | `["pending", **true**]` | `crates/live/tests/preconf_live_giwa.rs:210`，注释 `:37/:181` 同写法 |
| M9.4 录制证据里的形状 | **4348 条全部 `full_object`** | `data/evidence/m9/m9.4/protocol.json.transaction_shapes` |
| Radar 解码器对"只有 hash 的列表"的态度 | **拒绝**：`PreconfError::HashOnlyTransaction`，注释说明"hash-only 不能命名目标，因此整帧拒绝而不是半解码" | `crates/live/src/preconf_decode.rs:105-116`（`transaction_from_value`） |

后果（这是设计层面的结论，不是猜测）：若把 `EarlyRadar` 通过现成的 `PollingFrameSource` 接到 `HeadReader::pending_raw()` 上，**每一帧都会因为 `false`（交易列表为 hash 串）而被 fail-closed 拒绝**。
而 M5 的候选路径不受影响，因为它只数数组长度（`flashblocks.rs:156-159` 的 `transaction_count`）并要求 `gasUsed` 存在，不解析交易对象。

⇒ **正确做法（符合 §4.4「不得直接修改已有协议假设」）：将来若要接 Radar，新增一个 provider adapter，让它自己发 `["pending", true]`；不改 `pending_raw()`、不改 `EarlyRadar`、不改解码器。** 本地节点是否支持该形状，属 §14 未验证项。

### 8.3 Radar 与 M11 执行链的适配需求

M11 的 Multi-Lane（`crates/execution/src/lanes.rs`）、nonce/资金预留、ReceiptTracker 均**不消费 Flashblocks 数据**：执行链路的输入来自 canonical 路径（`preflight_facts.rs:381` 与 `sequencer_direct.rs:171` 都取 `["latest", false]`）。
⇒ **M12 对 M11 的适配需求是 0。** 本阶段未触碰 M10/M11 任何文件（§1.2 的 diff 检查覆盖）。

---

## 9. JSON-RPC 方法的实际使用矩阵

### 9.1 生产请求点逐点清单（27 处，全部经两个咽喉点）

| # | 文件:行 | 方法 | 参数/block tag | 用途 |
| --- | --- | --- | --- | --- |
| 1 | `chain/src/rpc.rs:102` | `eth_chainId` | `[]` | 建连时学链 id |
| 2 | `chain/src/rpc.rs:542` | `eth_blockNumber` | `[]` | canonical 头高 |
| 3 | `chain/src/rpc.rs:548` | `eth_getBlockByNumber` | `[<n>, false]` | 取块（无交易体） |
| 4 | `chain/src/rpc.rs:561` | `eth_getBlockByNumber` | `[<n>, false]` | 取块上下文 |
| 5 | `chain/src/rpc.rs:610` | `eth_getBlockByNumber` | `[<n>, true]` | 取块（含完整交易） |
| 6 | `chain/src/rpc.rs:642` | `eth_getBlockReceipts` | `[<n>]` | 整块 receipt |
| 7 | `chain/src/rpc.rs:710` | `eth_getLogs` | `[filter]` | 事件扫描（仅 discovery 使用，见 §9.2） |
| 8 | `chain/src/rpc.rs:725` | `eth_call` | `[{…}, <tag>]` | 合约读 |
| 9 | `chain/src/rpc.rs:734` | `eth_getCode` | `[addr, <tag>]` | 字节码 |
| 10 | `chain/src/rpc.rs:742` | `eth_getBalance` | `[addr, <tag>]` | 余额 |
| 11 | `chain/src/rpc.rs:752` | `eth_getStorageAt` | `[addr, slot, <tag>]` | 储备量 |
| 12 | `chain/src/rpc.rs:766` | `eth_getTransactionCount` | `[addr, <tag>]` | nonce |
| 13 | `chain/src/head.rs:133` | `eth_getBlockByNumber` | `["pending", false]` | 候选视图（HTTP 轮询） |
| 14 | `chain/src/head.rs:139` | `eth_getBlockByHash` | `[hash, false]` | 候选↔canonical 回查 |
| 15 | `chain/src/head.rs:159` | `eth_chainId` | `[]` | WS 侧链 id 核验 |
| 16 | `chain/src/head.rs:179` | `eth_getBlockByNumber` | `[tag, false]` | WS 侧取头 |
| 17 | `chain/src/head.rs:197` | `eth_blockNumber` | `[]` | WS 侧头高 |
| 18 | `chain/src/head.rs:213` | `eth_getBlockByNumber` | `["pending", false]` | WS 侧 pending |
| 19 | `chain/src/head.rs:220` | `eth_getBlockByHash` | `[hash, false]` | WS 侧回查 |
| 20 | `chain/src/ws.rs:191` | `eth_subscribe` | `newHeads` / `newBlockHeaders` | 订阅（失败即回落轮询） |
| 21 | `execution/src/giwa/preflight_facts.rs:381` | `eth_getBlockByNumber` | `["latest", false]` | 预检事实 |
| 22 | `execution/src/giwa/sequencer_direct.rs:171` | `eth_getBlockByNumber` | `["latest", false]` | 端点持有的块 hash |
| 23 | `execution/src/giwa/sequencer_direct.rs:192` | `eth_chainId` | `[]` | 提交前再核验 |
| 24 | `execution/src/giwa/sequencer_direct.rs:202` | `eth_maxPriorityFeePerGas` | `[]` | tip；缺失可容忍（`:206` → `Ok(None)`） |
| 25 | `execution/src/giwa/sequencer_direct.rs:307` | `eth_getTransactionCount` | `[addr, "pending"]` | 待提交 nonce |
| 26 | `execution/src/giwa/sequencer_direct.rs:359` | `eth_sendRawTransaction` | `[raw]` | 广播（受双闸门） |
| 27 | `execution/src/giwa/sequencer_direct.rs:407` | `eth_getTransactionReceipt` | `[hash]` | receipt；`null` 合法地表示"还没" |

（其余形似方法名的字符串不是请求点：`ws.rs:375` 的 `"eth_subscription"` 是通知方法名比较；`flashblocks.rs:251/262/270` 的 `"probe_method"` 是证据字段；`preflight_facts.rs:711` 的 `"eth_getTransactionCount"` 是 `source:` 标签；`cli/lib.rs:1333`、`runner.rs:903` 的 `"net_profit"` 是指标字段名。）

### 9.2 任务书 15 个候选方法逐项判定

| 方法 | 生产请求点 | 依赖 archive/历史状态 | 需要的 block tag | 需要 WS | 受同步状态影响 | 与 Flashblocks pending 语义相关 | 仅测试/证据/非热路径 | 公共端点实测（`data/evidence/m5/method-matrix.json`，sha256 `f0516ea7…0e15`，20699 字节） | 本地自建节点 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `eth_chainId` | 3（#1/#15/#23） | 否 | — | 否 | 否 | 否 | 否 | `0x164ce` ✓（4 种 host×transport 全通过） | 未验证 |
| `eth_blockNumber` | 2（#2/#17） | 否 | — | 否 | **是**（未同步→头高偏小） | 否 | 否 | `0x23bad5b` ✓ | 未验证 |
| `eth_getBlockByNumber` | 8（#3/#4/#5/#13/#16/#18/#21/#22） | 取决于块高 | `latest` / `pending` / `<n>` | 否 | **是** | **是**（pending） | 否 | 对象 ✓（`#full` 也通过） | 未验证 |
| `eth_getLogs` | 1（#7） | 取决于区间深度 | fromBlock/toBlock | 否 | **是** | 否 | **是**：唯一消费者是 `evm-discovery`（`scan.rs:302`、`reads.rs:282`、`reconstruct.rs:458/:589`），而 discovery 不是 cli/pipeline 的依赖 ⇒ 热路径不经过 | 数组 ✓；容量实测：10001 块区间可过、20000 块被 HTTP 400 拒；可回溯到块 7,219,032（`data/evidence/m9/m9.2/raw/probe-node-capacity.json`，4365 字节） | 未验证 |
| `eth_call` | 1（#8，typed `.call()`：`execution/src/giwa/reads.rs:145/:196/:282/:316`） | **是**（历史块状态需 archive） | `latest` / `<n>` / `pending` | 否 | **是** | **是**（官方 9 个 pending 语义方法之一） | 否 | 应答 ✓（探针 calldata 返回 `code 3 execution reverted`，说明方法可达） | 未验证 |
| `eth_getCode` | 1（#9） | **是** | `latest` / `<n>` | 否 | 是 | 否 | 否 | `0x60806040…` ✓ | 未验证 |
| `eth_getBalance` | 1（#10） | **是** | `latest` / `<n>` | 否 | 是 | **是** | 否 | `0x0` ✓ | 未验证 |
| `eth_getTransactionCount` | 2（#12/#25） | **是** | `latest` / `pending` | 否 | 是 | **是**（`pending` nonce） | 否 | `0x1` ✓ | 未验证 |
| `eth_getTransactionReceipt` | 1（#27） | 否（receipt 存在性） | by hash | 否 | 是 | **是**（官方列 pending receipt 语义） | 否 | **不在此矩阵内**；由 M6/M7/M10 的真实提交运行证据覆盖（`data/evidence/m6|m7|m10/`） | 未验证 |
| `eth_sendRawTransaction` | 1（#26） | — | — | 否 | 否 | 否 | 否 | 探针返回 `-32602 typed transaction too short` ⇒ **方法在白名单内**（M7 §23 亦实测可提交） | 未验证 |
| `eth_estimateGas` | **0**（只在 `pipeline/src/canonicalization.rs:630` 的分类臂 + M8.6 的 `CLASSIFIED_BUT_UNCALLED_METHODS` 判定，`pipeline/tests/m86_census/mod.rs:389`） | 是 | tag | 否 | 是 | **是**（官方列表内） | **是：已分类、未调用** | 应答 ✓（`code 3`） | 未验证 |
| `eth_gasPrice` | **0**（同上，非请求点；`src` 内 3 次命中均为分类/文档） | 否 | — | 否 | 是 | 否 | **是：未调用** | `0xf43b8` ✓ | 未验证 |
| `eth_feeHistory` | **0（整仓 `.rs` 命中 0 次）** | 否 | — | 否 | 是 | 否 | 是：从未使用 | 对象 ✓（含 `baseFeePerBlobGas`） | 未验证 |
| `eth_getTransactionByHash` | **0**（`src` 仅 `core/src/evidence.rs:15` 文档；其余在 2 个 `#[ignore]` 探针） | 否 | by hash | 否 | 是 | **是**（官方列表内） | 是：未调用 | **不在此矩阵内** ⇒ 公共端点是否应答也属未验证 | 未验证 |
| `eth_syncing` | **0（含测试在内，整仓命中 0 次）** | 否 | — | 否 | — | 否 | 是：**从未使用** | `false` ✓（4 种组合全部应答） | 未验证 |

**GIWA / OP Stack 特定扩展方法**（同一实测矩阵，公共端点全部 `-32601 rpc method is not whitelisted`）：`eth_sendTransaction`、`txpool_status`、`trace_block`、`rpc_methods`、`eth_getBlockRollupHashes`、`giwa_subscribe`、`eth_flashblocks`、`engine_subscribe`。
`eth_subscribe` 的四种标准 kind 在公共端点均返回 `-32603 Internal error`（含 `newHeads`、`newBlockHeaders`、`logs`、`syncing`，以及猜测的 flashblocks 类名）⇒ **公共端点无订阅能力，这正是当前 HTTP 轮询形态的由来**；机器人侧对 kind 的白名单是 `["newHeads","newBlockHeaders"]`（`live/src/websocket.rs:76`），失败回落轮询在 `:81-95`。

**Private / Direct Sequencer：** 保持条件能力，本阶段不作为依赖。`sequencer_direct.rs:156` 的原文判定是「BLOCKED: no direct-sequencer protocol exists on the GIWA testnet endpoint」，取证在 `data/evidence/m7/probe-sequencer-direct.json`（114586 字节，sha256 `ce9f4f5a…1622b`）。同时 `RETH_ROLLUP_SEQUENCERHTTP` 在官方 `.env.sepolia` 里默认指向公共 `https://sepolia-sequencer.giwa.io` ⇒ **自建节点的"读"是本地的，"提交路径"默认仍走公共 sequencer**（§10 第 5 项、§16 第 4 项）。

### 9.3 每把运行的实测请求量（不靠估算）

`data/evidence/m8/m8.6/rpc_surface.json`：3 次 route run 共 **246** 次物理请求、9 种方法。该矩阵里 `eth_getTransactionReceipt` 计数为 0（因为它只在 M6/M7/M10 的提交类运行里出现）—— 引用时须注意口径。

---

## 10. 本地节点接入所需的最小修改清单

**结论：0 行生产代码。** 全部是配置值与运维动作。

| # | 动作 | 当前值 → 目标 | 类型 | 卡点 |
| --- | --- | --- | --- | --- |
| 1 | 部署节点并等它同步完成（§3.4 检查法） | — | 运维 | 需要一台 8 核/16 GB/1 TB NVMe 机器 + 一个可用的 Sepolia L1 RPC 与 L1 beacon |
| 2 | `GIWA_RPC_URL`（或 `--rpc-url`） | `https://sepolia-rpc.giwa.io` → `http://127.0.0.1:8545` | 配置值 | 启动时的链 id 闸门会核验它答的是 91342 |
| 3 | `GIWA_WS_URL`（或 `--ws-url`） | 未配 → 可选 `ws://127.0.0.1:8546` | 配置值 | **只有 §14.1 的订阅实测通过才配**；不通过则继续 HTTP 轮询（`:155` 缺省时 `canonical_source` 自动取 HttpPoll，`cli/lib.rs:282-301`） |
| 4 | `GIWA_FLASHBLOCKS_URL` | 公共 flashblocks 端点 → **暂时保持"不配"** | 配置值 | §14.2 的本地能力实测通过前不得填本地值；公共值不得当本地值用（§9 禁止项） |
| 5 | 提交路径 | `RETH_ROLLUP_SEQUENCERHTTP` 默认指公共 sequencer | 节点侧配置 | 若希望提交也走本地节点，需明确改节点配置并接受 §14.3 的实测；本阶段不做 |
| 6 | 注册表与证据目录 | `--registry-dir`（多目录合并，全部必须同链，§46）、`--evidence-dir`（默认字面量 `data/evidence/m5/live`，`cli/lib.rs:190`） | 配置值 | 非端点，但换环境时一起改，别遗漏 |
| 7 | 私钥 | `GIWA_EXECUTION_PRIVATE_KEY`（`signer.rs:24`） | 环境变量 | 与端点无关；本阶段不新增签名 |

**明确不需要做的：** 新增 endpoint router / HA / 负载均衡（§2.1 已删，且 §9 禁止）；改写重试（保持 §7.1 的"1 次、只对传输失败"）；新增 RPC 调用；把公共 Flashblocks 端点当本地 RPC。

**一处标签需要留意（属 M12-B 决策，不是本阶段的代码改动）：** 执行 lane 在建连时把端点类型写死为 `EndpointKind::PublicHttpRpc`，共 2 个生产站点（`execution/src/stage.rs:348`、`execution/src/sequence.rs:1489`）。枚举另有 `FlashblocksHttpRpc`（`submitter.rs:34`）与 `Recorded`（`:37`），但 `FlashblocksHttpRpc` 在 `src` 里的构造只出现在单元测试（`submitter.rs:198`，位于 `#[cfg(test)]`@145 之后）。⇒ 自建节点跑起来后，证据表里的端点类型仍会写"public_http_rpc"，这是**命名与实际不符**，不是功能故障；如何标注须由 M12-B 定，且改动面必须锁在这一个语义标签上（§9）。

---

## 11. 配置模型建议

### 11.1 沿用现有体系，不引入 TOML

当前真实配置结构是 `crates/pipeline/src/config.rs` 的 `PipelineConfig`：
`:152 rpc_url: Option<String>`（注释 `:148` 原文「Never defaulted (§44)」）、`:153 ws_url`、`:157 flashblocks_url`、`:158 canonical_source`、`:163 registry_dirs`；`live()` 构造器 `:225-247`（`flashblocks_url: None`、`canonical_source` 由 `ws_url` 推得、`duration: 60s`）；`replay()` `:259-269`（`source.poll_interval_ms = 0`）。

**建议：保持"CLI/env → 单一 config 结构 → adapter 构造参数"这一条现有链路，不新增依赖、不新增配置文件格式。** 任务书 §5 的 TOML 片段只作为语义示意（canonical 与 flashblocks 分列），本仓库已用三个独立字段表达了同样的区分，引入 TOML 只会多一套真值来源（违反 §5 第 2、3 条）。

### 11.2 逐条对照 §5 的 9 项要求（当前是否已满足）

| §5 要求 | 现状 | 证据 |
| --- | --- | --- |
| 1 不散落 endpoint 默认值 | **已满足**（端点侧 0 默认值）；**间隔类默认值有重复**（见 §16 第 3 项） | `config.rs:148-158` + `cli/lib.rs:367/371` vs `live/src/source.rs:51`、`live/src/flashblocks.rs:60` |
| 2 优先复用现有配置体系 | **已满足**（`PipelineConfig` 为唯一真值载体） | `config.rs:146+` |
| 3 不强制新增 TOML | **已满足**（未引入任何配置文件；仓库只有 Cargo 与 rust-toolchain） | §1.1 |
| 4 公共 RPC 只作明确 dev/test 配置，不静默回退 | **已满足**：公共 URL 只出现在测试（其中 2 处代码位还都在 `#[ignore]` 后面）；生产无回退路径 | §5.2 |
| 5 本地 RPC 不可用 → 明确报错或暂停，不静默切回 | **已满足**：单端点，失败即 `ChainError`/`PipelineError`，无第二端点可切；`--rpc-url` 缺失直接拒绝（`cli/lib.rs:310-315`、`:325-329`）；live 运行有链 id 闸门（`runner.rs:1114-1126`） | 同左 |
| 6 启动时核验 chain id 91342 | **已满足，且方式更好**：核验的是"registry 声明的链 ↔ 端点应答的链"一致，而不是拿字面量 91342 比（`91342` 在生产代码里 0 次出现，由测试强制） | `runner.rs:1114-1126`、`arbitrage.rs:345`、`cli/lib.rs:559`、`sequencer_direct.rs:94`、`no_execution.rs:300` |
| 7 配置日志不打印 key/token/敏感 URL 参数 | **已满足**：端点在表里落成摘要 `rpc-…`；URL 仅作脱敏键；有专项测试 | `rpc_trace.rs:439/448/934/1255` |
| 8 HTTP 与 WS 语义分开 | **已满足**：`ws_url` 与 `rpc_url` 是两个字段、两条实现（`head.rs:106` http / `:159+` ws），`transport` 标签不同 | 同左 |
| 9 同址多能力必须由探测或明确配置确定，不猜端口 | **已满足（当前未猜）**：Flashblocks 是独立字段、独立闸门；本地是否同址属 §14 实测项 | `runner.rs:1229-1265` |

### 11.3 M12-B 需要落地的最小配置语义（本阶段不实现）

- 一个显式的"端点用途标签"，把 `EndpointKind` 与实际地址的对应关系记进证据表（§10 第 7 项）。
- 一个"节点就绪"闸门（§12），它是 §5 第 5 条"明确报错或暂停"的具体化。
- 不新增字段种类：现有 `rpc_url` / `ws_url` / `flashblocks_url` 三个 Option 已足够表达 canonical / WS / 候选三种语义。

---

## 12. 需要实现的单节点恢复能力（M12-B 范围，本阶段只列不改）

### 12.1 现有防线（保留，勿删——§2.1 明确要求）

| 能力 | 现状 | 出处 |
| --- | --- | --- |
| 连接超时 | 20s，一个进程级 client | `rpc.rs:93-94` |
| 请求重试 | 1 次，仅传输层失败；节点拒绝与解码失败不重试 | `rpc.rs:211-212`、`one_attempt:304` |
| WS 断连恢复 | 重连 + 订阅失败回落 HTTP 轮询 | `live/src/websocket.rs:76/81-95`、`live/src/preconf_loop.rs` `on_disconnect:814` |
| 安全停机 | 信号处理 + 队列排空 | `runner.rs:1033 shutdown_signal()`、`:1265` 之后 `drop(sink)` 的自然收束 |
| canonical 源失败记录 | 运行期计数 | `runner.rs:300`（`if source.is_canonical()` → `canonical_source_failed_during_run`） |
| pending 视图倒退/跳号 | 识别为异常并记录、不静默吞掉 | `flashblocks.rs:305/:316` |
| 提交与结果未知分离 | `SubmissionOutcome` 不含 `Included`（ inclusion 归 receipt 问题） | `submitter.rs:58+`、`sequencer_direct.rs:336/:347` 双闸门 |

### 12.2 缺口（按危险程度排序）

| # | 缺口 | 现状后果 | 为什么必须补 |
| --- | --- | --- | --- |
| G1 | **无同步状态闸门**：`eth_syncing` 全仓 0 命中 | 未同步节点的"缺块"表现为 `MissingData`（`rpc.rs:546`）或 `pending_raw()→None`（`head.rs:135`）。§7 第 7 条"不得把缺失数据解释成零储备/零利润"目前**没有代码防线** | 公共端点不会给你这个信号（它已同步）；自建节点一定会（首次同步要跑很久） |
| G2 | **"查不到"与"没有"不可区分** | 同上；且 `Ok(None)` 在候选路径是合法答案，无法反推节点健康 | 需要一处把"节点说自己还没到"与"链上确实没有"分开的判断 |
| G3 | 节点重启后的状态失效规则未成文 | `FlashblockSource` 的 `seen_numbers/last_number` 会因高度倒退被判异常（`flashblocks.rs:305`），但 GraphSnapshot / 已预留 nonce / lane 状态该失效哪些，没有一条明确规则 | §7 第 4 条要求；也是 §14 验收项 |
| G4 | 无"节点身份"证据 | 证据表已有端点摘要（`rpc-…`），但没有记录"这次跑的是本地节点还是公共节点" | 换环境后无法区分两批证据（§10 第 7 项的标签问题同源） |

> 补 G1 会引入一次新的 RPC 调用（`eth_syncing`）。**本阶段不做**（§9 禁止"新增未经证明的 RPC 调用"），列为 M12-B 第一项，并要求它同时给出"该调用在本地节点上的实测应答形状"作为证据（官方部署文档给的正是这个调用，见 §3.4）。

---

## 13. mock / replay 测试矩阵（供 M12-B/C 直接取用）

### 13.1 可复用的既有资产

| 资产 | 能力 | 位置 |
| --- | --- | --- |
| 真 TCP 假端点（4 种行为） | `Answer` / `Flaky`（首个 500、次个 200，用来分辨"重试的调用"与"两次调用"） / `Rejected`（JSON-RPC error） / `Empty`（200 且无 `result`） | `crates/chain/tests/rpc_trace_safety.rs:48-59`、`:110 serve()` |
| 状态桩 + 到达计数 | `ServedState::answer(method, params)`、`Stub::spawn` / `spawn_concurrent`、`url()`、`calls()`、`methods()`、`concurrent_peak()` | `crates/simulation/tests/support/stub.rs:56/245/261/323/335/343/352` |
| 录制端点适配器（离线，零网络） | `RecordedChainAdapter`（含链 id 核验、把录制当不可信输入） | `crates/chain/src/recorded.rs:18/23/83/179` |
| 候选帧重放源 | `ReplayFrameSource` + `RecordedFrame` | `crates/live/src/preconf_provider.rs:118/125` |
| Radar 的 12 个 fixture + 隔离/负控制/矩阵测试 | 形状、fail-closed、确定性 | `crates/live/tests/preconf_fixtures.rs`、`preconf_negative_controls.rs`、`preconf_radar_matrix.rs`、`preconf_link_loop.rs`、`preconf_support/mod.rs` |
| 结构性隔离测试（7 道） | Flashblocks 不能触达 canonical | `crates/live/tests/preconf_isolation.rs`（见 §8.1） |
| "无硬编码端点/链 id"守卫 | 配置面唯一性 | `crates/cli/tests/no_execution.rs:300` |
| CLI 参数测试 | 缺 URL/错误 tag 拒绝 | `crates/cli/tests/live_args.rs`、`validate_args.rs`、`arbitrage_args.rs` |

### 13.2 §8.1–§8.3 测试矩阵（状态列 = 本阶段核查结果）

| 组 | 用例 | 现有覆盖 | M12-B 待补 |
| --- | --- | --- | --- |
| 配置 | 本地 canonical URL 正确注入 | ✅ 端点全程作为值传递（§5.1） | 补"注入后 adapter 实际连到 127.0.0.1:8545"的断言 |
| 配置 | 缺必需 URL 明确失败 | ✅ `cli/lib.rs:310-315`、`:325-329` | — |
| 配置 | 链 id 不符拒绝启动 | ✅ 4 处闸门（§11.2 第 6 条） | — |
| 配置 | 公共↔本地切换不产生意外回退 | ✅ 无第二端点可退（单端点 + 无默认值） | 补切换前后配置快照对照 |
| 配置 | HTTP/WS 不误用 | ✅ `canonical_source` 由显式规则推得（`cli/lib.rs:282-301`） | 补"给了 ws URL 但订阅失败 → 报告如实记 http-poll" |
| RPC 兼容 | 正常响应 | ✅ `Behaviour::Answer` | — |
| RPC 兼容 | `result: null` | ✅ `Answered`（空值合法）`rpc.rs:one_attempt` | — |
| RPC 兼容 | JSON-RPC error | ✅ `Rejected` → `ChainError::RpcRejected`，不重试 | — |
| RPC 兼容 | HTTP 5xx | ✅ `Flaky`（500 后重试成功） | — |
| RPC 兼容 | 请求超时 | ❌ 未见专项（20s 超时只在生产设定） | 用假端点延迟 > 超时补一条 |
| RPC 兼容 | 断连 | 部分（`CLASS_SEND_FAILED` 非终态） | 补 TCP 中途关闭 |
| RPC 兼容 | 非 JSON 响应 | 分类臂存在（`CLASS_NON_JSON`，`rpc.rs:320`） | `Behaviour::Garbage` 补一条 |
| RPC 兼容 | 缺字段/错类型 | ✅ `CLASS_DECODE_FAILED` + `Empty` | — |
| RPC 兼容 | `eth_syncing` 三种应答形态（`false` / 对象 / 错误） | ❌ 从未使用 ⇒ 无覆盖 | **G1 的前置**：先 mock 再实测 |
| RPC 兼容 | 不同 block tag 处理 | ✅ `normalize_block_param`（`rpc_trace.rs:1357` 测试）、`block_param()` 系列 | — |
| Flashblocks | 同高度重复视图 | ✅ Radar fixtures | — |
| Flashblocks | 同高度交易列表增长 | ✅ `flashblocks.rs:677` 候选增长测试 + 录制 4348 条 full_object | — |
| Flashblocks | pending 视图 reset | ✅ `flashblocks.rs:305/:316`、`preconf_negative_controls.rs` | — |
| Flashblocks | canonical reconciliation | ✅ `CandidateResolved{…}` 路径 + `preconf_radar_matrix.rs` | — |
| Flashblocks | canonical 与 pending 不一致 | ✅ `CanonicalityConflict` 变体（`live/src/event.rs:140+`） | — |
| Flashblocks | pending 不可用 | ✅ `pending_raw()→None` 分支（`head.rs:135`） | — |
| Flashblocks | provider 形状改变时 fail-closed | ✅ `HashOnlyTransaction`（`preconf_decode.rs:105-116`） | 补"本地端点答 hash-only"这条真实路径（§8.2） |
| Flashblocks | 新增测试若需额外 RPC：调用次数与理由 | — | 必须用 `Stub::calls()` 记次并说明现有路径为何覆盖不到（§8.3 末条） |

> **约束提醒（§8.2 末条）：mock 测试不得用来宣称官方节点支持某方法。** 本报告的"公共端点实测"列全部来自已提交的录制证据（§9.2 出处列），"本地自建节点"列一律写"未验证"。

---

## 14. 未验证事项与真实节点启动后的验收清单

### 14.1 必须等真实节点才能验证（本阶段全部记 NOT_VERIFIED）

| # | 待验证 | 验证方法（不许猜端口） | 通过后才能解锁 |
| --- | --- | --- | --- |
| V1 | 本地 8545 的链 id、头高、块/receipt/日志/状态读是否与模型兼容（13 个方法逐个） | 逐方法单发 + 记录应答；与 `data/evidence/m5/method-matrix.json` 同口径对照 | `LOCAL_CANONICAL_RPC` |
| V2 | `eth_syncing` 在本地节点的三种应答形态与语义 | 官方文档给的就是这条调用（§3.4）；先 mock 形状，再实测 | G1 的实现依据 |
| V3 | 本地 `eth_getBlockByNumber("pending", false)` 是否应答、答成什么形状（有无 `gasUsed`、交易列表是 hash 还是对象） | 单发；与 `flashblocks.rs:156-159` 的必需字段对照 | 候选源可否用本地端点 |
| V4 | 本地 `pending` **是否 Flashblocks-aware**（即开 `--flashblocks-url` 后 pending 是否含未出块交易、刷新节奏是否 ~200ms） | 对照实验：vanilla 模式 vs flashblocks 模式各采一段，比 pending 高度/交易数分布 | `LOCAL_FLASHBLOCKS` |
| V5 | 本地 `eth_subscribe(newHeads/newBlockHeaders)` 是否可用（公共端点实测 `-32603`） | 真连一次；失败则记录并保持 HTTP 轮询 | 是否配 `GIWA_WS_URL` |
| V6 | Flashblocks-aware 是否与 canonical 同址（同 8545 还是要另一入口） | 只允许由 V3/V4 的探测结果或明确配置决定 | 配置模型是否需要第四个字段 |
| V7 | 8545 与 8546 的 API 命名空间差异（`miner` 只在 http.api 里） | 读 `reth/entrypoint.sh` 已给事实；实测确认 | — |
| V8 | `--flashblocks-url` 连不上时节点行为（继续 vanilla？退出？日志？） | 起一个错 URL 的节点观察 | 是否有静默降级风险 |
| V9 | 节点重启后 pending 视图/块高的连续行为 | 重启一次并采集 | G3 规则成文 |
| V10 | op-reth 上游 Flashblocks 实现细节（`getPayloadV5`、Karst 之后的组件要求） | 本沙箱 GitHub 代码搜索 401、无 `gh` CLI ⇒ **无法验证**，需在有认证环境重做 | — |
| V11 | 公共 flashblocks WS 主机名（官方 `.env.sepolia` 示范）与仓库实测的 `sepolia-rpc-flashblocks` 是否同一服务 | §5.3 显示仓库从未记录过前者 | 节点侧 `FLASHBLOCKS_WEBSOCKET_URL` 该填什么 |

### 14.2 真实节点启动后的验收清单（M12-B 直接抄）

1. 节点 `eth_syncing` 返回 `false`，且 `eth_blockNumber` 与浏览器头高差在既定容差内。
2. `eth_chainId` 应答 91342（= registry 声明，闸门通过）。
3. V1 的 13 个方法逐个有应答记录，失败项写明错误分类（对应 §7.1 的 5 个 class）。
4. 机器人以 `GIWA_RPC_URL=http://127.0.0.1:8545` 跑一把 replay-parity（零网络路径）之外的最短 live 窗口，证据表端点摘要稳定、且不含任何 URL 明文。
5. `--ws-url` 仅在 V5 通过时启用；否则 session 记录如实写 `http-poll`。
6. V4 未通过之前，`GIWA_FLASHBLOCKS_URL` 保持不配（不得用公共端点顶替）。
7. 提交路径若仍依赖 `RETH_ROLLUP_SEQUENCERHTTP` 的公共值，必须在报告里显式写明"读本地/提交公共"，不得笼统说"全本地"。
8. 全程无新增 RPC 调用以外的调用（用 `Stub::calls()` 或 trace 记录核算）。
9. `cargo test --workspace -- --test-threads=1` 串行通过；生产 diff 审计为 0（§1.2 两条命令）。

---

## 15. M12-B / M12-C 后续任务拆分

### M12-B（最小单节点配置 + 恢复能力）

| 任务 | 范围 | 验收 |
| --- | --- | --- |
| B1 | `eth_syncing` 就绪闸门（G1/G2）：只在启动与重连时各一次，失败即"明确报错/暂停"，绝不静默回退 | §14.2 第 1 项；调用次数入账 |
| B2 | 端点用途标签落地（§10 第 7 项、G4）：证据表能区分本地/公共，不改重试、不改 adapter 逻辑 | 证据字段 + 一条"标签与 URL 来源一致"的测试 |
| B3 | 节点重启失效规则成文并接线（G3）：哪些状态作废、哪些保留 | 一次受控重启实验 + 测试 |
| B4 | Flashblocks 兼容测试补全（§13.2 的 ❌ 行）：超时、断连、非 JSON、syncing 三形 | 每条有"真错仍红"的负控制 |
| B5 | 环境变量命名统一（§16 第 2 项）：`GIWA_FLASHBLOCKS_URL`（生产）与 `GIWA_FLASHBLOCKS_RPC_URL`（测试）收敛为一个 | 改名后 grep 只有一个拼法 |
| B6 | 间隔类默认值单一来源（§16 第 3 项） | `cli` 与 `live` 不再各写一份 900/250 |
| **不做** | 任何 HA、多端点管理、负载均衡、备份节点（§2.1/§9） | — |

### M12-C（延迟基准与观测完善）

| 任务 | 范围 |
| --- | --- |
| C1 | 本地节点上的端到端延迟基线（复用 `metrics::LatencyTrace` + `pipeline::latency`），并按 M8.1 的教训：样本不足就写 null，不用观测窗换结论 |
| C2 | canonical / pending / 提交三段各自的 p50-p99 与"节点本地 vs 公共端点"对照（同配置维度才可比，身份用语义键） |
| C3 | pending 视图刷新节奏与 V4 的对照实验合并成一张表 |
| C4 | 观测不得新增 RPC 调用；若必须新增，记次数并说明现有路径为何覆盖不到 |

**真实节点部署与 Flashblocks 实测仍是"基础设施就绪后的验收项"，不属于 B/C 的实现内容。**

---

## 16. 缺陷登记（本阶段只登记，不修改）

| # | 缺陷 | 位置 | 性质 | 影响面 |
| --- | --- | --- | --- | --- |
| D1 | Flashblocks 端点链校验的报错把 `registry` 与 `node` 两个字段角色写反：`registry` 填了 Flashblocks 端点的应答、`node` 填了 canonical 端点的应答，而 `#[error]` 模板读作「registry attests chain {registry} but the endpoint at {endpoint} answered chain {node}」 | `crates/pipeline/src/runner.rs:1235-1240`（判定在 `:1235`，`ChainMismatch` 构造在 `:1236-1239`），模板在 `crates/pipeline/src/error.rs:61-68` | **仅诊断文案**；比较本身正确、仍然 fail-closed 中止运行（因为 canonical 应答已在 `runner.rs:1114-1126` 对 registry 核验过） | 排障时会把两个链 id 说反；对照正确写法：`arbitrage.rs:345`、`cli/lib.rs:559` |
| D2 | Flashblocks 端点环境变量名分裂：生产 `GIWA_FLASHBLOCKS_URL`（`cli/lib.rs:159`），测试 `GIWA_FLASHBLOCKS_RPC_URL`（`live/tests/preconf_live_giwa.rs:645`、doc `:10`、`execution/tests/sequencer_direct_probe.rs:394`） | 同上 | 命名不一致 | 复现脚本与生产配置不能直接互用；B5 |
| D3 | 间隔默认值写两处：`cli/lib.rs:367` 的 `unwrap_or(900)` 与 `live/src/source.rs:51` 的 `900`；`cli/lib.rs:371` 的 `unwrap_or(250)` 与 `live/src/flashblocks.rs:60` 的 `250` | 同上 | §5 第 1 条的同类问题（**端点类没有此问题，只有间隔类有**） | 改一处忘另一处 ⇒ 报告口径漂移；B6 |
| D4 | 执行 lane 把端点类型写死 `PublicHttpRpc`；`FlashblocksHttpRpc` 变体在 `src` 无生产构造点；官方 `.env.sepolia` 的 `RETH_ROLLUP_SEQUENCERHTTP` 默认仍指公共 sequencer | `execution/src/stage.rs:348`、`execution/src/sequence.rs:1489`（枚举 `submitter.rs:30-38`） | 标签与实际不符 + "自建读 / 公共提交"的隐含假设 | 换本地节点后证据表会误标；B2、§14.2 第 7 项 |
| D5 | Radar 解码器要求 pending 交易为完整对象，而共享的 `pending_raw()` 发 `false`；`EarlyRadar`/`PollingFrameSource` 当前未接入 pipeline（`crates/live` 外 0 引用） | `live/src/preconf_decode.rs:105-116` vs `chain/src/head.rs:133` | **潜伏**（今天不发作，接线那天必发作，且是 fail-closed 方向 ⇒ 不会造成错误状态，只会造成"完全没有帧"） | §8.2；B4 |
| D6 | `eth_syncing` 全仓 0 命中；未同步与查无数据同为 `MissingData`/`None` | `chain/src/rpc.rs:546`（`get_block`）→ `:551`（`MissingData` 返回）、`chain/src/head.rs:131`（`pending_raw` 实现）→ `:135`（`Ok(None)`） | 缺防线（非代码错误） | G1/G2；B1 |

---

## 17. 判定

| 判定项 | 结论 | 依据 |
| --- | --- | --- |
| `M12_AUDIT` | **COMPLETE** | §1–§16 覆盖任务书 §10 全部 15 项内容；§11 十二条验收逐条对应（HEAD SHA §1.1、官方版本与配置 §2/§3、Flashblocks 配置实际用途 §3.3/§4、端点来源与调用关系 §5/§6、canonical/pending/Flashblocks-aware 三者区分 §4/§7、最小实现范围 §10、测试矩阵 §13、必须等真节点事项 §14、未改生产代码 §1.2、未新增 RPC/签名/广播 §1.2+§9 对照、可复核 §18、工作树与测试状态 §1.1/§1.3） |
| `SELF_HOSTED_NODE` | **NOT_RUN** | 本阶段未部署、未启动任何节点（任务书 §1 明确不要求） |
| `LOCAL_CANONICAL_RPC` | **NOT_VERIFIED** | §14.1 V1–V2 |
| `LOCAL_FLASHBLOCKS` | **NOT_VERIFIED** | §4、§14.1 V3–V6 |
| `MULTI_NODE_HA` | **OUT_OF_SCOPE** | 按 §2.1 项目决策删除；本阶段同时确认保留了超时/重试/断连恢复/安全停机四类现有机制（§12.1） |

关卡实测状态（串行 `--test-threads=1`）：`cargo fmt --all -- --check` 通过（0 字节输出）；`cargo clippy --workspace --all-targets -- -D warnings` 通过（4m48s，无 warning）；`cargo test --workspace -- --test-threads=1` **通过**：123 个测试目标，1611 passed / 0 failed / 29 ignored，wall 3975.39 s。三道关卡都在 HEAD `a1a6952`、工作树只含本任务两份文档（加一个由 M10 证据门自动重写 `git_commit` 字段的 `data/evidence/m10/manifest.json`，未纳入本任务提交）之上执行。

---

## 18. 复核方式（本报告每条数字都能重跑）

本报告不引用任何临时日志；所有结论的来源要么是仓库内文件（路径+行号），要么是 `data/` 下已提交的证据文件（路径+sha256+大小），要么是官方公开 URL。重跑以下命令即可复现 §5/§9 的计数：

```sh
# 1. 端点与链 id 未硬编码（期望：无 offenders）
cargo test -p evm-cli --test no_execution -- --test-threads=1

# 2. 生产 JSON-RPC 请求点总数（期望：27）
python3 - <<'PY'
sites={'crates/chain/src/rpc.rs':[102,542,548,561,610,642,710,725,734,742,752,766],
 'crates/chain/src/head.rs':[133,139,159,179,197,213,220],
 'crates/chain/src/ws.rs':[191],
 'crates/execution/src/giwa/preflight_facts.rs':[381],
 'crates/execution/src/giwa/sequencer_direct.rs':[171,192,202,307,359,407]}
import re
print(sum(len(v) for v in sites.values()))
for f,ls in sites.items():
    src=open(f).read().split('\n')
    for l in ls: assert re.search(r'"(eth|net)_[A-Za-z]+"', src[l-1]), (f,l)
PY

# 3. 从未使用的方法（期望：两条都输出 0）
grep -rho eth_syncing   --include='*.rs' crates | wc -l
grep -rho eth_feeHistory --include='*.rs' crates | wc -l

# 4. 端点无默认值（期望：三个字段全是 Option，且第 4 行命令无输出）
grep -n 'pub rpc_url\|pub ws_url\|pub flashblocks_url' crates/pipeline/src/config.rs
grep -rn 'rpc_url\s*=\s*"' crates/*/src | wc -l

# 4b. data/ 录制端点计数（与 §5.3 对照）
python3 - <<'PY'
import glob, os
forms={'canonical_https':'https://sepolia-rpc.giwa.io','canonical_wss':'wss://sepolia-rpc.giwa.io',
'flashblocks_https':'https://sepolia-rpc-flashblocks.giwa.io','flashblocks_wss':'wss://sepolia-rpc-flashblocks.giwa.io',
'sequencer_https':'https://sepolia-sequencer.giwa.io','flashblocks_env_wss':'wss://sepolia-flashblocks.giwa.io'}
for k,v in forms.items():
    n=0; fs=0
    for p in glob.glob('data/**/*',recursive=True):
        if not os.path.isfile(p): continue
        c=open(p,errors='ignore').read().count(v)
        if c: n+=c; fs+=1
    print(f"{k}: occurrences={n} files={fs}")
PY

# 5. Radar 未接入 pipeline（期望：除 crates/live 外无命中）
grep -rn "PollingFrameSource\|EarlyRadar\|PreconfLink" crates/*/src | grep -v '^crates/live/'

# 6. 官方文件指纹（与 §2.1 表对照；raw.githubusercontent.com 偶发返回空体，
#    空体的 sha256 是 e3b0c442…b855，见到它就重试）
for p in .env.sepolia reth/entrypoint.sh docker-compose.yaml README.md; do
  printf "%s " "$p"; curl -fsSL "https://raw.githubusercontent.com/giwa-io/node/main/$p" | shasum -a 256
done
```

结构化清单见 `data/evidence/m12/audit_manifest.json`（只含已核实项；未核实项一律记 `NOT_VERIFIED` 并附验证方法）。这张表里每个数字都是落笔时用脚本从磁盘文件里现取的（每处取值都带 assert），没有一个是手打的；取数脚本放在被 gitignore 的 `target/` 下、不作为证据保留，所以任何一个数都能用本节上面的复现命令重新推出来。
