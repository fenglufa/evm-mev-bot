# M10 Arbitrage Executor — 语义审计底稿（写码之前锁定的事实）

任务书：`docs/v0.1/M10 Coding.md`（§1–§71）
基线 commit：`66b421d`（M9.4 完成并推送之后）
证据目录（本次将建立）：`data/evidence/m10/`
固定夹具目录（本次将建立）：`fixtures/simulation-m10/`

本文件只做一件事：把 §4 要求的十个问题**用现量的文件和字段**回答掉，并把由此推出的三个接入点固定下来。
下游任何一段代码若与本文件冲突，以本文件重新现量之后的结果为准 —— 所以每条都带 `file:line` 或命令输出，不带回忆。

---

## 1. §4 的十个问题

### 1.1 当前是否已有 Solidity contract？

**没有。** 仓库顶层目录现量：

```text
config/  crates/  data/  docs/  fixtures/  target/  tests/
```

没有 `contracts/`、没有 `*.sol`（`find . -name '*.sol' -not -path './target/*'` 返回 0 行）。
所有与合约字节码有关的事实目前都以 `StateDump` 形式存在于 `fixtures/`（见 §1.5）。

### 1.2 当前是否已有 Solidity 编译工具链？

**没有，且没有框架。** 现量：

```text
ls foundry.toml hardhat.config.js hardhat.config.ts package.json remappings.txt   → 全部 No such file
ls -a | grep -iE "package|node_modules|yarn|pnpm"                                  → 0 行
command -v forge cast anvil solc                                                   → 四个全部 ABSENT
```

（同一条命令里 `npm`/`npx` 是**存在**的 —— 本机装了 node v24.14.0 —— 但仓库里没有一份 `package.json`，也没有 `node_modules`，所以「存在 node」不等于「存在合约工具链」，M10 不会因为 node 在 PATH 里就去装 hardhat。）

`crates/*/Cargo.toml` 里没有任何 solidity 编译依赖，`Cargo.lock` 中也没有 `foundry-*` / `solc` 相关条目。

因此 §4 末段的条件成立：**确认不存在合约工具链**，可以选最小方案。选择见 §2。

### 1.3 当前是否已有部署脚本？

**没有。** 现量分两层：

```text
grep -rin deploy 全仓（排除 target/、.git/）              → 768 命中，绝大多数在 data/evidence/ 的证据正文里
grep -rin deploy crates/ --include='*.rs'                 → 32 命中
上述 32 条去掉 predeploy / deployed 两种词形              → 0 命中
ls config/ tests/ fixtures/ 里的 deploy                   → 0 命中
```

也就是说：**代码里出现的 32 个 `deploy` 全部是「OP-Stack fee predeploy」和「deployed bytecode」两个既有词组，没有一处是部署脚本**。分布：`crates/execution/tests/live_reads_probe.rs` 9 条、`crates/execution/src/giwa/reads.rs` 4 条、`crates/simulation/tests/dump_replay.rs` 3 条，其余散在 `result.rs` / `plan.rs` / `preflight.rs` / `cost.rs` 的注释里。

链上写入能力是有的（见 §1.6/§1.7），缺的只是「一笔 `to = None` 的创建交易」这一条路。

### 1.4 当前是否已有 ABI 生成流程？

**有，且是本仓库自己的风格：不生成，而是用 `sol!` 从规范声明推导，再和链上观测值对账。**

- `crates/protocol/src/signatures.rs`：`sol! { event Swap(...) ... }` + `selector_of(sig) = keccak256(sig)[0..4]`；
- `crates/protocol/src/calls.rs`：`sol! { function swap(uint256,uint256,address,bytes) ... }`，`V2Call::selector()/encode()/decode_return()`；
- 该文件顶部的注释就是本里程碑要继承的理由：**手抄的 selector 是一个主张，`sol!` 推导出来的才是确认**。它的测试把 9 个推导 selector 钉在链上观测值上（swap = `0x022c0d9f`，锚在三笔真实交易 `0xc465dc6a…`/`0x17b7478a…`/`0x85b403a1…`）。

`alloy-json-abi` / `solc --abi` 生成的流程**不存在**，也不引入：M10 的 ABI 证据改为「solc 输出的 ABI JSON 里的规范签名字符串 → 本仓库同一套 `keccak256` → 必须等于 Rust 侧 `sol!` 推导的 selector」，见 §4.3。这正是 §47 要防的「Rust calldata ≠ 实际部署 contract ABI」。

### 1.5 当前是否已有 Executor / Router / Pairs fixture？

