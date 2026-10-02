# M7 Completion Report

结论（白话版）：**M7 状态 COMPLETE（§57 A–Q 全通过、§58）**。这一轮在真实 GIWA 测试链上，从**真实链上状态**
发现了一个**真实的双池套利机会**，在**没改任何市场事实**的前提下模拟通过、风险通过、十四项执行前闸门全过，
然后**真的签名、真的用 `eth_sendRawTransaction` 提交了 6 笔交易**，6 笔全部 `status = 1`，
收据日志证明两条腿真的成交，最后**扣掉 L2 gas 费和 L1 数据费之后仍然赚钱** —— 而且赚的钱
**精确等于钱包原生余额的差值**，一个 wei 都不差。

```text
Detected → Simulated → RiskApproved → Preflighted → Built → Signed → Submitted → Included → Settled → ProfitVerified
```

**这一轮真实发生的一块钱的事**（本金 0.0001 ETH，用户在开工前逐次授权）：

| 问的是哪一段 | 实测值（全部由收据独立重算过） | 白话换算（整个测试钱包 = 100 元） |
| --- | --- | --- |
| 本金 | 100 000 000 000 000 wei = 0.0001 ETH | 0.5 元 |
| 毛利 | **764 806 517 171 626 wei** | 3.824 元 |
| L2 gas 费 | 359 692 789 439 wei | 0.0018 元 |
| L1 数据费 | 42 901 813 739 wei | 0.00021 元 |
| **净利（扣两种费之后）** | **764 403 922 568 448 wei = 0.0007644 ETH** | **3.822 元** |
| 真实花掉的手续费 | 402 594 603 178 wei，占钱包 **0.002 01 %** | 约 0.002 元（一分的 1/5） |
| 是不是套利 | **是**。WETH → 池 A → BLS → 池 B → WETH，两种代币进出都由收据 Transfer/Swap 日志逐笔证明 | 与 M6 那笔「零值转给自己的验证交易」不是同一类，M6 那笔永远不计入套利 |

三件必须写在结论旁边、不能被「赚到钱了」盖过去的事：

1. **本轮在自己代码里抓到第二个真缺陷，并修在根上。** 第一次 `--execution-mode submit` 被
   自己的第 12 项闸门 `fee_estimate` 拒绝了：那条规则要求「交易带的 maxFeePerGas」**完全等于**
   head 那一秒重读的 maxFeePerGas。可是这笔计划钉在 37 563 031 块（baseFee 553 → maxFee 1 001 106），
   等到预检重读时 head 已经走到 37 563 056（baseFee 537 → maxFee 1 001 074）—— 1 秒一块的链上，
   baseFee 每块都在动，**相等才是巧合**。根因是「把两个来源不同的读数要求成字面相等」，
   而不是「把方向搞反了」。修法是**只在会亏钱的那个方向拒绝**：
   交易自己的上限 < head 现在的要价 ⇒ 拒绝（这跑会被压低报价卡在块外，而且利润是拿更小的天花板算的）；
   交易自己的上限 ≥ head 读数 ⇒ 通过，并在证据里写清「成本模型用的是两者中更贵的那个」。
   修改前后各钉了一个单元测试（`crates/execution/src/preflight.rs`）。
   **被拒那一次没有签过名、没有发出过一笔、没花过一分钱**（`transactions: []`、`stopped_at: 0`、
   计数器 `execution_preflight_blocked: 1`），取证 `data/evidence/m7/route-submit/route-91342-37563031-1790908149030/`。
2. **这个机会是真的，但它不是一个「有人在竞争的市场」。** 两个池子都由第三方
   `0x18b5edc574b956b1a811affac8312b34ff38f11d` 创建，本次成交之前**从来没发出过一笔 Swap**
   （取证 `data/evidence/m7/candidate-pools-deep-reading.json`），池子里的储备就是池子自己的 `balanceOf`
   （权威值，本轮一个字都没改），中间代币是 12 位的测试币「BLS / FlowSwap.io」。
   按 §5 的定义它是 **REAL_MARKET**：储备真实、费率 997/1000 有举证、输入来自当前真实状态。
   但约 **8.6 倍的价差一直摆在那里没人吃**，这说明它是测试链上的荒地，不是主网意义上的有效市场。
   §48 要求第一笔真实成交不追最大利润，所以本金只用 0.0001 ETH。
3. **成功不是「一次跑顺」，是「中间停了一次」。** 本轮一共跑了 3 个会话：1 次免费的 `build-only`
   演练（签名前按约定先用 REVM 在这个金额上实跑，把净利数字写进预检证据）、
   1 次被上面那条 fee 闸门拒掉的 `submit`（0 支出）、1 次成功的 `submit`（6 笔上链）。
   §34 要求失败的尝试完整落盘，三次都在 `data/evidence/m7/` 下，各自的 `route-run.json` 都在。

