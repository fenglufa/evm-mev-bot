# M4 Completion Report

结论（白话版）：M4 已经跑通，状态 **COMPLETE**（任务书 §65 的 A–Q 十五条逐条实测通过，其中 H 一条
是以 §38 明文允许的「只存在明确记录的执行层差异」形式满足的，见第 13 节）。
这一轮把 M3 的「理论上有利可图」第一次交给了真实的 EVM 去裁决：

> 真实区块 → 真实状态 → 真实市场图 → M3 Opportunity → 12 步真实交易序列 → REVM 执行
> （真实字节码 + 真实 calldata + 该区块结束时的真实状态）→ gas_used × 历史 base fee
> → simulated gross / net profit → 风险判定

在 GIWA Sepolia（chain 91342）的 **block 37 191 169** 上，M3 认定的那条 WETH → pool A → TTAX →
pool B → WETH 路线，**在真实执行里没有利润**：

| 问的是多少（minimum out） | 池子实际付了多少 | 结果 |
| --- | --- | --- |
| M3 的解析值 744 486 240 802 | 0（第二步 swap 被池子 revert） | 亏，且亏得连「到手多少」都还没有 |
| 1 wei（不设门槛） | 1 wei | 完成，但净亏 714 971 771 591 wei |
| 该路线能被满足的最高门槛 687 140 046 135 | 687 140 046 135 wei | 完成，净亏 27 831 754 257 wei |

原因是被执行**决定**而不是被假设的：这条路线中间那个代币 TTAX 每次转账都要抽税。第一次转账抽掉
27 257 482 417 237 311（正好是 `floor(付出量 × 3 / 100)`），第二次抽掉 44 066 263 241 200 320
（正好是 `floor(到账量 × 5 / 100)`），两笔合计 **71 323 745 658 437 631**，与模拟结束后代币合约
「销毁账户」那个存储槽的变化量**逐 wei 相等**。也就是说：M3 的 AMM 公式没有算错任何一个池子
（两个池子各自付出的数量都精确等于公式对它**实际收到**的那笔数量给的报价），M3 多的那 57 346 194 667
（解析值的 770/10 000）全部来自公式看不见的那一段 —— 代币在两次转账之间自己扣下的税。

三件必须说清楚、不能被这份「模拟跑通了」盖过去的事：

1. **这里没有任何东西被广播。** 没有私钥、没有签名、没有 `eth_sendRawTransaction`、没有 relay、
   没有 bundle。风险层即使给出 Accept，语义也只是「满足这两条阈值」，§47 的那句话被写进了
   每一条执行记录的正文里。
2. **本轮结论是「无利润」，不是「有利润但没抓住」。** 三个档位全部没有净利润：最高门槛那一档净亏
   27 831 754 257 wei（= 0.000000027832 WETH），而 gas 只占其中 127 079 400 wei
   （= 0.0000000001270794 native，约 1.3 × 10⁻¹⁰）。钱不是被 gas 吃掉的，是被税吃掉的。
3. **这些池子是测试网的种子流动性。** 输入 714 844 720 992 wei = 0.000000714844 WETH。这条链还是
   一个 OP-stack rollup：真实交易还要付一笔**不在 EVM 账本里**的 L1 数据费（同一区块里那笔真实
   套利 tx 的收据带 `l1Fee = 0x3e00eca01`）。M4 的 gas 账单只覆盖 EVM 自己扣的那一份，见第 12 节。

任务书要求的事实值（十进制原始整数，来自实跑输出，非估算；两个代币 `decimals()` 都是 18）：

| 项 | 值 | 换算 / 说明 |
| --- | --- | --- |
| chain / block | 91342 / 37191169 | `0x2377e01`，区块哈希 `0x56c628d7…48d670`，时间戳 1790536285，base fee 300 wei，区块 gas limit 60 000 000，35 笔交易 |
| 执行引擎 | revm `=43.0.3`（`std` + `asyncdb`），`EvmRules::Prague` | 一个 EVM 实例跑完整个 12 步序列，每步一笔交易 |
| 路线 | WETH `0x4200…0006` → pool A `0xf487d533…6578` → TTAX `0xcffe7472…2f62` → pool B `0x5bef6275…7440` → WETH | 与 M3 报告同一对池子、同一个输入上界 |
| 解析（M3） | 输入 714 844 720 992 → 中间 908 582 747 241 243 728 → 输出 744 486 240 802 | gross profit 29 641 519 810（+4.15 %） |
| 模拟（真实执行，ask=1） | 输出 1 | simulated gross **−714 844 720 991**；gas 423 502 × 300 = **127 050 600**；net **−714 971 771 591** |
| 模拟（真实执行，ask=容量值） | 输出 687 140 046 135 | simulated gross **−27 704 674 857**；gas 423 598 × 300 = **127 079 400**；net **−27 831 754 257** |
| delta（解析 − 容量） | 57 346 194 667 | 解析值的 770/10 000；等于两次转账税合计对第二跳输出的影响 |
| token tax（执行决定） | 3 % 然后 5 %，两次合计到达率 9215/10 000 | 与该区块那笔真实 tx 的日志实测值 9215 bps **一致**；两笔税之和逐 wei 等于销毁槽变化量 |
| 状态 | 冻结 fixture：6 账户 / 21 存储槽 / 64 次读取 | `fixtures/simulation-m4/dump-37191169.json`，sha256 `47eaea4b…28a53`，6 次独立进程录制**字节相同** |
| 风险判定 | greedy → Reject(simulation_success)；floor → Reject(minimum_net_profit)；无价格 → Unknown(minimum_net_profit) | 三条规则：§45 的 `simulation_success` / `maximum_gas` / `minimum_net_profit`，全实跑、全举证 |
| 关卡 | fmt / check / test / clippy 全部 exit 0 | 22 个测试二进制（unit + integration），**259 通过 / 0 失败 / 4 忽略** |