| 对象 | 有无 | 位置 |
| --- | --- | --- |
| Executor / Router 合约 | **无** | M4 明确拒绝：`crates/simulation/src/plan.rs:5-13`「no router, no executor contract, no flash swap」 |
| V2 Pair 夹具 | **有，两份，真字节码 + 真 storage** | `fixtures/simulation-m4/dump-37191169.json`：`0xf487d533…6578` 与 `0x5bef6275…7440`，各 5393 字节 code，slot 4..7 是真实 reserves |
| Token 夹具 | **有** | 同 dump：WETH `0x4200…0006`（2846 B，无税）、TTAX `0xcffe…2f62`（4827 B，**有转账税**）、`0x4200…0011`（2059 B） |
| 另一对 venue 的 dump | 有 | `fixtures/simulation-m7/dump-37529555-*.json` / `dump-37530593-*.json`（14954 B 的非 V2 venue code + 3744/3769 B 的 token） |

dump 的 schema（`crates/simulation/src/state.rs:495-517` `StateDump`）：顶层 `chain_id, block_number, block_hash, accounts{balance,nonce,code}, storage{address->{slot:value}}, block_hashes, header{timestamp,gas_limit,base_fee_per_gas,excess_blob_gas,beneficiary,prevrandao}, reads[]`；`accounts`/`storage` 是 `BTreeMap`，所以同一份状态写出来的字节稳定（`state.rs:639` `write_file`）。
加载入口 `crates/simulation/tests/support/mod.rs:47`。

**结论：§10「是否可以复用已有测试 fixture」= 可以**，pair/token/state 三类全都能复用；M10 唯一必须新增的合约是自己那一份 executor bytecode。

### 1.6 `evm-execution` 的 builder 如何构造 `to / data / value`？

一条直线，三个字段全部来自 intent，builder 不做任何决定：

```text
TransactionIntent { target: Address, value: U256, calldata: Bytes }   crates/execution/src/intent.rs:138-184
      ↓ Intent::unsigned()                                            crates/execution/src/intent.rs:490-503
UnsignedTransaction { to: Option<Address>, value, input }             crates/execution/src/tx.rs:64-85
      ↓ TransactionBuilder::build(intent, &BuildPolicy)               crates/execution/src/builder.rs:128
Build { unsigned, signing_payload, signing_hash, ids }                crates/execution/src/builder.rs:105-113
```

`build()` 现量出的检查顺序（`builder.rs:129-225`）：expected_chain_id 未设 → 拒；链不匹配 → 拒；sender 为 0 → 拒；**target 为 0 → 拒**（`builder.rs:147-153`，文案「a creation transaction is not what this bot builds」）；多步且无 sequence position → 拒；override 资金 → 拒；calldata 长度界；nonce；gas policy 解析 + 三条 gas 界。

**但线下的编解码层本来就支持创建交易**：`tx.rs:69` 写着 `to: None` 是 creation，`tx.rs:514` 编码成空 `to`，`tx.rs:530` 解回 `None`（不当作 0x0），并且有两份测试证明它（`tx.rs:644` `a_creation_encodes_an_empty_to_and_stays_distinct_from_the_zero_address`、`tx.rs:667` `a_creation_decodes_back_to_an_absent_to_and_not_a_zero_target`）。`builder.rs:343-348` 的 `to_text()` 也已经会给创建交易打印 `<creation>`。

→ 所以 §46 的部署不需要新_signer/submitter/codec_，只需要一条**不经过 intent 的那道 target 非零检查**的构造路径。接入点见 §4.2。

### 1.7 REVM simulation 如何注入 sender balance？

**不是注入 balance 字段，是 layer 一个 setup override**：

```text
TransactionSpec.sender: SimulationSender { address, label, endowment }   crates/simulation/src/request.rs:69-120
Endowment::Scaffolded   → 请求把该账户 balance 覆写成 endowment_wei       request.rs:57-66
Endowment::PinnedState  → 完全不覆写，用 pin 块上真实余额                 request.rs:64-65（M7 的真实运行用这个）
```