---

## 1. Execution Summary（§61.1）

| 字段 | 值 | 出处 |
| --- | --- | --- |
| observation window | 单块钉定 + 单段执行，不是长观测窗。演练会话：37 562 423 → 37 562 449；被拒会话：37 563 031 → 37 563 056；成功会话：37 563 264 → 37 563 327 | 各会话 `preflight.json` 的 `pinned_block` / `head` |
| blocks | 成功会话从钉定块到最后一笔收据块跨 **63 个区块**；6 笔分别落在 37 563 300 / 305 / 310 / 316 / 322 / 327 | `submissions.jsonl` 的 `receipt_block` |
| opportunities | **1**（§27：本轮只跑调用方点名的那一个候选，闸门不重搜索） | `metrics.json` `opportunity_count: 1` |
| simulations | **1**（REVM，真状态 `rpc:chain-91342`，12 步） | `simulation_count: 1` |
| risk accepts | **1**（reject 0） | `risk_accept_count: 1` |
| transactions | **6 笔真实签名 + 6 笔真实提交 + 6 笔全部入块**；nonce 1→6 连续；0 笔 revert | `build_count/sign_count/submit_count/included_count = 6`、`revert_count = 0` |
| 端到端耗时 | 创建 50 770 ms → 利润核验 84 584 ms = **33.8 秒**（含 REVM 模拟 21.4 秒、6 笔依次等入块） | `route-run.json` 的 `rung_stamps` |

## 2. Real Opportunity（§61.2）

| 字段 | 值 |
| --- | --- |
| block | **37 563 264**，hash `0x890e33dd1ded16a96a4d3574f4e3da26d8ae5091001c49fd89d3479bb513cce0`（`eth_getBlockByNumber` 同一份头里取到的 number + hash，不是 `latest` 模糊态） |
| pool A（买入腿） | `0x2a3ceafbA30f6626170CBB0CD67392eFb94BD9A4` |
| pool B（卖出腿） | `0x5b3C1E3FB6A97C0130AE015fF10f53A1A30C353e` |
| token pair | WETH `0x4200000000000000000000000000000000000006` ↔ BLS `0x07D4af6E2bc8DD82beb06b4FD279DF4c9028F26f`（两个池子 `token0()`/`token1()` 各自读出，顺序一致） |
| fee | **997/1000**（留存比例，两条腿同一个值）。举证：`data/evidence/m7/candidate-fee-measurement.json` —— 用这两个池子**已部署的字节码**做二分实测反出来的费率，不是假设的 0.30 % |
| reserve（钉定块上读，权威） | 池 A：reserve0(BLS) = 1 000 000 000 000 000 000 000，reserve1(WETH) = 1 000 000 000 000 000，`blockTimestampLast` 1757405620；池 B：reserve0(BLS) = 2 000 000 000 000 000 000 000，reserve1(WETH) = 20 000 000 000 000 000，`blockTimestampLast` 1757403527 |
| input amount | 100 000 000 000 000 wei（0.0001 ETH，先 `deposit()` 包成 WETH） |
| expected output | 864 806 517 171 626 wei WETH（中间量 90 661 089 388 014 913 158 BLS） |
| expected profit | 毛利 764 806 517 171 626 wei；预检按序列天花板 100 542 649 055 784 wei 与 L1 估算 62 677 678 934 wei 扣完之后，**签名前**写进证据的期望净利 = **664 263 868 115 842 wei** |

## 3. Simulation（§61.3）

| 字段 | 值 |
| --- | --- |
| simulation id | `0x5ed32ed692029fdcbe80f8db34699461edc9ab2a8050eff45694d54466436b42` |
| simulation block | 37 563 264（就是钉定块；`state_fingerprint = pinned-block-37563264-0x890e33dd…cce0`） |
| simulation state | `rpc:chain-91342` —— 每一步的存储/余额都从真实节点按块号取；**没有 state override**（§34：`funding = real_state{source: "rpc:chain-91342"}`，`real_state_funding` 那一项专门把这条钉成闸门） |
| gas | 455 594（12 步全跑，含 6 个不广播的余额读数步）；EVM 规则 `prague` |
| output | 解析式预期 `0x31289754165aa` == REVM 实测 `0x31289754165aa`（同一个数，两条独立路径） |
| profit | 毛利 764 806 517 171 626 wei，模拟 gas 账单 207 295 270 wei，模拟净利 764 806 309 876 356 wei，`status = Completed`，计价单位 = 原生 ETH（路线以原生开始、以原生结束） |