---

## 1. Status

**COMPLETE。**

判定依据：§65 的 A–Q 逐条实测（第 13 节），四道关卡在整个 workspace 实跑通过（fmt exit 0、
`cargo check --workspace --all-targets` 无输出、`cargo test --workspace` = 259 通过 / 0 失败 /
4 忽略、`cargo clippy --workspace --all-targets --all-features -- -D warnings` exit 0）。

必须点明的一处「不是以任务书字面形状满足」的地方：**H（无税 fixture）**。任务书没有造一个
手续费为 0 的假池子，M4 也没有；它给的是 §38 明文允许的第二种形态 ——
「或者只存在明确记录的执行层差异」。实测到的记录是：两个真实池子各自**付出**的数量都精确等于
M3 公式对它**实际收到**的数量所报的价（leg 1：908 582 747 241 243 728 = 公式值，leg 2：
687 140 046 135 = 公式值），因此 AMM 层是 exact 的；整条路线的差额 100 % 来自两次 ERC20 转账的
税，而这两笔税是被状态变化（销毁槽）逐 wei 证明的，不是假设的。第 12 节把「没有 fee-0 fixture」
列为限制而不是成就。

另外，「模拟出一条无利润的路线」并不影响 COMPLETE：§48/§25 明确允许报告 null 结果，判定挂在
管线是否成立上，不挂在这条路线是否赚钱上。

## 2. Simulation Engine

- **engine / version**：`revm = { version = "=43.0.3", default-features = false, features = ["std", "asyncdb"] }`
  （`Cargo.toml:36`）。版本是 pin 死的 `=`，不是 `^`。
- **configuration**：`EvmRules::Prague`（`crates/simulation/tests/support/mod.rs:169`），
  区块环境来自 fixture/nodes 自己的 header：`timestamp 1790536285`、`gas_limit 60000000`、
  `base_fee_per_gas 300`、`beneficiary 0x4200…0011`、`prevrandao 0x27b7fa76…`、
  `excess_blob_gas 0`。
- **形态**：一个 `Simulator` 抽象（§7）把 REVM 的类型全部关在 `crates/simulation/src/engine.rs` 里，
  对外只暴露 `SimulationRequest` / `SimulationResult`。状态通过 `StateProvider` 的 async 接口进
  REVM（`AsyncDb`），所以整条序列跑在**同一个 EVM 实例**上、**每步一笔交易**，nonce 由引擎自己
  递增（实跑：step 1 nonce 0 … step 10 nonce 9，step 7 nonce 6）。
- **缓存边界**：code 以 `chain_id + address` 为键（§63），storage 以 `chain_id + block + address + slot`
  为键（§64）。同一档位阶梯里第二次读同一槽不会变成第二次 RPC。
- **§34 的失败种类从未合并**：`SimulationError` 有 7 个变体
  （`StateMismatch` / `ChainMismatch` / `ProviderError` / `MissingCode` / `MissingState` /
  `InvalidTransaction` / `UnsupportedTransaction`），结果内部另有 3 个执行状态
  （`Reverted` / `OutOfGas` / `Halted`）与「完成了但不赚钱」这一类，一共 10 种互不相同的答案，
  没有 `SimulationFailed` 这种万能桶。本轮真实路线只触发了其中 2 种（`Completed`、`Reverted`），
  其余由各 fixture 触发（第 11 节）。
- **本轮发现并修掉的一个真实缺陷**（值得记进报告，因为它差点让「确定性」变成口头承诺）：
  §36 的状态差异要把每一步碰过的账户/槽**读回**起始值，而它当时是直接遍历累计用的
  `HashMap` 去读 —— 读回顺序 = 进程的哈希种子顺序。结果本身排序后一致，但**fixture 的
  `reads` 字段记录的是「按什么顺序问的状态」**，于是同一段被 pin 死的历史状态，两次独立进程
  录制出来的文件字节不一样。实测复现过两次：`cc1939…` → `74f216…`，再 `74f216…` → `22b46b…`，
  而两次读取的 64 条集合完全相同（差在顺序，从 index 39 开始）。修法是读回前先按
  (address)/(address, slot) 排序（`engine.rs` 的 `account_reads` / `slot_reads`），
  并加了一条能真的失败的单元测试：把同一批 8 个账户按两种插入顺序累计，断言读回序列既升序
  又与插入顺序无关；把排序删掉后该测试 FAILED（`left: […0a, …02, …03…]`），不是空断言。
  修完后同一区块连续 6 次独立进程录制（最后一次在写完本报告的过程中重跑），文件 sha256 全为 `47eaea4bab4585…b9f28a53`。

## 3. State Source

只有两个 provider（§62），没有第三个：

| 提供者 | 来源 | 用在哪 |
| --- | --- | --- |
| `RpcStateProvider` | `https://sepolia-rpc.giwa.io`，在 `BlockPin { number: 37191169, hash: 0x56c628d7…48d670 }` 上读 | 唯一的实跑 `crates/simulation/tests/real_chain.rs`（`#[ignore]`，31–55 s） |
| `DumpStateProvider` | `fixtures/simulation-m4/dump-37191169.json` | 全部离线套件（`dump_replay`、`risk_decision`、`failure_kinds`） |

- **§15/§20 的绑定**：header 与 route 必须同一区块（`Fixture::load` 断言
  `header.number == route.block_number`）；请求声明的 `state_source` 与实际跑的 provider 不一致时，
  引擎在**执行任何一步之前**返回 `StateMismatch` 并原样引用两边的名字：
  `refused before execution: the request was built against state source "rpc:chain-91342", but the provider it is being run on reports "/Volumes/superfs/evm-mev-bot/fixtures/simulation-m4/dump-37191169.json"`。
- **只做历史（§16/§51–§53）**：读的是 37191169 **结束后**的状态，取证文件里还留了下一块的
  `parentHash` 指向本块哈希，说明这一块不是链尖，状态是历史而非「马上会被改」。
