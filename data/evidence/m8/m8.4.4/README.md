# M8.4.4 — 跨阶段块上下文传递与受控复用（固定块 + live 证据）

§1 的问题：一个在上游取到、并且被显式身份校验过的 `block_number + block_hash`，能不能安全地从 Preflight 传到 Build，而 Build 仍然自己验证。本目录用固定历史块（chain 91342 / block 37530593 / hash 0xbe5cc13bfb394bf7fcd665398ea3c9b54fc7ff4d85e2c215c847333b77593c3e）回答其中一半：把块新鲜度排除在实验之外，只问「共享一个已验证的块身份」这件事本身成立不成立。3 把重复，`source: fixture`、`independent_runs: 0`；§20 的三把真实 BuildOnly 运行在 `live/`（下面单列一节），§21 里链没给过的那个场景由 `crates/execution/tests/sequence.rs` 的受控测试判。

只记录，不消除：没有新增缓存，没有动 §23 的 `StateReadCache`，没有 batch / prefetch / 并发（§2、§22），没有删掉任何执行前检查，没有真签名、没有广播。

## 结论

- RPC 数：baseline 24、reuse 24，净省 **0**（consumer 侧新增校验读数 0）。§33 的口径由这两个数直接决定：No net RPC reduction.。
- 两臂的块身份三要素逐把相等（3 把 × 3 个字段），Build 的 §12 十个字段 + sender + serialization + fingerprint 全部相等：fingerprint `0xda922714c890ad91a20f78785b866840e177f6d9f694e63cc5504519926f0aba`。
- negative control：3 次 `rejected`，原因一律 `block_hash_mismatch`（同高度、另一个 hash）。它照样建出同一笔交易并在 Built 停下——拒绝的是传下来的上下文，不是这一步的路线。
- `reused` 全为 `false`（9 行 consumer 记录里 1 条 true 都没有）：§2 保留本步自己的绑定读数，所以两臂对齐的是「同一个块的身份」，被消费的值仍然是 Build 自己读到的那个。§31 的 `reused = true` 在这条路上今天说不出口。
- 落到节点的字节数：0（三臂都是 BuildOnly 停在 Built，没有 key、没有签名、没有 submission）。

## 三个臂（§11 / §19）

| arm | 传下来的上下文 | producer | consumer | 建出来的交易 | 送出去 |
|---|---|---|---|---|---|
| `baseline` | 没有 | `refused / unverifiable_block_context` | `no_context` | 与另两臂逐字段相同 | 0 |
| `verified_context` | 本步 pin 的那个块 | `verified` | `accepted`（自带读数核对过） | 同上 | 0 |
| `negative_control` | 另一个真实块（同高度、hash 不同） | `verified`（它没撒谎，那是另一个块） | `rejected / block_hash_mismatch` | 同上 | 0 |


## live 三把（§20）

| run | 走到哪一步 | producer | consumer | 生命周期 RPC 数 | 签名/提交字节 |
|---|---|---|---|---|---|
| `route-91342-37746091-1791091208939` | failed | `verified` / fixed_historical | 没有 consumer 行：这一步没跑到 Build | 32 | 0 / 0 |
| `route-91342-37746182-1791091300296` | failed | `verified` / fixed_historical | 没有 consumer 行：这一步没跑到 Build | 32 | 0 / 0 |
| `route-91342-37746511-1791091629873` | built | `verified` / fixed_historical | `accepted`（自带读数 true，`reused` false） | 43 | 0 / 0 |