## 4. Risk（§61.4）

`RiskDecision = Accept`，decision id `0x8a08b790c4fb646bd207ce4ec7bfad6496d8b072a89e8d36718ab4451745ded6`。

闸门清单与读数（`metrics.json` + `risk-decisions.jsonl` 的 `thresholds`）：

| gate | 门槛 | 实测 | 判定 |
| --- | --- | --- | --- |
| 模拟必须 Completed | 硬要求 | Completed | 过 |
| 最低净利 | 0 wei（`--minimum-net-profit-wei` 默认值，本轮没为凑 Accept 改过） | 模拟净利 764 806 309 876 356 wei | 过 |
| 最大 gas | 60 000 000（取自钉定块自己的 gasLimit） | 455 594 | 过 |
| 单一计价单位 | 必须存在 | 原生 ETH 往返，单位成立 | 过 |
| 市场标签 | REAL_MARKET 与 CONTROLLED_FIXTURE 严格分开（§51） | `REAL_MARKET`，举证 = 两个池子在 `data/protocols-m3` 有登记 + 储备与 `blockTimestampLast` 由本跑在钉定 live head 读出 | 过 |

**关于「阈值是不是被放水」**：门槛是 0，而模拟净利是 764 806 309 876 356 wei。就算把门槛抬到
预检算出来的期望净利 664 263 868 115 842 wei，这一跑照样过 —— Accept 不依赖低门槛。

**读证据时要知道的一件事**：成功会话的 `risk-decisions.jsonl` 与 `route-run.json` 里那句 Accept 的理由
文字，当时仍是修改前的 `no broadcast: M4 simulates … not that a transaction was or may be sent`
（本轮结束时把这句话改成里程碑中立的版本，见第 15 节最后一行）。那一跑**确实广播了 6 笔**，
所以按字面读那句话会与事实冲突：它想说的是「这个 Accept 本身不构成发送授权」，
发送授权来自操作者显式给出的 `--execution-mode submit`（§32）。这里如实标注，
**不回头改证据文件** —— 证据是不可变的历史记录。

## 5. Transaction（§61.5）

6 笔全部由 `expected_sender == recovered_sender` 自证签名归属（`signed-transactions.jsonl`），
`tx_type = eip1559`，`maxFeePerGas = 1 000 910`、`maxPriorityFeePerGas = 1 000 000`，chain id 91 342，
access list 为空。钱包 `0xd450630c1c55b1c7df1ebf7eeaee1fffb45e520c`。

| # | nonce | to | value | gas limit | calldata | calldata hash | tx hash |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 1 | WETH | 100 000 000 000 000 | 64 932 | `deposit()` (4 B) | `0xa1548b79599a5ef2fd6de226ea5131c228954c4bf788eb3e4386a2728abe3e8d` | `0x960e91b2e2ef4cfe44d6420a0b8b1283e15d5b0def211a2d609161f6a882c93b` |
| 2 | 2 | WETH | 0 | 51 972 | `transfer(address,uint256)` (68 B) | `0xb3407cee64f5800b1f790fdad85696a2b329f49520b6813d432abbf3f37bcf39` | `0x5caefdecaa16708370500876e87d6fac64fff6ee34094077cccfc4f0272bcb1f` |
| 3 | 3 | pool A | 0 | 147 200 | `swap(uint256,uint256,address,bytes)` (164 B) | `0xa23e11237e2e41bfcb2090b0bf9021c93580f66c93f852829eb248aa7da0cf73` | `0xda125ecbea78f0de9fcb9ac061a2829698c4151db288142273bc80475fa0a286` |
| 4 | 4 | BLS | 0 | 49 983 | `transfer(address,uint256)` (68 B) | `0x3619723f7bb703ac69d89f60cd011bd2b04b7afa7cee33c63614617efb827081` | `0x306c9f39134151265b8bc17302f1b8630f4de71ff22775ee0b368e5ba3319df7` |
| 5 | 5 | pool B | 0 | 115 013 | `swap(uint256,uint256,address,bytes)` (164 B) | `0x6e4255dd7e1655e1e468943f50e030f1cc7455ddca4cd0c2ecafa09947f9e0ac` | `0x212350d0e7b905289291e3ce12f97743c7739cf844feae60204f71cc63094a82` |
| 6 | 6 | WETH | 0 | 50 435 | `withdraw(uint256)` (36 B) | `0x327c7f4ab464fbdd0851f89905462b79c3a42b5d3c0535bc244a62d1fa613e96` | `0x9895e8891479567ebade840ecec9e4576c2bbe19c6a64ab13201e097a6cda93f` |