- **fixture 内容**：6 个账户（WETH、TTAX、pool A、pool B、测试 sender、coinbase）、21 个存储槽、
  1 个 header、64 条读取记录。
- **override 只有 §18/§57 允许的那一类**：测试 sender 的 native 余额。本轮实跑里这个数是
  推导出来的而不是手填的：`endowment = gas_units × max_fee_per_gas + wrapped_native`
  （`request.rs:190-212`），即 `(30 000 000 × 12) × 300 + 714 844 720 992 = 822 844 720 992`；
  换到 gas 上限 89 864/步的那一档，同一公式给出 `715 168 231 392` —— 两个数字都是实跑打印出来的，
  不是抄的。没有对任何池子或代币的状态做过 override。

## 4. Transaction

12 步，全部是「一个 sender 直接对 Pair 调 `swap`」的序列（§12/§14：没有 universal router、
没有 flash loan、没有新建 executor 合约）。下表是实跑输出的原样（`from` 恒为测试 sender
`0x953E7e98562714C23BC22c7D186cdF516F9dfa6f`，每步 `gas_limit = 30 000 000`）：

| step | 调用 | to | value | calldata | selector | 这一步测到的量 |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | native 余额读（非调用） | — | 0 | 0 B | — | `sender_native_start = 822844720992` |
| 1 | `balanceOf(address)` | WETH `0x4200…0006` | 0 | 36 B | `0x70a08231` | `sender_input_start = 0` |
| 2 | `deposit()` | WETH | **714 844 720 992** | 4 B | `0xd0e30db0` | — |
| 3 | `transfer(address,uint256)` | WETH | 0 | 68 B | `0xa9059cbb` | — |
| 4 | `swap(uint256,uint256,address,bytes)` | pool A `0xf487d533…6578` | 0 | 164 B | `0x022c0d9f` | — |
| 5 | `balanceOf(address)` | TTAX `0xcffe7472…2f62` | 0 | 36 B | `0x70a08231` | `sender_mid_received = 881325264824006417` |
| 6 | `transfer(address,uint256)` | TTAX | 0 | 68 B | `0xa9059cbb` | — |
| 7 | `swap(uint256,uint256,address,bytes)` | pool B `0x5bef6275…7440` | 0 | 164 B | `0x022c0d9f` | — |
| 8 | `balanceOf(address)` | WETH | 0 | 36 B | `0x70a08231` | `sender_input_end`（ask=1 时 = 1） |
| 9 | `withdraw(uint256)` | WETH | 0 | 36 B | `0x2e1a7d4d` | — |
| 10 | `balanceOf(address)` | WETH | 0 | 36 B | `0x70a08231` | `sender_input_after_unwrap = 0` |
| 11 | native 余额读（非调用） | — | 0 | 0 B | — | `sender_native_end`（ask=1 时 = 107872949401） |

- **§59 的 `msg.value`**：整条序列只有 step 2 带 native，数额正好等于输入量
  714 844 720 992 —— 因为资金形状是 `Funding::WrapNative`，`deposit()` 是唯一把 native 变成
  这条路线的输入代币的动作；两个 `swap` 和两次 `transfer` 的 value 都是 0。
- **§21/§22 的滑点**：`asked_output` 就是写进第二跳 `swap` 的 `minimumAmountOut`，结果里带
  `SlippageRecord { expected_output, policy, minimum_output }`。ask=1 那一档实测
  `slippage.minimum_output == 1`（`dump_replay.rs:89` 断言）；ask=解析值那一档是
  `SlippagePolicy::Exact`（零滑点），也正是它被池子拒掉。
- **§10 的「Router/Executor」**：本轮没有 router 字节码被执行，因为设计上不需要 ——
  目标就是 Pair 本身（0x022c0d9f `swap`）。被执行到的字节码一共 4 段：两个 Pair（各 5393 B，
  **两者 sha256 相同** `75030560bcd62c8b…6802fa`）、WETH（2846 B `b0c360135740f26e…f7007d`）、
  TTAX（4827 B `02833fa622f5d852…04e544`）。这些取自 M4-Step1 的取证文件，`at = 0x2377e01`
  （就是本区块）。
- **§60**：`eth_getCode` 返回空 → `MissingCode`，在**读任何状态之前**返回
  （`failure_kinds.rs::a_contract_with_no_code_stops_the_run_before_it_reads_any_state`）。
- **§58 的 sender**：`0x953e7e98562714c23bc22c7d186cdf516f9dfa6f` 是链上真实存在的 EOA，
  本轮跑之前引擎先读它的代码并确认**没有字节码**（有代码就直接拒绝），并且没有任何私钥
  与它关联 —— 我们不持有它。它在这条序列里能被「充值」完全是 §18 的余额 override。

## 5. Execution

三个档位，同一个 route / 同一个 header / 同一份状态，只有 ask 不同：

| 档位 | 状态 | 输出 | gas_used | 日志 | revert |
| --- | --- | --- | --- | --- | --- |
| ask = 解析值 744 486 240 802（实跑 RPC） | `Reverted` @ step 7 | 无 | 340 703 | 8 条 / 8 步 / 21 个 topic | `Error(string)`，message `"K"` |
| ask = 1（实跑 RPC 与离线 fixture 各一次） | `Completed` | 1 | 423 502 | 12 条 / 12 步 / 30 个 topic | — |
| ask = 687 140 046 135（容量，离线 fixture） | `Completed` | 687 140 046 135 | 423 598 | 12 条 / 12 步 / 30 个 topic | — |

- **§35 的原始 revert 数据被完整保留**：step 7 带 100 字节原文
  `0x08c379a0…0020…0001 4b00…`，解码器只在载荷确实是 `Error(string)` 形状时才给出文本
  （`RevertData { raw, message: Some("K") }`），`"K"` 是 PancakeV2 风格的 `MINIMUM_AMOUNT_OUT`
  缩写；那 4 字节 `0x08c379a0` 由测试从 `"Error(string)"` 签名现算再对照常量
  （`result.rs:990`），不是手抄。
