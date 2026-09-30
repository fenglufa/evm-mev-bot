# M2 Completion Report

结论（白话版）：M2 已经跑通。把 M1 从链上日志里恢复出来的「池子储备量」投影成一张市场图之后，
得到的是：**代币是点，池子是边，一个池子给出两条方向相反的边**，整张图绑死在一个明确的区块上
（GIWA Sepolia，chain 91342，block 37258093）。这张图里没有任何一个数字是自己造的：

- 没查到状态的池子不会被当成「储备为 0」，也不会临时去问一次合约 —— 它被跳过，并且**把跳过的
  原因和它真实的状态位置写进报告**；
- 最新状态不在这个区块上的池子也不会被混进来（同一个区块才能构成同一张图）；
- 手续费没有证据，所以图里 `fee = None`，绝不是默认的 0.3%。

这一轮还把「真实池子到底有几个」查清楚了：**能举证的池子有 4 个**（M1 只有 1 个），它们分属两个
不同的 Factory，每一个都有身份 / 代币 / 状态三组链上证据。在 block 37258093 上，其中 2 个池子
刚好都有这个区块的状态，于是真实图 = **3 个点、4 条边**；另外 2 个池子的最新状态分别落在
1,054 万块和 545 万块上，距 target 2,671 万 / 3,180 万块，被明确跳过（它们在自己的区块上另有图，
见第 6 节）。同一份输入跑两遍，图与序列化字节完全相同；6 个真实区块重新向
RPC 抓一次，SHA-256 逐字节不变。状态：**COMPLETE**。

任务书要求的事实值（十进制，来自链上原始数据，非估算；四个代币 `decimals()` 实测都是 18）：

| 项 | 值 | 换算 |
| --- | --- | --- |
| chain_id | 91342 | — |
| block（图的绑定区块） | 37258093 | `0x238836d` |
| 快照位置（target） | block 37258093 / log 50 | StateSnapshot 的 applied position |
| 节点数 | 3 | HYANGNO、GIWAP、WETH |
| 边数 | 4 | 2 个池子 × 2 个方向 |
| 跳过的池子 | 2 | `NotAtTargetBlock`，位置见第 6 节 |
| pool 1 | 0x3978e57bbceb7666d54a03551c03691f897f6092 | GIWAP / WETH |
| ├ tx / log | 0x7fc2077e…e558（tx_index 17）/ 30 | — |
| └ reserve0 / reserve1 | 2849566765086289340844 / 141242920674394454220 | 2,849,566.765 GIWAP / 141.243 WETH |
| pool 2 | 0xcaafb95fc292c10a526f03fa480407bb438dac67 | HYANGNO / WETH |
| ├ tx / log | 0xbdea3d78…75b0（tx_index 26）/ 50 | — |
| └ reserve0 / reserve1 | 125338144751901190262 / 175861646008610376 | 125.338 HYANGNO / 0.1759 WETH |

---

## 1. Implementation

新增：

- `crates/graph`（新 crate，依赖只有 `evm-core`、`evm-state`、`alloy-primitives`、`serde`、`thiserror`
  —— **不依赖 `evm-chain`**，所以图这一层在类型上就没法碰 RPC；`evm-chain` / `evm-protocol` /
  `evm-replay` 只出现在 `[dev-dependencies]`，供真实验收测试读落盘区块）
  - `src/edge.rs`（223 行）：`EdgeId { pool, token_in, token_out }`、`GraphEdge`
    （`reserve_in` / `reserve_out` / `fee: Option<Fee>` / `state_position`）、`EdgeRejection`、
    `GraphEdge::pair()` —— 两个方向必须由同一次调用产出，因为「把 reserve0/reserve1 换成
    token_in/token_out」是这个类型唯一的工作，反了一次就静默地把所有价格颠倒。
    `Ord` 手写：先按 `EdgeId`，再按剩余字段；`Fee` 故意没有 `Ord`（比分子不等于比手续费），
    所以它以 `(numerator, denominator)` 原始对进入排序键。
  - `src/snapshot.rs`（128 行）：`GraphSnapshot { chain_id, block_number, nodes, edges, routes, peers }`，
    四个集合全部私有、访问器全部只读 ⇒ 建好之后不可能被偷偷改（§23）；`routes` / `peers` 是
    从 `edges` 派生的索引，`#[serde(skip)]`（边集合就是图，索引不重复序列化）。
    查询能力：`neighbors(token)`、`routes(token_in, token_out)`、`pool_edges(pool)`、`edge(EdgeId)`、
    `nodes()` / `edges()` / `node_count()` / `edge_count()` / `pool_count()`。
  - `src/builder.rs`（117 行）：`MarketGraphBuilder::{build, build_traced}`、`GraphBuild`、
    `SkipReason { StateUnavailable, NotAtTargetBlock, Rejected(EdgeRejection) }`、`SkippedPool`
    （带 `state_position`，让理由可以被审计）、`GraphError::NoBlockIdentity`。
  - `tests/graph_fixtures.rs`（485 行，13 个用例）：任务书 §28 的 Fixture A–F + §25 的两条 + 确定性/序列化。
  - `tests/real_graph.rs`（590 行，9 个用例）：真实数据验收。
