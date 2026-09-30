# M3 Completion Report

结论（白话版）：M3 已经跑通，状态 **COMPLETE**（任务书 §65 的 A–Q 十五条全部实测通过）。
这一轮把 M2 的市场图第一次变成了「有没有套利」的答案：

> 真实区块 → 真实日志 → 真实池子状态 → 真实市场图 → 两池候选 → 精确 AMM 数学 → 最优输入
> → gross profit → Opportunity

在 GIWA Sepolia（chain 91342）的 **block 37 191 169** 上，同一对代币（WETH / TTAX）上真实存在
**两个不同的池子**（由两个不同 Factory 创建，字节码逐字节相同）。这两个池子对同一笔交易给出的
价格相差 **9.12 %**，而往返手续费只吃掉 **0.60 %**，所以这个差价是真的存在：图里的 4 条有向路线
中 **2 条 gross_profit > 0**，另外 2 条（反向）亏损。两条机会都能一路追到具体的 tx、log index、
储备量和被证明的手续费，且同样的输入跑两遍字节级相同。

三件必须说清楚、不能被这份「找到机会」的结论盖过去的事：

1. **gross profit 不是可执行的钱。** 没有 gas、没有交易模拟、没有执行、没有贿赂、没有滑点。
   这里的「有利可图」只意味着「两个池子的报价差得比它们各自的手续费多」。
2. **真实历史里被扫出来的 42 条利润路由，M3 只承认 2 条。** 因为只有这一对池子的手续费是被
   **它们自己的成交记录夹逼证明**出来的；其余 40 条是筛查，用的是「假定 997/1000」。这不是
   保守：把 fee 换成 998/1000，会有 **10 条路由（5 组）凭空多出来**，同一批筛查里又有 10 条
   消失 —— 未举证的 fee 一旦进价格公式，报告就会造出一整个不存在的世界。
3. **这些池子是测试网的种子流动性。** 两个池子的初始储备都是 40 000 000 000 000 WETH wei
   （= 0.00004 WETH）与 40 TTAX；本次报告的利润折合 **0.0000000296 WETH** 与 **0.0369 TTAX**。
   它证明的是**管线**，不是**钱**。

任务书要求的事实值（十进制原始整数，来自链上数据，非估算；两个代币 `decimals()` 实测都是 18）：

| 项 | 值 | 换算 / 说明 |
| --- | --- | --- |
| chain / block | 91342 / 37191169 | `0x2377e01`，区块哈希 `0x56c628d7…48d670`，时间戳 1790536285 |
| 真实图 | 2 个池子、4 条有向边、4 个候选 | 快照绑定 block 37191169 |
| 候选结果 | 2 条机会 + 2 条亏损拒绝 | 另有 4 个「同一池子绕两次」的边对被跳过 |
| pool A | 0xf487d533cae6cddd0c7e7bbbac084dd04d876578 | WETH/TTAX，Factory 0x5f6e8a56…，创建于 37187524 |
| ├ reserve0 / reserve1 | 35099900253008 / 45655538604883371699 | 0.0000351 WETH / 45.656 TTAX（`Sync`，log index 29） |
| └ fee | 997/1000 | 由 4 笔它自己的成交夹逼证明 |
| pool B | 0x5bef6275607901dcd58160356660151be0637440 | 同一对代币，Factory 0x1e594a50…，创建于 37187526 |
| ├ reserve0 / reserve1 | 36641298079327 / 43677608078641141054 | 0.0000366 WETH / 43.678 TTAX（`Sync`，log index 34） |
| └ fee | 997/1000 | 由 1 笔它自己的成交夹逼证明 |
| 机会 1（WETH 端） | 输入 714844720992 → 输出 744486240802 | gross_profit **29641519810**（+4.15 %） |
| 机会 2（TTAX 端） | 输入 890134426448791298 → 输出 927044426434210645 | gross_profit **36909999985419347**（+4.15 %） |
| 价格乘积 | 1.0911880271733878 | 阈值 (1000/997)² = 1.0060271084064631，超出 8.47 % |
| 全链普查 | 1023 个池子 / 972 个币对 / 100 个 Factory | 由 `PairCreated` 日志全量恢复，0 异常 |
| 同对多池 | 50 个币对上有 ≥2 个池子（共 101 个池子，最多 3 个） | 本次 Sync 扫描覆盖其中 37 对 / 74 个池子 |
| 筛查结果 | 200 条有向两池路由 → 42 条在 997/1000 下为正 | 分布在 21 个区块 / 21 个币对 |
| 关卡 | fmt / check / test / clippy 全部 exit 0 | 23 个测试套件，**161 通过 / 0 失败 / 3 忽略** |

---

## 1. Status

**COMPLETE。**

判定依据是 §65 的 A–Q 十五条逐条实测（见第 12 节的对照表），且四道关卡在整个 workspace 上实跑
通过。「找到真实机会」不是 COMPLETE 的前提条件（§48 明确允许报告 null 结果），所以这个状态并不
依赖第 9 节那条机会成立 —— 即使把那两条机会拿掉，A–Q 仍然全过。

## 2. Implemented Components

M3 只新增一个 crate，没有改动 M1/M2 的任何一行实现代码（`git diff --stat` 显示只有
`Cargo.toml` +3 行、`Cargo.lock` +17 行）。

`crates/opportunity/`（`evm-opportunity`，源码 1 999 行 / 7 个文件，测试 3 382 行 / 6 个文件）：

- **Path —— `src/path.rs`（328 行）**
  `ArbitragePath`（字段私有，只能通过 `two_hops(first, second)` 构造）、`Hop { pool, token_in,
  token_out }`、`PathSimulation { path, input, output }`。构造函数按顺序检查三件事：两跳不是同一
  个池子（`PathError::SamePool`）、中间代币接得上（`TokenMismatch`）、路线回到起点代币
  （`NotACycle`）；因此「路径合法」这件事在类型层面就是**由构造保证**的，后面的定价不必再防。
  `identity()` / `pools()` / `first()` / `second()` / `input_token()` / `mid_token()` 提供排序与
  审计需要的全部可读信息。`gross_profit()` 用 `checked_sub`：输出不大于输入时返回 `None`，
  绝不返回被 clamp 成 0 的假利润；`compare_profit()` 用**交叉相加**而不是减法比较两个路线，
  避免下溢。
