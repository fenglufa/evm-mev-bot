# M6 Completion Report

结论（白话版）：**M6 状态 COMPLETE（§43、§55）**。这一轮把「一笔交易从决定到上链」这段路真的走完了，
而且是拿真实钱包在真实 GIWA 测试链上走完的：构造 → 签名 → 提交 → 拿到交易哈希 → 拿到收据 → 收据绑回
它自己签的那串字节 → 链上确认那个区块是真的。

```text
RiskApproved → TransactionIntent → TransactionBuilder → Unsigned Tx → Signer
      → Signed Raw Tx → GIWA Submission → Tx Hash → Receipt
```

**这一轮花掉的真实成本**（一笔 §35 验证交易，用户逐次授权后才发出）：

| 问的是哪一段 | 实测值 | 说明 |
| --- | --- | --- |
| 上链了吗 | 是。区块 **37 503 978**，收据状态 **成功**，gasUsed **21 000** | 交易哈希 `0x8a0ca6bf…471f95` |
| 花了多少 | **28 313 679 344 wei** = 0.0000000283 native | = L2 gas 费 21 008 421 000 + L1 数据费 7 305 258 344；钱包余额 20 000 000 000 000 000 wei，占 **0.0001416 %** |
| 白话换算 | 把整个测试钱包看成 **100 元**，这笔交易约等于 **0.00014 元**（一分钱的 1/700） | 单位：wei → native → 「如果钱包=100 元」 |
| 这是套利吗 | **不是**。零值转账、收款人就是自己、没有任何代币进出 | §42 要求它被单独标成 `M6 execution validation transaction`，两行证据文件里都带 `not_an_arbitrage` |

三件不能被「链路跑通了」盖过去的事：

1. **真实上链的那笔不是套利，是验证交易。** 真实历史里没有任何一个 `RiskDecision::Accept`：
   M4 已经判定 block 37 191 169 的两条候选路线，一条被池子 revert（revert 理由 `K`），一条在
   扣 gas 之前就净亏 714 844 720 991。M6 没有为了让 Accept 出现去动 reserve / fee / tax / 余额 / 阈值
   （§51 禁止，本轮也确实没改）。所以「RiskApproved → 上链」这一段的**真实机会版本**只能记 PARTIAL，
   原因见第 10 节第 1 条，而且它是 §4 范围决定下的结构性结果，不是实现没写完。
2. **GIWA 没有 SequencerDirect 这种专用提交协议。** 这不是「没找到文档」，是同一台端点上
   `mev_sendBundle` / `engine_submitBlock` / `sequencer_submit` / `giwa_sendRawTransaction` /
   `eth_sendRawTransactionFlashblock` / `txpool_status` / `txpool_content` 全部回
   `-32601 rpc method is not whitelisted`，并且故意拼错的 `etch_sendRawTransaction` 得到**同样**的
   `-32601` —— 这条阴性对照才是让 `-32601` 变成「确实不存在」而不是「万能报错」的关键。
   按 §24，`SequencerDirect` 记 **BLOCKED**（取证：`data/evidence/m6/probe-method-whitelist.txt`）。
3. **本轮在自己代码里抓到一个真缺陷，并修在根上。** 那笔交易其实成功上链了，但 `ReceiptTracker`
   把「收据说它在 37 503 978 块，而我这一秒读不到 37 503 978 块的头」判成 `Unbound`，于是整条
   链路把一笔已成功的交易报成 `failed`。修法是**把「没有信息」和「信息互相矛盾」分开**：前者花掉
   一次重试继续轮询，后者才终止（§27 的原意）。修完之后**用真实节点重新问了一遍同一笔交易**，
   追踪器给出 `Included`（第 6 节，取证 `live-retrack-37503978.txt`）。

---

## 1. Summary

新增 1 个 crate（`evm-execution`）+ 1 个 CLI 子命令（`validate`），把 M5 的闭环末端接上真实链：

| 交付物 | 职责 | 规模 |
| --- | --- | --- |
| `crates/execution/`（`evm-execution`） | Intent / Builder / RLP 编解码 / Signer / 门 / Nonce / Fee / 提交 / 收据 / 生命周期 / 证据 / 错误分类 / 模式 | 19 个源文件、**7 458 行**；5 个测试目标、**3 008 行** |
| `crates/cli` 的 `validate` 子命令 | §35 的一次受控尝试：默认「0 值转给自己、21 000 gas、当前最便宜的诚实价」，三档模式（build-only / sign-only / submit） | `crates/cli/src/lib.rs` +515 / −13 行；`tests/validate_args.rs` 7 个测试 |
| pipeline 侧的挂载 | `Abilities` 组装、execution-only 证据通道、按 registry 举证推配置 chain id | `runner.rs` +185、`evidence.rs` +104、`config.rs` +80 |
| M4 真实历史的验收 | `crates/simulation/tests/historical_gate.rs`（5 个测试） | §50 的前一半 |
| 证据 | `data/evidence/m6/` 33 个文件 / 236 KB | 只读探针 8 份原始回答 + 1 份中文汇总、2 次 validate 会话（各 4 个文件）、1 次 replay 车道冒烟（14 个文件）、节点回答冻结 1 份、实链复验输出 1 份 |