- `data/protocols/v2-narusswap-sepolia.json`：第二个协议部署（Narusswap）的真实池子证据文件。
- `fixtures/real/`：新增 4 个真实区块（`5455035`、`5457650`、`10544346`、`31390683`），
  与 M1 的 2 个合计 6 个，全部由 `HttpChainAdapter::get_block_data` 归一化后落盘，无手工转录。

修改：

- `crates/protocol/src/registry.rs`：
  - `load_dir()` —— 一个目录里的多份证据文件合并成一个 Registry（按文件名排序读取，顺序无关结果）；
  - 同一 `PoolId` 被两份文件写成不同内容 ⇒ `RegistryError::Conflict`，写成完全相同的内容 ⇒ 允许（重复不是冲突）；
  - `validate()` 增加「`token0 == token1` 的池子直接拒绝加载」；
  - 提交版 registry 的自检从「≥ 2 个池子」收紧成「恰好 4 个，且每个池子三组证据都齐、
    每条证据都带区块号、四条 (token0, token1) 互不相同」。
- `crates/replay/tests/record_fixtures.rs`：`capture_real_block` 从 2 个区块扩到 6 个，注释里逐块
  写清楚每个区块为什么需要（哪个池子在哪一块创建、哪一块最后一次 Sync）。
- `crates/replay/tests/m1_replay.rs`：M1 那条「本文件只举证 1 个池子」的断言按事实改成 3 个，
  并加上「三个 attest 是三个不同的 pair」。M1 的行为断言（一个区块 → 一个池子的真实储备）没有任何改动。
- 根 `Cargo.toml` / `Cargo.lock`：workspace 增加 `crates/graph`（成员从 5 个变 6 个）。

chain-neutral（§36）复核，脚本按 `#[cfg(test)]` 边界只统计非测试源码：

| 检查项 | 全 workspace 非测试源码命中 |
| --- | --- |
| `91342` / `giwa` / `naruswap` / 40 位地址 | 0 |
| `unwrap()` / `expect()` / `panic!()` | 0 / 0 / 0 |
| `f64` / `f32` | 0 |
| `eth_call` 字样 | 全 workspace 非测试源码共 **6 处 / 4 个文件**：`chain/src/rpc.rs` 2（真实适配器发起请求的语句本身）、`chain/src/recorded.rs` 1（写明录制适配器拒绝服务 `eth_call`）、`core/src/evidence.rs` 2（`EthCall` 枚举变体名 + 它的文档注释）、`graph/src/builder.rs` 1（注释里解释为什么不许临时问合约）。`graph/src/lib.rs`、`snapshot.rs`、`edge.rs` 与 protocol/state/replay 三个 crate 命中 0 |

也就是说 `crates/graph` 里唯一出现 `eth_call` 的地方是一句「不许这么做」的说明；真实链上事实只存在于
`data/protocols/*.json`（数据不是代码）和测试常量里。按 §33，没有为图加 trait、注册表或多态分发。

---

## 2. Pool Discovery

M2 实际使用的方式是**人工调查 + 证据文件**，不是流水线里的自动发现。调查顺序就是任务书 §26 要求的那条：

```text
已有协议证据  →  池子关系  →  创建证据  →  状态证据  →  确认过的 Pool Registry
```

**第一步：重扫本地已有语料（`/Volumes/superfs/giwa-mev`，只读，不迁移其架构、不做运行时依赖）**

`data/evidence/v0.4.3.1/semantic-events.json`，95,670 条收据日志（M1 普查过的同一份）：

| topic0 形状 | 命中 | 发射者 |
| --- | --- | --- |
| `Sync(uint112,uint112)` `0x1c411e9a96…` | 2,345 | 3 个地址：`0xad153c84…` 1,629、`0x3978e57b…` 698、`0xcaafb95f…` 18 |
| `PairCreated(address,address,address,uint256)` `0x0d3648bd…` | 0 | — |

Sync 的 2,345 / 3 个发射者与 M1 记录的数字逐一相同 ⇒ **同一段扫描代码是校准过的**，所以
「PairCreated = 0」是这份语料的真实属性（它抓的是 3,725 万高度附近的一段实时交易，而池子的创建区块在
545 万–3,139 万），不是一个可以据此下结论的「链上不存在」。这一点是我在第 10 节里撤回的那条错误结论的根因。

**第二步：从 Sync 发射者读出 Factory（归档 RPC，§26 明确允许）**

- `eth_call factory()`（selector `0xc45a0155`，keccak 现算）：`0x3978e57b…` → `0x4c91edd1…`，
  `0xcaafb95f…` → `0xd51d7c2a…`；两个 factory 都真实存在并可读。
- `allPairsLength()`（`0x574f2ba3`）在 block 37258093 返回 **3** 和 **42**；
  `allPairs(uint256)`（`0x1e3dd18b`）逐个读，45 个地址全部可读；对这 45 个地址调 `getReserves()`，
  **45 个全部两边储备非零**（说明这些配对确有流动性，不是空壳）。
- `getPair(address,address)`（`0xe6a43905`）交叉核对同一配对。

