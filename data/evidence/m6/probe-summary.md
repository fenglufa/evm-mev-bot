# M6 §5 / §6 / §41 — GIWA 执行面接取证（只读，未发送任何真实交易）

取证时间：2026-10-01（本机 UTC+8）。所有响应原文保存在本目录：
`probe-read-surface-1.txt`、`probe-read-surface-2.txt`、`probe-tx-types.json`、
`probe-submission-surface.txt`、`probe-method-whitelist.txt`、`probe-flashblocks-endpoint.txt`、
`probe-pending-and-gas.txt`。

| # | 项目 | 实测结果 | 出处 |
| --- | --- | --- | --- |
| 1 | chain id | `eth_chainId` = `0x164ce` = **91342**；`net_version` = `"91342"`（两者一致，配置校验可成立） | `probe-read-surface-1.txt` |
| 2 | 链头 | `eth_blockNumber` 在取证窗口内从 `0x23bfdbd`(37 486 013) 走到 `0x23bfe7b`(37 486 203)：约 190 块，块间隔 ≈1 s | 同上 + `probe-flashblocks-endpoint.txt` |
| 3 | EIP-1559 生效 | 最新块 `baseFeePerGas` = `0x173` = 371 wei；`eth_feeHistory` 返回 `baseFeePerGas` 数组与 `reward` 百分位 | `probe-read-surface-2.txt` |
| 4 | 建议费 | `eth_gasPrice` = `0xf43b2` = 1 000 370 wei = base(371) + tip(1 000 000 量级)；`eth_maxPriorityFeePerGas` = `0xf4240` = 1 000 000 wei；feeHistory 50 分位 tip = `0xf4227`/`0xf423f` | `probe-read-surface-1.txt`、`probe-read-surface-2.txt` |
| 5 | 交易类型实况 | 一个真实块的 38 笔交易：**19 笔 type `0x2`（EIP-1559）+ 18 笔 type `0x0`（legacy）+ 1 笔 type `0x7e`（OP 链存款交易）**。两种用户类型都被链接受，因此 M6 不能硬编码任何一种 | `probe-tx-types.json` |
| 6 | 真实 1559 交易字段 | `maxFeePerGas` = `0x3bc6d8`(3 917 784)、`maxPriorityFeePerGas` = `0xe9fb9`(957 369)、`gasPrice` = `0xea12c`(950 572)、`chainId` = `0x164ce`、`nonce` = `0x1af`、`accessList` = `[]`、`yParity` = `0x1` | `probe-submission-surface.txt` |
| 7 | `eth_sendRawTransaction` **可用** | 无参数：`-32602 missing value for required argument 0`（说明方法在白名单里）；喂 `0xdeadbeef`：`-32602 rlp: value size exceeds available input length`；喂截断的 1559 信封 `0x02f80101`：`-32602 rlp: non-canonical size information for types.DynamicFeeTx`。⇒ 方法存在且节点真的在解码（错误串来自 Go op-node 的 `types.DynamicFeeTx`） | `probe-submission-surface.txt` |
| 8 | SequencerDirect 专用协议 **不存在** | 同一 RPC 上：`eth_sendRawTransaction` 返回参数错误（存在），而 `mev_sendBundle` / `engine_submitBlock` / `sequencer_submit` / `giwa_sendRawTransaction` / `eth_sendRawTransactionFlashblock` / `txpool_status` / `txpool_content` 全部 `-32601 rpc method is not whitelisted`；阴性对照 `etch_sendRawTransaction`（故意拼错）同样 `-32601` ⇒ `-32601` 确实意味着「未白名单」，不是万能报错 | `probe-method-whitelist.txt` |
| 9 | 官方文档口径 | `docs.giwa.io` 「Connect to GIWA」只列 `https://sepolia-rpc.giwa.io` 与 `https://sepolia-rpc-flashblocks.giwa.io`，chain id 91342；「Flashblocks」页**没有任何**专用提交 RPC，原文只说 `When you submit a transaction on GIWA, it is normally confirmed once it is included in a block.` | 2026-10-01 抓取 |
| 10 | flashblock 端点也接受提交 | 在 `https://sepolia-rpc-flashblocks.giwa.io` 上 `eth_sendRawTransaction` 与主端点返回**逐字相同**的两条解码错误；`eth_chainId` 同为 `0x164ce` ⇒ 低延迟端点是同一提交入口的另一个前置，不是第二种协议 | `probe-flashblocks-endpoint.txt` |
| 11 | Receipt 形状 | `eth_getTransactionReceipt` 返回 `status`、`type`、`transactionHash`、`transactionIndex`、`blockHash`、`blockNumber`、`gasUsed`、`cumulativeGasUsed`、`effectiveGasPrice`、`contractAddress`、`logs`、`logsBloom`，**并且额外带 OP 栈字段** `l1Fee`、`l1GasPrice`、`l1GasUsed`、`l1BaseFeeScalar`、`l1BlobBaseFee`、`l1BlobBaseFeeScalar`、`blobGasUsed`、`daFootprintGasScalar` | `probe-submission-surface.txt` |
| 12 | **L1 数据费在 EVM gas 之外** | 该 1559 交易：L2 成本 = `gasUsed 47 270 × effectiveGasPrice 950 572` = 44 933 531 640 wei，另有 `l1Fee` = `0x1b7cc1c00` = 7 378 574 336 wei（≈L2 成本的 16%）。这笔钱**不体现在 `gasUsed × effectiveGasPrice`，revm 仿真也看不见** ⇒ M6 的 `estimated_execution_cost` 必须显式声明它不含 l1Fee，M7 结算 realized profit 时必须补上 | 同上，按上面两个 hex 现算 |
| 13 | pending 状态可寻 | `eth_getBlockByNumber("pending")` 返回完整块（`number` 比 canonical head 高、`baseFeePerGas` = `0x173`、49 笔交易）；`eth_getTransactionCount(addr,"pending")` 可用 | `probe-pending-and-gas.txt` |
| 14 | 其余 | `eth_accounts` = `[]`（节点不托管账户，签名必须本地做）；`eth_estimateGas`（自转 1 wei）= `0x5208` = 21 000 | `probe-read-surface-1.txt`、`probe-pending-and-gas.txt` |