- **AMM Math —— `src/math.rs`（218 行）**
  `swap_exact_in(reserve_in, reserve_out, fee, amount_in) -> U256` 与
  `swap_through_two_hops(...)`。全部 `U256` 整数、每一步 `checked_mul` / `checked_add`，
  失败返回 `MathError`，没有任何一处 `unwrap`。
- **Optimizer —— `src/optimizer.rs`（581 行）**
  `PricedHop`（从图边构造，**没有已举证 fee 就直接拒绝**，见 §5）、`PricedCycle`、
  `SearchPolicy { max_rounds, scan_width }`、`SearchStrategy::BoundedTernary`、
  `SearchRecord { strategy, rounds, evaluations, lower_bound, upper_bound, interval_closed,
  scan_count, policy }`、`find_optimal_input()`、`OptimizedCycle`。
- **Detector —— `src/detector.rs`（616 行）**
  `enumerate_candidates()`、`OpportunityDetector`、`detect_opportunities()`（lib.rs 的一次调用
  形式）、`Detection { chain_id, block_number, candidates, opportunities, rejected,
  skipped_pairs, policy }`、`Opportunity`（携带 `hops: [PricedHop; 2]` 与 `search: SearchRecord`，
  让结论自带市场数据）、`CandidateRejection { path, reason, peak }`（亏损路线**保留峰值**）、
  `SkippedPair`、`RejectionReason`（9 个变体）。
- **错误面 —— `src/error.rs`（78 行）**：`MathError`（4）、`PathError`（4）、
  `OpportunityError`（6，含 `UnattestedFee(PoolId, TokenId)`、`EmptySearchDomain { lower, upper }`）。
- **测试专用 —— `src/support.rs`（84 行，`#[cfg(test)]`）**：fixture 图也走「注册池子 → 写入状态
  → 投影成图」这条真实管线，所以 fixture 造不出真实管线 produce 不出的形状。
- 依赖：`evm-core`、`evm-graph` + `alloy-primitives`、`serde`、`thiserror`；`evm-chain`、
  `evm-protocol`、`evm-replay`、`evm-state`、`tokio` 只在 `[dev-dependencies]`（真实验收测试用），
  所以**定价这一层在类型上就碰不到 RPC 与状态存储**。

真实证据（新增，非代码）：

- `data/protocols-m3/v2-sepolia-42000006-cffe7472.json`（20 140 B）：两个池子的举证文件，
  每条结论都挂 `block_number / log_index / transaction_hash / source / signature`。
- `fixtures/real-m3/block-37191169.json`（49 753 B）：整块区块（header + 35 笔交易 + 35 份回执
  + 54 条日志），由归一化后的 `HttpChainAdapter::get_block_data` 落盘，无手工转录。

## 3. Mathematical Model

**公式**（`src/math.rs`，与 Uniswap V2 的 `_getAmountOut` 同形，用「保留比例」写成通用形式）：

```text
x_with_fee = amount_in × fee.numerator
out        = floor( x_with_fee × reserve_out
                    / (reserve_in × fee.denominator + x_with_fee) )
```

- **fee** 是**保留比例** `Fee { numerator, denominator }`（0.3 % 即 997/1000），来自池子的举证，
  而不是代码里的常量。校验：`denominator == 0` 或 `numerator > denominator` 直接
  `MathError::InvalidFee` —— 保留超过全额意味着报价高于无手续费曲线，任何已举证池子都不是这样。
- **整数算术**：全程 `U256`，除法向下取整（与链上一致），**没有一处 f64/f32**
  （实测：`crates/opportunity/src/` 生产代码里 f64/f32 出现 0 次）。
- **两跳复合**：`swap_through_two_hops` 先算第一跳，若中间量取整为 0 则整条路线直接输出 0
  （这是真实语义：小到买不起 1 个中间代币的输入确实拿不回任何东西），否则把中间量作为第二跳输入。
- **gross profit**：`output − input`，用 `checked_sub`，亏损与打平都得到 `None`，因此「利润」
  这个词只会在真的为正时出现。
- **溢出处理**：`amount_in × numerator`、`× reserve_out`、`reserve_in × denominator`、分母相加
  四处全部 `checked_*`，任何一步溢出就返回 `MathError::Overflow`，由 detector 记为
  `RejectionReason::Overflow`；生产代码里 `unwrap` / `expect` / `panic!` / `assert` 出现 0 次
  （`support.rs` 是 `#[cfg(test)]` 模块，不计入）。
- **可判定条件**：两池往返为正的充要条件是两侧价格之积 > `(d/n)²`。本例
  `(A_R1/A_R0) × (B_R0/B_R1) = 1.0911880271733878 > 1.0060271084064631`，反向之积是它的倒数
  `0.9164323426370419`，所以恰好两个方向为正、两个方向为负 —— 与检测结果一致。
- **未建模**：gas、执行、bundle、贿赂、滑点、代币税、价格冲击之外的任何成本（§41 明确
  gross profit ≠ executable profit）。

## 4. Candidate Enumeration

- **生成**：`enumerate_candidates(GraphSnapshot)` / `scan()` 在图上做三层嵌套遍历
  `token_in ∈ nodes` → `token_mid ∈ neighbors(token_in)` → `first ∈ routes(token_in, token_mid)`
  × `second ∈ routes(token_mid, token_in)`，每对边交给 `ArbitragePath::two_hops()`。
  遍历顺序由 `BTreeSet` / `BTreeMap` 决定，同一张图永远同一顺序。
- **去重**：候选集合本身是 `BTreeSet<ArbitragePath>`，路线身份（两跳的 `EdgeId`）就是键，
  所以同一条路由不同遍历路径命中两次也只会写进去一次。fixture 8 用 3 个池子验证：
  **12 条有向路线 = 6 个有序池对 × 2 个起点代币，`BTreeSet` 去重后仍是 12 条**（每条恰好一次），
  且 12 条各自的 `pools()` 都不重复。
- **拒绝同一池子绕两次**：`two_hops()` 的第一个判断是 `first.pool == second.pool` ⇒
  `PathError::SamePool(pool, pool)`，这类边对**不进候选集合**，而是进 `Detection::skipped_pairs`
  （带两侧 `Hop` 与 `PathError`），所以「为什么没有」也是可以被审计的输出而不是沉默。
  只有 1 个池子的图 ⇒ 候选 0 条、`skipped_pairs` 2 条；真实两池图 ⇒ 候选 4 条、`skipped_pairs` 4 条。
