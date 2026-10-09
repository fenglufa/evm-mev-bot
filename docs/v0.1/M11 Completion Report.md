# M11 多跳决策链（Multi-Hop Strategy Decision）— 完成报告

里程碑：M11（多跳定价 → 金额优化 → 多跳 REVM 仿真 → Risk → ExecutablePlan → M10 Executor → Multi-Lane → 证据）
任务书：`docs/v0.1/M11 Coding.md`（§1–§56）
语义审计底稿：`docs/v0.1/M11 Semantic Audit.md`（§1–§12，写码之前锁定的事实；本轮 32 条口径漂移的根因清单在该底稿 §10）
证据目录：`data/evidence/m11/`（16 个文件 = 15 个被摘要文件 + manifest 自身，§42）
夹具目录：`fixtures/simulation-m11/`（13 个文件：录制三角状态、追加段、合成夹具、探针产物、抓取说明）
基线 commit：`250cd26`（M10 完成报告之后）；M11 对既有 tracked 文件的改动清单见 §26 的生产 diff 审计
判定：`M11 = COMPLETE`，`REAL_PROFITABLE_ARBITRAGE = UNKNOWN`（§31、§53、§54）

---

## 1. Executive Summary

**白话版（先看这一段）**

M9.3 教会系统「在链上的兑换网络里找到一个环」，M10 教会系统「把一个环压进一笔交易，要么全成、要么当没发生」。
这两件能力之间缺一环：**找到一个环之后，该往里放多少钱、这笔钱真跑起来会剩多少、值不值得动手**。
M11 补的就是这一环。它把「一个环」变成一条完整的决策链：

- **多跳定价**：三跳、四跳也能算。逐跳用 `U256` 常数乘积公式，每跳按那个池自己的费率打折，
  算不出来的（费率没被证明）直接拒绝，不用猜的数补上。
- **金额优化**：不再只看「拿某个固定本金赚不赚」，而是在这条路线允许的本金区间里**搜最优点**。
  小范围全搜（并用独立暴力扫交叉核对），大范围先粗采样再局部细化，并且明说「这次没搜完整域」。
- **真 EVM 仿真**：把路线交给 M10 那个执行合约的真实字节码、在**钉住的历史块状态**上跑一遍，
  拿到 gas、日志、逐槽位改动、逐地址余额变化，以及「合约实际到账」和「定价说会给」的差额。
- **Risk**：十一条具名检查逐条回答，答案有三种：接受 / 拒绝 / 不知道。「不知道」不允许被写成「拒绝」，
  更不允许写成「通过」。
- **计划绑定**：仿真结果转成可执行计划时，计划的哈希与 calldata 哈希必须由 M10 那套编码**重算相同**，
  不是 M11 另起一套。
- **多通道（Lane）**：多个候选可以并行仿真，但只有被选中的那一个才真正占用一次 nonce 和一笔本金；
  输了、过期了、付不起的，都拿不到资源。这一层完全不碰网络、不碰私钥。

**这一轮真正跑通并留下证据的**：
- 受控 2 跳端到端：本金 1e14 wei 进、864 806 517 171 626 wei 出，毛赚 764 806 517 171 626 wei；
  合约实际到账与定价逐 wei 相同（差 0）；烧 274 067 gas；calldata 580 字节。
- 受控 3 跳端到端：本金 1e12 wei 进、1 186 871 066 266 wei 出，毛赚 186 871 066 266 wei；
  三条腿真是三条腿在合约里跑的（reserve 行改动 3 条）；烧 425 200 gas；calldata 772 字节。
- 3 跳原子回滚：把第三跳的下限抬到它拿不到的数额，整笔在链上 revert（`UniswapV2: K`），
  三个池的槽位改动计数 `[0,0,0]`、9 条余额行全部不变——**一笔都没走**，这就是原子性。

**这一轮不能声称的**（关键，别把上面那段读成「系统已经能赚钱」）：
- 上面的「赚」发生在**受控夹具状态**上。池子、代币、储备是真实录制来的，但那一份状态是本地喂给模拟器的。
  所以证据里 `market_claim.kind` 写的是 `REAL_MARKET_POOLS_ON_A_CONTROLLED_FIXTURE_STATE`，
  不是 `REAL_MARKET`。
- 真链上没有跑过任何一笔 M11 计划。`data/evidence/m11/real/` 三个文件（execution / failure / reconciliation）
  每一行都只有四个字段：这是什么、什么能回答它、为什么没测、为什么不能写成 0；`verdict = "UNKNOWN"`。
  按 §41，没有真实盈利机会时必须写 UNKNOWN，**0 是一个判定值，不是「没数据」**。
- 4 跳只在手写图上定价，没有进过 EVM（M9.3 的搜索层硬顶就是 3 跳）。
- 真链上不存在「M11 能驱动的三跳环」：录制里那三个互相成对的池（triangle），运行时字节码里没有
  `swap` 派发分支，M10 的合约叫不动它。这件事有专门的探针文件留证（§29）。

一句话结论：**M11 交付并证明了「决策链」这件能力本身（含 3 跳真进 EVM 与 3 跳原子回滚），
没有证明「这套东西在真市场上能赚钱」，也没有证明任何生产就绪性。**

---

## 2. Scope（本轮实际交付）

| 交付物 | 位置 | 量（现量） |
|---|---|---|
| 多跳路线 + 多跳定价 | `crates/opportunity/src/multihop.rs` | 539 行 |
| 有界离散金额优化器 | `crates/opportunity/src/multi_optimizer.rs` | 405 行 |
| 多跳 REVM 仿真层 + `SimulatedOpportunity` | `crates/simulation/src/multihop.rs` | 649 行 |
| 多跳 Risk（11 条具名检查） | `crates/risk/src/multihop.rs` | 580 行 |
| 仿真 → M10 计划的 hash 绑定 | `crates/execution/src/multihop_plan.rs` | 475 行 |
| Multi-Lane 状态机 + nonce/资本占用 | `crates/execution/src/lanes.rs` | 1 227 行 |
| **新增生产代码合计** | 6 个文件 | **3 875 行** |
| M11 测试 | `crates/{opportunity,simulation,execution}/tests/` | 16 个文件、**23 281 行** |
| M11 测试 target | cargo 报告 | **13 个 target：201 passed / 0 failed / 12 ignored**（声明数 213） |
| 证据目录 | `data/evidence/m11/` | 16 个文件；被摘要的 15 个共 **442 541 字节** |
| 夹具目录 | `fixtures/simulation-m11/` | 13 个文件；合成夹具 202 875 字节 |
| 对既有文件的改动 | 11 个 tracked 文件 | **+154 / −7**（明细见 §28） |

六个新生产文件里 **0 处** `panic!` / `unreachable!` / `unwrap()` / `expect(` / `todo!` / `unimplemented!`，
**0 处** `f64` / `f32`，且**没有一处 `#[cfg(test)]`**（M11 的测试全在 `tests/` 目录，见 §25、§20）。

---

## 3. Non-goals（§2 的 16 条禁令，一条都没越）

| 禁令 | 现量核对 |
|---|---|
| ❌ V3 / Curve / Balancer | 定价只走 V2 常数乘积；`crates/opportunity/src/multihop.rs` 无其他 AMM 分支 |
| ❌ Flashloan | 全程无借贷原语；本金来自 `CapitalDomain` 的自有额度（§13） |
| ❌ Sandwich | 无 mempool 读写、无受害者交易 |
| ❌ Cross-chain | 每条证据行的 `chain_id` 都是 91342；无跨链字段 |
| ❌ AI strategy / LLM | 优化器只有 `exhaustive` / `coarse_then_refine` 两种策略，无模型调用 |
| ❌ Private Sequencer | 无新增提交通路（§12 复用清单） |
| ❌ Self-hosted GIWA node | 未启动任何节点；`rpc_count = 0`（§20） |
| ❌ Flashblocks direct execution | M9.4 的 flashblock 模块一行没改，也没被 M11 接进执行链 |
| ❌ Bellman-Ford / SPFA 替代 PathFinder | `crates/pathfinder/` **零改动**（`git status` 不含该路径）；M11 只消费 `CycleCandidate` |
| ❌ 新 Executor Contract | `contracts/` 零改动；用的还是 M10 那份 425 行合约 |
| ❌ 新 Signer framework | `crates/execution/src/signer.rs` 零改动 |
| ❌ 新 Receipt framework | `crates/execution/src/receipt.rs` 零改动；3 跳端到端复用它取回执（§15） |
| **尤其：不修改 M9.3 的 `CycleCandidate` 语义** | `crates/pathfinder/src/candidate.rs` 不在 diff 内；7 字段与 3 跳硬顶原样，审计底稿 §2、§3 已现量 |