`endowment_wei(max_fee_per_gas, wrapped_native)` 现量在 `request.rs:253-257`：整计划 gas  allowance × 费率上限 +（若 `Funding::WrapNative`）msg.value，saturating。
setup 是在 `engine::run()` 里一次性叠上去的：`engine.rs:426-427` `let provider = provider.with_setup(setup)`，且 `SimulationRequest::preflight()` 必须先跑。
覆写的合法边界：`request.rs:473-504` `check_overrides` **只允许覆写 sender**；任何非 sender 地址的 override 都被拒 —— 这条规则的意义是「不允许伪造池子状态」，M10 必须原样尊重。
sender 若带 code 会被拒：`engine.rs:529-536`。
字节码的合法注入通道是 dump 本身：`StateDump::insert_account(address, balance, nonce, code)`（`state.rs:604`）+ `insert_storage`（`state.rs:615`），由 `ProviderDb::basic_async`（`engine.rs:144-174`）验 code_hash 后交给 REVM。
`Output::Create` 在 `engine.rs:941-948` 明确报错 → 模拟里不需要（也不允许）现网部署，executor 的 runtime bytecode 预置进 dump 即可。

### 1.8 receipt / profit / sequence 如何记录？

| 层 | 类型与位置 |
| --- | --- |
| receipt | `ReceiptStatus{Submitted,Pending,Included,Reverted,NotFound,Timeout}` `receipt.rs:27-44`；`ReceiptTracker::track(expected, read, verify_block)` `receipt.rs:244`，policy 12 次 × 1 s `receipt.rs:195-211`；`parse_receipt` 读 status/gas_used/effective_gas_price + 可选 `l1_*` 字段 `giwa/sequencer_direct.rs:434`、`receipt.rs:78-106`；**`contract_address` 字段已存在**（`receipt.rs:91`「Non-empty only for a contract creation」） |
| profit | `AssetSnapshot` `profit.rs:39-52`、`BalanceDelta::new(before, after)` `profit.rs:81`（native_delta/token_delta `:100-110`）、`RealizedProfit` `:155+`、`ProfitEquation::evaluate` `:227`、`ProfitVerificationStatus{Pending,VerifiedPositive,VerifiedNegative,Inconclusive}` `:273-288`、`counts_as_successful_real_arbitrage` `:294` |
| 记录 | `ExecutionRecord` `lifecycle.rs:194-240`：`execution_id = "exec-" + keccak(idempotency_key)[2..18]` `:522-525`，`idempotency_key = opportunity_id|simulation_id|state_fingerprint` `intent.rs:70-75`，另有 `transaction_hash`、`route_transactions`、`gas_used`、`effective_gas_price`、`l1_fee`、`realized_profit`、`profit_status`、`failure`、`blocked_reason`；去重靠 `Ledger::claim` `lifecycle.rs:570` |
| 状态机 | `ExecutionStatus{Detected,…,Included,Settled,ProfitVerified,Reverted,Failed}` `lifecycle.rs:49-84` |

**现量缺口**：`ExecutionRecord` 没有 plan 层标识位（最接近的是 `SequencePosition{index,count}` `intent.rs:87-92`）→ §39 要求用 opaque id 关联，M10 新增的是 `plan_id`/`route_id` 两个**字符串摘要**，不改 M6/M7 已有字段语义。

### 1.9 GIWA 的 gas / L1 fee 如何接入？

- 预签名 L1 fee：`estimate_l1_fee` `giwa/reads.rs:182-222`，对 `pre_signing_envelope`（`reads.rs:241`）调 `getL1Fee(bytes)`（手编 calldata `reads.rs:263`），oracle 地址是 predeploy 常量 `reads.rs:42-44`；
- native gas price：`eth_gasPrice` 现量 `0xf4342` = 1,000,770 wei（≈0.001 gwei），base fee 258，块 gas limit 60,000,000（`builder.rs:72-75` 的 ceiling 就是按实测块上限定的，证据 `data/evidence/m6/probe-read-surface-2.txt`）；
- post-receipt L1：`l1_*` 字段解析 `receipt.rs:78-106`；成本汇总 `cost.rs:86`；
- §33 的边界由 `crates/execution/src/giwa/mod.rs:1-17` 声明并执行：GIWA 特有的方法名/receipt 字段/oracle 只能出现在 `execution::giwa`。M10 不新增第二个 gas 或 L1 oracle，也不把 GIWA 名字写到该模块外。

一笔被包含的交易最少约 10 次 RPC（fee 2、nonce 3、endpoint chainId 1、block_hash_at 1、balance 1、send 1、receipt ≥1、block verify 1）—— §51 的「instrumentation 不得新增 hot-path RPC」就是相对这个基线说的。

### 1.10 能否复用已有测试 fixture？

能，且必须。三处直接复用：