- **定价阶段的拒绝**：`MissingFee(PoolId)` / `InvalidReserve` / `InvalidFee` / `InvalidAmount` /
  `Overflow` / `InvalidPath(PathError)` / `MissingHop(PoolId)` / `EmptyDomain` / `Unprofitable`
  共 9 种，都作为 `CandidateRejection` 保留；其中 `Unprofitable` 与 `EmptyDomain` 之外没有 peak。
- **同一币对多池子**：全部保留（上面 12 条就是 3 池场景），不做「只取最深池子」这类简化。

## 5. Optimal Input

- **搜索域是推导出来的，不是调出来的**：`PricedCycle::input_upper_bound() =
  second.reserve_out − 1`，下界恒为 `1`。理由是第二跳要付回的代币最多就是它持有的那么多；
  若 exit 池只持有 ≤1 单位，域为空，报 `OpportunityError::EmptySearchDomain{lower, upper}`
  （而不是 0 利润）。真实两条路线的域上界分别是 `36 641 298 079 326`（= B 的 WETH 储备 − 1）
  与 `45 655 538 604 883 371 698`（= A 的 TTAX 储备 − 1）。
- **算法**：离散三分搜索（`SearchStrategy::BoundedTernary`）用于单峰整数利润序列，收窄到
  `scan_width` 以内后进入**收尾比较**：窗口内每个输入逐一询价，取平局时选最小输入。平局时
  区间**两端同时收缩**——被取整的利润曲线是阶梯而不是光滑曲线，平局往往意味着整段平台，
  单边收缩会把平台砍掉。
- **迭代预算与精度（全部公开，无隐藏常量）**：`SearchPolicy::default() =
  { max_rounds: 256, scan_width: 32 }`。256 的理由写在类型文档里：`Sync` 把储备编码成
  `uint112`，每轮把区间乘 2/3，`112 × ln2 / ln(3/2) ≈ 192` 轮就能压到 `scan_width` 以下。
  每条机会都自带 `SearchRecord`（用了多少轮、询价多少次、区间上下界、收尾比较了几个输入、
  **区间是否真的闭合**、当次 policy），预算没跑完就说「闭合了」是撒谎，所以 `interval_closed`
  是一个被测试盯住的事实：饿死预算（`max_rounds: 8`）时它必须是 `false`，且报告仍然不许超过
  穷举上界。
- **真实搜索的实测预算使用**（由 `the_real_search_closes_its_interval_inside_the_published_budget`
  钉住，policy 就是 `SearchPolicy::default()`）：

  | 路线 | 域 | 轮数 | 询价次数 | 收尾比较 | 闭合 |
  | --- | --- | --- | --- | --- | --- |
  | WETH → A → TTAX → B → WETH | `1 ..= 36641298079326` | 50 | 130 | 30 | 是 |
  | TTAX → B → WETH → A → TTAX | `1 ..= 45655538604883371698` | 104 | 232 | 24 | 是 |

- **平台宽度**：WETH 路线在 `714 844 520 992 ..= 714 845 020 992`（500 001 个输入）上都付
  29 641 519 810，所以**利润是事实，具体输入只是当次 tie-break 的产物**；报告里两者都写。
- **最优性证明**：对未取整的复合函数求闭式极大值，
  `P = n²·Ro1·Ro2`、`Q = d²·Ri1·Ri2`、`S = d·n·Ri2 + n²·Ro1`，
  `x* = (√(PQ) − Q)/S`，`max = (P + Q − 2√(PQ))/S`。`P·Q` 会溢出 256 位，但
  `√(PQ) = n·d·√(Ro1·Ro2·Ri1·Ri2)`，根号内 ≈ 2.5e66 < 2^256，一次整数开方即可。
  由于每一跳都向下取整，任意整数输入的离散利润 ≤ 同一输入的光滑值 ≤ 光滑极大，
  因此「报告利润 = ⌊光滑极大⌋」就意味着**它是整个定义域上的整数全局最优**。
  WETH 路线实测相等（两边都是 29 641 519 810）；TTAX 路线报告 36 909 999 985 419 347，
  其闭式天花板 36 909 999 985 422 472，**差 3 125**（阶梯取整的代价，实测值，不是宣称）。

## 6. Test Results

工具链 1.96.1（`rust-toolchain.toml` 固定），构建环境 `CC=clang CXX=clang++`，依赖离线
（`--offline`）。四条命令都在**整个 workspace** 上实跑（不是只跑新 crate）：

```bash
cargo +1.96.1 fmt --check                                          # exit 0（无任何 diff 输出）
cargo +1.96.1 check --offline --workspace --all-targets            # exit 0
cargo +1.96.1 test  --offline --workspace                          # exit 0
cargo +1.96.1 clippy --offline --all-targets --all-features -- -D warnings   # exit 0
```

`cargo test --workspace` 实测：**23 个测试套件，161 通过，0 失败，3 忽略**（3 个忽略项是 M1/M2
留下的需要在线 RPC 的录制用例）。M3 新增的 79 个用例分布：

| 套件 | 用例 | 覆盖 |
| --- | --- | --- |
| `src` 单元测试（math 9 / optimizer 10 / path 9 / detector 6 / lib 1） | 35 | 公式、取整、溢出、域、拒绝归因、排序 |
| `tests/fixtures.rs` | 12 | 任务书 fixture 1–8 + 零 fee 对照组 + 未举证 fee + 宽对双向穷举 |
| `tests/error_matrix.rs` | 10 | 任务书点名的 10 种错误条件，逐个找到「可达的那扇门」 |
| `tests/determinism.rs` | 7 | 同图两次定价 = 同一份报告、注册顺序无关、镜像平局、排序规则、序列化字节稳定、审计行一致 |
| `tests/optimizer_cross_check.rs` | 3 | 独立穷举扫描 vs 有界搜索（第 7 节） |
| `tests/real_data.rs` | 12 | 真实数据验收（第 8、9 节） |

§45 要求的 14 项测试矩阵逐条落点：