---

## 4. 架构整合（§5.1）与复用面

任务书 §5.1 的前提（「pathfinder 需要纳入 workspace」）被现量推翻：**evm-pathfinder 早已在 workspace 里**
（审计底稿 §1）。M11 因此只做了一件事——把依赖方向接上，不新建 crate：

```text
crates/opportunity/Cargo.toml   +1 行（生产依赖 evm-pathfinder，读 CycleCandidate）
crates/simulation/Cargo.toml    +6 行（生产依赖 evm-opportunity；dev 依赖 evm-pathfinder）
Cargo.lock                      +2 行（evm-pathfinder 的这两条新边，现量：git diff 只见两处 +）
```

复用清单（M11 用到的、没有重写的东西）：

| 能力 | 复用自 | M11 里的位置 |
|---|---|---|
| 环的发现与身份（最小旋转） | M9.3 `CycleCandidate` / `RouteIdentity` | `multihop.rs` 只把候选转成路线，不重搜（`detector.rs` 未动） |
| 执行合约与 calldata 编码 | M10 `arbitrage.rs` 的 `canonical_text` / `calldata` | `multihop_plan.rs:475` 行全部是「绑定」，没有第二个编码器 |
| 协议侧 selector / 编解码 / revert 解码 | M10 `crates/protocol/src/executor.rs` | 仿真与 e2e 直接调 |
| REVM 执行引擎与钉块状态 | M10/M4 `crates/simulation/src/{revm,state,executor}.rs` | `simulation/src/multihop.rs` 是它的一个调用者 |
| 签名 / 提交 / 回执 / 时效闸门 | M6–M8 执行流水线 | 计划走同一 `ExecutablePlan` 出口，M11 没开第二条 |
| Gas 计价（EIP-1559、L1 fee） | M4/M7 `simulation/src/gas.rs` | `GasCharge::Priced` 由它产出，见 §10 |

---

## 5. `MultiHopRoute`（§7–§8）：不可变 + 旋转身份

- 构造即校验：不是闭环的 trail、闭环但带尾巴、 hop 数超过候选、非 2^256 域内的字段，全部按名字拒绝。
  证人：`crates/opportunity/tests/multihop.rs` 的 `a_trail_that_is_not_a_route_is_refused_by_name`、
  `a_closed_trail_with_a_tail_is_refused`、`an_amount_that_leaves_256_bits_is_refused_not_wrapped`。
- 不可变（§8）：`price()` / `optimize()` 都不写回路线，有专门测试 `pricing_a_route_does_not_change_it`、
  `searching_a_route_does_not_change_it`。
- 身份（§7）：同一环的三个座位只有一个身份 —— `three_seats_of_one_cycle_have_one_identity`；
  反向是另一条路线 —— `a_reversed_cycle_is_a_different_route_not_the_same_one`。
  证据里 `published.identity` 就是这个 `RouteIdentity` 的最小旋转，门禁按 `Vec<EdgeId>` 的序比较
  （`pricing/*.json` 的 `recomputed_independently.identity_method`）。
- 路线带着它读自哪个块（`the_route_carries_the_block_it_was_read_at`），
  不允许拿 A 块的储备给 B 块的候选定价（`a_candidate_is_not_priced_against_another_blocks_reserves`）。

---

## 6. 多跳定价（§9–§12）：U256、逐跳费率、floor 取整

口径：`out = in * fee_num * reserve_out / (reserve_in * fee_den + in * fee_num)`，逐跳 floor 取整，
费率取自 `GraphEdge.fee`；`fee == None`（未被证明）直接拒绝，不补默认值。

**实测三行**（`data/evidence/m11/pricing/`）：

| 行 | 输入 | 输出 | 毛赚 | 输入上限 | 跳数 | 市场断言 |
|---|---|---|---|---|---|---|
| `recorded_2hop` | 100 000 000 000 000 | 864 806 517 171 626 | 764 806 517 171 626 | 19 999 999 999 999 999 | 2 | `REAL_MARKET_POOLS_ON_A_CONTROLLED_FIXTURE_STATE` |
| `declared_3hop` | 1 000 000 000 000 | 1 186 871 066 266 | 186 871 066 266 | 2 399 999 999 999 999 | 3 | `DECLARED_SYNTHETIC_GRAPH` |
| `declared_4hop` | 1 000 | — | 80 | 999 999 | 4 | `DECLARED_SYNTHETIC_GRAPH` |

- 三行都带 `agrees.quote_with_the_fold = true`（发布值 = 本文件自己重算的折叠）与
  `agrees.identity_with_the_rotation_minimum = true`。
- 逐跳费率都是 `997 / 1000`（0.3%），`each_hop_is_priced_at_its_own_pools_fee` 是有证人测试的。
- `recorded_2hop` 的第一跳输出 90 661 089 388 014 913 158，池储备是录制的
  `1e15 / 1e21`；`state_source` 字段写明取自 `fixtures/simulation-m7/dump-37530593-07D4af6E.json`。
- 4 跳那行是手写图（chain 7、四个声明池 p1–p4），**只到定价为止**，理由见 §29。
- 边界行为有专门证人：零输入被拒绝而不是报 0（`a_zero_input_is_refused_rather_than_quoted_as_zero`）、
  零储备先于路线被拒（`a_zero_reserve_is_refused_before_a_route_can_read_it`）、
  买不到东西的一跳会终止这趟并说出来（`a_hop_that_buys_nothing_ends_the_trip_and_says_so`）、
  亏损就按亏损报数额（`a_loss_is_stated_as_a_loss_with_its_size`）、
  费率未证明时点名的那个池（`an_unattested_fee_prices_nothing_and_names_the_pool`、
  `the_first_unattested_hop_is_the_one_named`）、取整是 floor 且本金越大赚得越少（`rounding_is_floor_at_every_hop_...`）。

---

## 7. 金额优化（§13–§18）：有界搜索 + 独立暴力对照

策略只有两种（`OptimizationStrategy`）：`exhaustive`（在窗口内逐点）与 `coarse_then_refine`
（粗采样 256 点 → 在最佳邻域 64 跨度内细化）。政策上限 `exhaustive_limit = 4096`，
超限会被夹住并报告（`a_policy_outside_its_caps_is_clamped_and_reported`）。
每次搜索都产出 `Termination`，并诚实记 `covered_the_route_domain` —— 证据五行该字段**全为 `false`**，
即「这次没有覆盖整条路线的域」，没有把「窗口内最优」写成「全局最优」。

**实测五行**（`data/evidence/m11/optimizer/`，策略三值均为 `coarse_points 256 / exhaustive_limit 4096 / refine_span 64`，`refusals = 0`）：

| 行 | 求值次数 | 最优输入 | 最优输出 | 最优毛赚 | 搜索域 | 终止原因 |
|---|---|---|---|---|---|---|
| 2 跳单点 | 1 | 100 000 000 000 000 | 864 806 517 171 626 | 764 806 517 171 626 | — | `DomainExhausted` |
| 2 跳全窗 | 64（64 点） | 同上 | 同上 | 同上 | 上界夹到路线上限 | `DomainExhausted` |
| 2 跳粗+细化 | 385（256 粗） | 19 921 940 | 198 025 870 | 178 103 930 | 1 – 20 000 000 | `WindowRefined` |
| 3 跳单点 | 1 | 1 000 000 000 000 | 1 186 871 066 266 | 186 871 066 266 | — | `DomainExhausted` |
| 3 跳全窗 | 32 | 1 000 000 000 028 | 1 186 871 066 299 | 186 871 066 271 | 1e12 – 1e12+31 | `DomainExhausted` |

- §16 的 brute-force oracle 是真对照：`a_walkable_domain_is_walked_and_agrees_input_for_input`
  要求逐点输入相同、结果相同；`a_two_hop_route_agrees_with_the_two_hop_scan` 跨实现对照。
- §15 的边界/内部极值都有证人：`a_window_that_ends_before_the_peak_returns_its_own_edge`、
  `an_interior_peak_is_found_and_is_not_a_domain_edge`、`a_window_past_the_routes_ceiling_is_clipped_and_says_so`。