明确**没有**做的事（§4 的清单一条都没碰）：multi-chain、flashloan、V3、sandwich、liquidation、
bundle、private relay、MEV-Boost、新套利策略、multi-hop 执行、新 executor contract、自动改阈值。
M1–M5 的正确性语义**没有被为了通过验收而改动**：`crates/state`、`crates/graph`、`crates/opportunity`、
`crates/risk`、`crates/simulation/src` 五个 crate 的源文件在 `git diff` 里**一行未动**，
`crates/chain` 只加了 execution 需要的三个 RPC 读法（`parse_u64` / `parse_u256` / `parse_address` 等
公开化，`rpc.rs` +20 / −8 行、`lib.rs` 改 1 行），`crates/protocol` / `crates/replay` / `crates/live` / `crates/metrics` 零改动。

## 2. Code Changes

**新增 crate `evm-execution`（19 个模块）**

| 模块 | 行 | 承担 |
| --- | --- | --- |
| `lib.rs` | 82 | crate 边界与再导出；§4「不含策略、不含机会发现、不含签名以外的判断」写在模块文档里 |
| `intent.rs` | 475 | §7 的 `TransactionIntent`、`from_run`（含「一步之外就是多步，§4 不允许」的拒绝）、§35 的 `validation` / `validation_call`、§30 的幂等键、`SenderFunding` |
| `builder.rs` | 362 | §9/§10/§13：`TransactionBuilder::build`、`GasPolicy`（模拟 gas + 固定余量 / 显式上界）、§14 的 `round_trip`（八个字段逐个比） |
| `tx.rs` | 678 | §7/§14 的交易本体：legacy 与 EIP-1559 两种信封、最小长度 RLP 编码、`maximum_cost_wei` |
| `rlp.rs` | 277 | 手写 RLP（不引入新依赖），含「前缀边界那一字节没有长度头」「padding 必须拒绝」 |
| `signer.rs` | 398 | §16–§19：`ExecutionKey`（`Debug` 里不含密钥字节）、只在 `GIWA_EXECUTION_PRIVATE_KEY` 出现于 sign 时、`recover_sender`、`Signer::without_key` |
| `gate.rs` | 588 | §32 的 9 条文门：一次报全、顺序稳定；`BlockBinding` / `BalanceEvidence` / `NonceEvidence` 三态（含 `Unverified`） |
| `nonce.rs` | 210 | §11：confirmed / pending 两个视图 + 车道占用；pending 低于 confirmed 是矛盾而不是负数 |
| `fee.rs` | 382 | §12：`FeePolicy`（base fee × 头部余量 + tip）、`FeeReading` 写明来源、legacy 不配 tip 字段 |
| `chain_read.rs` | 58 | §32 里「必须问链才知道」的两条腿：`ChainReader` + `read_binding` |
| `submitter.rs` | 210 | §21/§23：`TransactionSubmitter`、`EndpointKind`（含 `Recorded`，replay 与 live 同一套 API）、`SubmissionOutcome::{Accepted,Rejected,Unknown,Blocked}` |
| `giwa/sequencer_direct.rs` | 487 | §22/§23 的实测端点：唯一被白名单答应的 `eth_sendRawTransaction`；`direct_protocol()` 直接把 BLOCKED 原文说出来 |
| `receipt.rs` | 566 | §26/§27/§28：`bind` 三条腿、`ReceiptTracker::track`、`Receipt::l2_cost_wei`（L1 费单独一个数） |
| `lifecycle.rs` | 1 114 | §28/§29/§30：单向阶梯、身份三元组、车道、幂等 claim、记录表 → 证据行 |
| `stage.rs` | 926 | §44–§46：`ExecutionStage`（价格 → claim → 门 → 模式 → 签 → 提交 → 收据），§45 的「Risk 没点头就连端点都不读」 |
| `evidence.rs` | 422 | §52/§53：签名行与提交行各自的字段清单，§58「不得出现能当密钥的东西」 |
| `mode.rs` | 101 | §20：BuildOnly（默认）/ SignOnly / Submit，`may_sign` / `may_submit`，名字认错不会退回安全档 |
| `error.rs` | 101 | §39 的 11 个分类全在，外加 `OverrideDependent` / `ModeGate` / `AlreadyExecuted` / `ReceiptBinding` |
| `giwa/mod.rs` | 21 | 端点适配器的目录，只有 `pub mod sequencer_direct;` 与文档 |

（表里 19 行加起来就是上面那个 **7 458 行**，逐文件 `wc -l` 现量得到。）