1. `fixtures/simulation-m4/dump-37191169.json` —— 两份真 V2 pair（含 storage）+ WETH + TTAX，是 §28 双池闭环的现成市场；
2. `crates/simulation/tests/dump_replay.rs:36` `the_frozen_fixture_replays_itself_exactly` —— 冻结夹具三次跑指纹相等的模式，D3 门禁照它写；
3. `SimulationResult::fingerprint()` `result.rs:845-848`（对结果 serde JSON 取 keccak256）—— §48「可重建 simulation result」用的就是这个函数，不另造摘要。

---

## 2. 工具链决定（§4 末段 + §46）

**决定：单个钉住版本的官方静态 `solc`，不引入任何框架。**

| 项 | 值 |
| --- | --- |
| 可执行 | `target/tools/solc-0.8.37`（`/target` 已被根 `.gitignore` 忽略，不进版本库） |
| 大小 | 32,260,112 字节 |
| 自报版本 | `Version: 0.8.37+commit.f401782d.Darwin.appleclang` |
| 本地 SHA-256 | `a27396e7732aa52e80ff89ad7bd8a2e46fec2a6dcc4ef20cd16e5e0c502d6821` |
| 本地 Keccak-256 | `3581bfdf7211ad70fe593bd43db6e355918886e22361f006f8a39f44627c7ae7` |
| 官方 `list.json`（`binaries.soliditylang.org/macosx-amd64/list.json`，`version == 0.8.37` 条目） | `sha256 = 0xa27396e7732aa52e80ff89ad7bd8a2e46fec2a6dcc4ef20cd16e5e0c502d6821`、`keccak256 = 0x3581bfdf7211ad70fe593bd43db6e355918886e22361f006f8a39f44627c7ae7` |
| 对账结果 | **两个哈希逐字节相等**（不是前缀相等） |

keccak 这一路能本地算，是因为这台机器的 openssl 3 提供 `dgst -keccak-256`。它的阳性对照（同一实现必须先被证明是 keccak 而不是 SHA3-256）：空输入的 keccak-256 是公开值 `c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470`，`openssl dgst -keccak-256 /dev/null` 输出的正是这一串。

→ 附带结论：**M10 的证据重算门可以有一个与 Rust 无关的 keccak 实现**（`alloy keccak256` ↔ `openssl -keccak-256`），§48 的「重新验证关键字段」因此不必只信自己写的那套哈希。
其余链条：TLS 来源 + 版本自报含 commit hash + 双次编译字节相同（P1 的门禁）。

### 2.1 P1 落地补记（编译完成后现量）

编译已完成，产物与复现命令固化在 **`contracts/BUILD.md`**（数字只写在那一处，本底稿不复制，避免两份文档的摘要漂移）。补记三条本审计当时无法预知的事实：

1. **产物已进版本库**：`contracts/artifacts/ArbitrageExecutor.{abi,bin,bin-runtime,signatures}` 是 solc 一条命令的原样输出（未改写），P3 的 REVM 模拟与 P6 的真实部署都读这一份，因此「模拟用的字节码 == 部署用的字节码 == 库里那份」可以靠 `cmp` 而不是靠描述保证。
2. **门禁形式是字节 `cmp`，不是摘要字符串比对**：把 `contracts/artifacts/` 与一次全新编译的产物逐文件 `cmp`，四个文件全部相同；同一命令写两个目录同样相同。摘要（含 `keccak256(runtime) `，即 §46 的 `bytecode_hash`）只用于写证据和链上 `eth_getCode` 对账。
3. **legacy 管线（不用 `--via-ir`）是有代价的决定**：16 槽栈上限三次触发 `Stack too deep`，最终靠把 `execute` 拆成六个阶段函数解决（`_checkRoute`/`_pullInput`/`_checkSides`/`_pushInput`/`_pullOutput`/`_settle`）。换来的是编译只需一条命令、产物可用普通 `--optimize` 复现。

静态面（§42/§62 合约三项）在同一次现量里测得：非注释行中 `delegatecall` / `staticcall` / 低层 `.call(` / `selfdestruct` / `assembly` / `unchecked` / `payable` / `send(` / `receive()` / `new X` 各 **0 次**；这两个词在全文件各命中 1 次，命中的是本文件第 10 行与 `_lock` 注释里的**自我提及**（词面扫描的自指，必须写明）。代码里实际发生的外部调用只有三族：`IERC20` 的 `transfer/transferFrom/balanceOf/allowance`、`IV2Pair` 的 `token0/token1/swap`，且每个 `pool`/`token` 地址在动用之前都先过 `pairAllowed`/`tokenAllowed`（`_checkRoute` 全量先查，任何 token 移动之前）。