- 全域负/全平的路线不硬说赚：`a_route_that_never_pays_reports_its_smallest_loss`、`a_route_that_breaks_even_is_reported_as_even`。
- 空域是拒绝不是 0：`an_empty_domain_is_a_refusal_not_a_zero`；越出 256 位域的域报出自己的拒绝
  （`a_domain_that_leaves_256_bits_reports_its_refusals`）。
- §18：`OptimizedCandidate` 只带「路线 + 报价」，不带任何执行许可
  （`a_candidate_carries_the_route_and_the_quote_and_nothing_else`；`multi_optimizer.rs` 里 `OptimizedCandidate` 无传输字段）。

---

## 8. 多跳 REVM 仿真（§19–§25）

请求构造（§20–§21）：`crates/simulation/src/multihop.rs` 按定价的每一跳、按交易顺序生成腿，
**每腿的输入必须等于前腿的输出**，每腿下限必须等于「该腿自己报价的输出」。全部有证人
（`multihop_adapter.rs` 的 `the_amount_chain_chains`、`each_floor_is_its_own_quoted_output_exactly`、
`legs_that_are_not_the_priced_routes_are_refused`、`a_route_one_leg_past_the_contracts_cap_is_refused`、
`a_broken_amount_chain_is_refused_at_the_leg_that_broke`、`a_first_leg_spending_another_amount_is_refused`）。
请求形状仍是 M10 那一套（`the_request_is_the_existing_m10_shape`），调用方**没有字段去重述链号或块号**
（`the_caller_has_no_field_to_restate_the_chain_or_the_block`）。

§22（REVM 必须验真实 Executor）与 §23（必须钉块）：仿真跑在 M10 合约字节码与 `BlockPin` 上，
`the_request_is_byte_for_byte_the_m10_request`、`the_pin_is_the_providers_and_agrees_with_the_block_the_plan_claimed`、
`a_request_naming_state_the_provider_does_not_serve_is_refused`。

§24 新模型 + §25 失败必须成为明确状态：`SimulationStatus` 五种结尾（delivered / reverted / out-of-gas /
halt / no-return-data 类），`gas_never_enters_gross`、`no_failure_is_ever_written_as_profit_zero`、
`a_success_with_no_return_data_is_not_a_zero_delivery`、`out_of_gas_and_a_halt_are_not_each_other`。

**实测两行**（`data/evidence/m11/simulation/`）：

| 行 | 状态 | 到账 | 毛赚 | gas_used | gas 费用（wei） | 槽位改动 | 日志 | 余额行 | priced vs delivered | market_moved |
|---|---|---|---|---|---|---|---|---|---|---|
| `recorded_2hop` | `delivered` / `Success` | 864 806 517 171 626 | 764 806 517 171 626 | 274 067 | 99 212 254 | 13 | 13 | 6 | `even`，差额 0 | `true` |
| `declared_3hop` | `delivered` / `Success` | 1 186 871 066 266 | 186 871 066 266 | 425 200 | 153 922 400 | 18 | 18 | 9 | `even`，差额 0 | `true` |

- gas 计价是 `GasCharge::Priced`：`effective_gas_price = 362`、`base_fee_per_gas = Some(362)`、
  `priority_fee_per_gas = 0`，provenance 写明「37530593 块自己头里的 base fee；历史块上的假想交易不竞争进块，所以不给小费」。
  `gas_limit = 3 000 000`。
- 身份：`identity_hash` 是对 `canonical_text` 的哈希，2 跳 `0xc80475f9…b9e8`、3 跳 `0x4c97ec3c…c6d5`；
  calldata 哈希 2 跳 `0xde854866…bcc4`（580 字节）、3 跳 `0x34d7232e…0ed4`（772 字节）。
  `canonical_text` 前缀为 `m11-multi-hop-simulation\nchain_id=91342\nblock=37530593\nblock_hash=0xca54d5e5…`。
- 状态来源：`fixtures/simulation-m10/fixture-37530593-executor.json`（追加段 22 372 字节）与
  `fixtures/simulation-m11/fixture-37530593-triangle.json`（追加段 100 772 字节、合成 202 875 字节）。
- `market_moved = true` 的含义被写成 absence 而非 promise：储备确实在仿真里动了（这是真实执行的样子），
  但这是夹具状态上动的，不是市场上动的。

---

## 9. Risk（§26–§29）：十一条具名检查，三种答案

`RiskCheck` 11 个变体（`crates/risk/src/multihop.rs:130–140`）：
`SimulationSuccess`、`InputPositive`、`OutputAboveInput`、`MinimumGrossProfit`、`MaximumGas`、`FinalGuard`、
`RouteValidity`、`ChainValidity`、`ExecutorValidity`、`SimulationFreshness`、`StateFreshness`。
§27 的每一行都是一个具名检查（`every_line_of_twenty_sevens_list_is_a_named_check`），
`all_eleven_lines_reach_an_answer` 要求 11 条都走到答案。
`ChainValidity` 与 `ExecutorValidity` 故意分两问（失败原因不同），`FinalGuard` 再读一次实际到账对计划下限。

三态不可塌缩：`unknown_and_reject_stay_two_answers_about_the_same_check`、
`an_absent_head_is_unknown_rather_than_a_stale_rejection`、`an_absent_state_version_is_unknown`、
`a_delivery_with_no_gross_figure_is_unknown`、`the_three_states_are_distinguishable_in_json`。

§28（Risk 不发交易）是结构性事实 + 证人：`risk_reaches_no_network_and_no_key`。

**实测三行**（`data/evidence/m11/risk/decision.json`）：

| 行 | case | 结论 | 触发检查 | 关键数字 |
|---|---|---|---|---|
| `accept-recorded-2hop-0xc80475f9…` | recorded-2hop | `accept` | —（无失败项） | delivered 864 806 517 171 626 / input 1e14 / gross 764 806 517 171 626 / floor 0 |
| `accept-declared-3hop-0x4c97ec3c…` | declared-3hop | `accept` | — | delivered 1 186 871 066 266 / gross 186 871 066 266 |
| `reject-floor-above-the-delivery-…` | recorded-2hop（下限抬到到账之上） | `reject` | `MinimumGrossProfit` | floor = 2^256−1；reason 前缀 `minimum_gross_profit\|the round trip gains 764806517171626 … against a floor of 1157920892373161954235709850086879078532699846656405640394575840079131…` |

政策（三行同源）：`chain_id 91342`、`executor 0x1000…0010`、`minimum_gross_profit "0"`、
`maximum_simulation_age 3`、`maximum_state_age 3`、`maximum_gas` = **本次运行自己测出的烧量**（274 067 / 425 200）。
最后一项要在 §29 老实说：它是「描述这次跑」而不是「市场阈值」。
`market_facts` 记录 head 37530593、state version 37530593，并写明「没有向任何节点问过」（§28）。

`minimum_gross_profit` 取 `"0"` 的理由写在 policy.provenance 里：§27 的 `output > input` 已经是它上面那一问，
这一层再造一个正数门槛就等于把「值不值得做」的假设塞进判定。

---

## 10. 计划绑定（§29–§30）：hash 只能由 M10 那套编码决定

`crates/execution/src/multihop_plan.rs`（475 行）只做三件事：
`MultihopPlanContext`（链/块/执行合约/签名者的上下文核对）→ `MultihopBinding`
（仿真记录与上下文逐字段相等才成立）→ `plan_from_simulation` / `executable_plan`
（交给 M10 的 `canonical_text` / `calldata` 编码，然后取它的哈希）。

证据里这条链是闭合的（`controlled/2hop/chain.json` 的 `stages.binding`）：
`execution_calldata_hash == simulation_calldata_hash == 0xde854866…bcc4`，
`plan_hash = 0x29c59c8b…14b9`，`the_five_claims` 里发布的 plan/calldata/identity 三个哈希
都有 `*_recomputed` 复算同值。证人：`a_real_run_that_is_not_the_candidates_plan_is_refused`、
`a_recorded_run_with_an_edited_route_never_becomes_a_plan`、
`a_run_must_belong_to_the_candidate_it_is_filed_under`、
`the_two_paths_to_the_evm_agree_on_every_measured_number`。

M11 没有引入第二个执行契约（§2 禁令），`ExecutablePlan` 就是 M10 的那个类型。

---

## 11. Multi-Lane（§31–§36）

`crates/execution/src/lanes.rs`（1 227 行）：`LaneState` 13 态、其中 4 态持 nonce；
合法箭头**只有一个定义处** `LaneState::allows`（审计底稿 §7）。
`CapitalDomain` 的不变式 `available + reserved == capacity` 由每一步重算（`sums_to_capacity_recomputed`）。

