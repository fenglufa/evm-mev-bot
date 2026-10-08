# M10 Arbitrage Executor Contract（原子执行原语）— 完成报告

里程碑：M10（Arbitrage Executor Contract + 确定性执行计划 + 现有执行生命周期接入 + REVM 真字节码模拟 + 真实 GIWA 原子执行与失败回滚）
任务书：`docs/v0.1/M10 Coding.md`（§1–§71）
语义审计底稿：`docs/v0.1/M10 Semantic Audit.md`（写码之前锁定的事实，§4 十个问题全部现量）
证据目录：`data/evidence/m10/`（32 个文件 = 31 个 JSON + 1 个 README，§60）
夹具目录：`fixtures/simulation-m10/`（两份：追加段 + 合成后的整夹具）
基线 commit：`66b421d`（M9.4 完成并推送之后）；证据内登记的 commit 为 `66b421de57e5e8db2836ec974d20a450533aa482`
判定：见 §30

---

## 1. Executive Summary

**白话版（先看这一段）**

前面九个里程碑做的事，本质上是「在链上找到一条可能赚钱的环形兑换路线，并算出它值不值」。
M10 要解决的是下一个问题：**这条路线怎么一次性下注**。

原来的做法要发好几笔交易：先在 A 池买，再在 B 池卖。问题在于这两笔之间有缝隙——第一笔成功了、
第二笔失败，钱就变成一堆你不想要的币卡在你手上；第一笔成功后市场动了，第二笔就被别人夹走。
缝隙是没法靠「算得更准」消除的，只能靠**把它压进同一笔交易**消除。

M10 交付的就是这个「压进一笔」的容器：

- 一份 425 行的 Solidity 合约 `contracts/ArbitrageExecutor.sol`：**要么整条路线全部成交，要么当这笔交易没发生过**。
  中途任何一步失败，前面已经赚到的部分也一起回滚，不会留下半截仓位。
- Rust 侧一份**确定性的执行计划**（`ArbitrageExecutionPlan` → `ExecutablePlan`）：路线、金额、下限、收款人一旦通过校验就
  锁死，编码成 580 字节的 calldata，字节哈希与链上真正跑的那笔交易逐字节一致。
- 它**骑在 M6/M7 已有的执行流水线上**（签名、提交、回执、nonce、时效闸门），没有第二条发送通路。

真实链上（GIWA 测试网，chain 91342）跑了 9 笔交易，nonces 57–65：7 笔部署与授权、1 笔真正的两跳套利、
1 笔**故意设高下限**让它在链上回滚。两笔「执行」都留下了可核的账：

- 成功那笔：本金 0.0001 WETH（1e14 wei）进去，毛出 0.000528136 WETH，扣掉本金净得 **0.000428136 WETH**，
  模拟算出的毛出与链上真实到账**差 0 wei**；链上花的 229 302 gas 与 REVM 事先跑出的 229 302 **一个数**。
- 失败那笔：回执 `status = 0`，WETH 余额**一个 wei 都没动**，只付了 56 723 gas 的手续费——这就是「原子性」在真链上的样子。

**但这一轮不能声称赚钱。** 赚到的 0.000428136 是 WETH，付掉的手续费 0.000000229365 是链的原生币，
这一把没有喂任何价格预言机把两者换成同一把尺子，所以「净赚」这个数**算不出来**，
证据里 `realized_profit = null`、`real_profitable_arbitrage = NOT_PROVEN`。
另外，成功那笔在链上带的最低交付门槛是 1 wei（不是利润门槛），所以「合约在真链上替我挡住了不划算的成交」
这一条也只是在 REVM 和夹具层被证明，不是在真链上。

一句话结论：**M10 交付并证明了「原子执行」这件能力本身（含真链上的成功与失败两种落地），
没有证明「这套东西能赚钱」，也没有证明任何生产就绪性。**

**判定**：`M10 STATUS = COMPLETE`（§30；§68 的四组判据逐条给出处）。
`REAL_PROFITABLE_ARBITRAGE = UNKNOWN`（§68 明确写：这一项不成立不阻塞代码完成，其他判据齐了就算完成）。

---

## 2. Scope（本轮实际交付）

| 交付物 | 位置 | 量 |
|---|---|---|
| 执行合约 | `contracts/ArbitrageExecutor.sol` | 425 行 / 21 407 字节 |
| 编译产物（solc 直出，未改写） | `contracts/artifacts/` 四个文件 | abi 8 132 / bin 15 130 / bin-runtime 14 694 / signatures 1 969 字节 |
| 编译与复现说明 | `contracts/BUILD.md` | 5 节 |
| 执行计划模型 + calldata | `crates/execution/src/arbitrage.rs` | 1 637 行（单测 11） |
| 部署会话（创建交易通路） | `crates/execution/src/deploy.rs` | 831 行（单测 7） |
| 合约 ABI 侧的 Rust 镜像（selector / 编解码 / revert 解码） | `crates/protocol/src/executor.rs` | 1 605 行（单测 9） |
| REVM 侧执行合约的模拟层 | `crates/simulation/src/executor.rs` | 793 行 |
| 集成/回归测试 | 6 个 test target + 1 个共享模块 | 58 个测试函数（其中 2 个 `#[ignore]`） |
| 确定性夹具 | `fixtures/simulation-m10/` | 合成夹具 134 403 字节 |
| 证据目录 | `data/evidence/m10/` | 31 个 JSON + README |

M10 新增测试函数合计 **85**（单测 27 + 集成 58）。词面 `#[ignore]` 命中 2 处，与 cargo 报的 2 个 ignored 一致
（本文件所有「词面命中」类数字都包含自指：报告自身也会命中一次扫描词）。

---

## 3. Non-goals（本轮明确不做）

任务书 §2/§41/§44/§53/§69 划的线，一条都没越：

- **不做搜索、不做定价、不做 sizing**：合约与计划模型都不计算「该换多少」。路线是别处（M9.3/M11）算好后传进来的参数。
- **不做闪电贷**：输入来自操作员自己的钱包（`transferFrom` + 预先授权），不是借来的。
- **不做 flashloan / 不做 oracle 依赖 / 不做多签 / 不做角色系统**：权限就一个 `operator` 地址，一行能读完。
- **不做 M11 的 amount optimizer、不做 3-hop、不做 multi-hop simulation**：合约支持最多 4 腿（`MAX_LEGS = 4`），
  但本轮真实跑的是 2 腿。
- **不做 Flashblocks 执行**：M9.4 的早雷达与本里程碑没有任何代码耦合（§26 会显示 `crates/live` 生产文件零改动）。
- **不做私池 / private sequencer / 竞争延迟优化**。
- **不做资金管理、HA、24×7 runtime、Telegram/webhook/KMS、生产告警**。
- **不引入新框架**：没有 Foundry、Hardhat、npm 包（§4 的实测决定用最小方案：一个固定版本 `solc` 二进制 + 一条命令）。
- **不新增依赖**：`Cargo.toml` / `Cargo.lock` 本轮零改动（见 §26）。

---

## 4. Repository Audit（§4 的实测结论）

审计底稿逐条现量，这里只取结论与「所以本轮怎么做」：

| §4 的问题 | 现量结果 | 因此 |
|---|---|---|
| 已有 Solidity 合约？ | 无。顶层只有 `config/ crates/ data/ docs/ fixtures/ target/ tests/`，`find . -name '*.sol'`（排除 target）0 行 | 合约从零写 |
| 已有编译工具链？ | 无。`foundry.toml`/`hardhat.config.*`/`package.json`/`remappings.txt` 全部不存在；`forge cast anvil solc` 四个命令全 ABSENT；`Cargo.lock` 无 solc/foundry 条目 | 最小方案：固定版本 `solc` 二进制放 `target/tools/`（被根 `.gitignore` 忽略，不进版本库），产物进 `contracts/artifacts/` |
| 已有部署脚本？ | 无。`grep -rin deploy crates/ --include='*.rs'` 32 命中，去掉 `predeploy`/`deployed` 两种词形后 **0 命中** | 新增 `deploy.rs` 一条创建交易通路 |
| 已有 ABI 生成流程？ | 无 | `protocol/src/executor.rs` 用 `alloy_sol_types::sol!` 声明 + solc 产物四文件对账 |
| 已有 executor/router fixture？ | 无 executor；pairs 状态以 `StateDump` 形式存在于 `fixtures/` | 夹具 = 既有 dump + M10 追加段（§14、§22） |
| builder 怎么定 `to/data/value` | 三字段全部来自 intent，`builder.rs:147-153` 明确拒绝 `target == 0`；但编解码层 `tx.rs:69/514/530` 本来就支持 creation | 部署走**不经过 intent 那道非零检查**的构造路径，复用既有 codec / signer / submitter（§5） |
| REVM 怎么给 sender 补钱 | 覆写只允许作用于 sender（`request.rs:473-504`），且带 code 的 sender 被拒（`engine.rs:529-536`）；`Output::Create` 在模拟里报错（`engine.rs:941-948`） | 模拟用 dump 预置 runtime bytecode，**不在 REVM 里部署**；绝不伪造池子状态 |
| receipt / profit / sequence 记录 | 已存在且带完整语义 | 直接复用，不新建第二套（§5） |
| gas / L1 fee | M7 已把 L1 fee 正式纳入 profitability | 沿用同一套费用口径（§21） |