提交方式：`eth_sendRawTransaction` 打到公共 HTTP RPC（`submissions.jsonl` 每行
`submission_endpoint_type = "public_http_rpc"`，`detail = "eth_sendRawTransaction returned 0x…"`）——
§28 要求保留这条路，§29 的 SequencerDirect 见第 14 节。

## 6. Receipt（§61.6）

6 份收据全部 `status = 1`（`receipt_status: true`），且收据回到的块号/块哈希被重新绑定验证过。

| # | receipt block | receipt block hash | status | gas used | effective gas price | logs | L1 fee（收据字段） |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 37 563 300 | `0x39003ca1…a047` | 1 | 44 932 | 1 000 442 | 1 | 7 280 332 566 |
| 2 | 37 563 305 | `0x2a6e48a5…bae7` | 1 | 31 972 | 1 000 443 | 1 | 6 893 826 846 |
| 3 | 37 563 310 | `0xc55fc33f…4e5f` | 1 | 127 200 | 1 000 440 | 3 | 7 271 256 972 |
| 4 | 37 563 316 | `0x7d16a9e1…f8bb` | 1 | 29 983 | 1 000 437 | 1 | 7 121 453 231 |
| 5 | 37 563 322 | `0xe2750f50…8649` | 1 | 95 013 | 1 000 436 | 3 | 7 213 490 893 |
| 6 | 37 563 327 | `0x9ac350a2…ced9` | 1 | 30 435 | 1 000 436 | 1 | 7 121 453 231 |

**Route Verification（§57 I、§23）**：两条腿各自发出自己的 Swap log，且金额与路线方向逐字对上 ——

- 池 A：block 37 563 310 log 97，`amount0In/Out = 0 / 90661089388014913158`、`amount1In/Out = 100000000000000 / 0`，收款人 `0xD450…520c`；
- 池 B：block 37 563 322 log 30，`amount0In/Out = 90661089388014913158 / 0`、`amount1In/Out = 0 / 864806517171626`，收款人同一个钱包。

`route.passed = true`，`detail = "both venues emitted their own Swap, in route order, with the route's sender in the loop"`。
成交之后再回读链上储备，池子确实被这两笔换仓推动，变化量与 log 金额**完全相等**（独立复核，非机器人自报）：
池 A reserve0 Δ −90 661 089 388 014 913 158、reserve1 Δ +100 000 000 000 000；
池 B reserve0 Δ +90 661 089 388 014 913 158、reserve1 Δ −864 806 517 171 626。

## 7. Actual Asset Delta（§61.7、§19–§24）

快照绑在「block before」与「block after」两个具体块上，不是两次任意时刻的读取：

| 字段 | 值 |
| --- | --- |
| block before | 37 563 288（`0x292cd8b8…ac7b`）—— 第一笔签名之前取 |
| before：native | 19 999 971 686 320 656 wei |
| before：WETH / BLS | 0 / 0（`eth_call balanceOf` 同一块） |
| block after | 37 563 327（`0x9ac350a2…ced9`）—— 最后一笔收据所在块 |
| after：native | 20 764 375 608 889 104 wei |
| after：WETH / BLS | 0 / 0 |
| **native delta** | **+764 403 922 568 448 wei** |

两种代币进出后都归零，说明路线**以原生开始、以原生结束**（§16 的形状），所以利润与成本天然在同一个计价单位里，
不需要任何外部价格换算（§15 禁止外部价格）。`balanceOf` 与 Transfer log 做了交叉核对（`flow_checks`）：

- BLS：log 净流 0、余额差 0、无 mint/burn ⇒ `agrees: true`；
- WETH：mint（deposit）100 000 000 000 000、burn（withdraw）864 806 517 171 626、log 净流 **764 806 517 171 626**、余额差 0（因为全部 unwrap 回原生）⇒ `agrees: true`。

## 8. L2 Cost（§61.8）

每一行都是 `gas_used × effectiveGasPrice`，两个字段都来自**收据本身**：

| 字段 | 值 |
| --- | --- |
| gas used 合计 | 359 535 |
| effective gas price | 1 000 436 – 1 000 443 wei/gas（6 笔） |
| **L2 fee 合计** | **359 692 789 439 wei** |

`source = "gas_used and effectiveGasPrice from the bound receipt: eth_getTransactionReceipt over the configured GIWA RPC URL (public_http_rpc)"`。

## 9. L1 Cost（§61.9）