| §45 项 | 用例 |
| --- | --- |
| single-hop math | `amount_out_floors_down_to_the_last_whole_unit`、`a_deeper_outside_pays_more_and_price_impact_is_not_linear`、`an_input_too_small_to_buy_one_unit_comes_back_as_zero_not_an_error` |
| two-hop math | `two_hops_chain_the_same_way_by_hand` |
| no arbitrage | `fixture_1_equal_prices_produce_no_opportunity` |
| profitable arbitrage | `fixture_2_obvious_arbitrage_reports_both_ends_of_the_cycle`、`fixture_4_the_profit_curve_has_an_interior_peak_and_the_search_finds_it`、`tests/real_data.rs` 全部 |
| fee erases profit | `fixture_3_a_real_spread_smaller_than_the_fees_is_not_an_opportunity` + 对照组 `a_zero_fee_market_makes_fixture_3_pay_what_the_fee_took`（同一市场把 fee 设成 1/1 就会付钱 ⇒ 证明是 fee 吃掉的） |
| zero reserve | `fixture_5_an_empty_side_is_refused_at_every_door`、`a_zero_reserve_is_refused_before_it_can_be_priced`、`an_empty_side_is_no_market` |
| zero input | `a_zero_input_buys_nothing_and_is_refused` |
| overflow | `huge_reserves_overflow_to_an_error_instead_of_panicking`、`the_overflow_refusal_reaches_the_report_as_a_reason`、`fixture_6_112_bit_reserves_price_without_overflowing` |
| same pool | `fixture_7_a_round_trip_through_one_pool_is_never_a_candidate`、`one_pool_round_trip_is_refused_by_name`、`one_pool_round_trip_never_reaches_the_optimizer` |
| token mismatch | `hops_that_do_not_join_at_the_middle_token_are_refused`、`a_route_that_does_not_close_is_refused` |
| multiple pools | `fixture_8_three_pools_on_one_pair_give_twelve_directed_routes`、真实两池 |
| optimal input | `the_derived_upper_bound_contains_the_peak_and_brute_force_agrees`、`the_search_beats_the_closed_form_instead_of_only_matching_it` |
| determinism | `determinism.rs` 7 个 + `identical_inputs_produce_identical_search_results` |
| ordering | `opportunities_come_out_profit_first_and_route_identity_breaks_the_tie`、`rejections_and_skips_have_their_own_documented_order`、`results_are_ordered_by_profit_then_route_and_reproduce_exactly` |

## 7. Mathematical Cross-check

要求（§46）是「优化器不能自己验证自己」。这里做了两层，都用**另一条代码路径**：

1. **网格穷举交叉验证**（`tests/optimizer_cross_check.rs`，独立实现的 `u128` 连续输入穷举扫描，
   与 `evm-opportunity` 的有界搜索无关）。`cargo test -p evm-opportunity --test
   optimizer_cross_check -- --nocapture` 实测打印：

   ```text
   grid: 2401 routes, 1076 with a positive peak; search tied the exhaustive maximum on
   2352, fell short on 49 (worst shortfall 14 at Route { reserve_in_first: 1201,
   reserve_out_first: 29, reserve_in_second: 40, reserve_out_second: 1201 }, whose top
   held 1 input(s); 0 misses outside their route's floor envelope;
   [(Route { 250, 61, 250, 1201 }, 1)] routes where the scan profits and the search
   reports none
   ```

   两件事被硬断言：**搜索绝不会报告高于全域穷举的利润**（2 401 条路由逐条检查，一次都没有），
   以及**每一次不足都在该路线自己的「取整包络」之内**。包络不是常数，是从储备量推出来的：
   `1 + second.reserve_out / second.reserve_in`（一跳取整丢掉的不足 1 单位，按第二跳的边际价格
   折算成输出代币）。0 条超出包络。唯一「穷举为正而搜索报无利润」的路由是
   `(250, 61, 250, 1201)`，其穷举峰值只有 1 个单位、由单个输入达到 —— 这是安全方向的错
   （路线仍作为拒绝连同 peak 留在报告里），但被 `assert_eq!` 钉死成事实，将来变了会响。

2. **闭式天花板**（第 5 节末）：对两条真实路线，用未取整复合函数的解析极大值独立复算。
   WETH 路线报告利润与天花板**逐位相等** ⇒ 它是全局整数最优，而不是「预算内找到的最好值」。

另外还有一条第三方验证：调查阶段用 Python 写的独立定价（不 import 任何 Rust 代码）在同一市场
上给出 WETH 路线 **29 641 519 810**（与 Rust 完全一致）、TTAX 路线 36 909 999 985 125 083
（比 Rust 少 294 264 —— 那个脚本只在解析峰附近做局部扫描，Rust 的收尾比较找到了更高的台阶）。

真实市场上还有第三种验证：`an_independent_scan_does_not_beat_either_reported_peak` 在测试里
自己做 100 000 点全域扫描 + 峰值附近逐单位窗口扫描，断言报告利润不低于扫描结果，并用测试自己的
取整公式复算报告的输入/输出对。

## 8. Real-data Evidence

### 8.1 被搜索的范围（历史普查，RPC 全量）

| 层次 | 实测 |
| --- | --- |
| 全链池子普查 | 7 480 个区块回执文件里的 `PairCreated` 日志 ⇒ **1 023 个池子 / 972 个币对 / 100 个 Factory，0 条异常** |
| 同对多池 | **50 个币对有 ≥2 个池子（共 101 个池子，单对最多 3 个）** |
| Swap 活跃度 | 3 740 个日志文件里 **946 372 条 `Swap`**，来自 200 个发射者；101 个候选池子里**只有 44 个真的成交过** |
| Sync 扫描覆盖 | 候选池子里的 **74 个（37 个币对）**被逐块 `eth_getLogs` 扫过（topic0 `0x1c411e9a…fbbad1`），命中 **223 条 `Sync`**，落在 **37 187 516 .. 37 224 031 的 69 个区块**上；这 74 个池子的创建区块是 37 187 504 .. 37 223 488 |
| 未覆盖部分（如实说明） | 另外 **27 个池子 / 13 个币对**（创建区块 4 058 192 .. 31 721 667，都是更早的老池子）**没有进入 Sync 扫描**，所以本次「真实历史搜索」并没有覆盖全部 50 个多池币对 |
| 候选枚举 | 同一区块内 ≥2 个池子同步的 (区块, 币对) 组 **50 个** ⇒ **200 条有向两池路由**全部定价 |
| 筛查结果 | 按 997/1000 计价有 **42 条为正**（21 个区块 / 21 个币对）；利润区间 **510 614 680 .. 155 141 086 515 621 165** |
| 流动性 | 这 42 条里**没有任何一条**的最小储备达到 1e18 wei（1 个 18 位小数的代币单位）；4 条最大利润（155 141 086 515 621 165，价格乘积恰好 1.2）来自四个不同币对上形态完全相同的种子流动性（40 000 000 000 000 000 000 / 48 000 000 000 000） |
| fee 敏感性 | 同一批路由按 998/1000 计价有 **52 条为正**，其中 **10 条（5 组）只在错误 fee 下存在** —— 这就是 M3 不承认这 40 条的根本原因 |