**依赖图审计（§3）**：M10 的接入点全部落在 `execution / protocol / simulation` 三个 crate 内，
没有新增 crate，也没有让 M9.x 的任何 crate 反向依赖执行侧（§26 的 diff 会证明这一点）。

---

## 5. Existing Execution Reuse（没有重写任何已有通路）

§68 的三条「no duplicate」判据，出处如下——`crates/execution/src/deploy.rs:40-55` 的 import 清单本身就是证据：

| 能力 | 复用的既有件 | M10 新增了什么 |
|---|---|---|
| 签名 | `crate::signer::Signer` | 无。部署会话拿同一个 `Signer` trait |
| 提交 | `crate::submitter::SubmissionOutcome` / `TransactionSubmitter` | 无 |
| 回执 | `crate::receipt::{ExpectedTransaction, Receipt, ReceiptPolicy, ReceiptStatus, ReceiptTracker, TrackedReceipt}` | 无 |
| nonce | `crate::nonce::NonceAllocator` | 无 |
| 时效/绑定闸门 | `crate::gate::{Freshness, BlockBinding, BalanceEvidence}`，M10 只加 `PlanBinding` | 一个新闸门变体（§16） |
| 阶段流水线 | `crate::stage`（M8.4.x 的 stage 依赖矩阵登记过的那些阶段） | 让执行阶段接受 plan 构造出来的交易，不改阶段顺序 |
| 编解码 | `crate::tx`（`to: None` = creation 早已支持） | 无 |
| REVM 引擎 | `crate::simulation::engine` / `gas` / `result` / `state` | 一层执行合约专用的模拟封装 |

**没有**第二个 signer、**没有**第二个 submitter、**没有**第二套 receipt 系统——这三句是可指的：
`crates/execution/src/` 里 `Signer`/`TransactionSubmitter`/`ReceiptTracker` 各自只有一处定义，
M10 的两个新文件全部是 `use crate::…`。

同样地，合约侧「不引入大型依赖树」（§20）：整份 `.sol` 只有两个 `interface` 声明（IERC20、IV2Pair），
没有 import 任何第三方库，手写 6 行重入锁代替 OpenZeppelin。

---

## 6. Contract Architecture

`ArbitrageExecutor.sol` 的骨架（§9–§22）：

```
constructor(address operator_)            → 只设一个 operator，地址为 0 直接 ZeroAddress
setOperator / setPairAllowed / setTokenAllowed   → 全部 onlyOperator
execute(Leg[] legs, inputToken, amountIn, minFinalAmount, recipient)
        onlyOperator nonReentrant
   → _checkRoute     形状 + 上限 + 白名单 + 连续性 + 往返闭合，全部在任何转账之前
   → _pullInput      transferFrom 操作员钱包，delta 校验
   → 逐腿 _deliverLeg(index, leg, carried)
          _checkSides（问池子要 token0/token1，不自己排序）
          _pushInput （持有量 == 计划 amountIn，池子收到的 delta == amountIn）
          _pullOutput（swap 后 balanceOf 差值 == amountOut，且 ≥ minAmountOut）
   → _settle        交付 inputToken 给 recipient，交付量以收款人侧 delta 计，且 ≥ minFinalAmount
withdraw(token, to, amount)  onlyOperator nonReentrant   → 救援通路，同样按 delta 计量
```

设计上的两个「反直觉但故意」的点：

1. **每个数都是 claim，不是 return value。** 合约不看 `transfer` 的返回值判断成功，
   而是前后各读一次 `balanceOf`，把差值与 calldata 里声称的数字对比。
   代价是每次多两读 SLOAD/BALANCE，收益是**转账税代币不可能被静默吃差价**——它只会 revert。
2. **精确相等，不是「至少」。** `_pushInput` 要求合约此刻持有量**正好**等于 `leg.amountIn`。
   留一点 dust 在合约里就意味着「这份余额不再只是这笔交易的事实」，那就不再是同一个可复现的对象。
   （对应测试：`leftover_dust_in_the_executor_is_a_mismatch`。）

`execute` 主体故意只有五行调用：这是 legacy 代码生成 16 槽栈上限逼出来的拆分，
不是风格偏好（§42 的 `Stack too deep` 三次，见 `contracts/BUILD.md` §5）。

---

## 7. Plan Model

`crates/execution/src/arbitrage.rs`：

- `ArbitrageExecutionPlan`（§5）＝路线 + 三个「它为什么存在」的理由：
  模拟所在的块、模拟自己的答案、合约将要执行的那条利润策略。
- `validate()`（§15/§16/§12/§14/§7/§36）的规则**刻意窄**：只检查合约在链上也会检查的东西，
  外加合约看不见的两个绑定（chain id、executor 地址）和一个没有 opcode 会读的内部一致性
  （计划的下限与它的利润策略必须是同一个数）。
  理由写在模块头上：**离线规则如果链上不执行，那它是文档而不是闸门。**
- `AmountDerivation`（每腿标注「本腿的花费从哪来」）：第一腿 = `PlanInput`（计划的本金），
  之后 = 上一腿 `amount_out` 全额结转。链上同一条规则叫 `AmountChainBroken`，
  离线版本保证「说错 derivation 的计划根本变不出 calldata」。
- `ExecutablePlan`（§6）＝通过校验的计划。字段私有、没有 setter，
  于是「校验之后到节点之前不能被改」是类型而不是承诺；改了就是另一个 `plan_hash`，即另一次执行。
- `ExecutionClass`（§40）保留六种的区分，含三条不等式：**submitted ≠ included，included ≠ successful，
  successful ≠ profitable**。最后一条不由本文件裁定——`IncludedSucceeded` 只说回执 status=1，
  赚没赚仍归 `crate::profit::ProfitVerificationStatus`。

计划身份（本轮真跑那一把，全部来自 `data/evidence/m10/manifest.json`）：

| 字段 | 值 |
|---|---|
| `plan_hash` | `0x00c9831d71a87cd9e9257e7ca9983705d02801ba5805eccab6e2ccd084377335` |
| `calldata_hash` | `0x9ca9b1d030a424b741113d13dd204ecd028afa711b95548857affe7ab25c26b9` |
| calldata 长度 | 580 字节 |
| `simulation_hash` | `0x9fae73afd75f1a135452e084677fd1c576eb6e1e7d54ea291e0dfd501713801d` |

---

## 8. Route Model

路线在合约里就是 `Leg[]`，每腿六个字段：`pool / tokenIn / tokenOut / amountIn / amountOut / minAmountOut`。
两个字段各管一件事，不能合并：`amountOut` 是**向池子要的确切数**（V2 的 exact-out 语义），
`minAmountOut` 是**本腿继续下去的下限**；`amountOut < minAmountOut` 是畸形计划（`AskBelowFloor`），
不是乐观计划。

`_checkRoute` 依次判：非空（`NoLegs`）、不超上限（`TooManyLegs`，`MAX_LEGS = 4`）、每腿形状与白名单、
腿间连续性（`BrokenContinuity`）、末腿必须回到 `inputToken`（`NotRoundTrip`，§17 的规则：
终点不等于起点的路线是对冲缺口，不是套利）。**全部在任何 token 移动之前完成**——
一条非法路线的代价是一次调用的 gas 和零余额变动。

本轮真跑的路线（`data/evidence/m10/real/giwa_execution.json` 的 `route` 段）：

```
WETH(0x4200…0006) → buy  0x2a3ceafb…d9a4 → token0(0x07d4af6e…f26f)
token0           → sell 0x5b3c1e3f…353e → WETH
ask leg1 = 63 958 436 793 687 409 358
ask leg2 =    528 136 078 126 613   (= 0.000528136 WETH)
```

---

## 9. Access Control

- `address public operator` —— 单地址，不是角色系统。`onlyOperator` 是 3 行：不相等就
  `revert NotOperator(msg.sender)`，把调用者地址回出来当证据。
- 执行和救援**共用同一个权限**：`execute` 与 `withdraw` 都是 `onlyOperator nonReentrant`。
- 换 operator 只有一条路：`setOperator`，同样 `onlyOperator`，且禁止置零；换人发 `OperatorSet(previous, next)`，
  所以「谁在什么时候把钥匙交给谁」在链上可查。
- 部署时 `constructor(operator_)` 一次性设定，没有「未初始化时可被任何人抢」的窗口。

证明位置：`an_unauthorized_caller_is_refused_by_the_access_control`（REVM 层，`NotOperator`）、
`a_withdrawal_from_anyone_but_the_operator_is_refused`、`a_withdrawal_of_the_zero_token_never_reaches_the_contract`、
`a_withdrawal_of_nothing_or_of_more_than_is_held_is_refused`，
以及 planted control `data/evidence/m10/negative_controls/wrong_operator.json`（verdict PASS，expected = observed = `NotOperator`）。

---

## 10. Token / Pair Allowlist

- `mapping(address => bool) public pairAllowed` / `tokenAllowed`。
- **腿的两端都必须在名单里，input token 也要**（`_checkLeg` + `execute` 头部各查一次）。
- 检查发生在 `_checkRoute` 阶段，即任何 `transfer`/`transferFrom` 之前。
- `setPairAllowed` / `setTokenAllowed` 只有 operator 能调，禁止零地址，各自发事件。