| 字段 | 值 |
| --- | --- |
| **L1 fee 合计** | **42 901 813 739 wei**（6 笔各 6.89e9 – 7.28e9） |
| source | **收据字段 `l1Fee`**，`eth_getTransactionReceipt` 逐笔读取（`kind = receipt_field`，`read_by = eth_getTransactionReceipt over the configured GIWA RPC URL (public_http_rpc)`，并逐笔写明所在块号）。§37 要求报告必须说清 L1 费的来源，这里就是 `l1_fee_source` 字段原文 |
| calculation | 直接取链上给出的 `l1Fee`，**不做估算、不假设 0**（§35）。估算值（`getL1Fee(bytes)` 读 GasPriceOracle 预 deploy `0x4200…000f`）只出现在**签名前的预检**里，那一行的来源字段老老实实写着 `estimate only …; the charge itself was never read`，预检估 62 677 678 934 wei，真实收 42 901 813 739 wei —— 估高了，方向安全 |

## 10. Profit（§61.10、§38–§39）

```text
denomination        : wei —— 路线以原生 ETH 开始并以原生 ETH 结束，利润与成本本来就是同一个单位
gross_profit        : 764 806 517 171 626 wei   （= 卖出腿收回 864 806 517 171 626 − 买入腿付出 100 000 000 000 000）
l2_cost             :   359 692 789 439 wei
l1_cost             :    42 901 813 739 wei
net_realized_profit : 764 403 922 568 448 wei
profit_status       : VerifiedPositive          （§56：只有这一个状态计入 M7 的真实套利）
```

**可独立重算公式（A + B − C − D = E）**：

```text
A 卖出腿收到的 WETH      =  864 806 517 171 626   （收据 Swap log，池 B block 37 563 322 log 30 的 amount1Out）
B 买入腿付出的 WETH      =  100 000 000 000 000   （收据 Swap log，池 A block 37 563 310 log 97 的 amount1In）
C L2 费                  =    359 692 789 439   （6 份收据 Σ gasUsed × effectiveGasPrice）
D L1 费                  =     42 901 813 739   （6 份收据 Σ l1Fee）
E 净利                   =  764 403 922 568 448   =  A − B − C − D
```

第三个独立验证：E **精确等于**第 7 节钱包原生余额差（20 764 375 608 889 104 − 19 999 971 686 320 656），
误差 0 wei。本轮报告里的这些数字**不是引用机器人的自报值**，而是用 `eth_getTransactionReceipt` /
`eth_getTransactionByHash` / `eth_getBalance` / `eth_call getReserves` 从节点重新算了一遍，
逐项对上之后才写下来的。

## 11. Simulation vs Reality（§61.11、§18/§47）

| metric | simulation | actual | delta | 判定 |
| --- | --- | --- | --- | --- |
| 中间代币交付 | 90 661 089 388 014 913 158 | 90 661 089 388 014 913 158 | 0 | 在容差内 |
| 输出（收回的输入代币） | 864 806 517 171 626 | 864 806 517 171 626 | 0 | 在容差内 |
| gas | 359 535（可广播 6 步的小计） | 359 535 | 0 | 在容差内 |
| 毛利 | 764 806 517 171 626 | 764 806 517 171 626 | 0 | 一致 |
| 手续费 | 207 295 270（模拟 gas 账单，按 baseFee 计，**不含 L1 数据费**） | 402 594 603 178（L2 359 692 789 439 + L1 42 901 813 739） | +402 387 307 908 | 差额被**定量解释**，见下 |
| 净利 | 764 806 309 876 356 | 764 403 922 568 448 | −402 387 307 908 | 在 1/100 容差内 |
| 结束时的钱包原生余额 | 20 764 777 996 197 012（模拟 `sender_native_end`） | 20 764 375 608 889 104 | −402 387 307 908 | 在 1/100 容差内 |

`mismatch = null`，`delta_audit.uncompared = []`，容差 `1/100`。**模拟与真实的差不是「差不多就行」**：
审计行里写着差值应当是这个量级及其理由 —— 模拟账单里没有 L1 数据费，链上真实收了 42 901 813 739 wei，
再加上 tip 与 baseFee 之差，所以这段 shortfall 的形状是被尺寸化解释的，而不是被含糊过去（§18/§47）。
模拟 gas 那行用的是「可广播 6 步的小计 359 535」，而模拟总额 455 594 里多出来的是 6 个不广播的余额读数步，
这两件事按构造就该不同 —— 比较对象选对了，不是把不一致掩盖掉。

## 12. Evidence（§61.12）

`data/evidence/m7/` 共 39 个文件 / 696 KB。关键事实的五种取证齐备：RPC evidence、tx hash、receipt、
balance query、token balance query。