**第三步：创建证据只能靠 `PairCreated` 日志**（本次实测复现，`eth_getLogs` 单块查询）：

```text
factory 0x4c91edd1… @ block 5455035  log 3  tx 0x5a3b723f92fd94c8496e8ed99458373ab0db5777a0d901c68ebb9cfa71621c9e
   PairCreated(0x304912af…, 0x4200…0006, pair 0x3978e57b…, index 1)
factory 0xd51d7c2a… @ block 31390683 log 7  tx 0x55f28ca8295e4526361edfc735b3eedb69052051591c52d98fac06b278ef3578
   PairCreated(0x0274c573…, 0x4200…0006, pair 0xcaafb95f…, index 39)
```

`index 1` 与 `allPairs(0)`、`index 39` 与 `allPairs(38)` 对得上（PairCreated 的 256 位序号是 1 基，
`allPairs` 下标是 0 基）——这是两条独立来源的互相校验。

**第四步：状态证据必须是链上日志，不是 `eth_call`**（§11）。每个进入 registry 的池子都要有
一条由池子自身发出的 `Sync(uint112,uint112)`（含 tx hash + 块内全局 log_index），`getReserves()`
在同一区块的返回值与 header timestamp 只做**交叉核对**。

明确没有做的事：

- 没有把 Factory 接进 chain→protocol→state→replay 流水线（§10）。`PoolCreatedEvent` 类型从 M1 就在
  `crates/protocol/src/event.rs` 里，但 M2 没有任何一条路径因为它发了 `PairCreated` 就往状态库里加池子。
- 没有「因为某个地址发了 Sync 就认定它是池子」（§11）。反例现量复核过：
  `0xad153c844ccac3d2ea991170624200e54730be74` —— `getReserves()` 返回两个非零 word，语料里发过
  1,629 条 Sync 形状日志，但 `factory()` `execution reverted`，且**不在两个 factory 的 45 个
  `allPairs` 里的任何一个**，M1 时期还实测过对它调 `token0()` 直接 revert。拿不到代币身份，
  按 §12 它就不进 registry、不进图（真实验收测试里有一条断言专门守住这一点）。
- 没有依赖 M1 的 Known Pool Registry 之外的任何「猜地址」路径：新增的 3 个池子全部由上面四步取证。

---

## 3. Confirmed Pools

**4 个**，全部在 chain 91342，全部 `protocol = v2-compatible`、`pool_type = ConstantProduct`、`fee = None`：

| # | Pool | token0 / token1 | 创建（PairCreated） | 状态证据（Sync） | Factory |
| --- | --- | --- | --- | --- | --- |
| 1 | `0x3978e57bbceb7666d54a03551c03691f897f6092` | GIWAP `0x304912af…` / WETH `0x4200…0006` | block 5455035 log 3 | 37257255 log 181；**37258093 log 30**（图用的这条） | `0x4c91edd1…`（GIWA） |
| 2 | `0x8df9062fc2995b06de754c040f1f7eac411abbdb` | GIWAP `0x304912af…` / USDT `0xfa0d1d17…` | block 5457650 log 8 | block 5457650 log 13（该池唯一一次） | `0x4c91edd1…`（GIWA，`allPairs(1)`） |
| 3 | `0x4db758ab5e494d5b81627dd39258240aa9db9d46` | WETH `0x4200…0006` / USDT `0xfa0d1d17…` | block 10544346 log 0 | block 10544346 log 6（该池唯一一次） | `0x4c91edd1…`（GIWA，`allPairs(2)`） |
| 4 | `0xcaafb95fc292c10a526f03fa480407bb438dac67` | HYANGNO `0x0274c573…` / WETH `0x4200…0006` | block 31390683 log 7 | 31390683 log 13；**37258093 log 50**（图用的这条） | `0xd51d7c2a…`（Narusswap，`allPairs(38)`） |

- Factory 侧覆盖度：两个 factory 一共 45 个已知配对，**4 个进入了 registry**；其余 41 个 Naruswap
  配对储备非零但在回放语料里没有任何权威 `Sync`，按 §11/§12 留在 registry 外。
- GIWA factory 自己是 3/3 全覆盖（`allPairsLength() = 3`，三个全部取证）。
- 4 个池子对应 4 个不同的代币对；实测这两个 factory 的 45 个配对之间**没有重复配对**
  （唯一的重合就是池子 4 自己），见第 10 节第 4 条。
- 涉及的真实代币共 4 个：GIWAP `0x304912af…`（`symbol() = "GIWAP"`, 18）、
  WETH `0x4200000000000000000000000000000000000006`（`"WETH"` / `"Wrapped Ether"`, 18）、
  USDT `0xfa0d1d1703b55929e262d9301a001d30e45b97a2`（`"USDT"` / `"USDT Token"`, 18）、
  HYANGNO `0x0274c57358c3a6b8a08b7297ba51665c303a471f`（`"향로"` / `"HYANGNO"`, 18）。

---

## 4. Token Graph

真实图（block 37258093，完整 registry）：**3 个节点**，`TokenId` 排序输出：