**新增 trait（4 个，全部可被脚本端点替换）**：`FeeSource`、`NonceSource`、`ChainReader`、
`TransactionSubmitter`。`Abilities` 把四者组装成一次运行需要的读能力，所以 §40 允许「把提交接口
脚本化」而不是伪造市场数据。

**新增 CLI**：`evm-mev-bot validate`（`parse_validate` / `ValidationPlan::to_plan`），
`--rpc-url`（可来自 `GIWA_RPC_URL`）、`--execution-mode`、`--sender`、`--to`、`--value-wei`、
`--gas-limit`、`--registry-dir`、`--evidence-dir`、`--json`（逐条出自 `--help` 现读）；
**没有任何接受密钥材料的参数**（`tests/validate_args.rs` 用合成的 `0xaaaa…` 试过一次：被 clap 当
`unexpected argument` 拒掉，且 stderr 不得回显该串）。

**测试新增（本轮 7 个新 target）**

| target | 数量 | 覆盖 |
| --- | --- | --- |
| `execution/tests/lane_matrix.rs` | 14 | §40 生命周期矩阵：claim/幂等/未知答/超时/回滚 |
| `execution/tests/stage_matrix.rs` | 15 | §Q/§L/§M/§P/§R/§T/§U/§J：整条阶梯 + 每一档模式的天花板 |
| `execution/tests/real_codec.rs` | 7 | §14：链上真实交易的信封 decode 回验（同一字节换 chain id 就不再恢复出同一发送方） |
| `execution/tests/real_validation_receipt.rs` | 5 | §41/§V：把冻结的节点回答喂进真解码路径 + 本轮修复的回归钉 |
| `execution/tests/real_validation_live.rs` | 2（`#[ignore]`） | 修好的追踪器**今天再问一次真节点**（第 6 节） |
| `simulation/tests/historical_gate.rs` | 5 | §50 前一半 + §K：RiskRejected 连端点都不能读 |
| `cli/tests/validate_args.rs` | 7 | §35/§58：参数面、默认值、拒收密钥、两子命令不混淆 |

守卫 `cli/tests/no_execution.rs` 从「全仓不许出现执行」改成 M6 的口径：**关键字禁令覆盖除
`crates/execution/src` 之外的每个 crate 源目录**，同时把 64 位十六进制字面量扫描扩到
`crates/execution/src` 与 `crates/execution/tests/*.rs`（fixtures 目录除外，因为真实交易的 `r`/`s`
和密钥长得一样）。这条扫描现在要求「必须真的打开过 `real_codec.rs`」，否则一个什么都不读的守卫
会因为读不到东西而绿色。

## 3. Transaction Construction

那一笔真实上链的交易，**每个字段的实际来源**（全部出自 `validate-91342-1790849090542` 会话记录，
不是事后补写）：

| 字段 | 值 | 来源 |
| --- | --- | --- |
| chain id | **91342** | 三方对照（§6）：配置侧由已提交 registry 的池子归属推出（`attested_chain_ids`，代码里没有 `91342` 常量）；intent 侧来自 pin；端点侧 `eth_chainId` = `0x164ce`。不相等 = 停止，不是警告 |
| 交易类型 | **EIP-1559**（`dynamic_fee`） | §5 取证：同一块 38 笔交易里 19 笔 `0x2` + 18 笔 `0x0` + 1 笔 `0x7e` ⇒ 类型是 intent 的显式字段，默认 1559，legacy 同样过解码回验（`probe-tx-types.json`） |
| nonce | **0** | `eth_getTransactionCount`：confirmed 在 `#37 503 974` 读、pending 单独读，两次都挂在同一次 `eth_getBlockByNumber("latest")` 之后（§11 要的是「有没有在途」这件事，不是猜） |
| gas limit | **21 000** | §35 的默认最省钱档（值 0、无 calldata 的转账）。套利 intent 走另一条：`GasPolicy::SimulationGasPlus { margin: 20 000 }`，上界 `maximum_gas_limit = 60 000 000`（见 `lane-smoke` 会话的 `execution.configuration`） |
| maxFeePerGas | **1 000 806** | 原文：`fee: eip1559 maxFeePerGas = baseFee(403) * 2 + tip(1000000) = 1000806, read at block 37503972`。base fee 取 pin 的块头，tip 取 `eth_maxPriorityFeePerGas`，头部余量 2 块，全程 U256 |
| maxPriorityFeePerGas | **1 000 000** | 同上，端点自己报的数 |
| value | **0**（`0x0`） | §35：验证交易不转钱 |
| calldata | 空（`data_hash = keccak256("")`） | 没有合约调用；`to` 就是发送方自己 |
| 状态 | pin 在 block **37 503 972** / `0x677a529f…adf715` | §8 的三条绑定：块号、块哈希、simulation id 一起进 intent，门再拿端点当时的回答核对那个块号还是不是那个哈希 |
| §34 边界 | `require_unoverridden_state = true` | 凡是需要 state override 才成立的 simulation，intent 在 Builder 就停（`an_intent_whose_run_needed_a_state_override_never_reaches_bytes`、`an_override_funded_intent_never_reaches_bytes`），本轮还修掉一处根因：override 标志不能写死 `false` |