- **§28 的 gas_used ≠ gas_limit**：每步上限 30 000 000，12 步总上限 360 000 000，实际用量
  423 502（ask=1）—— 结果里两个数同时打印：`gas used 423502 of 30000000 allowed per step`。
- **§41 的 OOG 是真的把上限调小跑出来的**：把每步上限设为 89 864（= 最大的一步 pre-swap 步
  44 932 的两倍，第一个 `swap` 需要 94 088），序列停在 step 4，`OutOfGas`，gas 190 761，
  2 条日志 / 5 步。计划、ask、状态、价格都没动，所以答案里唯一变化的解释只能是上限。
- **§36 的状态审计**（同一份实跑，账户与存储都读回 pin 值再比对）：
  - WETH 余额变化：`+[714844720992] −[1]`（sender 侧进入 = 输入量，离开 = 输出量）
  - TTAX 余额变化：`+[837259001582806097] −[71323745658437631, 908582747241243728]`
    其中 −71 323 745 658 437 631 那一项就是代币合约自己的**销毁/税槽**，恰好等于两笔转账税之和。
  - 为什么不采信 REVM journal 的 `original`：journal 是**每笔交易**的窗口，而这里一步一笔交易，
    `finalize()` 只剩最后一步窗口里看到的那几个账户和槽，而整个序列写过 4 个合约的槽；且被记录到的
    那个槽的 `original` 是「窗口打开时」的值（sender 的 TTAX 槽记成 881 325 264 824 006 417，
    而 dump 里是 0）。把运行的中间当起点比没有审计更糟，因为它看起来像审计。
- **§37 的确定性**：`the_frozen_fixture_replays_itself_exactly` 把同一个请求跑两遍，再用**新读一次
  文件**的 provider 跑第三遍，三次的 `SimulationResult` 逐字段相等（含日志与状态差异）且
  fingerprint 相等。加上上面那件事修复之后，实跑录制的文件在 6 次独立进程里字节一致。

## 6. Analytical vs Simulation

§26 要的三件套，逐个档位（全部为原始整数 wei）：

| 档位 | analytical_output | simulated_output | delta |
| --- | --- | --- | --- |
| ask = 解析值 | 744 486 240 802 | 无（step 7 revert，未执行完） | 无法比较（`OutputComparison { analytical: 744486240802, simulated: None }`） |
| ask = 1 | 744 486 240 802 | 1 | −744 486 240 801 |
| ask = 687 140 046 135 | 744 486 240 802 | 687 140 046 135 | −57 346 194 667（解析值的 770 bps） |

差异的解释不是「simulation 错了」，而是把差异拆到能称的量级（§26 明确列的候选之一是 token tax）：

```
leg 1：pool A 付出 908 582 747 241 243 728  ==  M3 公式对 714 844 720 992 的报价（精确相等）
        sender 到账 881 325 264 824 006 417  =  付出量 − floor(付出量 × 3/100)
        差额（第一次转账税）27 257 482 417 237 311
leg 2：pool B 记入 837 259 001 582 806 097   =  sender 到账量 − floor(到账量 × 5/100)
        第二次转账税 44 066 263 241 200 320
        M3 公式对 837 259 001 582 806 097 的报价 = 687 140 046 135
        pool B 实际付出 687 140 046 135        ==  公式值（精确相等）
两笔税之和 71 323 745 658 437 631            ==  TTAX 销毁槽的变化量（逐 wei 相等）
```

于是解析值与容量的差 57 346 194 667 完全由这两笔税解释：M3 用「A 付给 sender 的 908 582 747 241 243 728」
去敲第二跳的价，而第二跳实际只收到 837 259 001 582 806 097（到达率 9215/10 000）。**这个 9215 bps
与同一区块里那笔真实套利 tx 的日志实测值完全相同**（`data/simulation-m4/execution-evidence.json`
的 `transfer_tax`：3 % + 5 %，`combined_retention_basis_points = 9215`）—— 一边是从历史成交
日志夹逼出来的，一边是本次执行的状态变化算出来的，两条独立路径给出同一个数。

容量 687 140 046 135 不是从公式推出来的：`the_highest_ask_the_pair_will_meet_is_the_capacity_it_pays`
对 `[1, 744486240802]` 做了 **40 次实跑二分**，两端都跑过 —— 687 140 046 135 完成、
687 140 046 136 被池子 revert。这是「合约自己的数」而不是「我们从储备量导出的数」，
它恰好等于公式对真实到达量的报价，这才构成 §38 要的交叉校验。

## 7. Gas

| 档位 | gas_used | effective_gas_price | gas_cost |
| --- | --- | --- | --- |
| ask = 解析值（revert） | 340 703 | 300 | 102 210 900 = 340 703 × 300（序列未完成，所以不进 net_profit） |
| ask = 1 | 423 502 | 300 | **127 050 600** |
| ask = 687 140 046 135 | 423 598 | 300 | **127 079 400** |
| OOG（上限 89 864/步） | 190 761 | 300 | 未计价（序列未完成） |

- **§29：价是历史区块声明的，不是猜的。** `GasPricing::Eip1559 { priority_fee_per_gas: 0 }`，
  base fee 取 header 自己的 `base_fee_per_gas = 300`；provenance 字段原话是
  「block 37191169's own base fee as the header reports it, with no tip: §44 takes the bidding
  question off the table」。没有动态 gas 定价、没有 priority fee 优化、没有 bribe/bundle 定价（§44）。
- **§30：`gas_cost = gas_used × effective_gas_price`，全程整数。** 423 502 × 300 = 127 050 600，
  423 598 × 300 = 127 079 400，两条都用 python 复核过；结果结构里没有任何 f64。