---

## 3. 依赖图审计（§3）

`cargo metadata --no-deps` 现量（只列 workspace 内部边，dev 边标 `(dev)`）：

```text
core        → (无)
chain       → core
protocol    → chain, core
state       → core
graph       → core, state                        (+ chain, protocol, replay (dev))
replay      → chain, core, protocol, state
discovery   → chain, core, graph, protocol, state (+ pathfinder (dev), replay (dev))
pathfinder  → core, graph                        (+ state (dev))
opportunity → core, graph                        (+ chain, protocol, replay, state (dev))
simulation  → chain, core, protocol              (+ execution, graph, metrics, opportunity, pipeline, replay, risk, state (dev))
risk        → core, simulation
live        → chain, core                        (+ protocol, replay, state (dev))
metrics     → (无)
execution   → chain, core, metrics, opportunity, protocol, risk, simulation (+ replay, state (dev))
pipeline    → chain, core, execution, graph, live, metrics, opportunity, protocol, replay, risk, simulation, state
cli         → 除 discovery/pathfinder 之外全部    (+ pipeline (dev))
```

三条结论：

1. **M10 不新增任何一条 workspace 边。** `execution` 已经依赖 `chain, core, metrics, opportunity, protocol, risk, simulation`（§3 允许的全部五个都在），合约 ABI 放 `protocol`（只依赖 chain+core），原子调用的模拟入口放 `simulation`（已依赖 protocol）。
2. **§3 的禁令自动满足**：`execution` 的依赖闭包里没有 `pathfinder / graph / discovery / live`（`pipeline` 和 `cli` 才有）。M10 代码若从 `execution` 引用它们会立刻成环或被闸门抓住。
3. 现存的三条**隔离闸门**，M10 必须绕开而不是改：
   - `crates/cli/tests/no_execution.rs:126-133`：`BANNED_DEPENDENCIES = [alloy-signer, alloy-network, alloy-provider, alloy-transport, alloy-rpc-client, foundry-evm]`，对**每个** crate 的 Cargo.toml 扫描（`crate_manifests()` `:288`），另有 `no_code_outside_the_execution_crate_can_send_a_transaction` `:136`；
   - `crates/live/tests/preconf_isolation.rs:164`：遍历全仓 Cargo.toml 检查 `evm-live` 的依赖闭包（§41 相关）；
   - `crates/discovery/tests/reconstruction_evidence_gate.rs:3660-3690`：只对 `crates/discovery/Cargo.toml` 设禁（M10 不动 discovery）。

`alloy-sol-types`（`=1.7.3`，workspace 依赖，现只 `crates/protocol` 直接用）**不在**禁单里，且 M10 的 ABI 模块就放在 protocol —— 所以零新增外部依赖。

---

## 4. 三个接入点（由上面全部推出）

### 4.1 executor 的 ABI 放在 `crates/protocol/src/executor.rs`

与 `calls.rs` 同构：`sol!` 声明 → `signature()` / `selector()`（`selector_of`）/ `encode()`，测试把推导 selector 钉在 **solc 自己吐出来的 ABI JSON 的规范签名串**上（`keccak256` 用 alloy，与 `calls.rs:tests` 钉链上观测值是同一个装置）。
放这里的理由：`simulation` 和 `execution` 都已经依赖 `protocol`，两边共用一份编码，物理上不可能出现「模拟用的 calldata 和执行用的 calldata 不同」。

### 4.2 部署走「不经过 intent 的 target 非零检查」这条窄路，其余全部复用

`Signer::sign(&UnsignedTransaction)`（`signer.rs:259`）签名的是 `UnsignedTransaction`，**不是** `Build`；`TransactionSubmitter::submit(&SignedTransaction)`（`submitter.rs:128-143`）与 `ReceiptTracker`（`receipt.rs:244`）同样只要求 `to: None`。
因此部署 = 直接组 `UnsignedTransaction{to: None, input: initcode}` → 现有 `Signer` → 现有 `GiwaSequencerDirect` → 现有 `ReceiptTracker` → `receipt.contract_address`（`receipt.rs:91`）。
新增的是一个 `execution::deploy` 模块（构造 initcode 的交易体 + 校验 initcode 非空/在界内 + 把 §46 要求的六个字段落成证据），**`builder.rs` 一行不改** —— 这样 §63 的 production diff 里 arbitrage 路径的语义完全没被动过。

### 4.3 模拟复用 `engine.rs` 内部装置，另开一个入口，不复制第二台 EVM