- producer 三把全 `verified`，验过的身份与本步 pin 三要素相等的 3 把；scope 由「读到的 head 是否还等于 pin」推出来，三把都是 `fixed_historical`（实测 `head_minus_pin` 14–17）。因此 **live 现场没有 §21 的 Case B**，计数 0，那一格由 `crates/execution/tests/sequence.rs` 在受控条件下判，不假装链给过这个场景。
- consumer 只在走到 Build 的那 1 把上出现，结果 `accepted`、`consumer_read: true`；另 2 把被 §26 的费率线挡在建单之前，consumer 行**不存在**——按缺失记录（§30），不填一个空值上去。整棵树 `reused` 为 true 的计数 0。
- §33 的计数对照（`live/rpc-census-comparison.json`）：把走到 Build 的这一把与 M8.4.2 在本里程碑存在之前跑的三把成对比，按 method × stage 切单元格，3 / 3 对的单元格表完全一样，逐对净省 provider 调用（82 次口径：生命周期 43 次 + 仿真自己记的 39 次）[0, 0, 0]。六把的端点指纹都是 rpc-faa716cada04a9ef。所以「No net RPC reduction.」不只是固定块里的算术：真实节点上多出来的校验是 0 次，省下的也是 0 次。
- live 的安全边界：3 把全部 `build-only`，三把合起来的 signed-transactions.jsonl 与 submissions.jsonl 分别是 0 字节和 0 字节，`successful_real_arbitrage` 为 true 的 0 把。

## 文件（§16 的清单，一份不多）

- `summary.json`
- `contract/verified-block-context.json`
- `contract/producer-verification.json`
- `contract/consumer-verification.json`
- `contract/negative-controls.json`
- `baseline/rpc-summary.json`
- `baseline/pipeline-calls.json`
- `baseline/build-result.json`
- `reuse/rpc-summary.json`
- `reuse/pipeline-calls.json`
- `reuse/build-result.json`
- `comparison/rpc-comparison.json`
- `comparison/block-identity-comparison.json`
- `comparison/build-comparison.json`
- `comparison/correctness-comparison.json`
- `live/live-runs.json`
- `live/rpc-census-comparison.json`
- `README.md`
- `fixed-block/run-NN.json` — fixture 的原始行，由 `crates/execution/tests/sequence.rs` 写入，本装配只读不改
- `live/route-runs/<session>/`、`live/rpc/<session>/` — live 的原始记录，由 CLI 写入，本装配只读不改

§16 的 `fixed-block/` 与这里的 `fixed-block/` 是同一份东西；`baseline/` 与 `reuse/` 是 §11 两臂各自的投影，negative control 的记录在 `contract/negative-controls.json`，不再多开一份目录。上面列出的 18 份是**生成文件**；原始行/原始记录（`fixed-block/`、`live/route-runs/`、`live/rpc/`）不算在里面。

## 重新生成（§36：串行跑，每条命令各自设自己的环境变量）

1. `M844_FIXTURE_EVIDENCE=data/evidence/m8/m8.4.4 cargo test -p evm-execution --test sequence` — 写 `fixed-block/` 原始行。
2. `M844_BLOCK_CONTEXT_REFRESH=1 cargo test -p evm-execution --test block_context_evidence` — 从原始行与 live 记录装配上面这些文件。

live 的三份记录**不能**由测试重新生成：它们是 3 次真实 BuildOnly 运行的产物，重新跑一次只会得到另一批块（链在动）。本目录的 `live/` 因此按「读到的原始记录」处理——`live/live-runs.json` 与 `live/rpc-census-comparison.json` 是它们的投影，门禁会独立重算一遍对账。不设环境变量的运行只做逐字节比对，不动 committed 证据。

## 本目录证明不了的事

- that any RPC was saved — net_rpc_saved is 0 by construction, because §2 keeps the step's own binding read
- anything about a market: source is fixture, independent_runs is 0, and the lane is scripted
- that latency improved: none of the fixture's rows measures a duration at all, and the live runs' call times are printed per run without being paired (§33 asks count, duration and wall clock to be told apart; only the count is compared in this tree)
- that the header is reusable inside a simulation or across any other stage edge — the identity contract is about one block, and M8.4.3's MustRefetch verdict for the other flows is unchanged
- the §42 result label: that is declared over the live runs as well, in the completion report

§32 的口径：fixture 每把单独一行、aggregate 是 3 把同一驱动之和（不是 3 个独立样本），且 fixture 侧一个时长都没测（§34）。live 那 3 把是各自独立的进程，逐把列在 `live/live-runs.json`，不做之和也不做均值；它们的`rpc_duration_total_ns` 只作为实测值印出来，§33 要求 RPC 次数、RPC 时长、wall-clock 三件事分开说，本树只对第一件下了结论。