- **§28：EVM 的用量与钱包的付款分别记账。** 每步上限 30 000 000 与区块上限 60 000 000 都记录在
  结果里；revert/OOG 那两档照样有 `gas_used`（340 703 / 190 761），但序列没完成，所以不进净利润。
- **实跑与 fixture 的 gas 完全一致**：ask=1 在 RPC 上和离线文件上都是 423 502。
- **一个必须写清的边界**：这条链是 OP-stack rollup。同区块那笔真实 tx 的收据带
  `l1GasUsed = 0xe2c`、`l1GasPrice = 0x38758f9f`、`l1BlobBaseFee`、`l1Fee = 0x3e00eca01`、
  `blobGasUsed = 0x16120`。这些钱**不经过 EVM 的 native 账本**，因此 M4 算出的 gas_cost 只是
  EVM 自己扣掉的那一份；把 L1 费折进来需要一条能证明它的通道，本轮没有，所以没有折
  （见第 12 节）。

## 8. Profit

| 档位 | analytical gross profit（M3 定义，未改） | simulated gross（§27：simulated_output − input） | gas cost | net profit |
| --- | --- | --- | --- | --- |
| ask = 解析值 | 29 641 519 810 | 无（未执行完） | 有量无价不进 net | `NotComputable` |
| ask = 1 | 29 641 519 810 | **−714 844 720 991** | 127 050 600 | **−714 971 771 591**（`Shortfall`） |
| ask = 容量 | 29 641 519 810 | **−27 704 674 857** | 127 079 400 | **−27 831 754 257**（`Shortfall`） |
| ask = 1，不声明价 | 同上 | −714 844 720 991 | 无 | `NotComputable`（理由引用 §29 的缺价声明） |

- **§31/§32：净利润只在计价单位能被证明时才存在。** 两条完成档位的 `net_profit` 都带
  `Denomination`，其 `proved_by` 是执行自己产出的句子（原文）：
  `withdraw(687140046135) executed on 0x4200000000000000000000000000000000000006 at step 9 of a
  12-step plan, between the native balance reads that open and close it; the sequence was funded
  by an executed deposit() of the input amount`。
  平衡式 `converted + native_start == native_end + native_spent + gas_paid` 由
  `NetProfit::compute` 检查；ask=1 那档代入的是
  converted 1、native_start 822 844 720 992、native_end 107 872 949 401、native_spent 714 844 720 992、
  gas_paid 127 050 600。
- **没有把 1 native 当 1 WETH。** 换算量是「执行里 `withdraw` 出来的那 1 wei / 那 687 140 046 135 wei」
  本身，即 `converted`，不是任何汇率假设；`token` 字段写的是 WETH 地址而不是 native。
- **§33：完成但不赚钱 ≠ 失败。** 两条 `Shortfall` 都是 `Completed` 状态；把价撤掉之后同一次跑的
  `gas_used` 一模一样（423 502）但 net 变 `NotComputable`，`a_run_with_no_declared_price_measures_gas_and_reports_no_net_figure`
  的打印原话：`no price declared: gas used 423502 (identical to the priced run's 423502), native 714844720992 → 1, output 1, net refused`。
- **§4 的定义没有为了对齐而改。** M3 的 `gross_profit` 保持原义，M4 只是另加一个
  `simulated gross = simulated_output − input`；`git diff` 里 `crates/opportunity` 一行未动（§69）。

## 9. Risk

`crates/risk`（1 009 行源码 / 3 个文件）只做 §45 的三条检查，一条不多：

```
RiskRule::SimulationSuccess   —— 序列是否 Completed
RiskRule::MaximumGas          —— 实测 gas_used 是否超过上限（未计价的跑也照样管）
RiskRule::MinimumNetProfit    —— net_profit 是否严格大于最小净利
```

判定顺序是 status → 上限 → net（`RiskThresholds::evaluate`）。真实数据上的实跑答案（原文）：

| 输入 | 判定 | reason 原文（截取） |
| --- | --- | --- |
| ask = 解析值 | `Reject` on `simulation_success` | `step 7 (nonce 6) from 0x953E7e98… to 0x5bef6275…: swap(uint256,uint256,address,bytes), value 0, selector 0x022c0d9f: reverted — K` |
| ask = 1 | `Reject` on `minimum_net_profit` | `the sequence came back 714844720991 short of what it spent before gas and the bill was 127050600, so it is 714971771591 under water — there was never a gross profit for gas to eat into` |
| ask = 容量 | `Reject` on `minimum_net_profit` | 同上形状，数字换成 27 704 674 857 / 127 079 400 / 27 831 754 257 |
| ask = 1，无价 | `Unknown` on `minimum_net_profit` | 引用 §29 那句「没有声明价」，理由原样带出执行的说法（`no price`） |

- **§46 的第三种答案是被实跑证明为「知识问题而不是交易问题」**：同一次跑，把价声明回来就是
  `Reject`，撤掉价就是 `Unknown`，`gas_used` 一个字节没变。
- **阈值判据挂在真正会执行的那条路径上**（配对对照）：上限设为实测 `gas_used` 时答案是
  `minimum_net_profit`，上限设为实测 `gas_used − 1` 时答案变成 `maximum_gas`，reason 里引用的
  就是那个被实测到的数（`the_gas_ceiling_is_on_the_path_that_decides`）。
- **不存在任何阈值组合能把这条路线变成 Accept**：3 个档位 × 3 个净利下限全跑一遍，
  判定与 `(status.completed(), net_profit)` 逐一吻合，从未出现 `Accept`
  （`no_threshold_this_route_could_satisfy_turns_it_into_an_accept`）。