**实测一行**（`data/evidence/m11/lanes/lane_matrix.json`，行 id `lane-ledger-§31-§36`）：

- **43 步**，6 条 lane；动作计数：`advance 13`、`open 6`、`begin_simulation 6`、`record_simulation 5`、
  `choose_winner 3`、`reserve_for 1`、`settle 1`，另 8 步是控制臂/断言。
- 域容量 `"1000"`，每条 lane 认领 `"100"`；终态：`available 1000`、`reserved 0`、`settled_input 100`、
  `invariant_holds true`。6 条 lane 终态 `settled / ready / ready / expired / simulating / ready`。
- 胜者规则：`gross` 更大者胜，平局取 lane id 更小者（`winner_selection_is_a_total_order_independent_of_input_order`）。
- **§36 的「不自动抢跑」在数据里的形状**：43 步里 `reserve_for` 只出现 **1 次**，且发生在 `choose_winner` 之后 ——
  只有胜者占用 nonce 与资本（`nothing_is_reserved_before_a_winner_exists`）。
- 7 个控制臂（全部以 refusal 收口，正向臂单列）：

| 控制臂 | refusal_code |
|---|---|
| `nc36_a_losing_lane_is_turned_away` | `lane_not_selected` |
| `nc12_a_committed_pair_cannot_be_taken_twice` | `nonce_held` |
| `nc12_the_next_nonce_is_granted`（正向臂） | —（nonce 1 被发出） |
| `nc15_expiry_returns_the_pair` | —（成对释放，附 from/to/disposition） |
| `nc14_one_plan_hash_binds_one_lane` | `duplicate_plan` |
| `nc14_one_candidate_gets_one_lane` | `duplicate_lane` |
| `nc13_a_winner_that_cannot_pay_is_refused` | `insufficient_capital` |

- 另有断言步 `three_lanes_simulate_in_parallel`（§33 的并行仿真不占资源）。
- 拒绝的诚实性：`every_refusal_carries_a_unique_label_and_a_section`、
  `every_failure_names_its_state_and_its_disposition`、`an_unseen_lane_is_refused_by_name_and_never_panics`。
- Lane 层同样拿不到传输能力（§28 边界，审计底稿 §5）：
  `what_this_is` 写的是「nothing here touches REVM, an endpoint, or a key」。

---

## 12–13. 受控 2 跳端到端（§37）

`data/evidence/m11/controlled/2hop/chain.json`，行 id
`chain-91342-block-37530593-recorded-pair-0x29c59c8b…14b9`（id 尾段就是 plan hash —— 语义键，见审计底稿 §9）。

`stages` 七个阶段齐全：`pricing`、`search`、`optimizer`、`simulation`、`risk`、`binding`、`plan`。
`the_contract_answered`：delivered `864 806 517 171 626`、quoted_output 同值、gas_used `274 067`、
`min_final_output_met true`、`reserve_rows_moved 2`、`status delivered`。
`stages.plan`：`calldata_len 580`、`min_final_output 864 806 517 171 626`、
`simulation_block_hash 0xca54d5e5…99b7`。`not_claimed` 3 项。

证人（`multihop_e2e.rs`）：`the_controlled_two_hop_walks_from_a_found_cycle_to_a_bound_plan`、
`the_two_hop_plan_reaches_the_receipt_framework_and_the_balances_reconcile`（走 M10 回执框架 + 余额对账）、
`the_declared_cycle_is_a_replay_of_the_recorded_pair`。

## 13–14. 受控 3 跳端到端 + 3 跳原子回滚（§37–§39）

`data/evidence/m11/controlled/3hop/chain.json` 两行：

**成功行** `chain-91342-block-37530593-declared-triangle-0x8656c2ba…60ec`：
delivered `1 186 871 066 266`、gas_used `425 200`、**`reserve_rows_moved 3`**（三跳真是三条腿）、
`min_final_output_met true`、plan hash `0x8656c2ba…60ec`、`calldata_len 772`。
证人：`three_legs_reach_the_contract_as_three_real_legs`（§38 的正身）、
`the_gas_ladder_shows_the_three_legs_ran_in_order`（gas 阶梯证明腿是按序跑的）。

**回滚行** `three-hop-rollback-at-the-third-pool`（§39）：
`status Reverted`、`revert_reason "UniswapV2: K"`、原始 revert 数据 `0x08c379a0…`、
**`changed_slots_in_the_three_pools = [0, 0, 0]`（三个池各自点名）**、
`balance_rows 9` 且 `balance_rows_unchanged 9`、`delivered null`、`market_moved false`。
证人：`a_failed_third_leg_leaves_every_balance_and_reserve_untouched`。

这一行是 M11 最有分量的证据之一：它不是「回滚被 mock 了」，而是**第三跳在真字节码里 revert，
前两跳已经改过的槽位全部还原**。它证明的是机制，不是市场。

---

## 14. 四种 Profit 严格分开（§4）

| 层 | 本报告的出处 | 状态 |
|---|---|---|
| Estimated | §6 定价三行（`gross.state = gain`） | 有数 |
| Simulated | §8 仿真两行（`gross` + `gas_charge`，两者不同单位、绝不相减） | 有数 |
| Executed | §13 受控合约层（`the_contract_answered`）；**真链 0 笔** | 只有受控 |
| Realized | — | **无**。跨单位换算没有预言机，且真链未跑 |

口径纪律（沿袭 M4/M7/M10）：gas 费用 wei 与代币 wei 是两个东西，`gas_never_enters_gross` 是证人测试；
因此本报告没有把「764 806 517 171 626 − 99 212 254」写成净利润。两个数都在证据里，相减是假账。

---

## 15. Real GIWA（§40–§41）：逐字段 UNKNOWN

§40 要求真实跑时记录 12 项：`chain_id`、`block`、`route_id`、`candidate_id`、`input`、`output`、
`simulation result`、`tx hash`、`receipt`、`gas`、`fee`、`balance delta`。

M11 **没有真链跑**，所以 `data/evidence/m11/real/` 三个文件（execution / failure / reconciliation）
每一行只有四个字段，且不含上述任何一项的具体值：

| 字段 | 内容 |
|---|---|
| `what_this_is` | 这一行本该回答 §42 的哪一问 |
| `what_would_answer_it` | 需要什么样的真实动作才能填 |
| `not_measured_because` | 本轮为什么没做 |
| `why_not_zero` | 「§41 forbids publishing a real-market verdict from a CONTROLLED_FIXTURE run, and 0 is a verdict.」 |

`evidence_that_does_exist_for_the_mechanism` 分别给 5 / 4 / 4 条**能打开的路径**（含 M10 的真链证据
`data/evidence/m10/real/giwa_execution.json` 等），`verdict = "UNKNOWN"`。
manifest 顶层 `verdict` 同样是 `"UNKNOWN"`，`verdict_means` 写成：受控链已测且可重算，
真实链那一侧的问题 M11 没问，因此不报为 0。

**禁止的写法**（本报告也不这么写）：`REAL_PROFITABLE_ARBITRAGE = 0`、把 `CONTROLLED_FIXTURE` 当 `REAL_MARKET`、
把受控毛赚换算成「能赚到的钱」。

---

## 16. 证据目录（§42）

```text
data/evidence/m11/                     16 个文件
├── pricing/{recorded_2hop,declared_3hop,declared_4hop}.json      6 811 / 8 824 / 9 475 B
├── optimizer/{recorded_2hop,declared_3hop}.json                    191 795 / 24 747 B
├── simulation/{recorded_2hop,declared_3hop}.json                   15 880 / 19 559 B
├── risk/decision.json                                              13 479 B
├── lanes/lane_matrix.json                                          48 576 B
├── controlled/2hop/chain.json                                      37 302 B
├── controlled/3hop/chain.json                                      57 575 B
├── real/{execution,failure,reconciliation}.json                    2 204 / 1 941 / 2 011 B
├── manifest.json                                                    9 264 B
└── README.md                                                        2 362 B
```

manifest 现量字段：`schema "m11-evidence-v1"`、`files_written 16`、`len(tree) 16`、
`len(file_digests) 15`（关系是 `digested + 1 == declared`，因为 manifest 不能摘要自己）、
`total_bytes 442 541`、`total_bytes_scope` 把口径写成文字且**不含数字字面量**、
`rpc_count 0`、`verdict "UNKNOWN"`、`self_digest "not included: a file cannot hash the bytes it is about to contain"`、
`determinism {assemblies_in_this_run: 2, byte_identical: true}`、
`not_claimed` 3 项、`not_measured` 3 项、
`assembled_by "crates/simulation/tests/multihop_evidence.rs"`、
`checked_by "crates/execution/tests/multihop_evidence_gate.rs"`、
`assemble_command` / `check_command` 两条命令原文可复制执行。