```text
TokenId(91342, 0x0274c57358c3a6b8a08b7297ba51665c303a471f)   HYANGNO
TokenId(91342, 0x304912af0ce0dd6479735634d567715107bdc0c6)   GIWAP
TokenId(91342, 0x4200000000000000000000000000000000000006)   WETH
```

节点集合不是配置出来的，是边推出来的：只有当一个**有本区块状态**的池子存在时，它的两个代币才成为节点。

- WETH 是两个池子共同的对手方，因此是唯一有两条出边的节点：
  `neighbors(WETH) = [HYANGNO, GIWAP]`（确定性顺序，用例断言逐字比过）。
- `neighbors(GIWAP) = [WETH]`、`neighbors(HYANGNO) = [WETH]`。
- USDT `0xfa0d1d17…` **不是节点**，尽管它在 registry 里被两个池子举证过 —— 那两个池子的最新状态
  不在 37258093（§25）。这条也是测试断言：`nodes()` 里没有它、`neighbors()` 为空、没有任何一条边的
  任一端是它、它所在池子的储备值（1e21 / 2e21 / 1e9 / 2.8e12）在图里一次都不出现（反向对照）。
- `0xad153c84…`（Sync 形状的模仿者）也不在任何位置：既不是节点也不是边。
- 跨链维度：`TokenId` 身份本身就是 `ChainId + Address`，同一个地址在两条链上是两个节点
  （Fixture D 的用例 `the_same_address_on_two_chains_is_two_graphs` 负责这条）。

---

## 5. Edge Graph

**4 条边**（2 个池子 × 2 个方向），每条边带 `EdgeId = PoolId + token_in + token_out`：

| EdgeId | reserve_in | reserve_out | fee | state_position |
| --- | --- | --- | --- | --- |
| `0x3978e57b…` GIWAP → WETH | 2849566765086289340844 | 141242920674394454220 | None | (37258093, 30) |
| `0x3978e57b…` WETH → GIWAP | 141242920674394454220 | 2849566765086289340844 | None | (37258093, 30) |
| `0xcaafb95f…` HYANGNO → WETH | 125338144751901190262 | 175861646008610376 | None | (37258093, 50) |
| `0xcaafb95f…` WETH → HYANGNO | 175861646008610376 | 125338144751901190262 | None | (37258093, 50) |

- §18（方向不能颠倒）：两个方向由同一次 `GraphEdge::pair()` 产出，反向前后 `reserve_in/reserve_out`
  互换；单元测试 `the_two_directions_share_one_pool_and_never_cross_reserves` 与真实用例
  `every_edge_matches_the_log_it_came_from` 都直接比这四个数。
- §22（边身份不止于代币对）：`EdgeId` 含 `PoolId`，所以同 pair 多池天然是多条边；
  Fixture B（`two_pools_on_one_pair_are_four_edges_and_never_overwrite_each_other`）验证
  2 个池子给出 4 条边、`routes(A,B).len() == 2`，且第二个池子不会覆盖第一个。内部索引是
  `BTreeMap<(TokenId, TokenId), Vec<GraphEdge>>`，任务书禁止的 `HashMap<(A,B), SinglePool>` 形状不存在。
- §19（fee）：字段只是把 `PoolMeta.fee` 搬过来，图里没有任何一处读它做计算；`None` 不等于 0。
- §16（只拒绝对市场不成立的形状）：`EdgeRejection` 只有 `SameToken` 与 `EmptySide` 两种；
  「极小但非零」的储备明确仍然是合法市场（单元测试 `a_tiny_but_nonzero_reserve_is_still_a_market`）。
- 序列化的边顺序是规范顺序（用例把 JSON 数组排序后与原数组比较，逐字相等）。

---

## 6. Real Block

真实图绑定的区块（从落盘 fixture 读出，与 RPC 一致）：

```text
chain_id    91342   (https://sepolia-rpc.giwa.io)
number      37258093  (0x238836d)
hash        0x8f54305c9596f8ad3c733ae096d6521e28b1b2909ecabef629940fec4c67f490
parent_hash 0xa3c63459c1faed2162fcad0110ee50683fe7bd899ee8fb0761b84144a0d07f1f
timestamp   1790603209
48 transactions / 78 logs / 27 个不同 topic0
```

这一块的 78 条日志里，`Sync` 形状的只有 2 条，恰好来自两个已举证的池子（log 30、log 50）；
`Swap` 形状 2 条，同样来自这两个地址。

target block 不是配置项，是状态快照自己的 applied position `(37258093, log 50)` —— 即整段回放
应用到最后的那条更新的位置。6 个真实区块的回放在这一条上给出：

```text
4 个池子的记录（都在 registry 里被举证且都发过 Sync）
  ├─ 0x3978e57b…  state @ (37258093, 30)   → 进图
  ├─ 0xcaafb95f…  state @ (37258093, 50)   → 进图
  ├─ 0x4db758ab…  state @ (10544346, 6)    → 跳过，NotAtTargetBlock（比 target 早 26,713,747 块）
  └─ 0x8df9062f…  state @ (5457650, 13)    → 跳过，NotAtTargetBlock（比 target 早 31,800,443 块）
```