这条白名单是「不任意调用」的正面答案：合约里**没有任何一个 `address` 参数是未经白名单核对就被调用的**。
证明位置：`an_unlisted_pair_or_token_is_refused_before_anything_moves`，
planted controls `invalid_pair.json`（`PairNotAllowed`）、`token_not_allowed.json`（`TokenNotAllowed`），
状态层证据 `data/evidence/m10/states/{pair_not_in_the_allowlist,token_not_in_the_allowlist}.json`。

---

## 11. Atomicity

「原子」在这份合约里不是形容词，是三件可分别证伪的事：

1. **一条交易**：整条路线是**一次** `execute` 调用，腿是 EVM 内部函数调用，不是外部交易。任何一腿 revert，
   整笔交易回滚，包括已经成功的前腿。
2. **不留半截仓位**：`forced_second_leg_failure_leaves_no_residue`（§27 的「最重要的一条测试」）——
   夹具里把第二腿强行做成失败，然后逐个账户、逐个 slot 对比：无余额变化、无储备变化、无存储变化。
   对应 planted control `forced_second_leg_revert.json`（expected `reverted` / observed `Error(string)`，PASS）。
3. **真链上也确实如此**：§58 那笔故意把 `minFinalAmount` 设为 `10 000 000 000 000 000`（1e16 wei = 0.01 WETH，
   高于这个市场付得出的任何数），回执 `status = reverted`，`no_partial_state = true`，
   WETH 余额前后**一个 wei 都没动**（操作员 1 693 741 728 651 230、执行合约 0），只付了 56 723 gas。

重入保护是手写 6 行（`_lock`，1 = 解锁、2 = 上锁）而不是引库，`ReentrancyDetected` 独立可分类。
另外合约的外部调用面只有 `swap`（白名单池）和 ERC20 的四个方法（白名单 token），
不存在「回调进来再调 execute」的通路；`a_locked_deployment_refuses_the_route` 另有一层：
状态被判定为 locked 的部署直接被拒。

---

## 12. Slippage Guard

三层，从严到宽，各有一个可指认的失败码：

| 层 | 判据 | 失败时 |
|---|---|---|
| 单腿交付 | 池子付来的 balance delta **必须恰好等于** `leg.amountOut` | `DeliveryMismatch`（税币/非标准 token 走这里） |
| 单腿下限 | `received ≥ leg.minAmountOut` | `LegShortfall` |
| 最终下限 | 收款人侧 delta **≥ minFinalAmount** | `FinalShortfall` |

`DeliveryMismatch` 与 `LegShortfall` 被刻意分成两个错误（§12/§21/§22 的要求）：
前者是「这个池子付的东西不是我要的那份」，后者是「付对了但不够格继续」。证据必须能区分这两种失败。

**为什么「精确相等」不是自找麻烦**：V2 的 `swap` 是 exact-out，池子内部本来就会用 K 不变量兜底；
合约仍然要求差额恰好为零，是因为它不信任「池子说了算」的那部分——被夹、被前后跑、池子被人换成别的地址，
都会表现成交付数与要数不等。

真链上这一条是**被验证过它真的会咬人**的：§58 那笔的 `FinalShortfall` 回滚数据
`0xec5122fa…`（在模拟记录里可读）里带的就是「实付 428 136 078 126 613 vs 门槛 10 000 000 000 000 000」。

---

## 13. Profit Guard

利润闸门的**表达形式**是 `minFinalAmount`（最终必须交付多少 input token），
它把「赚不赚」变成「够不够」——因为合约看不见价格，只能看见数量。

三条边界写清楚（§14/§12 的本意）：

1. `minFinalAmount` 必须非零（`execute` 头部 `ZeroAmount`），所以「一次什么都没要求就成功的跑法」
   不存在于类型里。
2. 计划的 `min_final_output` 与它的利润策略必须说同一个数，`validate()` 在离线就拒不一致的（§16 的 `PlanBinding`）。
3. **利润 ≠ 同一把尺子里的两个数相减。** 本轮真跑的路线以 WETH 结账，手续费以链的原生币结账，
   这一把没有喂 oracle 把两者换算，所以 `realized_profit` 只能是 `null`（不是 0，不是 false）。
   证据原话：`the gain is WETH, the fee is the chain's native asset … this run prices one in the other by no oracle`。

`AmountDerivation`、`NotRoundTrip` 和这条利润口径合起来，才是「利润守卫」的完整含义：
**守卫的是「这笔兑换闭合且交付量达到门槛」，而不是「保证你赚钱」。**

---

## 14. REVM Simulation

§25/§55 要求的六组结果，全部在真实 bytecode + 真实 pair 状态上跑（不是 mock 池子）：

| 组 | 结果 | 出处 |
|---|---|---|
| 成功 2-hop | `Success`，`delivered = 528 136 078 126 613`，`gas_used = 229 302` | `successful_two_hop_route_profits_on_real_bytecode` |
| 合约层 revert | 各错误码逐一复现（`NotOperator` / `PairNotAllowed` / `TokenNotAllowed` / `ZeroAmount` / `BrokenContinuity` / …） | `crates/simulation/tests/executor_revm.rs` 32 个测试 |
| min-output 拒绝 | `LegShortfall` / `FinalShortfall` | `second_leg_asking_low_leaves_the_floor_unmet`、`final_floor_above_the_priced_output_reverts_final_shortfall` |
| 利润拒绝 | `FinalShortfall`（夹具层 `floor_too_high`） | 同上 + `fixtures/profit_guard.json` |
| 余额/授权不足 | 在**任何转账之前**被拒 | `insufficient_input_balance_is_refused_before_any_transfer`、`insufficient_allowance_is_refused_before_any_transfer` |
| 确定性双跑 | 同一次调用两次跑，整个 observed block 逐字节 JSON 相同 | `two_runs_of_the_same_call_are_byte_identical`（§49 D3 的另一半跨进程形式在门里） |

字节码怎么进 REVM：把 solc 的 runtime 字节塞进既有 `StateDump` 的账户 code 槽（`fixtures/simulation-m10/` 的追加段），
由 `ProviderDb::basic_async` 验 `code_hash` 后交给 REVM。**没有在 REVM 里部署**（§4 审计：`Output::Create` 在模拟层报错）。
测试 `the_deployed_code_is_the_committed_artifact` 与 `fixture_rebuilds_the_committed_bytes`
把「模拟跑的字节 == `contracts/artifacts/` 的字节 == 链上 `eth_getCode` 的字节」钉成一条。

执行封装 `crates/simulation/src/executor.rs` 提供 `ExecutorRun / ExecutorOutcome / Watch / BalanceRow / ReserveRow / ReserveSnapshot`，
其中 `Watch` 是「跑完之后逐账户、逐 slot 对比」的采集面，§11 的零残留判据就是它喂出来的。

---

## 15. Negative Controls

两组，各管各的层次，不能混着报：

**A. 任务书 §26 的 NC1–NC12（模拟必现的拒绝）**

| NC | 条目 | 承载测试（本轮实测绿） | 源码内是否带 §26 标注 |
|---|---|---|---|
| NC1 | wrong chain | `a_run_that_lies_about_where_it_is_is_refused` | 是（`executor_revm.rs:1088`） |
| NC2 | stale plan | 同上 + `a_stale_plan_is_blocked_by_the_existing_freshness_leg`、`a_stale_attempt_stops_at_the_gate` | 是 |
| NC3 | unauthorized operator | `an_unauthorized_caller_is_refused_by_the_access_control` | 是（`:681`） |
| NC4 | broken token continuity | `broken_continuity_open_routes_and_an_empty_route_are_rejected` | 是（`:828`） |
| NC5 | wrong token | `an_unlisted_pair_or_token_is_refused_before_anything_moves` | 否（映射由本报告给出） |
| NC6 | insufficient input balance | `insufficient_input_balance_is_refused_before_any_transfer` | 是（`:645`） |
| NC7 | insufficient allowance | `insufficient_allowance_is_refused_before_any_transfer` | 是（`:661`） |
| NC8 | min output violated | `second_leg_asking_low_leaves_the_floor_unmet` | 否（映射由本报告给出） |
| NC9 | final profit invariant | `final_floor_above_the_priced_output_reverts_final_shortfall` | 否（映射由本报告给出） |
| NC10 | pair swap revert → 整笔回滚 | `forced_second_leg_failure_leaves_no_residue` | 否（映射由本报告给出） |
| NC11 | malformed calldata | `broken_continuity_open_routes_and_an_empty_route_are_rejected`、`claims_that_do_not_chain_are_rejected` | 是（`:829`） |
| NC12 | zero amount | `zero_amounts_are_refused` | 是（`:784`） |

诚实标注：12 条里 7 条在源码注释里自带 `§26 NCn` 标签，其余 5 条的「哪条测试负责它」是本报告的映射，
不是代码里的自证。

**B. 任务书 §50 要求的十项 planted controls**（落盘 `data/evidence/m10/negative_controls/`，十份 JSON，每项 verdict = PASS）