一致性（§14/§15）：`TransactionBuilder::round_trip` 在**每一次**阶梯运行里都跑（`stage.rs` 签名之前），
八个字段 sender / target / value / calldata / nonce / gas_limit / fee / chain_id 逐个比，不是只比
`to` 和 `data`。target 缺席（合约创建）时按「不存在」比较，绝不换算成零地址。

## 4. Signer

* **签名**：k256 ECDSA over EIP-1559 的 signing hash，`sign_and_recover` 一次给出字节与恢复结果；
  `signing_hash` 与 `signed_tx_hash` 都进了 §52 的证据行（所以「签的是哪串」可复核）。
* **发送方恢复**：真实链上这笔的 `recovered_sender` == 收据里的 `from` == §52 行的 `expected_sender`，
  三方一致；`real_codec.rs` 另外证明「同样的字节放到别的 chain id 上就恢复不出这个发送方」。
* **私钥处理**（§17/§19）：只从 `GIWA_EXECUTION_PRIVATE_KEY` 读，只在 sign 那一刻进内存；
  `ExecutionKey` 的 `Debug` 不含原始字节（有测试钉住）；BuildOnly 模式下即使环境变量存在也**不去读**
  （`build_only_never_reads_the_environment_even_when_the_variable_is_present`）；
  `mode` 与 `signer` 不一致时 `ExecutionStage::new` 直接拒绝装配。
* **测试不拿真钥匙**：§40 要求的签名测试全部用合成标量 `TEST_SCALAR = 1`；控制注入用 `0xaaaa…`。
* **本轮实测的钱包**：`0xd450630c1c55b1c7df1ebf7eeaee1fffb45e520c`，私钥只存在于仓库外的
  `~/.giwa/m6_test_key`（权限 600）与进程环境。仓库、fixture、日志、证据、报告里 0 命中
  （第 8 节 J 条给出扫描方法）。

## 5. Submission

| 问的事 | 答案 | 证据 |
| --- | --- | --- |
| 真实验证过吗 | **是**。`eth_sendRawTransaction` 返回了交易哈希，交易随后被挖进块 | `validate-91342-1790849090542/submissions.jsonl`：`outcome=submitted`、`detail="eth_sendRawTransaction returned 0x8a0ca6bf…"` |
| 走的是哪种端点 | `public_http_rpc`（`https://sepolia-rpc.giwa.io`，从环境/证据读，代码里没有常量 URL） | `probe-summary.md` 第 7 行（方法可用）+ 第 9 行（官方只列这两个 URL）；提交行 `submission_endpoint_type = public_http_rpc` |
| flashblocks 端点呢 | 同一个提交入口的另一个前置：`eth_sendRawTransaction` 与主端点返回**逐字相同**的两条解码错误，`eth_chainId` 同为 `0x164ce` ⇒ 不是第二种协议 | `probe-flashblocks-endpoint.txt` |
| SequencerDirect | **BLOCKED**。§24 的处置是照字面执行：类型保留 §22 给的名字，提交路径用端点真正答的那个方法，`direct_protocol()` 直接把「不存在」连同取证文件名一起说出来 | `probe-method-whitelist.txt` |
| 有没有重试造成重复交易 | 没有。提交层**一次都不重试**：`Unknown` 保留车道并占住 nonce（§25），只有明确 `Rejected` 才释放车道并把本次判失败 | `only_a_refusal_proves_nothing_is_in_flight`、`an_unknown_answer_holds_the_lane_and_the_record`；真实运行里那笔 `submitted` 之后车道正是被**占住**的（`lane.held` 那句话就是 §25 的行为） |

## 6. Receipt

真实收据（tx hash / block / status / gas 四项，§59 点名要的）：

```text
transactionHash    0x8a0ca6bf59dada20c7ffc64c43f51fa603e821bae7e62d874e0c17b159471f95
blockNumber        37503978   (0x23c43ea)
blockHash          0xfdf3508c899e7ff9d8fd8fd35300a7d6adfab97ca869f5df2e0b086d685ce3af
status             0x1  → ReceiptStatus::Included
gasUsed            21000
effectiveGasPrice  1000401 wei        → L2 bill 21008421000 wei
l1Fee              7305258344 wei     → OP 栈额外收的 L1 数据费，单独一个数
from / to          0xd450630c…520c / 同一地址（验证交易付给自己）
nonce              0 → 1（该账户只执行了这一笔）
```

**这一节的核心不是「拿到了收据」，是「收据拿到了却被我们报错」这件事怎么被抓到、怎么修的。**

1. 现场：live 那次提交结束后 `status=failed`，理由是
   `receipt binding broken: the endpoint has no block 37503978 at all, and the receipt says this
   transaction is in it`。