§25 要求「复用这个边界，而不是重新实现第二套 EVM 模拟器」。现量可复用的私有助手（全在 `crates/simulation/src/engine.rs`）：
`ProviderDb`（`:131`）、`block_env`（`:767`）、`build`（`:827`，同一个 `Context::mainnet().with_cfg.with_block.with_tx.with_db.build_mainnet()`）、`tx_env`（`:872`）、`Views::call`（`:1020`）、`state_changes`/`Touched`（`:1229-1235`、`:672`）、`executed_logs`（`:981`）、`GasBudget`、`ExecutedStep`（`result.rs:152-169`）。

因此 M10 的模拟入口是 `engine.rs` 里**新增的一个 `pub async fn`**：同一条 pin 校验链（`header.chain_id` → `check_pin` → `state.source` → `codes` 非空 → sender 无 code），同一台 EVM 实例上顺序跑「measure → 一笔对 executor 的调用 → measure」（measure 必须是真交易而不是 `Views`，才能看见上一步 commit 后的状态 —— 这正是 M4 计划里 `PlanStep::MeasureErc20` 的用意，`plan.rs:505-509`）。
返回**新的**结果类型（before/after 余额、每池 reserves、status、logs、gas、revert data、指纹），**不改** `SimulationResult`，因为 `SimulationResult` 的 `route/plan_summary/slippage/compared` 四组字段属于 M3/M4 的 `PricedRoute` 语义，塞假值进去会污染 M4/M5/M7 的证据形状（§63/§65）。

### 4.4 真实链前置条件（现量，2026-10-07）

| 项 | 值 |
| --- | --- |
| endpoint 指纹 | `rpc-309bd2bd`（URL 只在运行环境里，不进证据） |
| `eth_chainId` | `0x164ce` = 91342 |
| `eth_blockNumber` | `0x2438b3f` = 37,984,319 |
| `eth_gasPrice` | `0xf4342` = 1,000,770 wei |
| 部署/执行 EOA | `0xd450630c1c55b1c7df1ebf7eeaee1fffb45e520c`：balance **0.035764 ETH**、nonce **7**、code **0 字节**（地址已在 `data/evidence/m8/**` 提交物里公开 2322 次） |
| 另一个此前被称作 operator 的地址 | `0x37cd68c3aa5cbb95918844f0cc341d1905f4fcbd`：balance 0、nonce 1、**code 2826 字节** → 它是合约不是 EOA，不能当 operator，本节把它排除掉（此前会话里的口头称呼不可信） |
| M4 的两池 | `0xf487d533…6578`、`0x5bef6275…7440` 此刻 `getReserves()` 仍有答案、code 各 5393 字节 → 真实双池可用 |

成本现量：1.5 M gas 的部署 ≈ 1.5e-6 ETH；§57 整条阶梯（部署 + 配置 + approve + 执行 + 一次刻意失败）合计 < 0.00002 ETH，钱包余额够，**不需要补款、不需要 faucet**。
真实交易面：1 笔部署 + 3–4 笔配置/授权 + 1–2 笔受控执行，全部 `value = 0`、金额极小、由 §33 的现有 pipeline 提交。

---

## 5. 由本审计直接锁定的接口

```solidity
struct Leg { address pool; address tokenIn; address tokenOut; uint256 amountIn; uint256 amountOut; uint256 minAmountOut; }
execute(Leg[] legs, address inputToken, uint256 amountIn, uint256 minFinalAmount, address recipient)
```

- `amountOut` 是**向 pair 索要的 exact-out 数额**：V2 的 `swap` 是 exact-out（`plan.rs:16-25` 已经写过这个事实），合约不可能「不知道输出多少就 swap」；
- 真正的守卫不看返回值，看 `balanceOf` 前后差：**每条腿收到的必须 ≥ `minAmountOut`**，最后一笔必须让 `recipient` 的 input token 余额增量 ≥ `minFinalAmount`（§11/§12/§13）；
- 腿间衔接：第 i>0 腿转给 pool 的数额必须等于上一腿**实测收到**的数额，且必须等于 calldata 里声明的 `legs[i].amountIn`，三者不一致就 revert（这让 calldata 成为可检验的断言，而不是自适应的搬运）；
- 费用税：input 侧的税会让 pair 的不变量检查自己 revert；output 侧的税会表现为 `received < amountOut`，被上面的 `minAmountOut` 抓住。**显式拒绝 fee-on-transfer，不静默算错**（§21/§22）。