| control | 层 | expected | observed |
|---|---|---|---|
| wrong chain | plan | `ExecutionError::PlanRejected`，码 `wrong_chain` | 一致，且**恰好只有这一个拒绝码** |
| wrong executor | plan | 同上，码 `wrong_executor` | 一致 |
| wrong operator | revm | `NotOperator` | 一致 |
| broken route（token continuity） | revm | `BrokenContinuity` | 一致 |
| invalid pair | revm | `PairNotAllowed` | 一致 |
| token not allowed | revm | `TokenNotAllowed` | 一致 |
| zero amount | revm | `ZeroAmount` | 一致 |
| min output | revm | `FinalShortfall` | 一致 |
| final profit | revm | `AskBelowFloor` | 一致 |
| forced second-leg revert | revm | `reverted` | `Error(string)`（PASS，判定按「回滚且零残留」） |

两项 plan 层控制的做法值得记一句：它们**改的是执行绑定**（chain id 挪到 91343、executor 末位 hex 挪一位），
路线本体一个字没动，并且各自带一个 `unmutated_twin`（未变异的孪生跑通、构造成功）。
这样「拒绝来自绑定不符」而不是「拒绝来自我顺手改坏了路线」才是可证的。

门里的汇总（`recompute/deterministic.json`）：`all_ten_answered = true`，
`expected_contract_layer = 8` + `expected_plan_layer = 2` = 10，`planned_by_this_gate = 2`。

---

## 16. Execution Lifecycle

M10 接进 M6/M7 的流水线，而不是在旁边另搭一条。改动只有两处语义：

1. **`gate.rs` 新增 `PlanBinding`**（§16 的新闸门变体）：带 `describe()` 与 `has_simulation()`，
   把「这份计划绑定在哪个块、哪次模拟答案上」变成流水线上一个会被别的闸门读到的事实。
2. **`lifecycle.rs` 新增 `attach_route_id()`**：把路线身份挂到 run 记录上，
   于是「同一个 route 的两次不同金额尝试」在记录里是两条共享一个身份的行，而不是两行无来源的数。

`stage.rs` 侧接的是 `crate::arbitrage::{ExecutablePlan, ExecutionBinding}`：执行阶段可以接受一份 plan 构造的交易，
阶段顺序、既有 freshness/余额/绑定判定一律不动（回归里 `stage_matrix` 15 项、`lane_matrix` 14 项、
`sequence` 44 项全绿即为证）。

本轮真跑那一把走完全程（`real/giwa_execution.json` 的 `report.lifecycle`）：

```
created_at 2 224 ms → built_at 2 224 → signed_at 2 897 → submitted_at 3 066 → included_at 5 771
status = included   lane = released   mode = submit   transaction_type = dynamic_fee   value = 0
opportunity_id / route_id / simulation_id / risk_decision_id 四个身份全部落进记录
```

M10 自有生命周期覆盖：`executor_lifecycle.rs` 9 项 + `executor_deploy.rs` 12 项。
其中 deploy 那 12 项的意义是**「拒绝」也能被测**：一个可能广播的会话在构造期就被拒、
head 挪了就停在任何定价之前、钱包付不起上限就不发、nonce 一步不结算就不交出去——
这些分支在没有真链的时候无法证明，所以全部对着脚本端点跑，脚本答案的形状取自 M6 实测的
`data/evidence/m6/probe-read-surface-2.txt` / `probe-submission-surface.txt`（chain 91 342、base fee 371 wei、
tip 1 000 000 wei、回执带 L1 fee 字段）。

---

## 17. Deployment

`data/evidence/m10/contract/{bytecode_hash,deployment}.json`：

| 项 | 值 |
|---|---|
| 编译器 | `solc 0.8.37+commit.f401782d`（本地 sha256 与上游清单逐字相同，keccak256 另比一次） |
| 产物身份 | `ArbitrageExecutor.bin` sha256 `69ae3d93…9d9a2680`（写进每一条真链证据的 `abi_version`） |
| creation code | 7 565 字节，keccak256 `0x8c49d87cc6173d83a36c97fd1f919555143e431b4124fea08368054a676c7263` |
| runtime code | 7 347 字节，keccak256 `0x917ed914c157e86958d0b2de9d85a62a3a62c381b49166e091e3b67255d42cca`（< EIP-170 的 24 576） |
| 部署交易 | `0x226c71f642a1febed9cab35af945486aca88d6ab47ec393f204979da40023eb7`，nonce 57，input 7 597 字节，`to` 为空 |
| 定价 | eip1559：`maxFeePerGas = baseFee(274)*2 + tip(1 000 000) = 1 000 548`，在块 38 023 916 读到 |
| gas | 上限 5 000 000，最大可花 `5 002 740 000 000` wei（≈ 0.005 002 74 原生币） |
| 回执 | block 38 023 919，`gas_used 1 691 500`，`effective_gas_price 1 000 274`，L1 fee 6 803 wei，L2 成本 1 691 963 471 000 wei，1 条日志，`status = included` |
| 地址预测 | `predicted == actual == 0x1fd5512eb6d2c56d5d1fface550013bad9f78517`，`address_matches_prediction = true` |
| 恢复出的发送者 | `0xd450630c1c55b1c7df1ebf7eeaee1fffb45e520c`（= operator） |

`contract_hash_readings` 那条把「读证据的人怎么自查」写明了：只拿部署交易就能重建 creation code、
算出 `0x8c49d87c…`，再用 `eth_getCode(executor, block)` 还原运行时字节算出 `0x917ed914…`；
manifest 里带的是前者，理由是部署交易的 input 是公开数据而 code 需要一次节点查询。
**本轮没有任何测试去调 `solc` 或 `eth_getCode`**（§51），所以这两把摘要的链上那一次是在 §57 真跑窗口里读的、
落进 `deployment.json`，日常关卡比对的是「源码 ↔ 产物 ↔ 已记录摘要」三者一致（§22）。

---

## 18. GIWA Real Test（§57/§58/§59）

链 91 342，一个 EOA，一段连续 nonce 57–65，共 **9 笔**交易：

| label | nonce | block | gas_used | 状态 |
|---|---|---|---|---|
| deploy | 57 | 38 023 919 | 1 691 500 | included |
| allow-pair-a | 58 | 38 023 923 | 47 833 | included |
| allow-pair-b | 59 | 38 023 928 | 47 833 | included |
| allow-token-0 | 60 | 38 023 933 | 47 800 | included |
| allow-weth | 61 | 38 023 937 | 47 584 | included |
| wrap-principal | 62 | 38 023 940 | 27 832 | included |
| approve-executor | 63 | 38 023 945 | 46 129 | included |
| execute（§57 的受控调用） | 64 | 38 023 960 | 229 302 | included，成功交付 |
| execute-floor-too-high（§58 的受控失败） | 65 | 38 023 967 | 56 723 | **reverted** |

manifest 的 `nonce_ladder = 8` 指的是「7 步 setup + 那笔 execute」；失败那笔记在 `real/giwa_failure.json` 里，
所以 `real_transaction_hashes` 三条（deployment / execute_included / failure_reverted）与九笔总额不冲突，
这一条口径差异写在 `field_provenance` 里，不在报告里悄悄抹平。

前置条件（`real/preconditions.json`）在签名之前读过：head 块 38 023 914、两个池子的 `token0/token1/reserve0/reserve1/blockTimestampLast`
全部现量、原生余额 `35 150 697 468 677 639` wei（≈ 0.035 15 原生币）、本金计划 1e14 wei。
密钥来源只有 `GIWA_EXECUTION_PRIVATE_KEY`，读取一次，**值不打印、不存储、不写进证据**（§19/§35）；
端点来源只有 `GIWA_RPC_URL`，源码里不烧任何 URL（§5）。

`#[ignore]` 的那一项 `the_ladder_runs_on_giwa` 是唯一的真实广播入口，
注释原话：`broadcasts real transactions on a live chain (§57); needs GIWA_RPC_URL and GIWA_EXECUTION_PRIVATE_KEY,
and spends testnet gas on every run`。本轮它只在 §57 的受控窗口里被点过一次，日常关卡不跑它。

**§57 那一把的真实结果**：毛出 `528 136 078 126 613`（0.000 528 136 WETH），
本金 `100 000 000 000 000`（0.000 1 WETH），净得 `428 136 078 126 613`（0.000 428 136 WETH）。
但这次在链上带的门槛是 `minFinalAmount = 1 wei`（不是利润门槛）——
所以「合约在真链上挡住了不划算的成交」这一条**不由这笔证明**，由 §58 那笔（门槛 1e16、回滚、零残留）证明。

---

## 19. Simulation vs Receipt

§71 的四句话，本轮的四组数，一句都不合并：

| 陈述 | 谁说了算 | 本轮的值 |
|---|---|---|
| 这笔交易**应该**执行 | REVM 模拟 | `Success`，`delivered 528 136 078 126 613`，`gas_used 229 302` |
| 这笔交易**已被提交** | 提交层答案 | `submission = submitted`，`sent = true`，hash `0x1105c89b…f82771` |
| 这笔交易**已被打包** | 回执 | `receipt_status = included`，block 38 023 960，hash 绑定 `execution_block_hash 0xd2610226…6192` |
| 这笔交易**确实产出了这个结果** | 余额对账 | 见 §20 |

三条不等式的实测（§40/§52/§59）：