两条跳过都带**它自己的**状态位置（不是猜测），所以「图为什么比 registry 小」可以事后审计。
「被跳过」也不是「它不是市场」：把回放区间换成它自己那一个区块单独跑，它就正常成图 ——
`0x8df9062f…` 在 block 5457650 给出 1 池 / 2 边 / 节点 `[GIWAP, USDT]`、跳过清单为空、
`state_position == (5457650, 13)`；`0x4db758ab…` 在 block 10544346 给出 `neighbors(WETH) == [USDT]`。
这条不是推断，是用例 `a_pool_stale_at_the_target_block_is_a_market_at_its_own_block` 的实测。
真实用例：`the_graph_refuses_to_mix_in_a_pool_from_an_earlier_block`、
`nothing_attested_is_left_out_without_a_reason`（这条断言 `图内池子 ∪ 被跳过池子 == 状态库池子`，
按 PoolId 全序逐一比对）。

作为对照，把 target 换成更早的区块会得到另一张自洽的图（用例 `the_graph_prices_the_latest_sync_not_the_first`）：
block 37257255 ⇒ 1 个池子 / 2 条边 / 2 个节点，位置是那一块里的**第三次** Sync（log 181），
不是第一次（log 150）；而 block 5457650 单独回放 ⇒ 只有池子 2 进图。三张图互不混用。

回放统计（6 块，完整 registry）：`blocks = 6`，`rejected_syncs = 0`。

---

## 7. Real Pool Evidence

证据文件就是 `data/protocols/*.json`，`crates/protocol/src/registry.rs` 在加载时强制校验：
三组证据（identity / tokens / state）任一为空即 `RegistryError::Unevidenced`，任一条缺 `block_number`
即拒绝。四个池子的证据条数与关键条目：

**1. `0x3978e57b…`（GIWA，GIWAP—WETH）— identity 8 / tokens 6 / state 4**

- identity：`getReserves()` → 3 word（37257255）；`name() → "Giwap LPs"`；`symbol() → "GIWAPDEX-LP"`；
  `factory() → 0x4c91edd1…`；`ChainLog` **block 5455035 / tx 0x5a3b723f… / log 3 的 `PairCreated`**；
  池子自身 LP `Transfer`（log 180）与 `Mint`（log 182）；
  `feeTo()`（`0x017e7e58`）与 `feeToSetter()`（`0x094b7415`）都 `execution reverted` ⇒ 手续费层级无法举证，`fee` 留 `null`。
- tokens：`token0() / token1()` 各自返回地址；`symbol()/decimals()`（GIWAP 18、WETH 18）；
  同笔交易里两条真实 `Transfer`（log 177 / 179）与两个地址对上。
- state：`ChainLog` 37257255 log 181（reserve0=2849387467534192263206 / reserve1=141265148507902942209）+
  **37258093 log 30（图用的这一条）**；`getReserves()` 在同块的返回值与最后一次 `Sync` 相同，
  且 `blockTimestampLast = 1790603209` == 该块 header timestamp（只做交叉核对，不是状态来源）。

**2. `0x8df9062f…`（GIWA，GIWAP—USDT）— identity 5 / tokens 6 / state 3**

- identity：`ChainLog` **5457650 / tx 0x16bb7239… / log 8 的 `PairCreated`**；`getReserves()` 3 word；
  `name()/symbol()`；`factory() → 0x4c91edd1…`，并写明该池在 `allPairs(1)`（`allPairsLength() = 3`）；
  `feeTo()/feeToSetter()` 均 revert ⇒ `fee = null`。
- tokens：`token0()/token1()`；GIWAP(18)、`0xfa0d1d17…` 的 `symbol()="USDT"`、`name()="USDT Token"`、18；
  首块同笔交易的 log 11 / log 12 两条 `Transfer`。
- state：`ChainLog` 5457650 **log 13** reserve0=1000000000000000000000（1,000 GIWAP）
  reserve1=2000000000000000000000（2,000 USDT）；`getReserves()` 同块一致且
  `blockTimestampLast 1758802766` == header timestamp；同笔交易的 `Mint`（log 14）注明是池子自身的流动性事件，
  不是合成储备。

**3. `0x4db758ab…`（GIWA，WETH—USDT）— identity 5 / tokens 6 / state 3**

- identity：`ChainLog` **10544346 / tx 0x47b9abdb… / log 0 的 `PairCreated`**；`getReserves()`；
  `name()/symbol()`；`factory() → 0x4c91edd1…`，该池在 `allPairs(2)`；`feeTo()/feeToSetter()` revert。
- tokens：`token0() → WETH`（`"WETH"` / `"Wrapped Ether"` / 18）、`token1() → 0xfa0d1d17…`（USDT / 18）；
  首块同笔交易 log 4 / log 5 两条 `Transfer`。
- state：`ChainLog` 10544346 **log 6** reserve0=1000000000（0.000000001 WETH）
  reserve1=2800000000000（0.0000028 USDT）；`getReserves()` 同块一致 + `blockTimestampLast 1763889462`
  == header timestamp；同笔交易 `Mint`（log 7）。

**4. `0xcaafb95f…`（Narusswap，HYANGNO—WETH）— identity 7 / tokens 6 / state 2**