职责分离（审计底稿 §11 第 7 条）：写入器**只装配不判定**（2 passed + 1 ignored setup），
门禁**只重算不写盘**（6 passed）。

---

## 17. 独立重算（§43）

§43 要求六项可独立验证：`pricing`、`optimizer`、`simulation summary`、`route identity`、`plan hash`、`calldata hash`。
门禁 `crates/execution/tests/multihop_evidence_gate.rs`（5 393 行）跑 **11 个相位**（源码 grep `phase 1..11`），
用库 API **重新跑一遍 REVM**，而不是读一遍文件：

- 定价：门禁自己做常数乘积折叠，与 `published.quote` 逐 wei 比；
- 身份：自己做最小旋转，按 `Vec<EdgeId>` 与 `EdgeId` 自带的 `Ord` 比（不用位置键）；
- 仿真：重跑执行，比 delivered / gas / 槽位 / 日志 / 余额行；
- 计划与 calldata：用 M10 的编码器重算哈希，与发布值比；
- 目录完整性：16 个文件在第一个相位与最后一个相位之间逐字节相同（门禁自己不写盘的可核证据）；
- 密钥形状扫描（§50）与 `not_claimed/not_measured/UNKNOWN` 的字段要求。

门禁 target 本轮最后一次运行（第 6 次）的终态，逐字誊录：`test result: ok. 2 passed; 0 failed`
（`the_evidence_directory_recomputes` + `gate_compact_boundary::cuts_at_a_char_boundary`）。

**负对照（篡改必须被抓住）**：本轮跑过一次「把 `real/execution.json` 改 1 字节 + 把 `total_bytes` 改 +2」，
门禁报出 4 条：
该文件 digest（published bytes 2204 / keccak `0x66cb5674…` vs recomputed bytes 2203 / keccak `0x94a839de…`）、
`manifest total_bytes`（被 inventory 与「wrote nothing」两个相位各抓一次）、
一条指向不存在的 `data/evidence/m10/real/execution.json` 的假指针。逐字节还原后门禁再次全绿。
这条纪律的原因是本轮真实踩过的坑：写入器与门禁首跑报 **32 条漂移、去重后 31 个字段**，
六类根因（口径/snapshot/字段名）逐类记录在审计底稿 §10，**没有一处是为了让门禁变绿而改数字**。

上面这些运行日志（`target/m11-scratch/`，仓库根 `.gitignore` 的第 1 行 `/target` 让它本来就不入库）
是本轮的过程产物，**在提交前删除**；决定性的行已经逐字誊进本节与 §25，
读者要用 §16 的 `assemble_command` / `check_command` 重跑即可复现，不需要那份日志。

---

## 18. RPC 增量（§44）：`rpc_count = 0`

manifest `rpc_count 0`，`rpc_count_basis` 三条同时成立：
1. 每一条运行都由 `evm_simulation::state::DumpStateProvider` 服务（本地 dump，无 endpoint）；
2. `endpoint_variable_present: false`（环境里没有 `GIWA_RPC_URL`）；
3. 唯一可能产生 RPC 的文件 `crates/simulation/tests/multihop_capture.rs` 是 `#[ignore]` 的，
   不在本次装配内；它自己的成本发布在 `fixtures/simulation-m11/capture-37224031-triangle-notes.json`。

§44 禁止的四个方法（`eth_call` / `eth_getBalance` / `eth_getCode` / `eth_getLogs`）在 metrics、logging、
evidence、debug 四条路径上都没有被 M11 增加 —— 因为 M11 的证据链根本没有传输面（§11 的 Lane 与 §9 的 Risk
都是结构性无网络，审计底稿 §5）。

---

## 19. 测试矩阵（§45）→ 实到 13 个 target

| §45 分组 | 要求项 | 承载 target（cargo 现量） |
|---|---|---|
| Pricing | 2/3/4-hop、zero input、zero reserve、missing fee、broken route、integer rounding、U256 overflow boundary | `evm-opportunity / multihop.rs`（21 passed） |
| Optimizer | brute-force oracle、boundary optimum、interior optimum、all-negative、zero-profit、large U256、determinism | `evm-opportunity / multi_optimizer.rs`（19 passed） |
| Simulation | 2/3/4-hop、revert、stale block、wrong chain、wrong executor、min output、final guard | `multihop_revm.rs`（18）+ `multihop_adapter.rs`（30）+ `multihop_negative_controls.rs`（13） |
| Risk | profitable、unprofitable、gas too high、simulation reverted、stale、invalid route | `multihop_risk.rs`（40 passed） |
| Lane | parallel simulation、nonce conflict、capital conflict、release、commit、expiration、duplicate plan、winner selection | `evm-execution / multihop_lanes.rs`（35 passed） |
| 端到端 | §37–§39 | `multihop_e2e.rs`（9 passed + 1 ignored：`a_first_look_at_the_declared_cycle`，开发用臂） |
| 确定性 | §47 | `multihop_determinism.rs`（5 passed） |
| 证据 | §42–§44 | writer `multihop_evidence.rs`（2 + 1 ignored setup）、gate `multihop_evidence_gate.rs`（6） |
| 真实侧取证 | 3 跳环可执行性 | `triangle_probe.rs`（3 passed + 9 ignored 测量臂）、`multihop_capture.rs`（1 ignored） |

**合计 13 个 target：201 passed / 0 failed / 12 ignored**；声明的测试属性 213 处，与 cargo 完全对得上
（`#[tokio::test]` 15 处，故只按 `#[test]` 词面数会少数）。12 个 ignored 的去处：
`triangle_probe.rs` 9（测量臂，需真跑才发现，注明复现命令）、`multihop_capture.rs` 1（读 live 节点写夹具）、
`multihop_e2e.rs` 1（开发臂）、`multihop_evidence.rs` 1（装配 setup）。

---

## 20. Critical Negative Controls（§46）：NC1–NC15 + nc36

| NC | 任务书要求 | 证人（现量测试名） |
|---|---|---|
| NC1 wrong chain | 链不符必须拒 | `nc01_wrong_chain_is_refused_by_the_policy_and_by_the_plan_binding`、`a_run_on_the_wrong_chain_is_refused_before_the_address_is_asked` |
| NC2 stale simulation | 时效过期必须拒 | `nc02_staleness_is_measured_at_both_boundaries_and_is_unknown_when_unasked`、`a_recorded_run_behind_the_head_is_rejected`、`a_run_behind_the_head_by_more_than_the_bound_is_rejected`、`a_run_ahead_of_the_head_is_rejected_not_wrapped_around`、`state_that_has_moved_past_the_run_is_its_own_rejection` |
| NC3 broken route | 路线不闭合必须拒 | `nc03_five_broken_route_shapes_are_refused_before_any_price`、`legs_that_are_not_the_priced_routes_are_refused`、`a_route_that_does_not_come_back_is_refused` |
| NC4 missing fee | 费率未证明必须拒 | `nc04_an_absent_fee_and_an_impossible_fee_are_both_refused`、`an_unattested_fee_prices_nothing_and_names_the_pool`、`an_unattested_fee_stops_the_search_and_names_the_pool` |
| NC5 insufficient liquidity | 池子付不起必须停 | `nc05_a_market_that_cannot_pay_is_stopped_before_any_price`、`nc05_an_ask_the_pool_cannot_pay_is_reverted_by_the_pool` |
| NC6 optimizer boundary | 越界必须夹住并说 | `nc06_the_search_domain_ends_at_the_route_ceiling_and_says_so`、`a_window_past_the_routes_ceiling_is_clipped_and_says_so`、`a_domain_that_leaves_256_bits_reports_its_refusals` |
| NC7 simulation revert | revert 必须传导到下游 | `nc07_a_reverted_run_is_refused_by_every_layer_after_the_evm`、`a_revert_keeps_the_words_it_came_with_and_no_profit` |
| NC8 min output failure | 下限不可达必须三处都拒 | `nc08_an_unreachable_floor_is_refused_before_and_inside_and_after`、`a_one_wei_under_plan_reverts_and_reports_no_profit`、`a_delivery_under_its_own_guard_is_refused_as_an_edited_record` |
| NC9 final profit failure | 利润门槛严格比较 | `nc09_the_profit_floor_is_strict_and_sits_on_the_gross`、`the_profit_floor_is_compared_strictly`、`no_failure_is_ever_written_as_profit_zero` |
| NC10 calldata mismatch | calldata 不许互绑 | `nc10_two_runs_of_one_market_do_not_bind_to_each_others_plan`、`the_calldata_hash_is_a_hash_of_the_requests_bytes` |
| NC11 plan hash mismatch | 一份运行一个计划哈希 | `nc11_the_same_run_under_two_freshness_declarations_is_two_plans_one_call`、`the_identity_is_stable_for_one_run_and_moves_for_every_fact` |
| NC12 nonce collision | 同一 (signer, nonce) 不能被占两次 | `nc12_a_committed_pair_cannot_be_taken_twice`（`refusal_code "nonce_held"`）+ 正向臂 `nc12_the_next_nonce_is_granted`（发出 nonce 1）；`a_pair_is_a_signer_and_a_number_not_a_number_alone`、`a_nonce_collision_costs_no_capital` |
| NC13 capital collision | 付不起的胜者必须拒且不留资源 | `nc13_a_winner_that_cannot_pay_is_refused`（`refusal_code "insufficient_capital"`）、`nc13_a_winner_that_cannot_pay_is_refused_and_keeps_nothing` |
| NC14 duplicate lane | 一哈希一 lane、一候选一 lane | `nc14_one_plan_hash_binds_one_lane`（`duplicate_plan`）、`nc14_one_candidate_gets_one_lane`（`duplicate_lane`）、`a_lane_cannot_reserve_twice`、`a_lane_appears_in_the_ranking_once` |
| NC15 expired plan | 过期计划在同一字节上判 stale，到期成对归还 | `nc15_a_plan_past_its_own_window_is_stale_at_the_same_bytes`、`nc15_expiry_returns_the_pair`、`nc15_expiry_from_reserved_returns_the_pair_and_the_capital` |
| 附加（§36） | 败者不得进入算术 | `nc36_a_losing_lane_is_turned_away_before_it_sees_the_arithmetic`（`lane_not_selected`） |