- **§48 的执行器只有两个，且都不碰网络**：
  - `NullExecutor` 对什么都丢弃，连 Accept 也不放行，note 里带 §47 那句话；
  - `DryRunExecutor` 只把 policy Accept 的请求序列化成一行 JSON（`chain_id`、`block`、`targets`、
    `input_amount`、`maximum_gas`、`decision`），本轮两条真实判定都是 Reject，所以实跑到的是
    「拒绝」这条分支，原文：
    `the dry run refused a request the policy did not accept: Reject on minimum_net_profit: …`
    以及 `the NullExecutor was handed a policy Reject and did nothing with it: no broadcast: M4
    simulates. An Accept here means the simulation satisfied the stated thresholds, not that a
    transaction was or may be sent.`
  - `ExecutionRequest::from_run` 的 `targets` 就是路线两跳的池子地址（按跳序），不是另编的。

## 10. Real Historical Evidence

§66 要求「必须能追溯」，逐项给出可核对的来源：

| 要素 | 值 | 来源 |
| --- | --- | --- |
| block | 37191169 / `0x56c628d7102131d35d5bdf6a0ed2c6ec3d4d00edc4419a20641c7b396c48d670` | RPC 的 header，与 `execution-evidence.json` 的 `state_pin.block_hash` 比对后才执行（`real_chain.rs:86-90`） |
| 下一块指向本块 | 37191170 的 `parentHash` == 本块哈希 | 同上文件 `state_pin/next_block` |
| pool A | `0xf487d533cae6cddd0c7e7bbbac084dd04d876578`，Factory `0x5f6e8a56…`（存储槽 3） | token0/token1 = 槽 4/槽 5；储备 = 槽 6 拆包 == `getReserves()` == 两个 `balanceOf(pair)`（四重吻合） |
| pool B | `0x5bef6275607901dcd58160356660151be0637440`，Factory `0x1e594a50…`（槽 3） | 同上四重吻合 |
| 储备量 | A：35 099 900 253 008 / 45 655 538 604 883 371 699；B：36 641 298 079 327 / 43 677 608 078 641 141 054 | 本区块的 `Sync` 日志 index 29 / 34（M2/M3 已举证，本轮直接复用同一区块状态） |
| token | WETH `0x4200000000000000000000000000000000000006`；TTAX `0xcffe7472a7a1a6947f56233854ae91a54c862f62` | 两者 decimals 实测 18 |
| 交易目标 | step 4 → pool A，step 7 → pool B，step 2/3/9 → WETH，step 5/6 → TTAX | 结果里每步都带 `to`/`value`/`calldata`/`selector`（第 4 节表） |
| bytecode | Pair 5393 B ×2（sha256 相同 `75030560bcd62c8b…`）；WETH 2846 B `b0c36013…`；TTAX 4827 B `02833fa6…`，`at = 0x2377e01` | `data/simulation-m4/execution-evidence.json` 的 `code` 段 |
| state | `fixtures/simulation-m4/dump-37191169.json`：6 账户 / 21 槽 / 64 次读取，sha256 `47eaea4bab458527…b9f28a53` | 由实跑的 RPC 响应录制，**同一段历史状态 6 次独立进程录制字节一致** |
| 实跑 ↔ fixture | 把 fixture 里的 `state_source` 这一个字段归一之后，`SimulationResult` **整结构相等** | `real_chain.rs` 末尾新增的断言：`assert_eq!(replayed, floor)`；打印 `the fixture replays this run exactly: 0xd5489173…ad9dbf67 (64 reads, 12 steps)` |
| 真实历史 tx（仅当市场事实，不当答案） | `0x6132a9da…9692ba`，type 0x2，gas_used 172 207，effective price 1 000 300，赚 529 344 897 227 WETH wei | `execution-evidence.json`；§70 明确它不是 ground truth：它在另一个输入量（2 829 357 023 446）上跑，且那时池子的状态与本块不同 |

指纹说明（避免读者把两个数当同一个）：实跑 ask=1 的 fingerprint 是 `0xd5489173…ad9dbf67`，
离线 fixture 同一 ask 的是 `0x0fb7c215…d208c6`。两者**只差一个字段**：`state_source`
（`rpc:chain-91342` vs fixture 的路径）—— fingerprint 覆盖全部字段，所以字符串不同必然指纹不同。
把这一个字段归一之后整结构相等，这条断言写在实跑测试里并通过。

## 11. Fixtures

§65/§39–§42 要求的形状，以及本轮各自由谁满足（全部为真实字节码或真实录制的状态，没有手写
的 mock 池子）：

| 要求 | 载体 | 实测到的证据 |
| --- | --- | --- |
| normal V2 | `fixtures/simulation-m4/dump-37191169.json` 里的两个真实 PancakeV2 形状 Pair | 每一步 `swap` 都是对 5393 B 的真实 Pair 字节码执行；AMM 出价逐 wei 等于公式（第 6 节） |
| transfer-tax token | 同一个 fixture 里的 TTAX（`0xcffe7472…`） | 两次转账税 27 257 482 417 237 311 + 44 066 263 241 200 320 = 销毁槽变化量；到达率 9215/10 000 |
| revert | ask = 解析值 744 486 240 802 | step 7 `Reverted`，100 字节原文保留，message `"K"` 只在确实是 `Error(string)` 时才解 |
| out of gas | 每步上限 89 864 | step 4 `OutOfGas`（第一个 swap 需 94 088），gas 190 761，2 条日志 / 5 步 |
| state mismatch | 两条独立路径 | ① `a_request_named_for_another_state_source_is_refused_before_it_runs`：请求声明 `rpc:chain-91342` 却跑在 fixture 上 → 执行前 `StateMismatch`；② `failure_kinds.rs` 的 `MissingCode` / `MissingState` / `ProviderError` 三种是三种不同答案（`the_three_refusals_are_three_different_answers`） |
| determinism | `the_frozen_fixture_replays_itself_exactly` | 两次进程内 + 一次重读文件，整结构相等、指纹相等；另有排序读回的回归测试 |
| 历史 gas | header 的 `base_fee_per_gas = 300` | `GasPricing::Eip1559{priority 0}`；无价档 `Unknown` |