- identity：`getReserves()` 3 word；`name() → "Narusswap V2"`；`symbol() → "NARU-V2"`；
  `factory() → 0xd51d7c2a…`；`ChainLog` **31390683 / tx 0x55f28ca8… / log 7 的 `PairCreated`**
  （序号 39，与 `allPairs(38)` 对应）；本块池子自身发出的 `Swap` 形状日志（log 51，3 topic / 128 字节）；
  `feeTo()` revert ⇒ `fee = null`。
- tokens：`token0() → 0x0274c573…`（`"HYANGNO"`，链上 `name()` 是 `"향로"`，18）、
  `token1() → WETH`（18）；同笔交易 log 48 / log 49 两条 `Transfer`。
- state：`ChainLog` 37258093 **log 50** reserve0=125338144751901190262（125.338 HYANGNO）
  reserve1=175861646008610376（0.17586 WETH）；`getReserves()` 同块一致且
  `blockTimestampLast 1790603209` == header timestamp。

三个数字对上的「三重一致」（`Sync` 的两个储备 == `getReserves()` 的两个储备 == `blockTimestampLast`
等于 header 时间戳）对四个池子逐个成立，这条不是推断：`data/protocols/*.json` 里每个池子的 `state`
组都同时引用日志与 EthCall，M1 的实时对照用例（`#[ignore]`）证明同一区块走 RPC 与走落盘文件结果全等。

---

## 8. Determinism

- 图相等 + 字节相等：真实用例 `the_real_graph_is_deterministic` —— 用同一套 registry 与同一批 fixture
  完整跑两遍，`GraphSnapshot` `PartialEq` 相等，`serde_json::to_string` 输出**逐字符相同**，
  序列化出的 `nodes` / `edges` 数组长度、`chain_id`、`block_number` 都断言过，边数组与它的排序结果相等
  （规范顺序，不是巧合）。
- 纯函数性：`MarketGraphBuilder` 是 `Copy` 且无内部状态，同一个 `StateSnapshot` 连建两次得到同一个图
  （fixture 用例 `the_same_state_snapshot_always_builds_the_same_graph`，并故意把两个池子的注册顺序颠倒
  以证明顺序不被继承）。
- 集合底座：`BTreeSet<GraphEdge>` / `BTreeMap` 索引 ⇒ 不依赖哈希序；`skipped` 在返回前按 `PoolId`
  显式排序，所以跳过清单也是确定的。
- 输入可复现：6 个真实区块的 SHA-256（本报告写之前重新向 RPC 抓了一轮，逐一比对**完全不变**）：

  ```text
  3834875c33b1fe28253d2232043492cfc58df3365314422281a83c303175e3c9  block-5455035.json
  6bf9a1da392b2be33b4c91b228080d5608f1b71c603d5e258245cb1a8157eafa  block-5457650.json
  511ea6625e587260f9d63771b79a60fa1dd3e52ee4e71178b812764dd9e603ca  block-10544346.json
  98be8d52b673b6bd5bee149c1d5ffc034211813c255987c0a9492a933c945005  block-31390683.json
  602aec52a41dccceb9b086ec90cacf93a59c7e9ab990ad8f500c8f843d9efd3f  block-37257255.json
  267c3ce081c8c93137de0ffb949975cd3686e7796204eabaedda415f0fb07d32  block-37258093.json
  ```

- 没有浮点：全仓库非测试源码 `f64`/`f32` 命中 0，储备与金额的运算都在 `U256` 上。

---

## 9. Tests

| 关卡 | 结果 |
| --- | --- |
| `cargo +1.96.1 fmt --all -- --check` | PASS（退出码 0） |
| `cargo +1.96.1 check --offline --workspace --all-targets` | PASS，无 warning |
| `cargo +1.96.1 test --offline --workspace` | **82 passed / 0 failed / 3 ignored** |
| `cargo +1.96.1 clippy --offline --all-targets --all-features -- -D warnings` | PASS |

分 crate：

```text
evm-core       6        evm-chain     9        evm-protocol  18      evm-state   9
evm-graph      4（单元）+ 13（fixtures）+ 9（真实验收）= 26
evm-replay    14 passed + 1 ignored（m1_replay.rs，实时对照）
              2 ignored（record_fixtures.rs：合成 fixture 生成器、真实区块抓取器）
```

`cargo test` 默认不跑那 3 个 `#[ignore]`，本轮它们各自被实跑过一次并通过，结果记在第 6、8 节：
真实区块抓取器重新向 RPC 抓 6 个区块（第 8 节的 SHA-256 就是它的产物）、合成 fixture 生成器重跑后
8 个文件字节不变、实时对照与离线回放逐字段相等。

Fixture A–F（§28）到用例的对应：