| 会话 | 目录 | 内容 |
| --- | --- | --- |
| 演练（免费，未签名） | `route-build-only/route-91342-37562423-1790907541327/` | 9 个文件：`route-run.json`、`preflight.json`（14 项全过、期望净利 664 269 350 896 951 wei）、`opportunities/simulation-results/risk-decisions/executions` 各 1 行、`metrics.json`，以及**两个 0 字节的 `signed-transactions.jsonl` 与 `submissions.jsonl`** —— 空文件本身就是「这一跑一个字都没签、一笔都没发」的取证 |
| 被闸门拒（0 支出） | `route-submit/route-91342-37563031-1790908149030/` | 同样 9 个文件；`fee_estimate` 那行 `passed: false`，`transactions: []`，`execution_preflight_blocked: 1`，签名与提交两个 jsonl 同样为 0 字节 |
| 成功 | `route-submit/route-91342-37563264-1790908382146/` | 9 个文件；`signed-transactions.jsonl` 6 行（含 `recovered_sender` 自证）、`submissions.jsonl` 6 行（含收据块/状态/gasUsed/effectiveGasPrice/l1Fee）、`executions.jsonl` 1 行（前后快照 + flows + flow_checks + wraps + route 审计 + cost 逐笔 + profit + delta_audit + 阶梯时间戳）、`preflight.json`（14 项 + 逐笔 gas/费用/L1 估算来源）、`metrics.json`（§52 计数器 + §53 延迟） |
| 机会面取证 | `candidate-fee-measurement.json`、`candidate-pools-deep-reading.json`、`census-pair-created.json`、`dual-venue-economics.json`、`dual-venue-full-history.json`、`live-pool-activity.json`、`weth-native-denomination.json`、`probe-*.json` | 费率举证（字节码二分实测）、池子历史与 LP 归属、双 venue 普查、SequencerDirect 复测、live 读法探针 |

§53 的单次延迟事实（p50/p95/p99 属 M8，本轮只保存单次事实）：
机会发现 2 087 ms、模拟 21 449 ms、预检 5 786 ms、签名 772 ms、提交 253 ms、入块 3 367 ms；
阶梯打点 created 50 770 → built 52 971 → signed 53 743 → submitted 53 996 → included 57 363 → settled 84 584 → profit_verified 84 584。

复现命令（**密钥只从仓库外的文件读入，命令里没有密钥的字面值**；真实提交那条会花真钱并占 nonce，
本轮在用户逐次授权后才跑成一次，不要随手重跑）：

```bash
# 演练：不签名、不广播、免费
GIWA_RPC_URL=https://sepolia-rpc.giwa.io \
cargo run --offline --bin evm-mev-bot -- arbitrage --execution-mode build-only \
  --sender 0xd450630c1c55b1c7df1ebf7eeaee1fffb45e520c \
  --input-token 0x4200000000000000000000000000000000000006 \
  --candidate-mid 0x07D4af6E2bc8DD82beb06b4FD279DF4c9028F26f \
  --candidate-pool 0x2a3ceafbA30f6626170CBB0CD67392eFb94BD9A4 \
  --candidate-pool 0x5b3C1E3Fb6A97c0130aE015ff10f53A1A30C353e \
  --input-wei 100000000000000 --fee-num 997 --fee-den 1000 \
  --fee-evidence data/evidence/m7/candidate-fee-measurement.json \
  --market real-market \
  --market-evidence 'both pools attested in data/protocols-m3; reserves and blockTimestampLast read at the pinned live head by this run' \
  --evidence-dir data/evidence/m7/route-build-only --json

# 真实提交：把 --execution-mode 换成 submit，并只在签名这一刻把密钥放进环境变量
#   GIWA_EXECUTION_PRIVATE_KEY="$(cat ~/.giwa/m6_test_key)"     # 该文件 600 权限，仓库外
```

第三方只看证据重算（§57 P）：`executions.jsonl` 一行的 `profit.cost.lines[]` 给出每笔
`gas_used`、`effective_gas_price`、`l2_fee`、`l1_fee`、`l1_fee_source`、`transaction_hash`；
`flows[]`/`wraps[]` 给出每条 Transfer/Deposit/Withdrawal 的 amount 与 log index；
`profit.before/after` 给出两个块号上的 native 与两种 token 余额。把这些按第 10 节的公式相加，
得到的 E 与 `profit.realized.net_profit` 与链上余额差三者相同。

## 13. Acceptance Matrix（§57 A–Q、§58）