`failure_kinds.rs` 的 5 个用例覆盖的是「节点/状态给不出答案」的世界，与上面「状态给得出、
执行不赚钱」的世界互不混淆 —— 这正是 §34 要的效果。

## 12. Limitations

任务书 §66 第 12 节点名要写的六条，加上本轮真实存在、不该被报告藏起来的其它几条。

**明确没有做的事（设计上就不做）**

1. **no broadcast**：没有 `eth_sendRawTransaction`、没有私有 relay、没有 bundle 提交（§11）。
   `RiskDecision::Accept` 在这套代码里的语义只是「满足阈值」，§47 的原文随每条执行记录一起写出。
2. **no signer**：没有私钥、没有 KMS、没有硬件钱包、没有签名（§10）。sender 是无代码的 EOA，
   我们并不持有它；整个 workspace 里没有任何签路径。
3. **no bundle**：一次模拟是**同一 sender 的 12 笔交易序列**，不是一笔原子交易，也不跨 sender 打包。
   因此模拟结果隐含一个真实执行不具备的假设：这 12 步之间没有别人插队（第 4 节 step 表的两端
   余额读只证明「序列自己」的收支，不证明市场在两步之间不动）。
4. **no vault / strategy engine / universal router / flash loan**（§14）。资金来自 §18 的余额
   override，不是闪电贷，也不是任何真实钱包。
5. **no dynamic gas bidding**：价是历史区块自己的 base fee + 0 tip（§44），没有 priority fee 优化、
   没有 bribe/bundle 定价。
6. **no live pipeline**：没有 WebSocket，没有任何 `latest` 读取（§49）。每一个输入都是显式区块 +
   显式状态；唯一的实网测试是 `#[ignore]` 的 `real_chain.rs`，它读的是一个固定高度 37 191 169（该区块铸于 2026-09-27T19:11:25 UTC，本报告的 3 天前），不是最新区块。

**本轮证据自身的边界**

7. **一个区块、一条路线。** 结论是「block 37191169 上这条 WETH↔TTAX 两池路线在真实执行里不赚钱」，
   不是「所有路线都不赚钱」，也不是「这个市场没有可执行利润」。M3 在同一块上还报了第二条（TTAX 端）
   机会，本轮没有对它做端到端模拟。
8. **测试网的种子流动性。** pool A 的储备是 0.0000351 WETH / 45.656 TTAX 这个量级；本轮输入
   0.000000714844 WETH。管线成立 ≠ 有钱可赚。
9. **L1 数据费没有计入。** 这条链是 OP-stack rollup，真实交易还要付 `l1Fee`（同区块那笔 tx 是
   `0x3e00eca01` wei 量级），它不经过 EVM 的 native 账本，所以本轮的 `net_profit` 是
   「EVM 账本内的净利」。这一条不影响「亏不亏」的方向（税造成的缺口比它大 4 个数量级），
   但如果以后要拿 net_profit 做决策，必须先把 L1 费挂进同一份证明里。
10. **容量是靠 40 次实跑二分找到的，不是靠搜索策略。** 它证明的是「合约能接受的最高 ask」，
    不证明任何最优输入；把它当策略输出用是过度解读。
11. **没有 fee-on-transfer 为 0 的合成 fixture（§65 H 的字面形状）。** 本轮用 §38 允许的替代形态
    满足它：AMM 层在两条真实 Pair 上逐 wei exact、整条路线差额被两笔转账税完全解释并对着销毁槽
    核过。缺的那一格是「一个不含税代币走完整条路线，解析值 = 模拟值」；如果之后要做，
    最短路径是在这条链上找一个 `transfer` 不扣税的币对（需要先举证它的 fee 语义），
    而不是造一个假池子 —— 假池子证明不了我们对真实字节码的那部分信心。
12. **`Halted` 这条状态从未被真实数据触发。** 它是 §34 计数里的一员，由单元/构造层覆盖，
    实跑路线没走到。
13. **`Simulator` 的 future 不是 `Send`**（REVM 借生命周期所致）。当前所有实跑都是
    `multi_thread` runtime + `worker_threads = 1`，所以没有暴露问题；把它接进任何并发调度之前
    需要单独处理。

## 13. §65 A–Q 逐条对照

| 条 | 要求 | 实测证据 | 结论 |
| --- | --- | --- | --- |
| A | 存在 `crates/simulation` 且职责清晰 | 9 个源文件 / 7 617 行，lib.rs 的边界注释；不依赖 opportunity（dev-only 除外） | PASS |
| B | 真正的 EVM 执行引擎 | revm 43.0.3，`EvmRules::Prague`，async DB | PASS |
| C | Opportunity → SimulationRequest | `tests/support/mod.rs` 的 `route()` + `request()`（M3 类型只出现在 dev/测试路径） | PASS（依赖方向偏离见 §14） |
| D | 真实 Pool + Token + Router/Executor 字节码 | 2×Pair(5393 B)、WETH(2846 B)、TTAX(4827 B)；无 router 是 §14 的设计结果 | PASS |
| E | state 与 opportunity 同块 | `header.number == route.block_number` 断言 + 块哈希核对 | PASS |
| F | success / output / gas_used | 第 5、7 节的三档表 | PASS |
| G | revert / OOG / missing state / state mismatch 可区分 | 7 个 `SimulationError` 变体 + 3 个执行状态，`the_three_refusals_are_three_different_answers` | PASS |
| H | 无税 fixture：解析 ≈ 模拟，最好 exact | AMM 层两条 leg 逐 wei exact；无 fee-0 合成 fixture，差额 100 % 归因并被销毁槽核实 | PASS（§38 的第二形态；见 §12 第 11 条） |
| I | 税 fixture：解析 ≠ 模拟，且证明来自 token 执行 | 两笔税 floor(3 %)/floor(5 %) 求和 == 销毁槽 Δ；与历史 tx 的 9215 bps 独立吻合 | PASS |
| J | simulated gross profit | −714 844 720 991 / −27 704 674 857 | PASS |
| K | gas cost | 423 502 × 300 = 127 050 600（整数） | PASS |
| L | 单位可证则 net，否则 NotComputable | 两条 `Shortfall` 带 `Denomination.proved_by`；撤价即 `NotComputable` | PASS |
| M | risk 至少管三件事 | §45 三条规则 + 配对对照证明判据在路径上 | PASS |
| N | 真实历史机会 M3 → M4 跑通 | `real_chain.rs` 实跑 31–55 s，通过并录制 fixture | PASS |
| O | 模拟确定性 | 整结构 + 指纹相等；fixture 6 次跨进程字节一致（含一处本轮修掉的读回顺序缺陷） | PASS |
| P | 四道关卡 | fmt / check / test(259-0-4) / clippy 全部 exit 0 | PASS |
| Q | 本报告 | 本文件，§66 的 12 节齐全 | PASS |