每个 refusal 都带唯一标签与章节引用（`every_refusal_carries_a_unique_label_and_a_section`）。

---

## 21. Determinism（§47）

相同 `GraphSnapshot` + `CycleCandidate` → 相同路线 / 报价 / 优化结果 / plan hash / calldata hash，至少连续两次。

- 跨加载：`two_loads_of_the_recording_search_price_and_optimise_to_one_answer`、
  `two_loads_of_the_recording_run_file_judge_and_plan_to_one_hash`（同一夹具两次独立加载比五个数）；
  `two_runs_of_the_declared_cycle_are_the_same_run`、`two_runs_of_the_recorded_route_are_the_same_run`。
- 敏感性对照（防止「恒定所以看似确定」）：`one_wei_less_stake_moves_the_quote_the_calldata_and_both_hashes`、
  `a_different_fee_moves_the_quote_while_an_absent_fee_stops_it`。
- 目录层：manifest `determinism = {assemblies_in_this_run: 2, byte_identical: true}`，
  口径写明「同一批行序列化两次并比较字节映射」，且门禁在另一个 crate 的进程里重算每行的算术。
- 跨进程比字节沿用 M3–M10 的既有要求（项目记忆：确定性要在产物层验证）。

---

## 22. `f64` / `f32` 扫描（§48）

命令逐字取自任务书：

```bash
rg "f64|f32" crates/opportunity crates/simulation crates/risk crates/execution
```

**现量 5 条命中，逐个解释，无一在 financial core**：

| # | 命中 | 判定 |
|---|---|---|
| 1 | `crates/simulation/src/gas.rs`：文档注释里写 `f64`，说的正是「不要用 f64 当最终成本」 | 注释，非使用 |
| 2 | `crates/execution/tests/fixtures/real-transactions-91342.json`：地址 `0x8807c62f64fb…` 里含子串 `f64` | 词面巧合 |
| 3 | `crates/execution/tests/multihop_evidence_gate.rs`：`Value::Number(number) if number.is_f64()` —— serde_json 的**判别式**，用来拒绝把整数写成浮点的发布值 | 反 f64 用途 |
| 4 | `crates/simulation/tests/multihop_capture.rs`：常量地址 `0xe1a9db8507570806ef64ff8d3583787c43f9eff0` 含 `f64` | 词面巧合 |
| 5 | `crates/simulation/tests/triangle_probe.rs`：同上，同一地址 | 词面巧合 |

命中数**包含自指**：本文件写了这条命令与 `f64` 字样，再跑同一命令会多命中本报告。
六个新生产文件对 `f64|f32` 的命中为 **0**（现量：`rg -c` 无输出）。

---

## 23. Secret Scan（§50）：private key literal = 0，且带双向对照

两道门，一条规则被抽成函数以便对照测试调到**真正在跑的那条**（审计底稿 §12）：

1. **源码侧**：`crates/cli/tests/no_execution.rs:190 no_private_key_is_written_into_the_code()`。
   谓词：token 去掉前导 `0` 后长度恰为 64 且全为十六进制 ⇒ 判定为密钥形状。
   范围：全部生产文件；执行 crate **整读含测试模块**（§40 要求那里用合成密钥签名，真密钥写进夹具就是用户的钱包）。
   其他 crate 的测试模块不读，因为仿真里一个期望 keccak 哈希同形状却不是秘密。
   反「空转」断言：必须打开 `crates/execution/tests/real_codec.rs`，否则报错。
   终态：workspace `no_execution.rs` target **5 passed / 0 failed**（在 §27 的那一跑里）。
2. **证据侧**（M11 新增，第 10 相位）：谓词 `is_secret_shape` = 长度恰 64、全十六进制、**且不含大写**，
   对 16 个 JSON 逐字符串走 `strings_under`。阳性对照在
   `crates/execution/tests/multihop_evidence_gate.rs:5083–5210`：
   - `fn planted() -> String { "1".repeat(64) }` —— 运行时构造，源码里绝不出现 64 个十六进制字符的字面量
     （否则正好触发第 1 道门）；
   - `manifest_with()` 只改内存里的解析树，**不落盘**，所以种子本身不可能进证据；
   - `tree_files()` 先断言 manifest 的 `tree` 确实发布了每一行文件（「扫了个空」的绿不算绿）；
   - 双向：种进去必须**在它被种下的路径上看到它**，且规则**明确看不见** `0x` + 64 位小写十六进制那种形状
     （`is_digest_shape`，len 66），因为本目录每个摘要都是那个写法 —— 把它也纳入等于用一个真密钥换一百个假证人。
   - 三个测试名：`a_planted_key_shape_is_seen_at_the_path_it_was_planted_at`、
     `the_digest_shape_it_rejects_is_the_digest_shape_the_directory_publishes`、
     `the_rule_is_the_shape_and_nothing_adjoining_it`，外加 `the_directory_holds_the_key_shape_under_neither_casing`。

本报告自身也遵守这条：所有摘要一律截断写法（`0xde854866…bcc4`），全文**没有** 64 位十六进制字面量。

---

## 24. Panic / unwrap 审计（§49）

**新代码**：6 个 M11 生产文件对 `panic!|unreachable!|unwrap\(\)|expect\(|todo!|unimplemented!` **0 命中**（`rg -c` 无输出）。

**全仓 production path**（扫描口径：`crates/*/src/**/*.rs`，排除 `*/src/**/tests.rs` 与 `tests/`，
并只读**第一处 `#[cfg(test)]` 之前**的内容；现量文件数 145）：

| 形态 | 生产路径命中 | 位置 | 是否新增 |
|---|---|---|---|
| `panic!` | 1 | `crates/metrics/src/trace.rs:317` | 否，且是文档注释里引用禁令 |
| `unreachable!` | 2 | `crates/pipeline/src/runner.rs:645`、`:779` | 否（M8 既有） |
| `unwrap()` | 6 | `runner.rs:378/381/384/385/386` + `execution/src/signer.rs:10`（文档注释） | 否 |
| `expect(` | 20 | 集中在 `opportunity/src/support.rs`（夹具构造）与 `pipeline/src/runner.rs`（序列化） | 否 |
| `todo!` / `unimplemented!` | 0 | — | — |

所以 **production panic hard = 0（新增 0）**。
不加 `#[cfg(test)]` 截断直接扫 src 会得到 `panic! 72 / unwrap() 104 / expect( 587` —— 那是测试模块的噪声，
两个口径都记在这里，是为了让下一个人能复现而不是重踩。
门禁侧硬 panic 的等价保护：`an_unseen_lane_is_refused_by_name_and_never_panics`、
`risk_reaches_no_network_and_no_key`、`a_rejection_carries_no_plan_to_execute`。