### 8.2 被提交的真实验收（可复现，`cargo test` 就能跑）

- **搜索区块 / 快照**：`fixtures/real-m3/block-37191169.json` —— chain 91342、number 37191169
  （`0x2377e01`）、hash `0x56c628d7102131d35d5bdf6a0ed2c6ec3d4d00edc4419a20641c7b396c48d670`、
  parent `0x4737ebf7…a069d6`、timestamp 1790536285、**35 笔交易 / 35 份回执 / 54 条日志**，
  `log_index` 从 0 连续。回放路径：`RecordedChainAdapter` → `ReplayEngine`（V2Adapter）→
  `MarketGraphBuilder` → 1 个 `GraphSnapshot`（2 池 / 4 边）。
- **池子**：A `0xf487d533…6578`（Factory `0x5f6e8a56…`，`PairCreated` at 37187524/16）与
  B `0x5bef6275…7440`（Factory `0x1e594a50…`，at 37187526/161），**同一对代币**
  WETH `0x4200000000000000000000000000000000000006` / TTAX `0xcFfE7472…2F62`；
  `eth_getCode` 两边都是 5 393 字节、sha256 `75030560…6802fa`（同一个 pair 实现）。
- **状态**：各自的 `Sync`（log index 29 / 34，同一笔 tx `0x6132a9da…`）给出
  A `(35099900253008, 45655538604883371699)`、B `(36641298079327, 43677608078641141054)`；
  `getReserves()` 在同一个区块上逐字相同，且 `blockTimestampLast 1790536285` == 区块头时间戳。
  A 在创建到当前 head（37402775）之间共 6 条 `Sync`、B 共 2 条，且 37191170..37402775 之间
  **0 条**（按 9 999 块窗口逐段查询，无失败窗口）⇒ 这两条 `Sync` 就是这个 pair 的**最新**状态。
- **fee 举证（每个池子各自证明，绝不默认）**：先确定日志语义 —— V2 的 `swap()` 先发 `Sync`
  再发 `Swap`，所以**同一笔交易里那条排在 `Swap` 前面的 `Sync` 装的是这笔成交的成交后储备**，
  不是成交前储备；成交前那一对来自**该池上一条 `Sync`**（更早一笔交易发的，或者建池时的播种
  `Sync`）。每一份举证都把这两个指针写进证据文本，实测的链条是（已全部用 `eth_getLogs` 复算，
  每行都满足「成交后 − 成交前 == (+amountIn, −amountOut)」，逐字相等）：

  | 成交（`Swap` 区块/日志） | 成交前储备来自（`Sync` 区块/日志） | 成交后储备（同交易的 `Sync`） |
  |---|---|---|
  | A 37187701 / 72 | 37187536 / 147（播种） | 37187701 / 71 |
  | A 37187959 / 22 | 37187954 / 29 | 37187959 / 21 |
  | A 37189041 / 56 | 37187959 / 21 | 37189041 / 55 |
  | A 37191169 / 30 | 37189041 / 55 | 37191169 / 29 == 快照储备 |
  | B 37191169 / 35 | 37187542 / 245（播种） | 37191169 / 34 == 快照储备 |

  最后两行（也就是快照那一笔）**不依赖 RPC 就能验**：用例
  `the_sync_inside_a_swap_transaction_carries_post_trade_reserves` 直接读已提交的区块 fixture，
  断言 `Sync` 就排在同交易的 `Swap` 前一条、`Swap` 的四个金额逐字相等、
  「成交前 + 流入 − 流出 == 同交易 `Sync` 的储备」，并断言同交易的 `Sync` **不等于**
  夹逼所依据的那对成交前储备。

  于是每笔成交给出 `y = floor(x·r·Ro/(Ri + x·r))` 对 `r = n/d` 的一个半开区间（该函数对 r 严格递增），交集再与
  21 个标准费率档比较：
  - A：4 笔成交（区块 37187701/37187959/37189041/37191169）⇒ 交集相对宽度 **1.577e-19**，
    区间 `[18132217877602982630/18186778212239701737,
    32197072838625987438738244969646/32293954702734190006858429707827)`；
  - B：1 笔成交（区块 37191169，`Swap` log index 35，amountIn 3 677 608 078 641 141 054、
    amountOut 3 358 701 920 673，成交前储备 (40000000000000000000, 40000000000000)）⇒ 相对宽度
    **3.250e-13**；
  - 两者结论相同：21 档里**恰好 997/1000（同一个有理数写成 9970/10000）落在区间内**，
    19 档在外，`996/1000` 与 `998/1000` 都在外 ⇒ **retained = 997/1000**。
  - 负面证据也入档：`token1() / name() / symbol() / decimals() / feeTo() / feeToSetter()` 在该
    区块上全部 `execution reverted`，两个 Factory 的 `allPairsLength() / feeTo()` 同样 revert
    —— 链上没有任何地方**直接**公布费率，所以才必须用成交夹逼。
- **代币侧的 pin**：`token0()` 返回创建日志 topic1（WETH）；`token1()` revert，所以第二侧由链上
  自己的余额钉住 —— `balanceOf(pool)` 在 37191169 上逐字等于该池 `Sync` 的两个 reserve0/reserve1。
  WETH = "Wrapped Ether"（18 位，2 846 B 字节码），TTAX = "GIWA Test Tax"（18 位，4 827 B）。
