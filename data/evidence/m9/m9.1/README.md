# M9.1 — Pool Discovery（真实历史链上普查）

## 一句话结论

在 GIWA Testnet（chain id 91342）的真实历史区块上，本次普查从 **85 个候选**（工厂发出的 `PairCreated` 声明）中，验证通过 **80 个池**、否决 **5 个**，并把通过的 80 个全部送进既有管线：Registry → StateStore → Graph，在区块 37224031 上得到 **48 条报价边**。整个过程签名 0 次、广播 0 次、套利 0 次。

**Discovery ≠ Trust**：链上说某个地址被创建了，只是一条“声明”；是不是 V2 池、两侧是什么币、有没有可交易的储备，全部要由这个地址自己的读取重新回答。**Verified Pool ≠ 可交易机会**：验证只证明合约行为像池子并且公布过储备，不证明此刻存在可执行的价差，更不证明有人能成交。

## 数字与其出处（每一个都可从 raw 记录回算）

| 问题 | 数值 | 出处 |
| --- | --- | --- |
| 扫描了多少区块 | 50000 | `discovery-summary.json#/totals/blocks_scanned` |
| 链返回多少条 `PairCreated` | 85 | `…/pair_created_logs`（原始日志在 `raw/pass-a.json`） |
| 候选（声明）多少条 | 85 | `…/candidates`，逐条在 `candidate-pools.json` |
| 通过身份验证 | 85 | `discovery-summary.json#/verification/identity_stage_reached` |
| 通过代币验证 | 85 | `…/token_stage_reached` |
| 通过状态验证 | 80 | `…/state_stage_reached` |
| 最终成为已验证池 | 80 | `…/verified`，逐条在 `verified-pools.json` |
| 被否决 | 5 | `…/rejected`，逐条含原因、搜索区间与判定所依据的记录字段在 `rejected-pools.json` |
| 否决原因分布 | `no_authoritative_state` 5 条 | `…/rejected_by_reason` |
| 写入 Registry 的凭证 | 80 | `state-graph-integration.json#/attested` |
| 图上的报价边 / 被跳过的池 | 48 / 56 | `…/graph/edges`、`…/graph/skipped` |
| 本次发起的 RPC 调用 | 441（第二把 440） | `rpc-calls.json#/passes` |

## 被否决的都是什么

5 条否决全部停在 **state** 阶段，原因全部是 `no_authoritative_state`：四次合约读取都成功了，但这个池在自己的声明之后、搜索区间之内从未发布过 `Sync(uint112,uint112)`。没有人公布过它的储备，就没有可交易的状态——这是判定，不是数据缺失。`rejected-pools.json` 每条都带着搜索区间与看到的日志条数（“0 条”和“有若干条但读不懂”是两种结论，不能合并）。

## 关于“验证过”但“图上没有”的那部分

80 个池拿到了凭证，图上只有 48 条边（即 24 个池在报价），56 个被图跳过，原因全部是 `NotAtTargetBlock`：图只在快照自己那个区块上报价，而这些池最新的 `Sync` 停在更早的区块。这正是 §30 的 **Verified ≠ Fresh**，本次数据把它量化了——`state-graph-integration.json#/graph/skipped` 里每个被跳过的池都记录了它自己的状态区块和落后多少区块。这不是 bug，也不是可以“顺手复用”的东西：把不同区块的价格拼进一张图，得到的只是几个时刻互相比较。

## 手续费：全程未知

80 条凭证的 `fee` 全部是 `null`，图上 48 条边的 `fee` 也全部是 `null`。发现层从不把常见费率当默认值填进去（§14）：未知就写未知，M3 的费率数学自己处理 `None`。

## 怎么复核（不需要节点）

```text
cargo test -p evm-discovery --test evidence_gate -- --test-threads=1
cargo test -p evm-discovery -- --test-threads=1   # 离线单测 + §20 负控制，全部不碰网络
```

重算走两条独立的路：一条调用 `verify`/`integrate` 本身，另一条只按字段做算术（数日志、比地址、查区块号）。
两者都必须与已提交表格逐字节相等；把两把运行的先后顺序交换后重做，行表还要逐字节一样（§21）。
想重跑真实普查（只读，441 次调用，需要节点）：

```text
GIWA_RPC_URL=<节点地址> cargo test -p evm-discovery --test historical_live \
-- -- --ignored --nocapture --test-threads=1
M91_EVIDENCE_REFRESH=1 cargo test -p evm-discovery --test evidence_gate -- --test-threads=1
```

## 目录里每个文件是什么

| 文件 | 内容 |
| --- | --- |
| `discovery-summary.json` | 窗口、总量、每个验证阶段的通过数、否决原因分布、两次运行是否一致、RPC 核算、边界声明 |
| `candidate-pools.json` | 每条 `PairCreated` 声明一行：池地址、发出者、两侧代币、链上位置、落在哪个窗口、最终判定 |
| `verified-pools.json` | 每个通过验证的池一行：合约自己回答的两侧代币、`getReserves()` 与它读取的区块、池自己发布的 `Sync` |
| `rejected-pools.json` | 每条否决一行：阶段、原因、这条判定所依据的记录字段（搜索区间、看到的 `Sync` 条数、读取成败），以及由这些字段拼出的一句话说明 |
| `verification-evidence.json` | 每条凭证的 `identity/tokens/state` 三组证据，以及每次读取自己记录的区块号（§22） |
| `state-graph-integration.json` | Registry 写入的行、快照位置、图的边与被跳过的池、与手工 registry 的对照 |
| `negative-controls.json` | 24 行控制：23 个负控制（覆盖 §20 点名的九项条件与补充项）+ 1 个正控制，逐个在此刻现跑，不是我引用的结论 |
| `rpc-calls.json` | 每一把的真实调用清单（方法、区块参数），以及超出只读白名单的方法（应为空） |
| `manifest.json` | 文件清单与摘要、§19 每个问题对应哪张表 |
| `raw/` | 普查自己写下的原始记录：两把运行的完整日志与读取记录、节点调用 trace。**所有数字的唯一来源** |

## 本次没有做、也不会声称做的事

- 没有签名、没有广播、没有执行任何套利（§25），trace 里没有任何写入类方法。
- 没有加缓存、没有跨阶段复用状态、没有削减 RPC（§23；M8.6 已判定 `NO_SAFE_RPC_REDUCTION_FOUND`）。
- 没有实现 Flashblocks 源（§24 只要求架构上留位：`DiscoverySource` 是可扩的枚举，下游不依赖具体源）。
- 没有把 `PairCreated` 当作信任；没有把 `getReserves()` 当作市场状态；没有替未知费率填默认值。
- 本次窗口列表取自 M7 全链普查的创建密集区间，是**样本**而不是全链清单：链上共 1,030 条 `PairCreated`，此处只覆盖 50000 个区块。

## 边界（§30，逐条仍在生效）

- Discovery != Trust
- Observed != Verified
- Verified != Fresh
- Verified Pool != Tradable Opportunity
- Graph != REVM State
- Flashblocks != Canonical State
- Duplicate != Reusable
- Reusable != Safe

端点在本目录里只以指纹形式出现（`rpc-faa716cada04a9ef`），推导规则与 `RpcTraceSink::endpoint_id` 一致；原始 URL 在运行时由 `GIWA_RPC_URL` 环境变量提供，目录内不含任何私钥。