## 14. 与任务书的偏离（§71：记录，不改范围）

1. **§8 的 `to`/`value`/`calldata` 在结果里，而不在请求里。** 它们是执行事实（一步真实跑了什么），
   由 `ExecutedStep` 记录，而不是请求方声称的输入。
2. **§29 的 `ChainProfile` → 显式声明的 `EvmRules` + header。** 本轮用 `EvmRules::Prague` 与区块
   自带的 base fee，不引入「链档案」这一层配置对象。
3. **§18/§57 的资金形状**：实跑没有用 native 直接给 sender 买币，而是 `Funding::WrapNative`
   （带 `msg.value` 的 `deposit()`），这样 §32 的计价单位证明能由执行自己产出。
4. **§34 多了一个 `Halted` 状态**：任务书列的失败种类之外，EVM 会给出「被停止」这一类，
   保留它比塞进 `Reverted` 诚实。
5. **§7 的 `Simulator` trait 的 future 不是 `Send`**（REVM 的 API 形状所致）。
6. **`BlockContext` 多了 `excess_blob_gas`**：blob 规则需要它；缺失时引擎拒绝 Cancun 及以后的
   跑，而不是替它填 0。
7. **§48 的 `Executor::execute` 返回 `ExecutionResult` 而不是 `Result<…>`**：「拒绝执行」是一个
   需要被记录的值，不是一个错误。
8. **§45 的 risk 层不接 `&Opportunity`**：依赖方向只允许 `risk → simulation`；机会字段是经由
   `SimulationResult.plan_summary` 进入判定的。为此 `evm-simulation` 在 **dev-dependencies**
   上反向依赖 `evm-risk`（cargo 允许 dev 环），把真实数据的判定放在有真实数据的那一侧。
9. **§35 的一条实现事实**：`Error(string)` 的选择器 `0x08c379a0` 由测试从签名字符串现算再对照，
   因为直接抄常量正是上一轮被抓到的那类错误。
10. **§36 的状态审计不来自 REVM 的 journal**（原因见第 5 节最后一条：journal 是每笔交易的窗口，
    多步序列会丢历史；改为读回 pin 值）。
11. **§65-H 用 §38 的替代形态满足**（第 12 节第 11 条），这是全表里唯一一格不是以字面形状满足的。

## 15. 复现：报告里每个数字来自哪条命令

```bash
# 关卡（P 条）
cargo fmt --all --check                                   # exit 0
cargo check --workspace --all-targets                     # 无输出
cargo test --workspace                                    # 259 passed / 0 failed / 4 ignored
cargo clippy --workspace --all-targets --all-features -- -D warnings   # exit 0

# N + O 的实跑：录制 fixture、打印三档里的两档、断言 fixture 能整结构复放
CC=clang CXX=clang++ CXXFLAGS="-include cstdint" \
  cargo test -p evm-simulation --test real_chain -- --ignored --nocapture
shasum -a 256 fixtures/simulation-m4/dump-37191169.json    # 47eaea4bab4585…b9f28a53（重跑仍一致）

# D/E/F/G/I/J/K/L/O 的离线证据：容量二分（40 次实跑）、税与销毁槽、日志与状态差异
cargo test -p evm-simulation --test dump_replay -- --nocapture

# M 条在真实数据上的判定、Unknown、配对对照、两个执行器
cargo test -p evm-simulation --test risk_decision -- --nocapture

# §34 的「三种拒绝是三种答案」等
cargo test -p evm-simulation --test failure_kinds

# §45 规则层与执行器层的单元测试（11 个）+ 排序读回的回归测试（1 个）
cargo test -p evm-risk
cargo test -p evm-simulation --lib engine::tests
```

实跑需要 `data/simulation-m4/execution-evidence.json` 里那个 archive RPC 可用；离线三条只需要
`fixtures/simulation-m4/dump-37191169.json`，它缺失时会打印出「先跑哪条命令」的提示。

## 16. 交付物

- 代码：`crates/simulation`（9 源文件 7 617 行 + 4 个测试文件与 1 个共享 support 模块 1 955 行）、
  `crates/risk`（3 源文件 1 009 行）、`crates/chain`（状态读取与录制）、`crates/protocol/src/calls.rs`
  （V2 selector 与编码）、workspace 清单与 `evm-risk` 依赖。
- 数据：`fixtures/simulation-m4/dump-37191169.json`（6 账户 / 21 槽 / 64 读取，sha256 `47eaea4b…`）、
  `data/simulation-m4/execution-evidence.json`（Step1 取证：pin、四重吻合、字节码 sha256、
  历史 tx 的税与 L1 费）、`data/simulation-m4/rpc-probe-37191169.json`。
- 文档：`docs/v0.1/M4 Coding.md`（任务书）与本文件。
- 两个提交：实现与 fixture/取证数据一个，文档一个；工作树在文档提交后干净；不推送任何远端。