| 要求的形状 | 用例 | 断言的要点 |
| --- | --- | --- |
| A 一个池子 | `one_pool_is_two_nodes_and_two_edges` | 2 节点 / 2 边，两方向共用 `PoolId` 与 `state_position`，储备互换正确 |
| B 同 pair 两池 | `two_pools_on_one_pair_are_four_edges_and_never_overwrite_each_other` | 4 边 / `routes()` 返回 2 条，各自带自己的储备 |
| C 三代币链 | `a_three_token_chain_reports_neighbors_in_both_directions` | 3 节点 / 4 边，中间节点双向可达且顺序确定 |
| D 同地址不同链 | `the_same_address_on_two_chains_is_two_graphs` | 两个 chain_id 各自成图，跨链查询为空 |
| E 缺状态 | `a_pool_without_state_is_skipped_not_priced_at_zero` + `a_registered_but_never_synced_registry_builds_no_graph` | 跳过并报告 `StateUnavailable`；绝不出现 `reserve = 0`，也不会临时 `eth_call` |
| F 无效状态 | `a_degenerate_pair_is_rejected_and_reported` + `empty_reserves_never_reach_the_graph` + `a_tiny_but_nonzero_reserve_is_still_a_market` | 只拒「空边 / 自配对」；极小非零仍是有效市场；空储备被状态层直接拒 |
| §25 同块 | `a_pool_stale_at_the_target_block_is_skipped_not_mixed_in` + `the_stale_pool_has_its_own_graph_at_its_own_block` | 混合被拒 + 跳过清单点名它自己的区块；被跳过的池子在自己的区块上另有图 |
| 身份/序列化 | `edge_order_is_canonical_so_routes_and_neighbors_are_reproducible`、`the_same_state_snapshot_always_builds_the_same_graph`、`a_snapshot_with_no_position_has_no_graph` | 规范序、字节相同、无区块身份就不建图 |

真实数据验收（§29/§30）9 条：见第 4、5、6 节引用的用例名，其中包含两条**反向对照**——
撤销某个池子的举证后它的两条边必须一起消失（`withdrawing_one_attestation_removes_its_edges`），
以及被跳过池子的储备值不得出现在图里（`the_graph_refuses_to_mix_in_a_pool_from_an_earlier_block`）。
没有这两条，命中只能说明「集合很热」，不能说明字段是对的。

`data/protocols` 侧另有两条收紧的自检：`the_committed_registry_files_all_validate`（恰好 4 个池子、
每个三组证据齐、每条带区块号、四个配对互不相同、链号一致）和
`the_registry_behind_the_graph_has_no_duplicates_or_merges`（4 份举证 → 状态库里恰好 4 条池子记录，
每条 `PoolMeta` 与举证逐字段相等，`PoolId` 无重复）。

---

## 10. Known Limitations

1. **撤回一条我自己写过的结论（重要）**。M1 报告 §8.1「没有 Factory / `PairCreated` 的证据，
   所以新池子无法自动进入状态」与 §8.3 把 `0xcaafb95f…` 归为「非 V2 的 Sync 形状发射者」，
   两条都被本轮实测推翻：`0xcaafb95f…` 是 Narusswap 的真实池子（有 `PairCreated`、`factory()`、
   `allPairs(38)` 三条独立来源），两个 factory 都真实存在且可枚举。
   根因是把「本地语料里搜不到」当成了「链上不存在」：搜索窗口只覆盖 3,725 万高度附近的一段实时交易，
   而池子的创建区块在 545 万–3,139 万。本轮又用同一份扫描代码重扫了同一份语料，
   `PairCreated` 确实 0 条、`Sync` 复现 2,345 条 / 3 个发射者 ⇒ 「本地没有」是对的，
   错的是由它推出「链上没有」。已在 M1 报告对应条目前加了指向本节的注记（文档改动，不改写 M1 的判定正文）。
2. **手续费仍然全部未证实**（4 个池子 `fee = null`）。`feeTo()` / `feeToSetter()` 用 keccak 现算的
   selector（`0x017e7e58` / `0x094b7415`）调用都是 `execution reverted`（顺带修正了此前一次探针用错
   selector 的事）。任何依赖手续费的定价在 M2 拿不到这个数；M2 也不做这个计算（§19、§34）。
3. **§25 同块一致性的代价必须写明白**：图只包含「最新状态恰好落在目标块」的池子。真实链上交易稀疏，
   本轮 6 个真实区块里只有 2 个池子落在目标块，另外 2 个被跳过 —— 也就是说同一份 4 池 registry
   在多数区块上只能给出一张很小的图。这不是 bug 而是任务书的要求（禁止把 N-100 的价格和 N 的价格
   混成 `GraphSnapshot(N)`），但下游必须知道：想要「as-of（≤ target）」语义就得显式扩语义并另立
   规则，绝不能靠静默混合实现。
4. **真实数据里不存在「同一个代币对多个池子」**，所以 §21 / §38.D 只能由 Fixture B 覆盖。
   这是实测出来的，不是没查：45 个 factory 已知配对（GIWA 3 + Naruswap 42）逐个读
   `token0()/token1()`，两个 factory 的配对集合彼此零重合（唯一重合就是池子 4 自己），
   4 个已举证池子也是 4 个不同配对。§38.I 的「多池真实验证」仍然满足：同一区块、2 个池子、
   3 个代币、4 条边。
5. **发现覆盖度：45 个已知配对里只举证了 4 个（8.9%）**。其余 41 个 Naruswap 配对
   `getReserves()` 双边非零（确有流动性），但回放语料里没有它们的权威 `Sync`，
   按 §11 不能凭 `PairCreated` + `getReserves()` 就把它们当成某一块的市场状态。
   要扩这张图，正确做法是扩大回放语料（多抓区块、找到它们的 `Sync`），不是放松证据要求。