- **submitted ≠ included**：`a_step_without_a_receipt_holds_the_lane_rather_than_retrying`、
  `a_receipt_that_never_arrives_is_a_timeout_and_holds_the_lane`——超时不写成失败，也不自动重发。
- **included ≠ successful**：`an_included_receipt_with_status_zero_is_a_reverted_run`；真链上 §58 那笔
  就是「included + reverted」的实例（`status = reverted`，但确实进了块 38 023 967）。
- **successful ≠ profitable**：`verdicts.real_profitable_arbitrage = "NOT_PROVEN"`，
  `evidence_row_31.realized_profit = null`。

模拟与回执的**同一个数**在这几处对齐（这是「同一次执行」而不只是「看起来一样」的关键）：

| 量 | 模拟 | 链上 |
|---|---|---|
| gas_used | 229 302 | 229 302 |
| calldata hash | `0x9ca9b1d0…5c26b9`（580 字节） | `0x9ca9b1d0…5c26b9`（input_bytes 580） |
| 毛出 | 528 136 078 126 613 | 528 136 078 126 613 |
| 差额 `difference` | — | **0** |

`cause_if_different` 把「如果这一步不等会是什么」也写死了：`state drift`——
计划在某个 canonical 块上定价、在更晚的块上被挖，而合约的要价是精确的，
市场动了就 revert 而不是将就成交。实测这一把的时效窗：定价块 38 023 945、发送时 head 38 023 955，
**相隔 10 块**，声明上限 20 块（`MAX_BLOCK_AGE`），freshness 判定 `Active`。

---

## 20. Balance Reconciliation

`reconciliation_32` 的三段（input / output / route）+ 三段余额快照（`wallet_before`、
`wallet_at_block_before_execute`、`wallet_after`）：

```
WETH 操作员：块 38 023 945 起 1 265 605 650 524 617 → 块 38 023 959 仍 1 265 605 650 524 617
              → 块 38 023 960 到 1 693 741 728 651 230      Δ = 428 136 078 126 613
principal 进入 = 100 000 000 000 000
差值 difference = 0（模拟毛交付 − 链上「净 Δ + 本金」）
外部窗口移动 external_movement_in_the_wait_window = 0
token0 中间币操作员/合约两侧 Δ 均为 0；执行合约 WETH 收尾 = 0；授权 = 0
```

失败那笔（`giwa_failure.json`）的对账更简单，也更有说服力：

```
WETH：前后完全相同（操作员 1 693 741 728 651 230，合约 0，授权 0）
token0：两侧都是 0
no_partial_state = true
residue_read_at_block = 38 023 967（失败回执自己点名的块，不是「最近一次 head」）
```

**四条口径写清楚，避免读者把四件事当一件**（§71）：

1. 「链上到账」＝同一段块区间里 balanceOf 的差，不是回执里的字段。
2. 「模拟说会到账」＝REVM 在这次 pin 块上算出来的数。
3. 「差为 0」＝上两条相减为 0，**不**等于「赚了」——本金要还回去。
4. 「赚了」需要同一个计价单位；本轮单位不统一（WETH 收益 vs 原生币账单），所以第 4 条是 `null`。

`token0_balance_delta = 0` 这一项值得单独指出：中间币种**净零**是「两腿真的闭合」的证据；
如果第二腿只成交一半而第一腿照常，这个数不会为零，而会留下一个没人认领的持仓。

---

## 21. Gas / L1 Fee

沿用 M7 的口径：L1 fee 是成本的一部分，来自回执字段，不是估的。

| 笔 | gas_used | effective_gas_price | L2 成本 | L1 fee | 合计（wei） |
|---|---|---|---|---|---|
| deploy | 1 691 500 | 1 000 274 | 1 691 963 471 000 | 6 803 | 1 691 963 477 803 |
| execute 成功 | 229 302 | 1 000 274 | 229 364 828 748 | 463 | **229 364 829 211**（≈ 0.000 000 229 365 原生币） |
| execute 失败 | 56 723 | 1 000 274 | 56 738 542 102 | 468 | **56 738 542 570** |

两笔 execute 的**钱包实付与账单逐 wei 相等**（本轮现量算术）：

```
成功：35 048 740 421 585 948 − 35 048 511 056 756 737 = 229 364 829 211  = receipt(l2+l1)
失败：35 048 511 056 756 737 − 35 048 454 318 214 167 =  56 738 542 570  = receipt(l2+l1)
```

这条等式是 `what_the_chain_charged_is_what_the_wallet_lost` 那项测试在真链上的对应物；
报告里它是**减出来的**，不是引用的字段。

gas 上限的三个数不许混用（`reconciliation_32.gas` 已写明用哪个）：

| 字段 | 值 | 含义 |
|---|---|---|
| `simulation_gas_used` | 229 302 | 模拟实际花的 |
| `simulation_proved_gas_limit` / `record_gas_limit` | 600 000 | 模拟跑完时用的上限，§39 的记录字段 |
| `signed_transaction_gas_limit` | 620 000 | **真正签出去的那个数** = 600 000 + 声明的 margin 20 000 |

margin 是声明的、不是临时加的；`which_number_was_sent` 明确指向签名的字节里那个字段，
于是「上限是不是发出去之后才改的」这个问题有答案。

**费用与收益不同单位**是 §13 的 `realized_profit = null` 的直接来源：
账单 2.2936e-7 原生币，收益 4.2814e-4 WETH，两个数之间没有本轮建立的换算。

---

## 22. Determinism

**编译侧**（`contracts/BUILD.md`）：一条命令、一个固定版本 solc、四个产物文件不改写；
`cmp` 门禁（同机、两个不同输出目录、四次产物逐字节相同，加上 committed 产物与一次全新编译相比）
是 M10-P1 期间**手工跑的一次性测量**，记录在 BUILD.md §4 里——
仓库里任何测试都不调用 solc（全仓只有 `crates/cli/tests/` 四处在 spawn 子进程，跑的是本项目自己的 CLI）。
日常 cargo 关卡验的是另外两件事：`executor_evidence_gate.rs` 从 `.sol` **源码**重新推导 selector、ABI 条目与
字节码摘要，再和 solc 早先写好的四个产物文件逐字节对账；`the_abi_identity_agrees_with_the_build_record`
把 BUILD.md §3 那把链上摘要与 `deployment.json` 里登记的 `abi_version` 对齐。
本轮复核 BUILD.md 时删掉过一条假引用（原文声称某个 `--ignored` 测试会真跑编译器），
这条修正在 §23 与 §27 各记一次。

**执行侧**（§49 的五道门，`data/evidence/m10/recompute/deterministic.json`）：

| 门 | 由谁回答 | 实测计数 |
|---|---|---|
| D1 | `plan_rebuild`，覆盖每一条已发布的场景行 | 14 行全部重建；14/14 的「字段拼写哈希」与「字节拼写哈希」相同；变异 28 个字段，28 个都改变 `plan_hash` |
| D2 | 执行 crate 的编码器 vs 模拟 crate 跑过的那串字节 | 14/14 字节相等、14/14 keccak 相等、14/14 长度相等 |
| D3 | `d3_replay`：另一个 crate、另一次 cargo 调用 | 9 次 REVM 运行全部重放，9/9 整个 observed block JSON 逐字节相同 |
| D4 | `d4_rejudge`：`decode_revert` 读已发布的回滚字节 + §27 残留判定 | 14 行重判，分类问题 0、判定问题 0、真链失败行问题 0 |
| D5 | `plan_rebuild` + 真跑发布的身份表 | 14/14 行身份点名了已发布的池子与 token；两种拼法指向同一条路线；真跑那把 route_id 各段相符 |

产物层（记忆条目「确定性要在产物层验证」的落地）：
`byte_identity` 7 个文件 `unchanged_from_previous_run = true`；`drift = []`、`drift_count = 0`；
整目录重算摘要 keccak256 `0x163aedbd483c75e605b58858855030b6b073e326d80b897c488f549045796b42`（20 031 字节）；
10 个 phase 全部 `ran = true` 且各自公布了自己写的 key。
夹具身份：`fixtures/simulation-m10/fixture-37530593-executor.json`，134 403 字节，
keccak256 `0x4672cca9fecb05bcc4771b1f74b2d6fe34fbeb5bbc76ed83f3211c9ddbe4c6cd`，目录里 14 行、点名它的 14 行。

---

## 23. Security

**合约侧扫描（§62 的三项，本轮现量）**

| 项 | 结果 | 说明 |
|---|---|---|
| arbitrary call | 无通路 | 外部调用只发生在白名单校验之后；`grep -c '\.call{'` = **0**；`staticcall/delegatecall/callcode` 形式全无 |
| delegatecall | 词面命中 **2** 处，同一行 | 都在 `contracts/ArbitrageExecutor.sol:10` 的文档注释里（那段话正是「本文件不存在任何 call/delegatecall/callcode」）。命中数含自指，实际代码 0 |
| selfdestruct | **0** | — |

权限面：`execute` / `withdraw` / 三个 setter 全部 `onlyOperator`，`withdraw` 只搬合约真正持有的数
（`held < amount` 就 `InsufficientBalance`，交付按 delta 校验，`moved != amount` 就 `PayoutMismatch`），
不能 mint、不能创建。救援通路的独立测试：`the_operator_withdraws_what_the_contract_holds`
加三条拒绝（非 operator / 零金额或超额 / 零地址 token）。