2. 取证（只读 RPC，直接问节点）：`eth_getTransactionReceipt` 给的块号是 37 503 978、块哈希
   `0xfdf3508c…`；`eth_getBlockByNumber("0x23c43ea", false)` 回来的哈希**逐字相同**。也就是说
   收据说的是真话，是我们的读法把「这一秒读不到」当成了「互相矛盾」。两份回答冻结在
   `data/evidence/m6/validation/node-answers-37503978.json`。
3. 根因修法（不是放宽 §27）：`track` 的两种否定答案被分开——
   **块读回来是「这个高度没有块」= 缺乏信息 = 花掉一次重试继续轮询**；
   **块读回来是「另一个哈希」= 事实矛盾 = 仍然 `Unbound` 并把两个哈希写进理由**。
   §27 的第三条腿因此一点没松。
4. 钉住：`receipt.rs` 两条单元测试
   （`a_block_that_is_not_readable_yet_spends_an_attempt_instead_of_ending_the_run`、
   `a_block_that_never_becomes_readable_times_out_without_blaming_the_receipt`）+
   `real_validation_receipt.rs` 两条（超时报 `Timeout` 而不是 `NotFound`；换了哈希仍然 `Unbound`）。
5. **实链复验**（本轮最后一步，只读、不花钱、不动 nonce）：用修好的代码在 `SignOnly` 模式（该模式
   不可广播）下，对**同一笔交易、同一个端点**重新走一遍 `submitter.receipt()` → `bind` →
   `chain.block_hash_at()` → `ReceiptTracker::track`，结果是 `Included`；同时余额与 nonce 的读值
   等于冻结成本段算出来的数（`before − total == after`，实测 `20000000000000000 − 28313679344 =
   19999971686320656`，链上现读一致；`confirmed == pending == 1`）。
   原始输出：`data/evidence/m6/validation/live-retrack-37503978.txt`。
   **注意诚实边界**：这条复验用的仍是那笔已上链交易，本轮**没有**发出第二笔交易。

## 7. Real Evidence

| 结论 | 证据 |
| --- | --- |
| 端点 | `https://sepolia-rpc.giwa.io`（配置来自 `GIWA_RPC_URL`；证据文件里记的是当次实际用的 URL，代码零硬编码） |
| chain id | `eth_chainId` = `0x164ce` = 91342，且 `net_version` = `"91342"` 一致（`probe-read-surface-1.txt`） |
| 区块 | 验证交易 pin 在 37 503 972（`0x677a529f…`），被挖进 37 503 978（`0xfdf3508c…`） |
| 交易哈希 | `0x8a0ca6bf59dada20c7ffc64c43f51fa603e821bae7e62d874e0c17b159471f95` |
| 收据 | 上面第 6 节整段 + `node-answers-37503978.json` 里的 `eth_getTransactionReceipt` 原文 |
| 会话记录 | `data/evidence/m6/validation/validate-91342-1790847616435/`（sign-only，1 行签名、1 行 `outcome=blocked`、**0 行提交**）与 `validate-91342-1790849090542/`（submit，`outcome=submitted`） |
| 车道 / 模式 | sign-only 那次的 `submissions.jsonl` 写的是 `blocked: "sign-only over a public_http_rpc endpoint: the signed bytes exist and were never handed to a node (§20/§24)"` —— §20 的三档不是枚举里的三个字符串，是证据里的一句话 |
| 真实历史不带执行 | `data/evidence/m6/lane-smoke/replay-91342-1790843285579/`：50 块 / 52 事件 / 9 次状态写入 / 2 个机会 / 1 次模拟 / **accept 0、reject 1** / `execution_skipped_not_accepted = 1` / `execution.attempts = 0` / `signed_bytes_to_a_node = 0` |
| 探针（只读） | `probe-read-surface-1/2.txt`、`probe-pending-and-gas.txt`、`probe-tx-types.json`、`probe-submission-surface.txt`、`probe-method-whitelist.txt`、`probe-flashblocks-endpoint.txt`、`probe-raw-tx-access.txt` + 中文汇总 `probe-summary.md`（14 行实测表 + 由此得到的 4 条设计约束） |
| 已知拿不到的 | `eth_getRawTransactionByHash` → `-32601 rpc method is not whitelisted`（`probe-raw-tx-access.txt`）⇒ 「把链上的原始字节取回来跟我们签的字节比」这条验证做不了，只能用块哈希 + 收据绑定 + 恢复出的发送方三条替代 |

## 8. Tests

四道关卡，本轮**最后一次全量运行**的真实结果（环境 `CC=clang CXX=clang++
CXXFLAGS="-include cstdint"`，`--offline`）：