6. **`0xad153c84…` 仍然无法建模**：`getReserves()` 有非零值、Sync 形状日志 1,629 条，
   但 `factory()` revert、不在两个 factory 的 45 个 `allPairs` 里、`token0()` revert（M1 实测）。
   拿不到代币身份就不能造边，按 §12 它留在 registry 与图之外；真实验收测试里有一条断言守着它。
7. `GraphSnapshot` 只实现 `Serialize`，没有 `Deserialize`（它的组成类型 `EdgeId` / `GraphEdge` 两者都有）：
   `routes` / `peers` 是从边集合派生的、不参与序列化，而快照又没有任何外部构造入口 —— 加 `Deserialize`
   就得决定「外部字节能不能造出一张派生索引与边集合不一致的图」—— M2 没有这个需求，而 §38 的验收
   也不要求序列化往返，所以没有顺手定（§23 把图当作不可变快照）。
8. 每次 `build` 重扫全部池记录，`pool_edges()` / `edge()` 是集合上的线性过滤；没有为规模做优化（§33 明确
   不要求）。当前规模 4 池 / 4 边，测试与真实路径都跑不满任何缓存需求。
9. 一张图只绑一条链（`chain_id` 来自 `StateSnapshot`），跨链边与跨链图不在 M2 范围。
10. 沿用 M1 的两条：`InMemoryStateStore` 不落盘；被拒绝的空储备 `Sync` 之后池子仍保留上一次有效储备
    （可能已过期），M2 没有「把池子标记为失效」这种状态更新类型。
11. 只覆盖 `v2-compatible` 一种形态。V3/V4、清算、三明治、以及任务书 §34 清单里的
    套利识别 / Bellman-Ford / 环搜索 / 最优输入量 / 利润 / gas / REVM / 模拟 / bundle / relay / signer /
    nonce 在本里程碑一律没有实现，代码里也不存在从图通向它们的路径。

---

## 11. M2 Status

**COMPLETE**

对照 §38 的完成定义（逐条，均可复跑）：

| 条件 | 状态 | 证据 |
| --- | --- | --- |
| A. Pool Registry 能保存经过验证的真实 Pool | ✅ | 4 份带三组证据的 attest（`data/protocols/*.json`），`the_committed_registry_files_all_validate` |
| B. 真实 Token 能成为 Graph Node | ✅ | 3 个真实代币节点（第 4 节），`the_real_block_becomes_a_three_token_graph` |
| C. Pool 成为 Token A ↔ Token B 双向 Edge | ✅ | 4 条边 = 2 池 × 2 方向（第 5 节），`every_edge_matches_the_log_it_came_from` |
| D. 真实数据若存在同 pair 多池必须全部保留 | ✅（真实数据不存在该形状） | 45 个已知配对实测零重合（第 10 节第 4 条）；形状本身由 Fixture B 覆盖，`two_pools_on_one_pair_are_four_edges_and_never_overwrite_each_other` |
| E. Edge 的 reserve 必须来自对应 `PoolState` | ✅ | 4 条边的两个数逐一等于该池 `Sync` 日志的 data（第 6、7 节），且 `state_position` 记录到 (block, log) |
| F. 一个 GraphSnapshot 对应一个明确 block | ✅ | target = `StateSnapshot` 的 applied position `(37258093, 50)`；非本块状态一律跳过并点名，`the_graph_refuses_to_mix_in_a_pool_from_an_earlier_block` |
| G. `neighbors(token)` 与 `edges(token_in, token_out)` | ✅ | `GraphSnapshot::neighbors()` / `routes()`（+ `pool_edges()` / `edge(EdgeId)`），断言见第 4、5 节 |
| H. 同一 StateSnapshot ⇒ Graph A == Graph B | ✅ | 第 8 节：图相等 + JSON 逐字节相同 + 规范边序 |
| I. 至少一个真实 Pool 进真实 Graph；能证明多池就必须做多池真实验证 | ✅ | 2 个真实池、3 个真实代币、4 条边在同一区块成图（另有 2 池在 registry 内、被 §25 跳过） |
| J. fmt / check / test / clippy 全过 | ✅ | 第 9 节：82 passed / 0 failed / 3 ignored，四道关卡全 PASS |

按 §40 的检验方式复述一遍这条链路，每一环都只消费上一环的产物：

```text
真实链上日志（6 个区块，RPC 抓取，SHA-256 可复现）
      ↓ ChainAdapter（录制 / 实时同一份代码）
归一化 ChainLog（块内全局 log_index 排序）
      ↓ V2Adapter（只认 registry 举证过的地址）
ProtocolEvent（Sync = 状态；Swap = 流量，永不变成储备）
      ↓ StateStore
PoolState + 它自己的 (block, log) 位置
      ↓ MarketGraphBuilder（不查链、不猜池子、不跨块混用）
GraphSnapshot(37258093)：3 节点 / 4 边 / 2 条跳过说明
```

M3 才开始承担 Opportunity。