**Rust 侧扫描（§62 的五项）**

| 项 | M10 新增生产文件 | 说明 |
|---|---|---|
| private key literal | 0 | 密钥只从 `GIWA_EXECUTION_PRIVATE_KEY` 读一次 |
| secret scan（64-hex 连续串 = 私钥形状） | **0** | 生产 4 个新文件 + `.sol`：0 命中；6 个 M10 测试目标：0 命中。这正是 `executor_giwa_live.rs:109-118` 把摘要**从 BUILD.md 运行时读出来**而不当字面量写死的原因 |
| URL scan | **0** | 现量口径：12 个 Rust 生产文件（4 新增 + 8 修改）+ `contracts/ArbitrageExecutor.sol` 逐个 `grep -c 'https\?://'` 全是 0；端点只来自 `GIWA_RPC_URL`（§5）。唯一例外是 `contracts/BUILD.md` 里的 1 条 `binaries.soliditylang.org/.../list.json`（§1 的编译器摘要取证 URL，非密钥、非节点端点） |
| panic scan（`panic!/unreachable!/unwrap()/expect(` 于 `#[cfg(test)]` 之前） | 4 个新文件全部 **0** | `arbitrage.rs 0 / deploy.rs 0 / protocol/executor.rs 0 / simulation/executor.rs 0` |
| unsafe scan | **0** | 12 个 M10 相关文件（新增 + 修改）逐个 grep `unsafe` = 0 |

工作区既有代码里 `.expect(` 有 7 处命中，全部在 M10 之前的文件里（本轮验证：`stage.rs`/`lifecycle.rs`
在 HEAD 与工作区的计数完全相同，即一条也没新增）。

**证据目录里的 URL**：`data/evidence/m10/` 下恰好 **1** 处 ——
`real/preconditions.json` 的 `"endpoint": "https://sepolia-rpc.giwa.io"`，即 §57 真跑实际打的那个端点。
本轮没有测试去扫证据目录里的 URL（M9.2/M9.4 的那两条 URL 扫描各自只负责自己的目录），
这与 M6/M7 的先例一致（`data/evidence/m6/**` 里同样登记着实测端点）。
**这是一条口径，不是一条通过**，所以同时写进 §27 与 §28。

**§60/§52 的「不许伪造成功」**：`giwa_success.json` **不存在**，且 manifest 的 `not_claimed.giwa_success_file`
把不存在的理由写死：`absent by design — §60 forbids creating a fabricated one, and §52 says an included receipt
proves none of the four things a success would need`。

---

## 24. RPC Safety（§51）

M10 的插桩**没有给热路径加一次 RPC 读**。三条各自成立：

1. **构造上为零**：`manifest.json` 的 `rpc_reads = 0`；
   `recompute/deterministic.json` 的原话是
   `none. This file builds no adapter and names no endpoint; every value in the five tables above was read out of
   this directory, which is §51's answer given by construction rather than by measurement.`
   —— 证据门是纯函数，读的是目录不是节点；这比「测到 0」更强。
2. **测试里有计数器**：`executor_deploy.rs` / `executor_lifecycle.rs` 对着脚本端点跑，
   每条「在任何东西移动之前就停住」的分支都用端点调用计数器把「之前」变成一个实测的零；
   `an_execute_reaches_the_endpoint_byte_for_byte_as_the_plan_encoded_it` 验的是发出去的字节而不是又一次读。
3. **流水线侧的读全部标注**：`every_lane_read_arrives_labelled_with_the_leg_that_asked`、
   `all_three_consumer_answers_ask_the_lane_for_exactly_the_same_reads`、
   `no_extra_rpc_outside_declared_header_reuse`（`crates/simulation/tests/executor_state/`）。
   M10 加的 `PlanBinding` 不产生任何新读——回归里 `chain` 49 项、`state` 15 项、
   `discovery` 111 项、`pathfinder` 44 项、`live` 141 项全绿，正是「没给它们加读」的旁证。

真跑那一把的读也确实被登记，而且只有被 §57 需要的那几个：
`preflight_reads` 2 条（两个池子的 reserve/两侧 token/`blockTimestampLast`）、
`reserves_at_pinned_block` 2 条、三处余额快照的 `block_number` 全部点名。
模拟读并发 `sim_read_concurrency = 8` 是既有配置项，本轮没改默认值。

---

## 25. Regression

**关卡（§64 的三道，串行、单一 cargo 进程、`CC=clang CXX=clang++ CXXFLAGS="-include cstdint"`）**

| 关卡 | 命令 | 结果 |
|---|---|---|
| fmt | `cargo fmt --all -- --check` | exit 0 |
| clippy | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | exit 0（`/tmp/m10-p7-clippy-final.log` `CLIPPY-EXIT:0`） |
| workspace tests | `cargo test --workspace --all-targets --all-features --no-fail-fast -- --test-threads=1` | **exit 0** |

日志：`/tmp/m10-p7-final-test2.log`，`WORKSPACE-EXIT:0`，`grep -c FAILED` = **0**。
本轮用 `--no-fail-fast`（cargo 自己的开关，必须写在 `--` 之前）而不是默认的 fail-fast，
因为第一处失败就截断的日志**不能**作为 §65 回归的证据。写这份报告时又重新解析了一遍日志，
结果与关卡当时一致：94 行 `Running`、94 行 `test result:`、passed 1 410 / failed 0 / ignored 17。

**按 crate 的全量（M1–M9.4 一起跑，不挑 subset）**

| crate | 测试目标数 | passed | ignored |
|---|---|---|---|
| pipeline | 18 | 331 | 1 |
| execution | 14 | 264 | 5 |
| simulation | 14 | 174 | 3 |
| live | 9 | 141 | 1 |
| discovery | 10 | 111 | 4 |
| opportunity | 6 | 93 | 0 |
| chain | 2 | 49 | 0 |
| cli | 6 | 46 | 0 |
| pathfinder | 2 | 44 | 0 |
| protocol | 1 | 42 | 0 |
| graph | 4 | 35 | 0 |
| metrics | 1 | 32 | 0 |
| replay | 3 | 16 | 3 |
| state | 1 | 15 | 0 |
| risk | 1 | 11 | 0 |
| core | 1 | 6 | 0 |
| mev_bot | 1 | 0 | 0 |
| **合计** | **94** | **1 410** | **17** |

（`mev_bot` 那 0 是根包的 lib 目标，无测试函数；17 个 ignored 是 cargo 报的权威计数，
词面 `#[ignore` 在仓库里命中更多，多出来的是文档注释里的自指。）

**§65 特别点名的四个里程碑**

| 里程碑 | 目标 | 本轮实测 |
|---|---|---|
| M9.1 discovery | 10 个目标 / 111 项 | 全绿（`scanning` 13、`verification` 14、`evidence_gate` 12 等） |
| M9.2 reconstruction | `reconstruction_evidence_gate` 16、`sync_reconstruction` 34 | 全绿 |
| M9.3 pathfinder | 2 个目标 / 44 项；`pathfinder_evidence_gate` 11 项挂在 discovery 下 | 全绿 |
| M9.4 radar | 9 个目标 / 141 项（`preconf_radar_matrix` 27、`preconf_evidence_gate` 21、`preconf_negative_controls` 14、`preconf_fixtures` 14、`preconf_link_loop` 13、`preconf_isolation` 7、`preconf_live_giwa` 1 ignored） | 全绿 |

**跨里程碑证据门：本轮撞到两次，都是真回归，也都按各自门的正规刷新通道处理**（见 §26 的逐项说明）。
两次都做了红→绿对照：先复现红（把锚点行号写成一个错的数，门立刻红），再走门自己的 env 通道转绿，
门的判据一个字没改。

---

## 26. Production Diff Audit（§63）

**已跟踪文件的改动（16 个）**：8 个代码文件 + 4 个 M8.6 证据文件 + 4 个 M8.4.3 证据文件，
合计 `721 insertions / 303 deletions`（`git diff --shortstat` 现量）。
工作区里还发现第 17 个改动——`.gitignore` 多了一行 `data/evidence/m8/state-ownership/ownership-matrix.json`。
这行是上一轮遗留：该文件从 M8.4.3 起就已被 git 跟踪，所以对已跟踪文件而言这条规则是空转
（`git check-ignore -v` 返回未忽略、`git status` 照常列出它），全仓库也没有任何代码读 `.gitignore`。
它不属于 M10 的声明范围，已还原成 committed 版本（`git checkout -- .gitignore`），因此上面才是 16 个文件。

### 26.1 代码侧 8 个文件，逐项理由