| 关卡 | 命令 | 结果 |
| --- | --- | --- |
| A 格式 | `cargo fmt --all -- --check` | exit 0。首次跑出 1 处 diff（`real_validation_receipt.rs` 换行），用 `cargo fmt --all` 修掉后复校 0 diff |
| B 编译 | `cargo check --workspace --all-targets` | exit 0 |
| C 测试 | `cargo test --workspace` | exit 0。**469 通过 / 0 失败 / 7 忽略**，55 组结果（41 个测试二进制 + 14 个 doc-test；M5 收口时是 350 通过 / 5 忽略 ⇒ 本轮 +119）。7 个忽略项：M5/M4 的 5 个网络/重录类 + 本轮新增的 2 个实链复验（都要真节点，关卡不能依赖它在线） |
| D clippy | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | exit 0。首次报 1 条：`crates/cli` 的 `large_enum_variant`（`Command::Live` 400 B vs `Command::Validate` 176 B）。修法是根因方向 —— `Live(Box<LiveArgs>)`，**没有加 `#[allow]`**；`--help` 文本与 21 个 CLI 测试不受影响（已复跑） |

`execution` crate 自己：62 单元测试 + 14 lane_matrix + 15 stage_matrix + 7 real_codec +
5 real_validation_receipt = **103 通过 / 0 失败**，外加 2 个 `#[ignore]` 的实链复验（手动跑 2/2 通过）。

## 9. Acceptance Matrix（§54 A–W 逐条）

| 条 | 判定 | 依据（可点开的东西） |
| --- | --- | --- |
| A Workspace `cargo fmt --check` | **PASS** | 第 8 节 A 行，exit 0 |
| B Check | **PASS** | `--all-targets` 一并跑，exit 0 |
| C Tests | **PASS** | 469 / 0 / 7 |
| D Clippy | **PASS** | 修 `large_enum_variant` 后 exit 0 |
| E Chain ID 真实 RPC = 91342 | **PASS** | `eth_chainId` + `net_version` 双读（`probe-read-surface-1.txt`）；§6 三方对照，任一不合就停（`a_stale_opportunity…` 之外的 `endpoint_chain_id` 腿 + `the_fixed_tracker…` 里现读的断言） |
| F Transaction Type | **PASS** | 同块 19×`0x2` + 18×`0x0` + 1×`0x7e` ⇒ 类型是显式字段；真上链那笔是 EIP-1559，legacy 也在解码回验里（`real_codec.rs`、`probe-tx-types.json`） |
| G Builder：RiskApproved intent 能生成 unsigned tx | **PASS**（脚本门）／**PARTIAL**（真实 RiskApproved） | 生成路径：`lane_matrix`/`stage_matrix`；真实历史没有 Accept ⇒ 见第 10 节第 1 条。§35 的验证 intent 走的是同一条 Builder |
| H Builder Round Trip（8 字段一致） | **PASS** | `round_trip` 每次运行都跑（`stage.rs` 签名前）；`real_codec.rs` 7 项对链上真交易；「少一个字节的签名重算不出同一个哈希」也有测试 |
| I Signer：sign → recover sender | **PASS** | 真实那笔 `recovered_sender == 收据 from == expected_sender`；跨链 id 不复现同一发送方（negative control） |
| J Private Key Isolation | **PASS** | 守卫扫 `crates/execution/src` + `tests/*.rs` 的 64-hex 字面量；另加人工全仓（含未跟踪与 `target/`）grep：仓库内 **0 命中**；密钥只在 `~/.giwa/m6_test_key`(600) 与环境中 |
| K Risk Gate：RiskRejected ⇒ no build | **PASS** | `historical_gate.rs` 5 项，四个端点方法接在一个「一问就 panic」的对象上，断言的是**没有 panic**；`execution_refused_before_claim` 计数 = 1；replay 冒烟里 `execution_skipped_not_accepted = 1`、`attempts = 0` |
| L Stale Gate：no sign / no submit | **PASS** | `a_stale_opportunity_blocks_and_build_only_cannot_sign`、`a_pin_the_chain_no_longer_holds_blocks_before_signing`（§32 的重org 腿） |
| M Balance Gate：insufficient ⇒ no submit | **PASS**（**上界本身有已知低估，见第 10 节第 3 条**） | `an_insufficient_balance_blocks_with_both_numbers_in_wei`、`an_insufficient_balance_stops_before_the_signer`；余额必须在 pin 块读、和 `gas_limit × max_fee + value` 比 |
| N Submission `eth_sendRawTransaction` 真实验证 | **PASS** | 第 5 节：真返回哈希、随后被挖；SequencerDirect 那半是 BLOCKED（W 条） |
| O Receipt 真实获取 | **PASS** | 第 6 节：收据 + 块哈希绑定 + 修好后的实链复验 `Included` |
| P Revert 记成 `Reverted` 而不是 Success | **PASS**（脚本；真实那笔没有 revert） | `a_reverted_receipt_is_recorded_as_reverted` ×2、`a_read_error_retries_within_the_budget_and_a_reverted_receipt_stays_reverted`；收据 `status` 只要不是 0/1 就直接报错而不是猜 |
| Q Lifecycle 全阶梯 | **PASS** | `a_submitted_validation_run_reaches_included`（Detected→Simulated→RiskApproved→Built→Signed→Submitted→Included 逐级）；真实侧到 `Submitted` + 收据 `Included`（第 6 节） |
| R Idempotency | **PASS** | `a_second_attempt_on_the_same_state_claims_nothing`、`a_duplicate_claim_returns_the_record_that_already_owns_the_state`；幂等键 = `opportunity_id\|simulation_id\|state_fingerprint`（真实会话里那一行就是这三段） |
| S State Binding（块号 + 块哈希 + simulation_id） | **PASS** | `state_binding()` 进 intent；门用现读块头核 `BlockBinding`，缺读 = `Unverified` = 不通过 |
| T No Override | **PASS** | `require_unoverridden_state = true`（replay 冒烟的 `execution.configuration` 里就在）+ 两条测试 + §34 的根因修正（override 标志不得写死 `false`） |
| U No Automatic Send | **PASS** | `ExecutionMode::default() == BuildOnly`；`only_the_named_mode_can_broadcast`；`build_only_never_reads_the_environment…`；CLI 里 `--execution-mode` 不存在时不签不发；本轮真实提交是**逐次问过用户**才做的 |
| V Real Evidence | **PASS** | 第 7 节表：每个真实链结论后面都挂着原始回答文件 |
| W No Fake Completion | **PASS**（合规意义上） | SequencerDirect、`eth_getRawTransactionByHash`、G 的真实侧、§50 第二半、§15 —— 全部记 BLOCKED/PARTIAL 并附原始 RPC 文本，没有一项被写成 PASS |