- **候选路径**：真实图上 `candidates = 4`（2 个起点代币 × 2 个池序）、`skipped_pairs = 4`
  （全部 `SamePool`）、`opportunities = 2`、`rejected = 2`（`Unprofitable`，各自带 peak）。
  把两个池子的 fee 从举证里剥掉再跑同一份区块：`candidates` 仍是 4，`opportunities` 变成 **0**，
  4 条全部作为「未举证 fee」被拒绝且**没有 peak** —— 结论对 fee 证据的依赖是被测出来的，
  不是被叙述的。

### 8.3 M2 的旧证据集（作为可审计的 null）

M2 举证的 4 个池子（`data/protocols/`，全部 `fee: null`）+ M1/M2 的 6 个真实区块
（5 455 035 / 5 457 650 / 10 544 346 / 31 390 683 / 37 257 255 / 37 258 093）重放之后：
图 = 2 池 / 4 边，**候选 0 条、定价 0 条、机会 0 条、拒绝 0 条、best 不存在**。
这就是 §48 要求的「找不到也要能审计」的那一份：搜索范围、候选数、评估数、best 全为 0 且有原因
（那 4 个池子分属 3 个币对，没有任何一个币对有两个池子）。

## 9. Real Opportunity

**找到了，两条，同一个市场状态。** 完整记录（这就是 `Opportunity` 的 `Display` 输出，
`the_audit_line_carries_every_number_a_reviewer_wants` 逐字段断言它包含
chain / block / 两个代币 / 两个池子 / 输入 / 输出 / 利润 / 997/1000 / 两个方向的储备 / 策略 /
`closed true`）：

```text
chain 91342 block 37191169
route 0xcFfE7472A7a1A6947f56233854Ae91a54C862F62 -> pool 0x5bef6275607901dCd58160356660151BE0637440 -> 0x4200000000000000000000000000000000000006 -> pool 0xf487D533Cae6cddd0C7E7BBbAc084DD04d876578 -> 0xcFfE7472A7a1A6947f56233854Ae91a54C862F62
input  0xcFfE7472A7a1A6947f56233854Ae91a54C862F62 amount 890134426448791298
output 0xcFfE7472A7a1A6947f56233854Ae91a54C862F62 amount 927044426434210645
gross profit     36909999985419347
pool 0x5bef6275607901dCd58160356660151BE0637440 reserves in/out 43677608078641141054/36641298079327 fee 997/1000
pool 0xf487D533Cae6cddd0C7E7BBbAc084DD04d876578 reserves in/out 35099900253008/45655538604883371699 fee 997/1000
search BoundedTernary over 1..=45655538604883371698 rounds 104 evaluations 232 scanned 24 closed true
```

第二条（同一市场的镜像起点，按利润排序在后）：

```text
route WETH 0x4200000000000000000000000000000000000006
     -> pool 0xf487d533cae6cddd0c7e7bbbac084dd04d876578 (35099900253008 -> 45655538604883371699)
     -> TTAX 0xcffe7472a7a1a6947f56233854ae91a54c862f62
     -> pool 0x5bef6275607901dcd58160356660151be0637440 (36641298079327 -> 43677608078641141054)
     -> WETH
input  714844720992        （= 0.000000714844720992 WETH）
output 744486240802        （= 0.000000744486240802 WETH）
gross profit 29641519810   （= 0.000000029641519810 WETH，输入的 4.15 %）
search BoundedTernary over 1..=36641298079326, rounds 50, evaluations 130, scanned 30, closed true
       利润 == 闭式天花板 ⇒ 全域整数最优（第 5 节）
```

**这个利润是从哪个真实市场状态算出来的**（§49 要求能回答的问题）：从 block 37191169 上这两条
`Sync` 日志（log index 29 和 34，同一笔交易 `0x6132a9da…`，该块第 18 笔）所声明的储备，加上两个
池子各自被 4 笔 / 1 笔自己的成交夹逼出来的 997/1000。

必须同时写下的事实：**该块的储备是这笔交易执行之后的状态**。tx 18 本身就是一次
`WETH → A → TTAX → B → WETH` 的往返：`Swap` log index 30 记 `amount0In 2 829 357 023 446 /
amount1Out 3 990 893 194 401 672 331`，index 35 记 `amount1In 3 677 608 078 641 141 054 /
amount0Out 3 358 701 920 673`，净得 **+529 344 897 227 WETH（+18.709 %）**。因此：

- M3 报出的是**这笔执行之后残余的**两池分歧，不是那笔已经被吃掉的分歧；
- 那笔交易之所以能拿到 18.7 %（远大于无税模型的 4.15 %），是因为 TTAX 在转账时被烧掉：
  A 付出的 3 990 893 194 401 672 331 个 TTAX 里只有 3 677 608 078 641 141 054 个进了 B，
  留存比例实测 **0.9215**（恰为 0.97 × 0.95，即 3 % + 5 %）。代币税属于 §3 明令排除的范围，
  M3 既没有建模也不该建模 —— 但它是「gross profit 不等于能拿到的钱」的一个真实样本，
  所以写在这里。

可复现性：`detection_is_repeatable_and_leaves_the_market_alone` 断言同一份输入两次得到的
`Detection` 完全相等，且检测不会把询价过的量写回池子状态（模拟量只活在这个 crate 里）。

## 10. Known Limitations

1. **No gas / No simulation / No execution**：没有 gas 估算、没有 gas price、没有 EVM 交易模拟、
   没有签名、没有广播、没有 nonce、没有 bundle、没有 flashbots/私有中继、没有优先级费/贿赂、
   没有执行风险、没有滑点容忍（全部是 §3 排除项）。
2. **gross profit ≠ 可执行利润**：见 §41 与第 9 节的代币税实例；本报告的「有利可图」仅指
   AMM 整数算术在同一区块储备上的差价。
3. **No multi-hop optimization**：只定价 `A → pool1 → B → pool2 → A` 的**恰好两跳**循环
   （`ArbitragePath::two_hops` 是唯一构造入口）。更长的环、环的组合、以及「多笔机会一起寻优」
   都不存在。
4. **离散取整会让搜索差一点点**：TTAX 路线实测比自身闭式天花板低 **3 125**；网格穷举的 2 401 条
   路由里有 49 条搜索低于穷举最大值（最差差 14 个单位），全部落在取整包络之内。搜索**从不**高于
   穷举。