| 文件 | 变更量 | 加了什么 | 为什么必须动 |
|---|---|---|---|
| `crates/execution/src/error.rs` | +8/−0 | 计划拒绝相关的错误变体 | 新增的 `PlanBinding`/计划校验要有自己的可分类失败；纯追加，不改既有分支语义 |
| `crates/execution/src/gate.rs` | +195/−12 | `pub enum PlanBinding`（含 `describe()`、`has_simulation()`） | §16 要求计划与块/模拟答案绑定；这是流水线能读到的载体 |
| `crates/execution/src/lib.rs` | +12/−1 | `pub mod arbitrage; pub mod deploy;` + 再导出 | 模块声明 |
| `crates/execution/src/lifecycle.rs` | +24/−0 | `attach_route_id()`（两个记录类型） | 路线身份必须挂到 run 记录上，§16/§40 |
| `crates/execution/src/stage.rs` | +138/−11 | 接受 `crate::arbitrage::{ExecutablePlan, ExecutionBinding}` | 执行阶段要能拿到 plan 构造的交易；阶段顺序与既有判定不动 |
| `crates/protocol/src/lib.rs` | +5/−0 | `pub mod executor;` + 再导出 | 模块声明 |
| `crates/simulation/src/engine.rs` | +95/−37 | 执行合约调用的模拟通路（读相位标注、exact-out 交付计量） | §25 要真字节码模拟 |
| `crates/simulation/src/lib.rs` | +2/−0 | `pub mod executor;` + 再导出 | 模块声明 |

（这一列的数字是 `git diff --numstat -- crates/` 现量：8 个文件 +479/−61。）

### 26.2 §63 特别检查的四个里程碑：**生产代码零改动**

`git status` 里没有任何 `crates/{chain,state,graph,discovery,pathfinder,live,pipeline,metrics,risk,replay,opportunity}/src/**` 文件。
M10 没有把执行依赖塞进发现/状态/路径/雷达侧——§65 那句「不能因为 M10 引入 execution dependency 导致这些语义变化」
是靠 diff 范围本身成立的，不是靠承诺。

### 26.3 新增文件

`contracts/`（1 份 `.sol` + BUILD.md + 4 个产物）、
`crates/execution/src/{arbitrage,deploy}.rs`、`crates/protocol/src/executor.rs`、`crates/simulation/src/executor.rs`、
6 个测试目标 + `crates/simulation/tests/executor_state/mod.rs`（被 `executor_revm.rs:40` 以 `mod` 引入的共享模块）、
`fixtures/simulation-m10/`（2 份）、`data/evidence/m10/`（32 份）、
`docs/v0.1/M10 Coding.md` + `M10 Semantic Audit.md` + 本报告。

### 26.4 依赖与工具链

`Cargo.toml` / `Cargo.lock` **零改动**。本轮没有新增任何依赖（含 dev-dependencies，含注释里的字面量），
这条纪律来自 M9.3-P4b 的教训：跨里程碑的证人用**已提交产物**，不用前一个里程碑的代码。

### 26.5 两处跨里程碑证据刷新（这是本轮唯一「动到别人家的东西」的地方，逐条交代）

| 门 | 被改文件 | 变化 | 通道 |
|---|---|---|---|
| M8.6 `rpc_reduction_evidence` | `data/evidence/m8/m8.6/{information_flow,rpc-census,rpc-reduction-candidates,rpc_surface}.json` | **222 个叶子**，全部是源码位置锚点。整数值三型：`line` 111 + `decision_line` 46 + `source_line` 26 = 183；`file:line` 字符串三型：`first_decision` 23 + `asked_at` 13 + `consumer_decision_line` 3 = 39。183 + 39 = 222 | 该门自己的 `M86_CENSUS_REFRESH=1` |
| M8.4.3 `state_ownership_evidence` | `data/evidence/m8/state-ownership/{lifecycle-contracts,ownership-matrix,reuse-verdicts,stage-dependency-matrix}.json` | **20 个叶子**，key 全部为 `line` | 该门自己的 `M843_STATE_OWNERSHIP_REFRESH=1` |

两点让这不算「偷偷改证据」：

1. **只有锚点变了。** 用脚本逐叶子对比 HEAD 与工作区：8 个文件的**叶子 key 集合完全相同**（增删 0），
   变化全部落在上面列出的位置类字段上。同一件事有两个独立口径互相印证：`git diff --numstat` 里
   M8.6 四份文件是 `72+72+6+72 = 222` 对增删行、M8.4.3 四份是 `6+5+6+3 = 20` 对，
   和叶子计数逐一对上（JSON 每行一个叶子）；5 组 M8.4.3 的新旧值分别是
   `stage.rs 380→420`（×9）、`gate.rs 90→97`（×3）、`gate.rs 74→81`（×3）、`gate.rs 63→70`（×3）、
   `lifecycle.rs 740→764`（×2），全部是「M10 在同文件上方插入代码导致行号下移」。
2. **每个新锚点都被独立验过指向真实文本。** 20 个 M8.4.3 锚点逐个在盘上取那一行、要求全文件唯一命中
   （`matches = 1`）；M8.6 侧抽查 `engine.rs:1157`＝`if claimed != found {`、`gate.rs:81`＝`pub enum BlockBinding {`、
   `engine.rs:1101`＝`let signature = call.signature();`，均落在其声明的那段代码上。

刷新流程的两个已知现象也照实写：刷新那一跑里，锚点测试读的是 committed 目录、字节门随后才重写它，
所以**刷新过程内会有 1 项红**（M8.6、M8.4.3 两次都一样），干净重跑即 12 + 9 全绿 exit 0。
这不是判据松，是同一进程内的读写次序；证据留在 `/tmp/m843-refresh.log`、`/tmp/m843-verify.log`、
`/tmp/m843-negctl.log`、`/tmp/m843-restore.log`。

---

## 27. Limitations（这一轮做不到什么）

1. **没证明能赚钱。** 见 §13/§28。收益以 WETH 计、账单以原生币计，本轮没建换算，所以「净利润」是 `null`。
2. **真链那把成功执行带的门槛是 1 wei**，不是利润门槛。利润门槛（`FinalShortfall`）在 REVM + 夹具层证明，
   在链上则由**失败那一笔**证明（门槛 1e16 → 回滚、零残留）。
3. **路线形状很窄**：只支持 V2 形状 pair、exact-out、每腿精确相等。
   带转账税的 token 在这里**不可能**成交（必然 `DeliveryMismatch`），这不是 bug，是设计边界；
   但也就意味着「能执行的资产集合」比市场实际提供的要小。
4. **只跑了 2 腿。** `MAX_LEGS = 4` 的上限有测试（`a_route_longer_than_the_contracts_own_bound_is_rejected`），
   但没有 3 腿或 4 腿的真链执行。
5. **没有价格预言机、没有 sizing、没有搜索**（§3 的 non-goals）。合约只执行别人算好的数。
6. **没有 MEV 保护的实测**：本轮 9 笔交易都在测试网、单笔、无并发对手，
   「被夹会怎样」只能由「合约要求精确相等 → 动了就 revert」这个结构性论证给出，不是竞争环境下的实测。
7. **gas 上限与 margin 是声明值**（600 000 + 20 000），不是从历史分布推的；
   M8.1 因为样本不足停在 INCOMPLETE 的那类延迟/成本基线，本轮没有补，也没有借用它的结论。
8. **一次性的手工测量**：`cmp` 字节门禁（§22）是 M10-P1 期间手工跑的，仓库里没有可重放的 solc 调用。
   本轮还因此**改掉一处文档假引用**——`contracts/BUILD.md` 原文声称某个 `--ignored` 测试会真跑编译器，
   实测全仓 `Command::new` 只有 `crates/cli/tests/` 四处且跑的是本项目 CLI，遂更正。
9. **证据目录的 URL 不做扫描**（§23 末）：目录里留有 1 条真实端点 URL，本轮没有为它建立判据，
   只登记了先例与口径。
10. **救援通路没有被真链验证**：`withdraw` 有独立测试（delta 计量、三条拒绝分支），
    但本轮没有实际在 91 342 上调用它——合约收尾余额为零，没有可救的东西，硬造一个残留反而违反 §57 的小额受控原则。

---

## 28. UNKNOWN / N/A

**UNKNOWN（本轮无证据，不猜）**

| 项 | 值 | 为什么是它 |
|---|---|---|
| `REAL_PROFITABLE_ARBITRAGE` | `NOT_PROVEN`（证据自己的词）；§68 的判据槽写 `UNKNOWN` | 两者同义：本轮没有建立跨资产换算。§68 明确：这一项不成立**不阻塞**代码完成 |
| `realized_profit` / `profit_status` / `realized_profit_status` | `null` | 「the gain is WETH, the fee is the chain's native asset … this run prices one in the other by no oracle」 |
| `gross_profit` / `gross_output` / `simulation_profit_wei`（记录字段） | `null` | 同上；这些字段属于流水线的记录面，不由真跑那一把填 |
| `settled_at_ms` / `profit_verified_at_ms` | `null` | 利润核验步骤本轮没有跑，不填时间戳冒充跑过 |
| 证据目录 `absent_fields` | `[]` | manifest 声明没有「应该存在而缺失」的字段 |

**N/A（这件事在本轮不适用，且理由可指）**