§55 的判定：**COMPLETE**。依据是 §43 明确过 M6 的完成定义就是
construction + signing + submission + receipt 四段真打通，而这四段有链上收据与实链复验背书；
realized profit 属于 M7。上面 G 的真实侧、以及第 10 节列的边界，都在 COMPLETE 之外单独记着。

## 10. Known Limitations

1. **真实 `RiskApproved` 的机会走不到执行**（结构性，非缺陷）。两个独立原因叠在一起：
   (a) M4 已判死：block 37 191 169 的两条候选一条 revert（理由 `K`）、一条扣 gas 前净亏
   714 844 720 991 ⇒ 任何阈值都不会把它抬成 Accept（`risk_decision.rs` 对每个最小值都证过）；
   (b) 真实路由是 **7 步**序列，而 §4 明令禁止新 executor contract / multi-hop 执行，所以
   `TransactionIntent::from_run` 对 `steps.len() != 1` 直接拒（错误原文就写着这个理由）。
   ⇒ §50 的第二半（RiskApproved fixture → Builder → Signer）与 §15 的完整箭头链只能
   **PARTIAL**：§50 允许「完全基于真实历史状态的 fixture」，但真实历史上没有既单步、又能诚实
   盈利/被接受的机会，造一个就等于 §51 禁止的那种伪造。承担这段的是 §35 的验证 intent（同一
   个 Builder、同一个 Signer、同一道门）+ 脚本车道（§40 明确允许脚本化提交接口）。
2. **SequencerDirect 不存在**：`BLOCKED`，见第 5 节。M7 若需要私有提交，得先有端点方给的接口，
   不是这里能补的。
3. **§33 的余额上界低估了 OP 栈账单**：`gas_limit × max_fee + value` 只覆盖 L2 那一半，
   `l1Fee` 在它之外。实测：签的上界 21 000 × 1 000 806 = **21 016 926 000 wei**（两个因子都出自
   §52 的签名行），链上真收 **28 313 679 344 wei**（L2 21 008 421 000 + L1 7 305 258 344）
   ⇒ **实际支出比门里那句「最大花费」大 34.7 %**（28 313 679 344 ÷ 21 016 926 000 = 1.347）。
   顺带一句白话：L2 那一半本身比上界少 8 505 000 wei（21 000 × (1 000 806 − 1 000 401)），
   少的那部分是「上界没花满」，多的那部分是「上界根本不含 L1 费」，两件事不要混。
   本轮的处理：把两处对用户说的话讲明白（`stage.rs` 的 balance source、`gate.rs` 的拒绝理由），
   但**没有**擅自给门加一个新旋钮（§4 不许自动改语义），而是把它作为待决项交给 M7 ——
   M7 结 realized profit 时必须把 `l1Fee` 记进成本（`Receipt::l1_fee` 已经是独立字段，
   `l2_cost_wei()` 明确只算 L2，测试 `a_receipt_folds_in_and_the_l1_fee_stays_its_own_number` 钉着）。
4. **`eth_getRawTransactionByHash` 未白名单** ⇒ 无法把「链上存着的原始字节」跟「我们签的字节」
   逐字节比，只能靠块哈希绑定 + 收据 `from` + 恢复出的发送方三条替代。