5. **只承认已举证的 fee**：`fee = None` 的池子会被 `UnattestedFee` / `MissingFee` 拒绝而不是默认
   0.3 %。代价是历史上 42 条筛查为正的路线里只有 2 条成为机会；收益是错误 fee 造成的 10 条假机会
   一条都没有。目前被举证的 fee 只有 997/1000 这一档（chain 91342 上的 V2 兼容池）。
6. **搜索范围有已知缺口**：50 个多池币对里有 13 个（27 个更早创建的池子）没有进入 Sync 扫描，
   所以 §8.1 的 200 条路由不是「全部可能的两池路由」。
7. **流动性是测试网种子级**：42 条筛查机会里 0 条的最小储备 ≥ 1e18 wei；两条被承认的机会
   分别折合 0.0000000296 WETH 与 0.0369 TTAX。
8. **声明但未构造的错误变体**：`OpportunityError::MissingState` 与 `PathError::HopCount`
   在当前代码里**没有任何构造点**（快照的边自带储备，两跳构造器表达不了别的跳数）。
   `error_matrix.rs` 的文档表与两个用例明确记录了这一点，让它们成为「有意识留着」而不是
   「被忘记」。
9. **`RejectionReason::InvalidFee` 不带池子身份**：`MathError::InvalidFee` 只有比例本身，
   归因不到 `PoolId`（`MissingFee` 可以）。
10. **序列化面只覆盖需要的那几个类型**（§56）：`Opportunity`/`Detection`/`SearchRecord`/
    `PricedHop`/`RejectionReason`/`PathError`/`MathError` 有 `Serialize`；没有为此扩大
    `Deserialize`，`ArbitragePath` 的序列化由 `Hop` 字段派生。

## 11. Scope Boundary

M3 到此为止：**`GraphSnapshot → Opportunity` 这一段是完整、可重放、可审计的**。下面这些
属于后续里程碑，M3 一行都没做，也没有为它们预埋任何运行时依赖：

- **M4：Execution / Profitability。** 把机会变成一笔可执行交易需要 gas 估算与价格、优先费与
  贿赂、滑点容忍、bundle 构建与私有中继、签名与广播、nonce 管理、执行风险与回滚。
  M3 的 `Opportunity` 携带 `hops`（两个池子的方向化储备 + 已举证 fee）与 `search`（域、轮数、
  是否闭合），M4 因此**不需要回头改定价层**就能审计「这个利润从哪来」。
- **M5+：多跳与组合。** `HopCount` 与 `PathError` 的形状允许将来把环拉长，但 v0.1 里
  两跳是唯一能被构造出来的路径。
- 仍然在范围外（PRD 与 §3 的共同决定）：V3/集中流动性、清算、三明治、NFT MEV、intent、
  跨链、AI/ML 通用语义分类、通用 DEX 支持、代币税与蜜罐检测。
- **不重新实现 replay framework**（§57）：`Chain Replay` 与 `State Replay` 继续用 M1/M2 已有的
  `evm-chain` / `evm-state` / `evm-replay`；M3 只消费 `GraphSnapshot`。

## 12. 验收标准 A–Q 对照（§65）

| 条 | 判定 | 实测依据 |
| --- | --- | --- |
| A 从 `GraphSnapshot` 生成 two-pool candidate | PASS | `enumerate_candidates(&snapshot)`；真实图 4 条候选（`the_detector_reports_the_two_routes_the_oracle_predicted`） |
| B 正确拒绝 same pool twice | PASS | `PathError::SamePool`；单池图 0 候选 / 2 跳过（`fixture_7`）；真实图 4 条 `skipped_pairs` 全是 `SamePool` |
| C 正确处理 multiple pools same pair | PASS | 3 池 1 对 ⇒ 12 条有向路由、去重后仍 12（`fixture_8`）；真实 2 池 ⇒ 4 条 |
| D AMM swap 用 U256 精确计算 | PASS | `src/math.rs` 全程 `U256`；生产代码 f64/f32 出现 0 次；112 位储备不溢出（`fixture_6`） |
| E Fee 正确参与计算 | PASS | 保留比例进分子与分母；零 fee 对照组 `a_zero_fee_market_leaves_an_equal_price_market_with_nothing_to_take`；真实市场剥掉 fee ⇒ 0 机会；举证据以的成交前储备来源由 `the_sync_inside_a_swap_transaction_carries_post_trade_reserves` 在 fixture 上钉住 |
| F two-hop output + gross profit | PASS | `swap_through_two_hops` + `PathSimulation::gross_profit`；真实两条 744 486 240 802 / 29 641 519 810、927 044 426 434 210 645 / 36 909 999 985 419 347 |
| G optimal input + 独立 brute-force 验证 | PASS | 网格 2 401 条路由穷举对照（第 7 节打印数字 + `outside_envelope == 0`）；真实市场 100 000 点独立扫描；闭式天花板 |
| H no-arbitrage fixture ⇒ no opportunity | PASS | `fixture_1`、`fixture_3` |
| I profitable fixture ⇒ gross_profit > 0 | PASS | `fixture_2`、`fixture_4` |
| J fee-erased fixture ⇒ no opportunity | PASS | `fixture_3` + `a_zero_fee_market_makes_fixture_3_pay_what_the_fee_took`（配对证明） |
| K zero reserve / invalid path correctly rejected | PASS | `fixture_5`、`a_zero_reserve_is_refused_before_it_can_be_priced`、`the_path_rules_refuse_the_shapes_that_are_not_cross_market_arbitrage`；10 项错误矩阵全部有可达的门 |
| L 真实 GraphSnapshot 进入 candidate detection | PASS | `the_captured_block_yields_one_graph_with_both_pools`（2 池 / 4 边）→ 4 候选 |
| M 真实历史数据完成搜索 | PASS | §8.1：200 条有向路由全部定价（42 条为正），另有 §8.3 的 M2 全量重放 null |
| N 真实机会可复现 | PASS | §9 完整记录 + `detection_is_repeatable_and_leaves_the_market_alone` + 序列化字节稳定 |
| O 结果 deterministic | PASS | `determinism.rs` 7 个用例（含注册顺序无关、镜像平局、审计行一致） |
| P workspace 全量四关 PASS | PASS | 第 6 节：fmt / check / test / clippy 全部 exit 0，23 套件 161/0/3 |
| Q M3 Completion Report 完成 | PASS | 本文档 |