---

## 25. Cargo 三道关卡（§51，严格串行）

前置环境变量（项目记忆：抄已验证可用的那一份，不自己重写）：
`CC=clang CXX=clang++ CFLAGS/CXXFLAGS="-include cstdint"`。所有 test 均 `-- --test-threads=1`，
因为 `target/pipeline-tests/` 的 scratch 跨进程共享，并发会造出假 `os error 2` 与假统计。

| 关卡 | 命令 | 现量结果 |
|---|---|---|
| 1 | `cargo fmt --check` | 退出 0，stdout+stderr 合计 0 字节（本轮实测，命令见下方说明） |
| 2 | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | 退出 **0**，输出中 `^warning` 行数 **0** |
| 3 | `cargo test --workspace -- --test-threads=1` | **1611 passed / 0 failed / 29 ignored**，跨 123 个 result group（17 unittest + 90 integration + 16 doctest）；`3:25.82 total` |

三道关卡的**运行日志是过程产物，不入库**（留在 `target/` 下，仓库根 `.gitignore` 第 1 行 `/target` 让它本来就不进 git；
本轮在提交前删除）。表里的数字是当时从 stdout 逐字读出的，读者复现方式就是把这三条命令按上面的环境变量和
`--test-threads=1` 再跑一遍 —— 三份产物（夹具、证据树、代码）都在库里，重跑不需要任何外部条件。

两处口径说明，避免下一个人误读：
- clippy 的三次运行覆盖了提交中的字节，链条是这样的：11:28:50 那次是**全 workspace** 的
  （输出列出 16 个 crate 的 Checking + pipeline 的 Compiling，`^warning` 行 0，7m16s）；
  此后**只有 `crates/execution/tests/` 下的文件被改过**（`find crates -name '*.rs' -newermt '2026-10-09 11:28:50'`
  只命中 1 个文件），所以 12:38:35 那次增量重检 `evm-execution` 是正确的，0 warning、11.68s；
  13:17:56 那次 0.95s 全缓存、`clippy exit: 0`。`fmt --check` 是最后跑的（13:44:11），输出 0 字节。
  这条核对的意义：「exit 0」不是引用一轮旧缓存的判定（项目记忆：此风险不存在会随基线过期）。
- `grep -c "test result:"` 会给 137，其中 14 条是 `crates/simulation` 里一个**名字叫 `result` 的模块**的测试行
  （`test result::tests::…`），真组数 123；按「每个 target 头配一条结果行」配对后 grand total 仍是 1611。

回归覆盖：第 3 关是 workspace 级，M1–M10 的所有门禁 target 都在其中并通过
（含 `evidence_gate`、`pathfinder_evidence_gate`、`reconstruction_evidence_gate`、`executor_evidence_gate`、
`rpc_reduction_evidence`、`no_execution`）。

---

## 26. 生产 diff 审计（改了哪些既有文件，为什么）

`git diff --stat` 现量：**11 个 tracked 文件，+154 / −7**。

| 文件 | 变更 | 性质 |
|---|---|---|
| `Cargo.lock` | +2 | evm-pathfinder 两条新依赖边（由 §4 的 Cargo.toml 引起） |
| `crates/opportunity/Cargo.toml` | +1 | 依赖 evm-pathfinder |
| `crates/simulation/Cargo.toml` | +6 | 1 行生产依赖 `evm-opportunity`；5 行落在 dev-dependencies（4 行说明 + 1 条**生效中的** `evm-pathfinder`），让 §37 的端到端夹具向搜索层要输入，而不是手写一张边表 |
| `crates/opportunity/src/lib.rs` | +26 | 挂两个新模块 + re-export + 文档说明 M3 的三目搜索为什么不能外推 |
| `crates/opportunity/src/error.rs` | +76 | 新增 `RouteError` / `RouteResult`（按名字拒绝路线形状） |
| `crates/opportunity/src/detector.rs` | +8 | `RejectionReason::InvalidRoute(RouteError)` 一个变体 + 映射一行。二跳检测**永不产生**它（它点的路线超过两个池），加它是为了让 `OpportunityError` 的映射保持全函数 |
| `crates/risk/src/lib.rs` | +13 | 挂 `multihop` 模块 + 文档说明 M4 的 `RiskThresholds` 三规则原样保留、两个 policy 共享 crate 不共享类型 |
| `crates/simulation/src/lib.rs` | +4 | 挂 `multihop` 模块 |
| `crates/execution/src/lib.rs` | +17 | 挂 `lanes` 与 `multihop_plan` |
| `data/evidence/m8/m8.6/information_flow.json` | 3 行改动 | **锚点行号漂移**：`pub struct Opportunity` 因上面 detector 的 +6 行从 102 移到 108，M8.6 的门禁按 `file:token→line` 现量重发布 |
| `data/evidence/m10/manifest.json` | 1 行改动 | `git_commit` 由 `66b421d…` 重写为当前 HEAD `250cd26…`，由 M10 门禁 `executor_evidence_gate.rs:3327` 自己写入 |

两份跨里程碑证据的处理纪律（项目记忆：不得手改证据）：
两者都是**由对应里程碑的门禁在自己那跑里重算并写盘**的产物，M11 只负责提交它们并在这一节说明原因。
m8.6 那三行如果被人手工「复原」，M8.6 的 anchor gate 会立刻变红 —— 它禁止的正是「表重复了装配时的想法但代码已移动」。
`crates/simulation/Cargo.toml` 里那条 `evm-pathfinder` 是 **dev-dependency**，而且上面带了 4 行说明：
§37 把端到端链路的起点写成 `CycleCandidate`，所以夹具该向搜索层要输入，而不是手写一张边表；
本 crate 的库部分不触及拓扑层，pathfinder 也不链接任何传输，所以没有哪个测试因此多出一个本来不会发的 RPC。
这条之所以要单独说明：M9.3 栽过一次「跨里程碑证人引了前一里程碑的代码」的坑（当时的处理是撤回 dev-dep、
改用已提交产物），本轮保留 dev-dep 的理由是 pathfinder 属于**同一决策链的上游**而非前一里程碑的证据。
依赖名与注释字面量都会撞上跨里程碑门禁的词表扫描（项目记忆），所以相关门在 §25 那一跑里必须还是绿的：
`pathfinder_evidence_gate` 11 passed、`rpc_reduction_evidence` 17 passed、`no_execution` 5 passed。

**生产代码零改动的部分**（禁令核对）：`crates/pathfinder/`、`contracts/`、
`crates/execution/src/{signer,receipt,arbitrage,deploy,lifecycle,gate,preflight}.rs`、
`crates/live/`、`crates/metrics/`。

---

## 27. 判定：§53 六项 + §54 七前置

**§53 完成判据**

| 判据 | 结论 | 证据 |
|---|---|---|
| M11.1 Architecture Integration | ✅ | §4（pathfinder 已在 workspace；依赖 +2 行；零新 crate；§26 生产 diff） |
| M11.2 Multi-Hop Pricing | ✅ | §5、§6 + `pricing/` 三行（2/3/4 跳） |
| M11.3 Amount Optimization | ✅ | §7 + `optimizer/` 五行（含 brute-force 对照与 `covered_the_route_domain false`） |
| M11.4 REVM + Risk | ✅ | §8、§9 + `simulation/` 两行、`risk/` 三行 |
| M11.5 Multi-Lane | ✅ | §11 + `lanes/lane_matrix.json`（43 步、7 控制臂 + 1 正向臂、winner-only 占用） |
| M11.6 End-to-End | ✅ | §12–§13 + `controlled/{2hop,3hop}/chain.json`（含 3 跳原子回滚行） |

**§54 例外条款的七个前置**（没有真实套利也必须齐的那七项）

| 前置 | 结论 | 指到哪个产物 |
|---|---|---|
| controlled 2-hop = PASS | ✅ | `controlled/2hop/chain.json` 七阶段 + `the_contract_answered.status "delivered"` |
| controlled 3-hop = PASS | ✅ | `controlled/3hop/chain.json` 成功行，`reserve_rows_moved 3` |
| REVM = PASS | ✅ | `simulation/` 两行由门禁**重跑** REVM 复算（§17） |
| Risk = PASS | ✅ | `risk/decision.json` 三行，11 条检查全走（`all_eleven_lines_reach_an_answer`） |
| Executor = PASS | ✅ | M10 合约真字节码执行；`the_request_is_byte_for_byte_the_m10_request` |
| Lane = PASS | ✅ | `lanes/lane_matrix.json`；`multihop_lanes.rs` 35 passed |
| real attempt = documented UNKNOWN | ✅ | `real/` 三行四字段 + `why_not_zero`；manifest `verdict "UNKNOWN"` |