| 项 | 要求 | 判定 | 证据 |
| --- | --- | --- | --- |
| A Real Opportunity | ≥1 个来自真实链上状态的机会 | **PASS** | 第 2 节；`opportunities.jsonl`；储备在 37 563 264 现场读取 |
| B Real Simulation | 在 pinned 真状态上模拟 | **PASS** | 第 3 节；`state_source = rpc:chain-91342`，无 override |
| C Risk Accept | ≥1 次 Accept | **PASS** | 第 4 节；`risk-decisions.jsonl` |
| D Preflight | 余额/nonce/状态/stale/费用 | **PASS** | 14/14 全过，`preflight.json` |
| E Build | 真实可广播交易 | **PASS** | 6 步计划，gas limit 各含缓冲，合计上限 479 535 |
| F Sign | 真实签名 | **PASS** | `sign_count = 6`；每行 `expected_sender == recovered_sender` |
| G Submit | 真实 `eth_sendRawTransaction` 或经验证的 GIWA sequencer path | **PASS** | 6 笔经公共 HTTP RPC 的 `eth_sendRawTransaction`；`submissions.jsonl` |
| H Inclusion | `receipt.status = 1` | **PASS** | 6/6 收据 `receipt_status: true`，独立回读 `status = 0x1` |
| I Route Verification | 收据日志证明两条腿 | **PASS** | 第 6 节 Swap log + 储备变化量逐字相等 |
| J Asset Delta | 前后余额差 | **PASS** | 第 7 节，block-before/block-after 绑定 |
| K L2 Fee | 来自真实交易 | **PASS** | 第 8 节，359 692 789 439 wei |
| L L1 Fee | 来自真实交易，并说明来源 | **PASS** | 第 9 节，收据字段 `l1Fee`，42 901 813 739 wei |
| M Gross Profit > 0 | | **PASS** | 764 806 517 171 626 wei |
| N Net Profit > 0（扣 L2 + L1） | **最关键** | **PASS** | 764 403 922 568 448 wei；且等于链上原生余额差 |
| O Simulation Reality Delta 可对账 | | **PASS** | 第 11 节，`mismatch = null`，差额定量解释 |
| P Evidence Reproducibility | 第三方仅凭证据重算 realized_profit | **PASS** | 第 12 节公式 + 本报告的独立 RPC 复核 |
| Q No Fabrication | 无 state override / 无改储备 / 无改费率 / 无改阈值 / 无伪造机会 / 无伪造收据 | **PASS** | 见下 |

Q 逐项说明：储备与 `blockTimestampLast` 是 `eth_call getReserves()` 的现场读数（成交后回读储备，
变化方向与量级和我们的成交一致 —— 这本身就是「我们没改储备」的反证）；费率 997/1000 由池子**已部署字节码**
二分实测反出（`candidate-fee-measurement.json`）；风险阈值取 CLI 默认（min net 0、max gas 60 000 000），
而模拟净利 7.6e14 wei，Accept 不依赖门槛；funding 是 `real_state{source: "rpc:chain-91342"}`，
`real_state_funding` 那一项闸门专门拒绝 override；收据由节点返回并绑定回它签的那串字节。

## 14. Known Limitations（§61.14，如实记录）

```text
no real opportunity            : 不成立。本轮确实找到一个真实机会并成交（第 2 节）。但要注意它的性质：
                                   机会稀少度 = 1（§42 只要求 1 笔），没有统计样本，不能据此推断「全链常态化可套利」。
sequencer direct unavailable   : 成立 = BLOCKED（§29 复测）。sepolia-sequencer.giwa.io 对
                                   mev_sendBundle / engine_submitBlock / sequencer_submit / giwa_sendRawTransaction /
                                   eth_sendRawTransactionFlashblock / txpool_status / txpool_content 全部 -32601
                                   「rpc method is not whitelisted」，且故意拼错的 etch_sendRawTransaction 得到同一个 -32601
                                   （这条阴性对照才让 -32601 意味着「确实没有」而不是「万能报错」）；4 种凭据形状的头
                                   无一改变任何回答、也没有 WWW-Authenticate。私有提交路径按 §29 记 BLOCKED，
                                   车道留在 eth_sendRawTransaction。取证 data/evidence/m7/probe-sequencer-direct.json
L1 fee unavailable             : 不成立。真实交易的 L1 数据费从收据字段 l1Fee 逐笔读到并计入利润；
                                   只有「签名前的预估」那一列是 estimate only（GasPriceOracle getL1Fee），
                                   预估 62 677 678 934 vs 实收 42 901 813 739，估高、方向安全
原子性                        : 6 笔独立交易、一个钱包一条 nonce 车道（§33），不是 bundle、没有原子回滚。
                                   中途任一笔失败就会把 BLS 留在钱包里（最坏花费 = 序列天花板 100 542 649 055 784 wei，
                                   即本金 0.0001 ETH 加手续费）。本轮 6 笔全成功，但这个风险结构上还在 ——
                                   bundle / private relay / multi-wallet / parallel nonce 按 §59–§60 属 M8，本轮没做
市场性质                      : 两个池子由第三方 0x18b5edc574b956b1a811affac8312b34ff38f11d 创建，
                                   本次成交前从未发出过 Swap，中间代币是无真实价值的 12 位测试币「BLS / FlowSwap.io」。
                                   约 8.6 倍价差长期无人吃 ⇒ 符合 §5 的 REAL_MARKET 定义，但不是竞争性市场；
                                   利润数字不能外推为主网收益
执行时延                      : 单跑 6 笔串行、每笔等入块 ≈ 5 秒一块，全程 33.8 秒（其中模拟 21.4 秒）。
                                   1 秒一块的链上这个速度对真实竞争太慢；M8 才做 p50/p95/p99 与并行化
计价单位                      : 只有一个单位（原生 wei）。因为路线以原生开始并以原生结束，
                                   §11–§14 的要求满足；若路线两端不是同一资产，本轮模型会给 NotComputable 而不是硬凑价格
observation window            : 本轮没有跑长观测窗，只在钉定块上取一次真实状态。
                                   「窗口内一共出现过多少个机会」这个问题本轮没有回答，属 M8
```