## 13. 最终验收链路（§66）

```text
真实区块        fixtures/real-m3/block-37191169.json（chain 91342 / 35 tx / 54 log / 已验证哈希）
    ↓ RecordedChainAdapter（不联网，只读落盘）
真实日志        Sync idx 29 (pool A) / idx 34 (pool B)；Swap idx 30 / 35（同一笔 tx 0x6132a9da…）
    ↓ V2Adapter + ReplayEngine + InMemoryStateStore（M1 的状态机）
真实池子状态    A (35099900253008, 45655538604883371699)
                B (36641298079327, 43677608078641141054)   ← getReserves() 在同一块逐字相同
    ↓ MarketGraphBuilder（M2）
真实市场图      2 池 / 4 边 / 绑定 block 37191169
    ↓ enumerate_candidates
两池候选        4 条有向路线（+ 4 个 SamePool 边对被记录为跳过）
    ↓ PricedHop::from_edge —— 没有已举证 fee 就拒绝
精确 AMM 数学   x·997·Ro / (Ri·1000 + x·997)，全 U256 整数、向下取整
    ↓ find_optimal_input（BoundedTernary，域 1 ..= 第二跳储备_out − 1）
最优输入        WETH 路线 50 轮 / 130 次询价 / 区间闭合 ⇒ == 闭式天花板（全局整数最优）
                TTAX 路线 104 轮 / 232 次询价 / 区间闭合（比天花板低 3 125）
    ↓
gross profit    29641519810（WETH 端） / 36909999985419347（TTAX 端）
    ↓
Opportunity     2 条，各带 hops（两池的方向化储备 + 997/1000）与 search 记录；
                2 条反向路线作为 Unprofitable 连同 peak 一起留在报告里
```

## 14. 对 M1/M2 事实的处理（§61）

- **没有修改 M1/M2 的任何已验证事实。** M3 的改动是纯增量：新增 crate、新增
  `data/protocols-m3/`、新增 `fixtures/real-m3/`，`data/protocols/` 与 `fixtures/real/`
  一个字节都没动（`git diff --stat` 只有 `Cargo.toml` / `Cargo.lock`）。
- **M2 的 `fee: null` 保持 null，这是正确的**：`crates/protocol/src/registry.rs` 与 M2 的
  真实验收测试都断言「M1 时代没有举证 fee」，那是当时的事实，不该被后来的举证改写。
- 需要记录的**新增事实**（不是修正）：M3 之后，`0xf487d533…6578` 与 `0x5bef6275…7440` 这两个
  池子有了逐池夹逼证明的 fee（997/1000），它们在 `data/protocols-m3/` 里；M2 的 4 个池子
  （含 `0x3978e57b…`）**仍然**没有举证 fee，因此 M3 的 detector 对它们一律拒绝定价 —— 这正是
  §8.3 那份 null 的成因。调查过程中曾用同样的夹逼法测得 `0x3978e57b…` 的成交与 998/1000 一致
  （而不是 997/1000），该结论**没有**被写进任何代码或 M2 的证据文件，因为 M3 不需要用它，
  而把它塞进 M2 的历史文件等于改写历史。留在此处，供 M4 决定是否正式举一份 fee。
- **本里程碑自己写下的证据散文修正了一处，数字一个没动**：`data/protocols-m3/…json` 里 5 条
  fee 夹逼证据原先写成「成交前储备 = 同一笔交易发出的那条 `Sync`」。这个因果方向是错的 ——
  同交易的 `Sync` 装的是成交**后**的储备，成交前那一对来自该池上一条 `Sync`（更早一笔交易，
  或建池时的播种）。修正只替换那段散文，并给每条举证补上「成交前 `Sync` 的区块/日志」与
  「成交后 `Sync` 的区块/日志」两个指针；复写脚本做了两道自检：文件里**没有任何数字被删除或
  改动**（数字 token 集合只增不减），且 `signature` 以外的结构逐字段相等。夹逼用的储备对、区间
  端点、997/1000 的结论全部原样保留，改的是「这两个数从哪条日志来」这句话。M1/M2 的证据文件
  里不含这句话（全库检索只有这一处），所以没有牵连历史。第 8.2 节原先重复了这个错误说法，
  现已按实测链条改写，并由 `the_sync_inside_a_swap_transaction_carries_post_trade_reserves`
  固化成可执行断言。
- **没有任何真实 reserve 被修改、没有任何池子被发明、没有任何 fee 被默认**（§50/§61/§69）。

## 15. 复现方式

```bash
cd /Volumes/superfs/evm-mev-bot
export CC=clang CXX=clang++
cargo +1.96.1 test --offline --workspace                    # 161 passed / 0 failed / 3 ignored
cargo +1.96.1 clippy --offline --all-targets --all-features -- -D warnings   # exit 0

# 第 7 节的穷举统计（原样打印）
cargo +1.96.1 test --offline -p evm-opportunity --test optimizer_cross_check -- --nocapture

# 第 9 节的两条机会：由 tests/real_data.rs 逐字段断言
#   the_detector_reports_the_two_routes_the_oracle_predicted
#   an_analytic_ceiling_proves_the_weth_route_is_the_global_optimum
#   the_real_search_closes_its_interval_inside_the_published_budget
#   the_audit_line_carries_every_number_a_reviewer_wants
#   without_attested_fees_the_same_market_produces_nothing   # 剥掉 fee ⇒ 0 机会
#   the_m2_attested_set_is_an_auditable_null                 # M2 证据集 ⇒ 可审计的 null
#   the_sync_inside_a_swap_transaction_carries_post_trade_reserves
#                                                          # 第 8.2 节的日志语义，直接读 fixture
cargo +1.96.1 test --offline -p evm-opportunity --test real_data
```

真实数据全部来自 chain 91342（`https://sepolia-rpc.giwa.io`）的公开 RPC；调查阶段的脚本与
中间产物没有入库（M3 的交付是 crate 与证据文件），但**每一条被本报告引用的结论**都能在
`data/protocols-m3/v2-sepolia-42000006-cffe7472.json` 的 evidence ref（block / log index /
transaction hash / source / signature）和 `fixtures/real-m3/block-37191169.json` 里查到原文。