5. **闪块端点没被当提交路径用过**：它跟主端点回答逐字相同（探针第 10 行），本轮没有理由走第二条
   通路，因此 `FlashblocksHttpRpc` 这个 `EndpointKind` 只有类型层的真实（会被门/证据读取），
   没有实链运行。
6. **§58「生产代码无 unwrap/expect」这条没做到字面零**：全仓生产代码（每个文件切到第一个
   `#[cfg(test)]` 为止）里共 **37** 处 `unwrap/expect/panic/unreachable`，其中 **M6 新增 crate 5 处**
   （`evidence.rs` 2 + `lifecycle.rs` 2 + `stage.rs` 1）、**M6 在 `pipeline/runner.rs` 加 3 处**
   ⇒ 本轮 8 处，其余 **29 处属 M1–M5**（`simulation/state.rs` 11、`pipeline/runner.rs` 6、
   `opportunity/support.rs` 6，另 6 处散在 live/pipeline/risk/simulation）。
   本轮这 8 处全是「序列化自家 `Serialize` 类型」或「刚刚从 map 里取出来的键」，链上任何回答都不能
   让它们触发。本轮**没有**为了这条指标去重构 M1–M5 的代码，如实报数字。其中
   `pipeline/runner.rs:370-378` 那 5 个 `as_str().unwrap()` 读的是本仓自己写的证据行，若证据格式漂移
   会 panic —— 那是 M5 遗留，M7 之前值得单独收一次。
7. **模拟出来的价与真实价之间没有闭环**：Builder 用 `GasPolicy::SimulationGasPlus`（模拟 gas +
   20 000 余量），M4 的 REVM 看不见 `l1Fee`、也看不见 pending 池里的竞争；M6 明确不做 gas 竞价
   （§4），所以「能不能被打包」这件事 M6 不承诺。

## 附 A：与任务书的偏离（记录，不改范围）

| 任务书写的 | 本轮做的 | 为什么 |
| --- | --- | --- |
| §59 报告路径 `docs/m6-completion-report.md` | `docs/v0.1/M6 Completion Report.md` | M1–M5 五份报告都在这个目录、这个命名；「以仓库真实约定为准」优先于文档里的相对路径。§59 要求的 11 节内容逐节都在（1 Summary / 2 Code Changes / 3 Transaction Construction / 4 Signer / 5 Submission / 6 Receipt / 7 Real Evidence / 8 Tests / 9 Acceptance Matrix / 10 Known Limitations + 本附） |
| §59 要求英文小节名 | 正文中文、小节名保留英文 | 用户协作口径（中文、结论先行、零术语），小节结构不变 |
| §13 建议的 receipt 重试参数 | 默认 `attempts=12, between=1 s`（≈12 个块，按 GIWA ≈1 s 出块实测）；实链复验测试用 `attempts=4, 250 ms` | 关卡里那条只在「已上链」的事实上跑，重试次数只是不给它 12 秒的空等 |

## 附 B：复现 —— 报告里每个数字来自哪条命令

```bash
export CC=clang CXX=clang++ CXXFLAGS="-include cstdint"

# 第 8 节 四道关卡
cargo fmt --all -- --check
cargo check --workspace --all-targets --offline
cargo test  --workspace --offline            # 469 通过 / 0 失败 / 7 忽略
cargo clippy --workspace --all-targets --all-features --offline -- -D warnings

# execution 自己的 103 项
cargo test -p evm-execution --offline

# 第 6 节的实链复验（只读；端点 URL 从证据文件读，SignOnly 模式不能广播，不花钱）
cargo test -p evm-execution --test real_validation_live --offline -- --ignored --nocapture
#   输出已冻结在 data/evidence/m6/validation/live-retrack-37503978.txt

# 第 7 节的 replay 车道冒烟（零真实提交：execution.attempts = 0）
cargo run --offline --bin evm-mev-bot -- live --replay-dir data/replay-corpus \
    --start-block 37191150 --end-block 37191199 --execution-mode build-only

# 第 10 节第 6 条的计数
#   生产代码（每个文件切到第一个 #[cfg(test)] 之前）里 unwrap/expect/panic/unreachable 的逐文件计数
#   全仓 37，其中 crates/execution/src = 5，crates/pipeline/src/runner.rs 新增 3（HEAD 6 → 现 9）

# J 条：仓库里不得有私钥。命令本身不写出密钥的值，从仓库外那个文件读进来再扫
rg -F --hidden -g '!target' -g '!.git' "$(cat ~/.giwa/m6_test_key)" .   # 0 命中
cargo test -p evm-cli --test no_execution --offline                          # 5 项
```

真实提交那条命令（**本轮只在用户逐次授权后跑过一次，不要随手重跑**，它会花真钱并占用 nonce）：

```bash
GIWA_RPC_URL=https://sepolia-rpc.giwa.io \
GIWA_EXECUTION_PRIVATE_KEY="$(cat ~/.giwa/m6_test_key)" \
cargo run --offline --bin evm-mev-bot -- validate --execution-mode submit
```