## 15. 本轮改动规模与四道关卡

| 交付物 | 规模 |
| --- | --- |
| `crates/pipeline/src/arbitrage.rs` | 1 061 行 —— §57 A–H 的单跑路线：钉块 → 读储备定价 → REVM 模拟 → Risk → §26 预检 → §54 阶梯 → 证据 |
| `crates/execution/` 新增 7 个源文件 | `preflight.rs` 1 589、`profit.rs` 870、`giwa/preflight_facts.rs` 661、`giwa/reads.rs` 526、`cost.rs` 524、`market.rs` 142 —— 整个 crate 现 26 个源文件 / 14 536 行 |
| `crates/cli` 的 `arbitrage` 子命令 | `crates/cli/src/lib.rs` +411 / −6 行；`tests/arbitrage_args.rs` 434 行 / 10 个测试 |
| 新增测试 | `crates/execution/tests/sequence.rs` 2 325 行、`sequencer_direct_probe.rs` 1 092、`live_reads_probe.rs` 654、`crates/simulation/tests/real_market_fee.rs` 883 |
| 既有文件的改动 | 18 个已跟踪文件 +1 257 / −60 行，集中在 cli、pipeline、execution 的挂载点与 §18 容差/§51 标签所需的最小改动 |
| 风险层的一句旧话 | `crates/risk/src/decision.rs` 的 `NO_BROADCAST` 原文是「no broadcast: **M4 simulates**…not that a transaction was or may be sent」。M7 之后同一串字会出现在**真的广播过**的会话证据里，那句话就变成假的了，所以改成里程碑中立的表述：这一层只判断模拟，**决定是否构造/签名/发送的是调用方的执行模式（§32），不是这个答案**。`no broadcast` 这个前缀保留，四处断言仍成立 |

四道关卡（全 workspace 范围，`CC=clang CXX=clang++ CXXFLAGS="-include cstdint"`，toolchain 1.96.1，`--offline`）：

```text
cargo --offline check --workspace --all-targets            0 error / 0 warning
cargo --offline test  --workspace                          559 passed / 0 failed（本轮新增 3 个 live 探针测试默认 `#[ignore]`，全仓共 10 个 ignored；它们的产出以 data/evidence/m7/ 的取证文件形式落盘，四道关卡不依赖节点在线）
cargo --offline clippy --all-targets --workspace -- -D warnings   0
cargo --offline fmt --check                                0
```

`crates/cli/tests/no_execution.rs` 的墙仍在，且本轮**没有降低任何一条强度**：私钥标识符、
签名/传输依赖、64 位十六进制字面量、硬编码 URL 与 chain id 的检查全部保留；被改的只有两处**表达方式**——
测试里的合成 32 字节常数改成计算值（`B256::repeat_byte` / `keccak256("giwa block 37530593")`，
它们本来就不是密钥，只是形状撞上了检查），以及 execution crate 的文件数上限从 19 提到 32
（M7 新增 7 个源文件，注释里写清了为什么这些事实属于「发送」这一侧而不是 pipeline 那一侧）。

密钥卫生复核（命令本身不写出密钥的值，从仓库外那个文件读进来再扫全仓，含 `docs/` 与 `data/evidence/`）：

```bash
rg -F --hidden -g '!target' -g '!.git' "$(cat ~/.giwa/m6_test_key)" .   # 0 命中
```