| 项 | N/A 理由 |
|---|---|
| `giwa_success.json` | §60 禁止凭「回执 included」造一份成功证据；`not_claimed` 里写死了这条 |
| REVM 内部署合约 | §4 审计：`engine.rs:941-948` 对 `Output::Create` 报错，模拟层不允许部署，runtime bytecode 预置进 dump |
| 非 sender 地址的状态覆写 | `request.rs:473-504` 的既有规则只允许覆写 sender；本轮原样尊重，**没有**伪造池子状态 |
| 闪电贷 / 借入资金 | 输入来自操作员钱包 + 预先授权（§18 模型），合约不借 |
| `--via-ir` | 没有用：三次 `Stack too deep` 靠拆函数解决（`contracts/BUILD.md` §5） |
| 重入库（OpenZeppelin） | 没有用：6 行手写锁，避免为一条 modifier 引整个依赖树（§20） |
| `delegatecall`/`selfdestruct`/低层 `call{` | 不存在，因此「防护」也是 N/A；见 §23 的扫描表 |

---

## 29. Four-Commit Record（§66）

严格四笔，顺序与内容如下（**本文件本身属于第 4 笔，所以它的 hash 不可能写在本文件里**；
四笔的 hash 请 `git log --oneline -4` 现量）：

| # | message | 只放 | 文件 |
|---|---|---|---|
| 1 | `feat(m10): add arbitrage executor contract` | 合约 + Rust 生产码 | `contracts/ArbitrageExecutor.sol`、`contracts/artifacts/`（4 份）、`contracts/BUILD.md`、`crates/execution/src/{arbitrage,deploy}.rs`、`crates/protocol/src/executor.rs`、`crates/simulation/src/executor.rs`，以及 8 个已跟踪代码文件的改动 |
| 2 | `test(m10): add executor contract and lifecycle coverage` | 单测/集成/REVM/负控制 | `crates/execution/tests/executor_{deploy,lifecycle,evidence_gate,giwa_live}.rs`、`crates/simulation/tests/executor_{revm,evidence}.rs`、`crates/simulation/tests/executor_state/mod.rs` |
| 3 | `evidence(m10): add executor execution evidence` | 证据/夹具/manifest/重算/真跑 | `data/evidence/m10/`（32 份）、`fixtures/simulation-m10/`（2 份）、以及 §26.5 那 8 份跨里程碑证据刷新 |
| 4 | `docs(m10): add arbitrage executor completion report` | 报告/架构说明/任务更新/证据 README | 本报告、`docs/v0.1/M10 Coding.md`、`docs/v0.1/M10 Semantic Audit.md`（`data/evidence/m10/README.md` 属第 3 笔产物，本笔不再触碰） |

提交前的 worktree 判据：`git status --porcelain` 里除 `crates/*/target/`（仓库只忽略根 `/target`，
所以子 crate 的 target 会出现在 status 里，属已知现象，不影响判据）之外不得有非本轮产物。

---

## 30. Final Verdict

### 30.1 §68 判据逐条

**Contract**

| 判据 | 结论 | 出处 |
|---|---|---|
| Executor contract exists | ✅ | `contracts/ArbitrageExecutor.sol`，425 行 |
| ABI deterministic | ✅ | `ArbitrageExecutor.abi` 8 132 字节 + 源码↔产物逐字节对账（§22） |
| bytecode deterministic | ✅ | creation 7 565 B `0x8c49d87c…`、runtime 7 347 B `0x917ed914…`；D 门与 §3 摘要一致 |
| deployment proven | ✅ | 预测 == 实际，`0x226c71f6…` 块 38 023 919，恢复出 sender = operator |
| operator control proven | ✅ | `NotOperator` 真 REVM 复现 + planted control PASS |
| pair allowlist proven | ✅ | `PairNotAllowed` + `invalid_pair.json` + `states/pair_not_in_the_allowlist.json` |
| token allowlist proven | ✅ | `TokenNotAllowed` + `token_not_allowed.json` + `states/token_not_in_the_allowlist.json` |
| route validation proven | ✅ | `_checkRoute` 六项拒绝 + `claims_that_do_not_chain_are_rejected` + D1 的 28 字段敏感性 |
| atomicity proven | ✅ | §11 三件：一腿失败夹具零残留 + 真链回滚零余额变化 |
| min-output proven | ✅ | `LegShortfall` / `DeliveryMismatch` / `FinalShortfall` 各自可复现 |
| final-profit guard proven | ✅（守卫本身）/ ⚠️（利润结论） | 门槛机制已证（`FinalShortfall`）；「所以能赚钱」**未证** |
| no arbitrary call | ✅ | 白名单 + `.call{` 计数 0 |
| no delegatecall | ✅ | 代码层 0（词面 2 命中在 1 行文档注释，含自指） |
| no flashloan | ✅ | 输入走 `transferFrom` + 操作员预付，`_pullInput` 计量 |

**Rust**

| 判据 | 结论 | 出处 |
|---|---|---|
| `ArbitrageExecutionPlan` exists | ✅ | `crates/execution/src/arbitrage.rs` |
| deterministic calldata builder | ✅ | D1/D2：14/14 行重建，28/28 字段变异改哈希 |
| chain binding | ✅ | `PlanBinding` + `wrong_chain.json`（plan 层拒，恰好一个码） |
| executor binding | ✅ | `wrong_executor.json`（§36 的目标漂移） |
| existing execution lifecycle reused | ✅ | `stage.rs` 接 `ExecutablePlan`；`sequence` 44、`lane_matrix` 14、`stage_matrix` 15 全绿 |
| no duplicate signer / submitter / receipt | ✅ | §5 的 import 表；M10 两个新生产文件全部 `use crate::…` |

**Simulation**

| 判据 | 结论 | 出处 |
|---|---|---|
| REVM successful path | ✅ | `successful_two_hop_route_profits_on_real_bytecode` + 真跑同数 |
| REVM revert path | ✅ | `forced_second_leg_failure_leaves_no_residue` |
| min-output rejection | ✅ | `second_leg_asking_low_leaves_the_floor_unmet` |
| profit rejection | ✅ | `final_floor_above_the_priced_output_reverts_final_shortfall` |
| insufficient funds rejection | ✅ | balance / allowance 两条，均「在任何 transfer 之前」 |
| deterministic simulation | ✅ | D3：9/9 整个 observed block JSON 逐字节相同 |

**Real GIWA**

| 判据 | 结论 | 出处 |
|---|---|---|
| deployment evidence | ✅ | `contract/deployment.json`（9 字段 + 头部绑定 + 恢复发送者） |
| controlled call attempted | ✅ | `real/giwa_execution.json`，nonce 64，块 38 023 960 |
| receipt evidence | ✅ | `included` + 块哈希绑定 + gas/价格/L1/L2 全量 |
| balance reconciliation | ✅ | §20：`difference = 0`、`external_movement = 0`、钱包账单逐 wei 相等 |

**Quality**

| 判据 | 结论 |
|---|---|
| fmt / clippy / workspace tests | ✅ exit 0 ×3（§25） |
| regression | ✅ 94 目标 / 1 410 通过 / 0 失败 / 17 ignored，`--no-fail-fast` |
| secret scan / panic scan / evidence URL scan | ✅ 0（URL 一项按 §23 口径：源码 0、证据目录留 1 条真端点、本轮未建扫描判据） |
| RPC safety | ✅ 构造性为零（§24） |
| clean worktree | ✅ 四笔提交后 `git status` 只剩 crate 内 `target/`（§29 的说明） |
| four commits | ✅ §66 的严格四笔（hash 请现量） |
| completion report | ✅ 本文件，30 节 |

### 30.2 §69 禁止声称清单 —— 本轮一条都没说

| 不许声称 | 本轮的实际写法 |
|---|---|
| Flashblocks execution | 与 M9.4 零耦合；`crates/live` 生产文件本轮未改（§26.2） |
| Private execution | 不涉及；只走公开提交通路 |
| MEV protection | 只说「结构性 revert 假设」，不说被夹测试通过（§27 第 6 条） |
| Guaranteed profit | 未声称；`realized_profit = null` |
| Stable profitability | 未声称；单把、无分布 |
| Production readiness | 未声称 |
| 24/7 readiness | 未声称 |
| Competitive latency | 未声称；本轮只有 lifecycle 的几个 ms 时间戳，没有基线 |
| Mainnet readiness | 未声称；一切在 chain 91 342 |

**M10 证明的是**：`Atomic execution primitive` —— 一个确定性的套利执行计划可以经现有执行流水线构造成交易，
由执行合约原子执行；成功时整条路线完整执行并满足最终资产 invariant，失败时整笔回滚，
且结果能被 simulation / receipt / balance evidence 三层分别验证。

**M10 没有证明的是**：`Profitable MEV production system`。

### 30.3 §71 的四句话（不许塌成一句）

```text
Simulation says:  这笔交易应该执行。      → REVM: Success，528 136 078 126 613，229 302 gas
Execution says:   这笔交易已被提交。      → submitted，0x1105c89b…f82771
Receipt says:     这笔交易已被打包。      → included，块 38 023 960
Reconciliation:   这笔交易确实产出了这个结果。→ 净 Δ 428 136 078 126 613 WETH，difference 0，external 0
```

第四句**没有**被写成「确实赚到了」，因为收益与账单不在同一单位（§13）。

### 30.4 判定

```text
M10 STATUS = COMPLETE
REAL_PROFITABLE_ARBITRAGE = UNKNOWN (evidence wording: NOT_PROVEN)
下一格（M11 Optimize）的前置条件：本轮的 plan / calldata / lifecycle 三段可原样接；
缺口只有两个 —— 跨资产计价（oracle 或统一分母）与 2 腿以上的路线执行。
```