**判定：`M11 = COMPLETE`。**
**`REAL_PROFITABLE_ARBITRAGE = UNKNOWN`**（§41；它不阻塞完成，与 M10 的验收哲学一致）。

---

## 28. 已判死 / 本轮做不到（下游别重复踩）

manifest `not_measured` 三条，逐条给出现量根据：

1. **四跳路线从未进过 EVM。** M9.3 搜索层硬顶是 3 跳
   （`PathFinderConfig::MAX_MAX_HOPS`，在 `CycleCandidate::assemble` 里执行，审计底稿 §3）。
   下游确实有能接四臂的代码（`multihop_adapter.rs` 造四腿请求、`multihop_risk.rs` 记一条四臂判定），
   但替代测量只有手写图上的 `pricing/declared_4hop.json`。
2. **真链上不存在 M10 能驱动的三跳环。** 录制 triangle 的三个池运行时字节码里没有 swap 派发分支。
   取证在 `crates/simulation/tests/triangle_probe.rs`（3 passed + 9 ignored 测量臂）与
   `fixtures/simulation-m11/probe-37224031-*.json` 九个探针产物，其中
   `probe-37224031-entry-points.json`（21 620 字节）发布 `PUSH4 <selector>` 站点计数，
   `probe-37224031-three-leg-slots.json` 发布三腿槽位。结论：**3 跳端到端只能在声明图上跑**。
3. **M11 计划没有真实执行 / 真实失败 / 真实对账。** `real/` 三行因此恒为 UNKNOWN；
   真实机制的证据在 M10（`data/evidence/m10/real/`），M11 只指向它、不重述它的数值。

其他本轮明确没做的事：

- 没有真市场盈利数字（§14 的 Realized 层为空，且跨单位换算无预言机）。
- Risk 的 `maximum_gas` 等于本次运行自己测出的烧量，**是描述不是门槛**；换成市场阈值需要 live 侧授权。
- `multihop_capture.rs`（唯一会花 RPC 的文件）保持 `#[ignore]`；跑它 = 一次 live 抓取，属 M12 之后的决定。
- Lane 只做决策层占用，没有接真实发送队列（§28 边界要求它接不上，且这是设计而非缺陷）。
- 金额优化不宣称全域最优（五行 `covered_the_route_domain` 全 false）。

---

## 29. UNKNOWN / N/A 汇总（谁读这份报告都不该去找一个不存在的数）

| 问 | 答案 | 出处 |
|---|---|---|
| 真市场上这套 M11 链条能赚多少？ | `UNKNOWN`（不是 0） | `real/` 三行 + manifest `verdict` |
| 真实执行过 M11 计划吗？ | 没有；机制证据在 M10 | `real/execution.json` 的 `evidence_that_does_exist_for_the_mechanism` |
| 有真实回执 / tx hash / 签名 / 私钥吗？ | 一个都没有，且被列为 `not_claimed` | manifest `not_claimed[2]` |
| 4 跳在 EVM 上跑过吗？ | `UNKNOWN`（只到声明图定价） | manifest `not_measured[0]` |
| 真实三跳环存在吗？ | 已判死（运行时字节码无 swap 派发） | §28 第 2 条 + 9 个 probe 产物 |
| 净利润（gas 与代币相减）？ | `NotComputable`（无同单位尺子，两个数不同单位） | §14 |
| 优化器找到全域最优了吗？ | 没有宣称（`covered_the_route_domain false` ×5） | §7 |
| Risk 那 11 条里有「不知道」吗？ | 有，且它和 reject 保持两个答案 | `unknown_and_reject_stay_two_answers_about_the_same_check` |
| 本轮新增 RPC？ | `0`（三条口径同 §18） | manifest `rpc_count` |

---

## 30. 四笔提交记录（§52）

| Commit | 消息（任务书原文） | 包含 |
|---|---|---|
| 1 | `feat(m11): add multi-hop pricing and optimization` | 全部**生产**改动：`crates/opportunity/{Cargo.toml,src/multihop.rs,src/multi_optimizer.rs,src/error.rs,src/detector.rs,src/lib.rs}`、`crates/simulation/{Cargo.toml,src/multihop.rs,src/lib.rs}`、`crates/risk/{src/multihop.rs,src/lib.rs}`、`crates/execution/{src/lanes.rs,src/multihop_plan.rs,src/lib.rs}`、`Cargo.lock` |
| 2 | `test(m11): add multi-hop simulation and execution integration` | 全部**测试**改动，REVM / 3-hop / Risk / ExecutablePlan / Lane / Nonce / Capital：`crates/opportunity/tests/{multihop.rs,multi_optimizer.rs}`、`crates/simulation/tests/`（`multihop_adapter|capture|determinism|e2e|evidence|negative_controls|revm|risk.rs`、`triangle_probe.rs`、三个共享模块目录 `multihop_market|recorded|state/mod.rs`）、`crates/execution/tests/{multihop_lanes.rs,multihop_evidence_gate.rs}` |
| 3 | `evidence(m11): add multi-hop execution evidence` | controlled / real / reconciliation / manifest：`data/evidence/m11/`（16 个文件 = README + 15 JSON）、`fixtures/simulation-m11/`（13 个文件）、被门禁重算刷新的 `data/evidence/m10/manifest.json` 与 `data/evidence/m8/m8.6/information_flow.json`（原因见 §26） |
| 4 | `docs(m11): add multi-hop execution completion report` | 本报告 + `docs/v0.1/M11 Coding.md`（任务书首次入库）+ `docs/v0.1/M11 Semantic Audit.md` |

两处切分口径，写清楚免得下一个人以为是漏了：
- 切的是**层级**（生产 / 测试 / 证据 / 文档），不是 crate，所以 `crates/risk/` 的库和它的测试分在 1、2 两笔；
  这与 M10 的四笔切法一致（M10 第 1 笔带了 4 个 crate 的生产码，第 2 笔集中 7 个测试 target）。
- 暂存以**整文件**为单位，不用 `git add -p` 拆文件，于是两处必然跨笔：
  `Cargo.lock` 的两行 pathfinder 边（1 行 opportunity 生产、1 行 simulation 的 dev-dep）都在第 1 笔，
  `crates/simulation/Cargo.toml` 的 6 行（1 行生产 + 5 行 dev-dep）也整体在第 1 笔。
  代价是第 1 笔里提前存在一条尚无使用者的 dev-dependency，好处是每一笔都能独立 `cargo check`，
  且不会出现「lock 与 toml 分家」这种让下一个 checkout 直接编不过的状态。

四笔之间不夹带无关改动；`target/m11-scratch/`（本轮 gate 跑、篡改对照、抽取脚本）在提交前删除，
它是过程产物而非证据（真正的对照数值已经誊进 §17、§23–§25，读者用 §16 的两条命令即可复现）。

---

## 31. 收尾：M11 之后系统真正变成了什么（§55–§56）

M10 之前：发现一个环 + 证明一条路线能原子执行。
M11 之后第一次闭合的环：

```text
M9.3   找到一个环
  ↓
M11    该投多少钱（有界搜索 + 诚实的终止原因）
  ↓
M11    用真实 EVM 在钉住的状态上跑一遍（真字节码、逐 wei 对照）
  ↓
M11    值不值得动手（11 条具名检查 + accept/reject/unknown 三态）
  ↓
M11    形成与 M10 哈希一致的可执行计划
  ↓
M11    多候选并行、只有胜者占 nonce 与资本
  ↓
M10    原子执行
  ↓
M10    确认链上结果
```

这就是 **Arbitrage Decision Loop**。它现在的边界也很清楚：循环里每一环都能自证，
但**最后一环（真的把计划发到链上、并拿回执回来对账）用的仍是 M10 在 2 跳上的一次真实执行**，
而「真市场现在有没有这个环」这个问题，M11 的答案是 `UNKNOWN`，不是 `没有`。

下一阶段（§56）：

```text
M12  Self-hosted GIWA Node + Low-Latency Infrastructure
M13  Production Operations
M14  Production Validation
```

也就是说 M11 是**策略层闭环**；M12 起才进入「把这条闭环放到能抢时间的物理位置上」。