## 执行钱包（本步只做只读查询，未构造、未签名、未发送任何交易）

- 由 `GIWA_EXECUTION_PRIVATE_KEY` 指向的测试私钥本地推导出发送方地址
  `0xd450630c1c55b1c7df1ebf7eeaee1fffb45e520c`（推导用的 k256 + keccak256 与后面 Signer 用的是同一套原语；
  私钥本身只存在于仓库外的本地文件 `~/.giwa/m6_test_key`，权限 600，**没有**写进仓库、日志、fixture、证据或报告）。
- `eth_getBalance(addr,"latest")` = `0x470de4df820000` = **20 000 000 000 000 000 wei = 0.02 native**。
- `eth_getTransactionCount(addr,"latest")` = `0x0`，`"pending"` = `0x0` ⇒ 该钱包从未发过交易，且当前无在途交易，
  两个 nonce 视图一致（§11 需要区分的两种 nonce 在真实 RPC 上都能取到）。
- 可支付能力（用实测数，百分比按 20 000 000 000 000 000 wei 的余额算）：
  21 000 gas × `eth_gasPrice` 1 000 370 wei = 21 007 770 000 wei = 余额的 **0.000105 %**；
  即便按真实交易观察到的 `maxFeePerGas` 3 917 784 wei 给 200 000 gas 上界 = 783 505 600 000 wei
  = 余额的 **0.003918 %**（本轮实际那笔验证交易的总账单 28 313 679 344 wei = 0.0001416 %）。
  ⇒ §33 的余额门在这个钱包上不会成为瓶颈，但门本身仍必须真实计算而不是假设。

## 由取证直接得到的 M6 设计约束

1. 提交路径 = `eth_sendRawTransaction`（两个已文档化端点均可）；**不存在的 SequencerDirect 专用协议按 §24 记 BLOCKED**，不写 mock 假装完成。
2. 交易类型：EIP-1559 与 legacy 都真实存在于同一链上 ⇒ `TransactionType` 是 Intent 的显式字段，由取证决定默认值（1559），legacy 保留为可选并同样通过解码回验。
3. fee 来源必须写明：base fee 取自 pinned 块的 `baseFeePerGas`，tip 取自 `eth_maxPriorityFeePerGas`，两者相加得 `maxFeePerGas`；全部 U256 运算。
4. Receipt 必须额外保留 `l1Fee` 等 OP 字段，否则 M7 会低估成本。
