# M5 Completion Report

结论（白话版）：**M5 的闭环真实成立，状态 COMPLETE（§86）**，但有 5 项只能记 PARTIAL/BLOCKED，
其中 2 项是端点能力本身不给（订阅推流、候选块取状态），3 项是本轮实测到的实现或证据边界。
这一轮把 M4 的「一次取回的历史区块」换成了「持续到达的市场」，并且**没有为此重写 M4 的任何
业务语义**：

> GIWA Testnet → Source（WebSocket / HTTP 轮询 / Flashblock 候选）→ Block + Logs → Protocol 解码
> → StateUpdate → StateStore → 每块 Graph 快照 → Opportunity（含 STALE 生命周期）
> → Simulation（pin 在该机会自己的 state version）→ Risk → Decision + Metrics + 证据文件

一句话结论：**同一个状态引擎、同一条 CLI 二进制，跑真实到达的区块和跑录制下来的区块，
输出的状态、机会、拒绝与模拟结果逐字段一致**；不一致的只有两类：每行都带的 `session_id`
（会话名本身，这四个产物里**没有任何时钟字段**），以及「状态从哪儿读的」这句话
（`rpc:chain-91342` vs 录制文件名，连带它派生的模拟 fingerprint）。第 13 节的表给出精确计数。

本轮实测到的两个事实值（原始十进制整数，来自 `data/evidence/m5/` 里已提交的产物，非估算）：

| 问的是哪一段 | 实测值 | 说明 |
| --- | --- | --- |
| 真实实时会话（WebSocket 源） | 120 s 内到达 **121** 个区块（会话记录 `start_block = 37 480 521` 是 §7 bootstrap 读到的 head，本身不重放；第一个被处理的 canonical 是 37 480 522，最后 37 480 642），**9 958** 条日志，**74** 次状态写入，**0** 个机会 | 74 次全部是同一个池子 `0x3978e57b…6092` 的 Sync（73 synced + 1 registered），72 条带 before/after |
| 该窗口为什么没有机会（结构原因，非运气） | 那 121 个区块里全链共 **8** 个地址发过 Sync 形状的日志；其中 **7** 个能答 `token0()/token1()`，落在 **7** 个不同的交易对上（各 1 个池），第 8 个（`0xad153c84…`，253 条里占 174 条）对这两个调用直接 revert ⇒ **没有任何一个交易对有 2 个活跃池** | 两池路线的定义性前提不成立；本地 registry 有 6 个已举证池子，其中只有 1 个在该窗口活跃 |
| 真实闭环（同一二进制，50 个真实区块） | `http-poll` 与 `replay` 各跑一遍：50 块 / 51 事件 / 9 次状态写入 / 4 个候选路线（`candidates` 计数，指被定价的两池候选；flashblock 候选文件 `candidates.jsonl` 是 0 行）/ 2 个机会 / 1 次拒绝降级 / 1 次模拟 / 1 个 Reject | 两条路线都在 block **37 191 169**，state_version = log_index **34** |
| 模拟用的是哪个区块 | pin = `37191169:0x56c628d7102131d35d5bdf6a0ed2c6ec3d4d00edc4419a20641c7b396c48d670` | gas_used **340 703**，第 7 步 swap 被池子 revert（revert 数据是 `"K"`），net_profit = **NotComputable** |
| 全程有没有发送 | **没有**。风险层输出 accept=0 reject=1 unknown=0，且 `--json` 之外每条记录正文都带 `no_broadcast` 那句话 | §26 的禁令由 4 个守卫测试断言（方法名、依赖名、URL/chain id、`unsafe`） |
| 关卡 | fmt / check / test / clippy 全部 exit 0 | **350 通过 / 0 失败 / 5 忽略**（33 个测试二进制 + 13 个 doc-test） |

三件必须说清楚、不能被「闭环跑通了」盖过去的事：

1. **`eth_subscribe` 在这台端点上不存在。** 不是没试，是试了两次（`newHeads`、`newBlockHeaders`）
   并且把 provider 自己的错误原文写进了会话记录：`{"code":-32603,"message":"Internal error"}`。
   因此本轮的「实时」是**在同一 WebSocket 连接上轮询 `eth_blockNumber`**，§56 的 subscribe 一条
   只能记 BLOCKED。链一跳 2 个块就会被判一次 gap（本轮 39 次，全部同一轮补齐，0 块被跳过）。
2. **完整闭环是在「实时读到的历史区块」上证成的，不是在「刚刚产生的区块」上证成的。**
   那 50 个区块（37 191 150 → 37 191 199）是通过实时 RPC 读的、状态是从实时节点按 pin 的区块哈希
   取的，但它们是约 3.3 天前的区块；刚刚产生的区块（live 窗口）没有两池路线，所以没有可模拟的机会。
   按 §76，这条只记为「0 机会 + 实测原因」，**没有**为了让 live 会话出现 Accept 去动 reserve / fee /
   tax / gas / 余额 / 阈值。
3. **Flashblock 端点能连、能解码、能和 canonical 对账，但不能作为状态源。** 它给出的 pending 块
   自己的哈希在链上查不到（`eth_getBlockByHash` 返回 `null`），所以候选只能进「观察 + 对账」通道，
   永远不能推进状态。§73 的四种验证里 sequence / stale / gap / fallback 是实测的，
   state integration 记 BLOCKED，端到端记 PARTIAL。

---

## 1. Summary

新增 4 个 crate + 1 个 CLI 子命令，把 M1–M4 的离线管线接上真实市场：

| crate | 职责 | 规模 |
| --- | --- | --- |
| `crates/live`（`evm-live`） | `MarketEvent` 抽象、`BlockTracker`（排序/去重/gap/reconcile）、`PollingSource`、`WebSocketSource`、`FlashblockSource` | 7 个源文件，22 个单元测试 + 12 个循环测试 |
| `crates/metrics`（`evm-metrics`） | 计数器、时钟差（区块时间戳 vs 墙钟）、11 个 hop 的延迟表（nearest-rank 分位数） | 5 个源文件，9 个单元测试 |
| `crates/pipeline`（`evm-pipeline`） | `MarketEngine`（一次会话的编排：state → graph → opportunity → 派活 → 收结果）、证据目录、优雅关闭 | 7 个源文件，5 个单元测试 + 7 个集成测试 |
| `crates/cli`（`evm-cli`，二进制 `evm-mev-bot`） | `live` 子命令（§47 的四个参数 + 取证/阈值/队列参数）、人类可读汇总与 `--json` | 2 个源文件，12 个测试（含 4 个 §26 守卫） |
| `crates/chain` 扩展 | `HeadReader` trait + HTTP / WebSocket / recorded 三个实现 | `head.rs`、`ws.rs`（8 测试）、`recorded.rs`（7 测试） |
| `crates/opportunity` 扩展 | `lifecycle.rs`：`OpportunityLedger`、STALE 判定、§17 的 `OpportunityId` | 14 个单元测试 |

明确**没有**做的事：M4 的 `evm-simulation` / `evm-risk` / `evm-state` / `evm-graph` 一行语义都没改，
只是被组合调用（`crates/pipeline/src/sim.rs` 构造 `SimulationRequest`，`engine.rs` 调 `evm_graph`
与 `evm_opportunity`）。`grep -c SourceKind crates/pipeline/src/engine.rs` = **0**，即状态/图/机会
这一层根本不知道数据是从 WebSocket、RPC 还是录制目录来的 —— 这是 §9「State Semantics 必须统一」
能被证成的原因。

## 2. Architecture

```
                      ┌──────────────────────────────────────────┐
   GIWA Testnet ─────►│ PollingSource<HttpChainAdapter>  http-poll│
   (RPC / WS)         │ PollingSource<WsHeadReader>      websocket│
                      │ RecordedChainAdapter              replay  │
                      └───────────────────┬──────────────────────┘
                                          │ mpsc<MarketEvent>（容量 32）
                      ┌───────────────────┴──────────────────┐
   Flashblock 端点 ──►│ FlashblockSource（pending 候选）      │──► candidates.jsonl（只观察）
                      └───────────────────┬──────────────────┘
                                          │ BlockTracker：按 number 排序、去重、欠号回补
                                          ▼
              MarketEngine（一个实例，两条来源共用）
                ├─ evm-state：ProtocolEvent → StateUpdate → StateStore
                ├─ evm-graph：每块全量重建快照
                ├─ evm-opportunity + lifecycle：候选 → 定价 → STALE
                                          │ JobPlan（16 容量队列）
                                          ▼
                      SimulationPool（2 个 worker，各自 current-thread runtime）
                                          │ mpsc<SimOutcome>（容量 32）
                                          ▼
                        evm-risk → Decision → metrics + 9 个证据文件
```

- **背压（§50/§51）三条策略写在同一个地方**（`crates/pipeline/src/config.rs`），并且随每次会话
  落到 `live-session.json` 里，配置的数和实际生效的数不可能漂移：
  - 事件队列满 → **阻塞源**，市场事件永不丢弃（`on_full = "block the source; market events are never dropped (§51)"`）；
  - 模拟队列满 → **拒绝这个 job**，计数 `decline.simulation_queue_full`，机会留在 ledger 里，摄取不等模拟（§23）；
  - 结果队列满 → **阻塞 worker**，不阻塞主管线。
- **关闭顺序（§49）**：先让所有源停（`stop` flag）→ 关掉派活与回话的队列 → 在 join 的同时把有界
  事件队列排空并计数（`market_events_left_at_shutdown`）→ 关 worker → 写证据。证据文件在会话开始
  就创建（9 个 `.jsonl` + `metrics.json` 先写 `.partial` 再 rename），所以任何中断都留下可读产物。
- **worker 模型（§22 的 Option C）**：`tokio::task::spawn_blocking` 里为每个 worker 建一个
  current-thread runtime，`SimulationRequest` 由主线程构造好再 move 进去。全程 0 个
  `unsafe impl Send`（由守卫测试 `the_m5_path_uses_no_unsafe` 断言 chain/live/pipeline/metrics/cli 五个 crate）。
- 队列实测容量：event 32、simulation 16、outcome 32，worker 2。live 会话里 2 个 worker 各
  idle_polls 397、jobs_run 0（因为没有机会）；parity 会话里 1 个 job 落在 worker 0 或 worker 1。

## 3. Real GIWA Evidence

四类证据分开存放，§72 要求的「不能混淆」按目录名区分：

| 类别 | 目录 | 内容 |
| --- | --- | --- |
| **live evidence**（真实时间到达） | `data/evidence/m5/live/websocket-91342-1790825638077` | WS 轮询 + Flashblock 双源，120 s，121 块，会话 `start_block` 37 480 521（bootstrap head，不重放）→ `end_block` 37 480 642 |
| live evidence（第二个窗口） | `data/evidence/m5/live/websocket-91342-1790824327875` | 120 块，37 479 211（bootstrap head）→ 37 479 331，70 次状态写入，0 机会 |
| **replay evidence**（录制下来的真实区块） | `data/evidence/m5/parity-replay/replay-91342-1790825517095`、`replay-arb/`、`replay-corpus/replay-91342-1790826072401` | fixture 来自 `crates/pipeline/tests/record_corpus.rs`（`--ignored` 显式录制），区块体与日志都是当时从节点真实取回 |
| **真实链上闭环（live 代码路径读历史区块）** | `data/evidence/m5/parity-live/http-poll-91342-1790825443028` | 同一二进制，`--source http-poll`，状态从 RPC 按 pin 取（`state_source = rpc:chain-91342`） |
| **fixture evidence** | `fixtures/live-m5/arbitrage-window/`（50 个区块 JSON，已提交） | 37 191 150 → 37 191 199 的真实区块，parity 与 determinism 都吃它 |
| **端点能力探针**（原始 RPC 往返） | `data/evidence/m5/probe-*.{json,txt,err}`、`method-matrix.json`、`ws-probe-raw.json` | §13 前置取证：WS 握手、pending 解码、订阅错误原文、eth_getLogs 形状、轮询节奏 |
| **本轮复现验证** | `data/evidence/m5/reproduce/{replay,http-poll}-*` | 附 C 的命令原样重跑：replay 那次与已提交产物在去掉时钟/身份字段后 11/11 或 9/11 相同（差的只有 worker 落位计数），http-poll 那次差的只有 `events.jsonl` 的 `GapDetected.to`（实时 head 每次不同）与源侧计数 |

真实链上的原始事实：

| 项 | 值 |
| --- | --- |
| chain | **91342**（由 `eth_chainId` 现场读出，不是配置里的数） |
| RPC / WS / Flashblock | `https://sepolia-rpc.giwa.io` / `wss://sepolia-rpc.giwa.io/ws` / `https://sepolia-rpc-flashblocks.giwa.io`（全部来自 `GIWA_*` 环境变量或命令行，业务代码里没有 URL，见 §15 的守卫测试） |
| live 窗口 | 121 块 / 542 事件 / 9 958 条日志 / 未认领 9 850 / Sync 73（全部 apply）/ Swap 35（不写状态，见第 4 节）/ 图边 240 |
| gap 处理 | 检出 39，恢复 39，未恢复 0；`blocks_skipped = 0`，`duplicates = 0`，`conflicts = 0` |
| 该窗口活跃池子 | 8 个地址发过 Sync 形状的日志（全链），其中 7 个能答 `token0()/token1()` ⇒ 7 个交易对，**无一对有 2 个活跃池**；第 8 个（`0xad153c84…`，174/253 条日志）对这两个调用 revert；registry 已知 6 池，其中 1 池活跃 |
| 闭环窗口 | 50 块 / 3 499 条日志 / Sync 5（5 个 apply）/ Swap 5 / 图边 102 / 候选 4 / 机会 2 |
| wrapped native | `0x4200000000000000000000000000000000000006`（WGIWA）；由 `--wrapped-native` 传入，不作为常量 |

## 4. State Semantics

**统一：一个 `MarketEngine`、一套 apply 路径，`SourceKind` 只存在于 `crates/live`。**
`crates/pipeline/src/engine.rs` 里对来源类型出现次数为 0（实测 `grep -c`），replay 与 live 的差别
只到「事件是谁送来的」这一个标签字段为止。第 13 节的产物比对是这句话的证据。

- **Sync = 储备权威，Swap = 信息**（§12）。实测：live 窗口 35 个 Swap 事件产生 **0** 次状态写入；
  parity 窗口 5 个 Swap 同样 0 写入，5 个 Sync 全部写入（`syncs_applied = 5`）。
- **不可举证的 Sync 被拒**：`rejected_syncs = 0`、`unattested_syncs = 0`（本轮所有 Sync 都能对上
  池子自己的账户对）。
- **每条状态写入可审计**：`state-updates.jsonl` 每行带 `block_number / tx_index / log_index / tx_hash /
  pool / event / before / after / written`。live 窗口 74 行里 72 行同时有 before 与 after
  （1 行是池子首次注册、1 行是首块无前置值）。
- **图每块全量重建**（§13/§14）：live 窗口累计 240 条边（第二个 live 窗口 238），parity 窗口 102 条。
  重建只在 store 已经有状态版本（`engine.rs:246` 的 `snapshot().position`）的块上发生，目标块就是这个
  position 所在的块，任何「最后一次陈述不在这个块上」的池子一律跳过并记下它真正的位置，绝不用旧值凑边
  （`crates/graph/src/builder.rs:75-92`）。实测两个窗口的差别正好把这条规则显示出来：parity 窗口的 store
  里有 **4** 个被举证的池子，最后陈述分属 37 191 156 / 169 / 190 三个块，所以 50 块里 44 次重建累计
  `graph_skip.NotAtTargetBlock = 79`；live 窗口的 store 里只有那 1 个持续被 Sync 的池子，每次重建的目标块
  就是它自己刚写入的块 ⇒ 同一计数为 **0**。
- **状态读取的读回顺序按 key 排序**（M4 修的确定性缺陷在这里继续有效）：录制目录跨进程字节一致。

## 5. Gap Recovery

- **规则就是 §5 的字面规则**：`head > last_head + 1` 即判 gap，并把 `next_expected..=head` 全部登记为
  欠号（`crates/live/src/tracker.rs:158` `note_head`）。之后由 RPC 逐个补读区块头 + 该块日志（§6）。
- **实测节奏**：GIWA 约 1 s 出块，轮询间隔 900 ms 加一次 RPC 往返，所以 head 经常一次跳 2 个号 ——
  本轮 121 块窗口出现 **39** 次，`to - from` 全为 1，**同一轮补齐**，`gaps_unrecovered = 0`、
  `blocks_skipped = 0`、`emitted = announced = 122`。
  这条必须在报告里点明白：**gap 这个字段名描述的是「head 跳过了一个从未被提供的号」，
  不是「我们丢了区块」**；读者要用 `to - from` 和 `gaps_recovered` 判断严重程度。
- **补不齐时不许假装同步**：`tracker.unrecovered_gap()` 一旦置位，`PollingSource` 以
  `EndedBy::GapUnrecovered` 结束并**命名那个永远读不到的区块号**，pipeline 把这条错误原样上报（§8）。
  循环层测试：`a_hole_the_provider_never_fills_ends_the_run_and_names_the_block`（脚本化 reader）。
- **bootstrap 与 reconciliation（§7/§8）**：先读 head 再确定起点，起点之后的所有号都是欠号。
  parity-live 会话里唯一的 gap 是 `37 191 150 → 37 480 326`（起点到实时 head 的距离，289 176 个号），
  它**如实留着没有被当成已同步**；replay 版本里同一条是 `37 191 150 → 37 191 199`（录制目录的末端），
  因此 replay 记 1 次恢复、live 记 0 次恢复 —— 这正是第 13 节里两处合理差异之一。
- 本轮新增的边界测试：`a_head_that_advances_one_at_a_time_is_lag_and_not_a_gap`
  —— head 一次只进 1、但有 2 个号待读时**不得**报 gap（这是队列不是洞）。用假阴性验证过：
  把判据改成 `head + 1 > from` 之后该测试立刻失败。

## 6. Duplicate / Ordering

- **到达顺序永远不是链上顺序**（§4）。两条有序路径：`BlockTracker` 按 `number` 升序排放，
  `MarketEngine` 内按 `(block_number, tx_index, log_index)` 落状态写入。
- 实测不变量：live 窗口 74 条状态写入按 `(block, tx_index, log_index)` **非递减**，去重后**严格递增**；
  其中 14 个区块有多条写入（例：block 37 480 523 = (tx 28, log 47) registered → 同 log 的 synced →
  (tx 30, log 54) → (tx 31, log 60)）。parity 窗口的 9 条同样成立（block 37 191 169 里
  (18, 29) 与 (18, 34) 两个池子各一对 registered/synced）。
- **重复只 apply 一次**（§6/§60）：`tracker.duplicates = 0`，`conflicts = 0`（本轮没有同块双哈希），
  `evicted_for_space = 0`。行为由 3 个测试钉住：`the_same_head_read_twice_does_not_deliver_a_block_twice`
  （循环层）、`the_same_block_twice_is_a_duplicate_and_never_advances_state`、
  `a_reconnect_redelivering_a_consumed_block_is_not_read_as_a_reorg`（内核层）。
- **一条链的 tracker 不许吃另一条链的块**（§45 的副作用防护）：`one_number_two_hashes_is_recorded_and_the_first_hash_wins`、
  `a_block_from_another_chain_is_reported_as_unknown_and_not_emitted`。
- **未知事件不 panic**（§52）：live 窗口有 **3** 条 `UnknownEvent`，内容是 Flashblock 源的
  「`pending` 从 37 480 538 跳到 37 480 540，有 1 个号从未以 pending 出现」—— 记进 events.jsonl，
  管线继续，不中断也不假装看见过。

## 7. Opportunity

**真实机会证据（2 条，都来自区块 37 191 169，`state_version = {block 37 191 169, log_index 34}`，
两个池子的 fee 都是 `997/1000`）：**

| 进入方向 | 输入 token | 输入量 | 解析输出 | 解析 gross | 搜索 | 结果 |
| --- | --- | --- | --- | --- | --- | --- |
| pool A `0x5bef6275…7440` | TTAX `0xcffe7472…2f62` | 890 134 426 448 791 298 | 927 044 426 434 210 645 | +36 909 999 985 419 347 | BoundedTernary，232 次评估 / 104 轮，上界 45 655 538 604 883 371 698 | **降级不模拟**：`funding_unavailable` |
| pool B `0xf487d533…6578` | WGIWA `0x4200…0006` | 714 844 720 992 | 744 486 240 802 | +29 641 519 810 | 130 次评估 / 50 轮 | 模拟 → **Reject**（见第 9 节） |

降级原因逐字记录在 `declines.jsonl`：这条路线花的是 TTAX，而本次运行允许被出金的 wrapped native 是
WGIWA；M4 的 §57 把状态 override 限制在测试 sender 自己身上，**不许凭空造 ERC-20 余额**，所以一个
pinned 状态没有出金能力的 sender 不能交易这条路线 —— 拒绝并计数，不改路线也不改余额。

`OpportunityId`（§17）就是上表可读出来的那串：`chain 91342 block 37191169 pools A|B entering A`。

**live 会话的 0 机会是实测结论，不是没跑**（§64/§76）：

- 121 个实时区块里 74 次状态写入全部落在**同一个**池子；两池路线在结构上不可能出现。
- 该窗口全链共 **253** 条 Sync 形状的日志、来自 8 个地址：其中 7 个能答 `token0()/token1()`，分属
  **7 个不同交易对**（每个交易对只有它自己这 1 个池），**没有一对出现 2 个活跃池**；第 8 个
  `0xad153c84…` 两个调用都 `execution reverted`，它不是这套 AMM 路由能定价的 pair（174/253 条日志来自它，
  对 registry 而言全是 `unclaimed_logs`）。
- 录制语料窗口（48 块，37 472 496 → 37 472 544）也是同一形状：42 次 Sync 全落一个池子，18 个 Swap，
  0 候选。
- registry 覆盖是真正的限制：本地 6 个已举证池子里，这些窗口只有 1 个活跃；全链活跃池子的发现
  不在 M5 范围（§82 不做 general indexer）。

**STALE 生命周期（§16/§24/§66）的实现与覆盖**：`crates/opportunity/src/lifecycle.rs`（14 个单元测试）
按「机会的 `state_version` ≠ 该池子当前状态版本 ⇒ STALE」判定；`invalidated_by_pool = 0` 在四个会话里
都是 0。实测原因（含阳性对照）见第 14 节 limitation 条目。

## 8. Simulation

- **用的哪个区块（§18/§19/§79）**：`pin = 37191169:0x56c628d7…48d670` —— 区块号 + 该区块自己的哈希。
  状态按这个哈希向节点要（`eth_accountAt/eth_storageAt`@hash），**代码里不存在 `"latest"` 这条路径**：
  `crates/pipeline/tests/state_is_always_pinned.rs` 有 3 个测试专门钉这点
  （`no_stage_ever_asks_for_the_latest_state` 等）。
- **状态源有两个真实值**：live/HTTP 会话是 `rpc:chain-91342`；带 `--state-dump` 的 replay 会话是
  `fixtures/simulation-m4/dump-37191169.json@37191169`。这两个值是第 13 节里唯一的语义差异。
- **执行结果（M4 引擎，未改）**：gas_used **340 703**；status =
  `Reverted { step: 7, … swap(uint256,uint256,address,bytes), selector 0x022c0d9f … revert "K" }`；
  `analytical_output = 744 486 240 802`；gross `29 641 519 810`；net_profit **NotComputable**
  （序列没跑完，sender 手里没有可比对的产出，因此不存在能抵扣 gas 的那一段）。
- **不阻塞摄取（§23/§74，实测）**：parity-live 会话里一次模拟耗时 **21 221 ms**（状态要从节点按 pin 现取），
  这 21 s 之内 canonical 区块继续到达并处理了 **26** 个（37 191 174 → 37 191 199，即该窗口的全部剩余区块），
  `simulation_queue_wait = 0 ms`。同一个 job 在 `--state-dump` 的 replay 会话里是 23 ms，
  业务结果完全一致 —— 差的是取状态的钱，不是语义。
- **拒绝规则集（8 条）**原样沿用 M4，本轮新增只在派活层：`state_unavailable`、`state_pin_mismatch`、
  `state_version_mismatch`、`funding_unavailable`、`no_wrapped_native_configured`、
  `route_not_executable`、`request_refused`、`simulation_queue_full`。每次拒绝都落 `declines.jsonl`。

## 9. Risk

- **判定**：`Reject`，规则 `SimulationSuccess`，理由逐字为
  「step 7 (nonce 6) from `0x953E7e98…dfa6f` to `0x5bef6275…7440`: swap(…), selector 0x022c0d9f: reverted — K」。
  四个会话的合计：`accept=0 reject=1 unknown=0`。
- **阈值原样、且如实声明未调**（`live-session.json.risk_thresholds`）：
  `minimum_net_profit_wei = 0`（0 的含义是「任何严格为正的净利」，是未调的默认，不是调过的门槛），
  `maximum_gas = null`（缺省由该区块自己的 gas limit 60 000 000 回答，派工单里记的就是这个数）。
- **机会绝不会直接变 Accept（§55 K）**：风险层的唯一入口是 `evaluate(SimulationOutcome)`，
  `Opportunity` 不是它的参数类型；类型系统里不存在从机会到判定的边。
- **Accept ≠ 发送 ≠ 已实现利润（§25/§26）**：`risk-decisions.jsonl` 每一行自带
  `no_broadcast` 字段（原文：「An Accept here means the simulation satisfied the stated thresholds,
  not that a transaction was or may be sent」），CLI 汇总里也原样打印这句。
- **§76 被执行的样子**：live 会话 0 机会 → 0 模拟 → 0 判定，报告里就是 0；没有出现任何
  为了演示而把 reserve / fee / tax / gas / 余额 / 阈值改动的痕迹 —— 上表的每一个数都能在同一目录的
  jsonl 里逐行找到。

## 10. Latency

分位数一律 nearest-rank（min / p50 / p95 / p99 / max），来自 `metrics.json`。

| hop | live（WS 121 块） | parity-live（HTTP 读历史 50 块） | parity-replay（录制 50 块） |
| --- | --- | --- | --- |
| `chain_to_received` | n=121 1 714 / **2 340** / 2 824 / 3 057 / 3 096 | n=50 289 149 059 / 289 163 425 / … / 289 178 607 | n=50 289 207 767 / 289 229 060 / … / 289 251 350 |
| `block_to_graph` | n=120 634 / **770** / 1 236 / 1 614 / 1 684 | n=44 671 / 826 / 1 480 / 1 971 | n=44 0 / 0 / 1 / 1 |
| `block_to_opportunity` | n=52 654 / **778** / 1 185 / 1 684 | n=4 810 / 820 / 1 698 | n=4 0 / 0 / 2 |
| `simulation_queue_wait` | 未测（无模拟） | n=1 0 | n=1 0 |
| `simulation_duration` | 未测 | n=1 21 221 | n=1 23 |
| `block_to_simulation_end` | 未测 | n=1 23 155 | n=1 26 |
| `block_to_risk` | 未测 | n=1 29 700 | n=1 27 |

与 §70 要求的六项对照：

| §70 要求的项 | 本轮的实现 | 状态 |
| --- | --- | --- |
| `block_received_latency` | `chain_to_received`（区块时间戳 → 本机读到） | 实测 |
| `state_update_latency` | 每条记录里的 `decoded_to_state_updated` / `block_to_state`（会话级未单独聚合，聚合到 `block_to_graph`） | **PARTIAL** |
| `opportunity_latency` | `block_to_opportunity` | 实测 |
| `simulation_latency` | `simulation_duration` + `simulation_queue_wait` + `block_to_simulation_end` | 实测（live 窗口无模拟） |
| `risk_latency` | `simulation_to_risk_decided`（逐条）；会话级 `block_to_risk` | 实测 |
| `end_to_end_latency` | `block_to_risk` | 实测 |

必须解释的两个数：

- **replay / 历史读区的 `chain_to_received` ≈ 2.9 × 10⁸ ms（约 3.3 天）不是管线延迟**，是「这个区块
  产生于多久以前」。刚出块的实时值在 live 窗口那行（p50 2 340 ms），里面还包含约 900 ms 的轮询间隔
  与一次 RPC 往返。
- **`block_to_risk − block_to_simulation_end = 6 545 ms`（parity-live）**：模拟结果已经产出，但要挤回
  单线程主管线才能被风险层判定。这是当前架构的实测代价，记在第 14 节。

## 11. Flashblock

`FLASHBLOCK_ENDPOINT = https://sepolia-rpc-flashblocks.giwa.io`（来自 `GIWA_FLASHBLOCKS_URL`/命令行；
业务代码里没有这个 URL）。以下每一行都是本轮实测，`live-session.json` 的
`capability.flashblock` 就是原始记录：

| 验收项 | 状态 | 证据 |
| --- | --- | --- |
| CONNECT | **PASS** | `eth_getBlockByNumber("pending")` 答出区块头 |
| SUBSCRIPTION | **NOT_PROBED_HERE** | 订阅由 WS 源实测并原样记录：`eth_subscribe` → `{"code":-32603,"message":"Internal error"}`（`newHeads` 与 `newBlockHeaders` 两次） |
| DECODE | **PASS** | 解出 number 37 480 525、hash `0x13cd966c…be0fe`、gasUsed 5 116 429、40 笔交易 |
| SEQUENCE | **PASS** | 239 次读、346 个事件、229 个不同候选、118 个区块号、111 次同块刷新哈希、0 个过期、3 次跳号（→ `UnknownEvent`） |
| STATE INTEGRATION | **BLOCKED** | 候选块 `0x13cd966c…be0fe` 用 `eth_getBlockByHash` 查询返回 `null`；pending 状态不可寻址，因此候选**永远不能推进状态** |
| NEWHEADS RECONCILIATION | **PASS** | 121 个 canonical 里 114 个被候选覆盖；候选哈希与 canonical 哈希精确匹配 **1** 次 |
| FALLBACK | **PASS** | canonical 摄取由 WS/轮询源负责，候选不能前进状态，所以没有可覆盖的东西 |
| END-TO-END | **PARTIAL** | 候选流确实穿过了统一管线并与 canonical 对账；但「候选到达 ⇒ 更快看到状态变化」无法证明（上一行 BLOCKED） |

按 §31/§73：abstraction（`MarketDataSource` + `FlashblockSource`）、adapter 边界（只发
`Candidate` / `CandidateResolved`，不发 `Canonical`）、测试（`crates/live/src/flashblocks.rs` 10 个）
都在，blocker 写进本表，**没有**任何「Flashblock 已完成」的表述。候选只进 `candidates.jsonl`
（339 行）供事后研究。

## 12. Determinism

两次独立 CLI 进程跑同一录制目录（`data/evidence/m5/determinism/replay-91342-1790825900382` 与
`…1790825906965`）：

- 全部 11 个产物的叶子字段差异共 **342** 处（一个字段都不剥的原始计数），逐类归属：
  `session_id` 120、时钟字段（`observed_at_unix_ms` 100、`chain_to_received_ms` 50、
  `deltas[].ms` 14+13、`started/ended_at_unix_ms` 2、`span_ms` 4、`queue_wait_ms` 1、
  metrics 分位数与极值 7+6+6+5+5=29）、worker 落位（`worker` 1、`jobs_run` 4、`idle_polls` 4）。
  合计 342，无未归类项。
- **业务语义 0 处差异**：50 区块、9 次状态写入、2 个机会、1 次降级、1 次模拟、1 个 Reject 全部一致；
  `candidates.jsonl` 两边都是 0 行。
- **模拟指纹一致**：两次都是 `0x5c8882b83b5aa3449d63…`（原文见产物），指纹覆盖结果的全部字段
  （M4 的 `a_fingerprint_covers_every_field` 保证）。
- 唯一「不是数值一致而是位置互换」的项：那 1 个模拟 job 落在 worker 0 还是 worker 1。
- 剥掉时钟与 `session_id`/`worker` 后再比：11 个产物里 **9 个逐字段完全相同**，只剩
  `live-session.json` 与 `status.jsonl` 各 4 处，全部是 worker 落位的两个计数
  （`idle_polls` 19 ↔ 21、`jobs_run` 1 ↔ 0，两个 worker 互换）。
- 第三个进程复现（`data/evidence/m5/reproduce/replay-91342-1790827794647`）与已提交产物比对（同一
  规范化）：与 `…1790825900382` 是 **11/11 完全相同**，与 `…1790825906965`、`parity-replay/replay-…1790825517095`
  是 9/11（差的仍是上面那两个 worker 计数文件）；`metrics.counters` 全部键值完全相同，
  包括 `market_events_left_at_shutdown = 1`。模拟指纹同上。

## 13. Replay / Live Parity

同一个二进制、同一份 50 区块真实数据、两个进程（`--source http-poll` vs `--source replay`）。
下表是**去掉时钟字段与 `session_id`/`worker` 之后**逐文件比对的结果。一个补充精度：原样（什么都不剥）
比时，前四行各多出「每行 1 处」的差异，全部是 `session_id` 这个会话名本身 —— 这四个产物里不出现任何
时钟字段（`state-updates` 9 处 / `opportunities` 2 处 / `declines` 1 处 / `risk-decisions` 1 处，
键只有 `session_id`）；而 `blocks.jsonl` 原样比是 309 处（50 × 时钟 3 个字段 + source 标签 + 会话名）、
`events.jsonl` 是 152 处，时钟字段集中在这里。

| 产物 | 行数（live/replay） | 差异叶子数 | 差异内容 |
| --- | --- | --- | --- |
| `state-updates.jsonl` | 9 / 9 | **0** | 完全一致 |
| `opportunities.jsonl` | 2 / 2 | **0** | 完全一致 |
| `declines.jsonl` | 1 / 1 | **0** | 完全一致 |
| `risk-decisions.jsonl` | 1 / 1 | **0** | 完全一致 |
| `candidates.jsonl` | 0 / 0 | **0** | 两边都没有 flashblock 候选（§73 的候选通道不在这条路径上） |
| `blocks.jsonl` | 50 / 50 | 50 | 只有 `announcement.source`（`HttpPoll` vs `Replay`） |
| `events.jsonl` | 51 / 51 | 51 | 50 × `Canonical.source` 标签 + 1 × `GapDetected.to`（实时节点 head 37 480 326 vs 录制末端 37 191 199） |
| `simulation-results.jsonl` | 1 / 1 | 2 | `state_source`（`rpc:chain-91342` vs dump 文件路径）与由它派生的 `fingerprint` |
| `metrics.json` | 1 / 1 | 1 | `market_events_left_at_shutdown`（33 vs 1） |
| `status.jsonl` | 4 / 4 | 12 | 源侧的 cycles/events_sent/tracker 计数（HTTP 要追 head、replay 只读目录）、worker idle_polls、上述两处 |
| `live-session.json` | 1 / 1 | 19 | `capability.*`（5，两个源各自记录自己被 probe 到的 transport）、`source` 标签（2）、`endpoints.rpc_url`（1）、`state_source`（1）、源侧 run report 的 8 个计数（cycles / events_sent / source / tracker 的 5 个）、两个 worker 的 `idle_polls`（2） |

11 个产物合计 **135** 处差异叶子，逐键归属就是上表那几类；决定「状态语义」的 5 个产物
（`state-updates` / `opportunities` / `declines` / `risk-decisions` / `candidates`）合计 **0**。

**语义层等价性举证（不是「数字看着差不多」）**：两条路线 pin 同一个 `37191169:0x56c628d7…`，
gas_used 同为 **340 703**，同为 `Reverted step 7 … reverted — K`，`analytical_output`
`744 486 240 802`、gross `29 641 519 810`、`net_profit = NotComputable` 全等，
风险判定同为 `Reject / SimulationSuccess`。唯一真实语义差 = 「状态是从哪儿读的」这句话本身，
而那正是 §32 要对比的两个世界。

## 14. Limitations

1. **`eth_subscribe` BLOCKED（端点能力）**：provider 原文 `-32603 Internal error`，两种订阅名都试过。
   所以 §3.1 的「订阅生命周期 / heartbeat / 故障侦测」只有代码 + 脚本化测试的形状证据
   （`crates/chain/src/ws.rs` 8 个测试：错误原话回传、非 JSON 帧上报而非丢弃、心跳阈值、
   订阅不支持时把每种请求的名字和各自答案写全），**没有**真实推流链上证据。
2. **Flashblock 不能取状态（端点能力）**：见第 11 节 STATE INTEGRATION = BLOCKED；因此 §73 的
   stale/gap/fallback 是在候选观察通道内验证的，不是状态推进通道。
3. **完整闭环未在「刚刚产生的区块」上证成**：见结论第 2 条 + 第 7 节实测原因（121 块窗口全链 8 个活跃池
   落在 7 个交易对，无一对有两个池；registry 只覆盖 1 个活跃池）。
4. **无真实 STALE（§66）**：`invalidated_by_pool` 四个会话均为 0。实测原因带阳性对照：唯一可配成对的
   两个池子在 block 37 191 169 各写 1 条 Sync，之后 3 000 个区块（37 191 170 → 37 194 169）里
   这两个池子**任何日志都没有**（`eth_getLogs` 按地址查为 0 条），而同一查询形状在范围
   37 191 170 → 37 194 169 上对全体池子返回 **4 462** 条 Sync、对 live 窗口的活跃池返回 461 条 ——
   即「0」不是查错 topic0 造成的假零（规则与 14 个单元测试都在，缺的是发生）。
5. **§24 的「飞行中失效」抑制分支没有端到端测试**：代码在 `crates/pipeline/src/sim.rs`
   （`on_outcome` 与 `live_finding` 双检查 → `simulation_result_suppressed_stale`），
   只有单元层覆盖；要端到端复现必须让一个机会在模拟的 20 s 内被同池 Sync 覆盖，本轮两个真实窗口
   都没发生（见第 4 条）。
6. **L1 数据费仍然不在账上**（M4 的结论继续有效）：GIWA 是 OP-stack rollup，模拟 gas 只覆盖 EVM 扣的
   那一份，收据里的 `l1Fee` 不进 net_profit。
7. **testnet 流动性**：那两条路线的输入量级是 714 844 720 992 wei = 0.000000714844 WGIWA 级别，
   种子流动性；「闭环成立」不等于「这条路线能赚钱」。
8. **没有执行**：不广播、不签名、无私钥、不部署执行合约、不竞价（§26 全清单由 4 个守卫测试断言）。
   M6 的第一步（把 Accept 变成一笔交易）刻意留白。
9. **主管线是单线程编排**：`block_to_risk − block_to_simulation_end = 6 545 ms` 是结果回话等调度的时间；
   §74 证明的是「不阻塞摄取」，不是「判定即时」。
10. **风险阈值未调**：`minimum_net_profit_wei = 0`、`maximum_gas = null`。任何 Accept 出现之前必须先
    有一个被举证的阈值，本轮不假装它存在。
11. **关闭时会丢弃队列尾部**：`market_events_left_at_shutdown` 本轮 33 / 5 / 1（按 §51 计数落盘，
    不是静默丢弃）。语义是「会话已结束，这些块本来也不会再被处理」，但字段名必须被读到才能理解
    该会话的 blocks 计数为什么小于源侧 `emitted`。
12. **gap 计数在 1 s 出块 + 轮询节奏下偏噪**：39/121（第 5 节）。按 §5 字面成立，但字段名容易读成丢块。
13. **`state_update_latency` 没有会话级聚合**（第 10 节 PARTIAL 条）；逐条记录里有 `block_to_state`。
14. **语料目录不入库**：`fixtures/live-m5/corpus/`（4.1 MB）在 `.gitignore` 里；已提交的
    `fixtures/live-m5/arbitrage-window/`（50 块，3.4 MB）足以复跑 parity/determinism/reproduce，
    语料可用附 C 的 `--ignored` 测试随时重录（归档节点仍有这些区块）。

## 15. Tests

§55 的 A1 四道关卡，全 workspace 实跑（`CC=clang CXX=clang++ CXXFLAGS="-include cstdint"`）：

```bash
cargo fmt --all --check                                              # exit 0
cargo check --workspace --all-targets                                # exit 0，无输出
cargo test --workspace                                               # 350 通过 / 0 失败 / 5 忽略
cargo clippy --workspace --all-targets --all-features -- -D warnings  # exit 0，无警告
```

5 个忽略项都是「主动写盘 / 打真实 RPC」的取证型测试：`pipeline/tests/record_corpus.rs`(1)、
`replay/tests/record_fixtures.rs`(2)、`replay/tests/m1_replay.rs`(1)、`simulation/tests/real_chain.rs`(1)。

§54 要求的四层，本轮落点：

| 层 | 位置 | 数量 |
| --- | --- | --- |
| 内核单元 | `crates/live/src/{tracker,flashblocks}.rs`、`crates/metrics/src/*`、`crates/opportunity/src/lifecycle.rs` | 12 + 10 + 9 + 14 |
| 循环/集成 | `crates/live/tests/source_loop.rs`（脚本化 `HeadReader`，12）、`crates/pipeline/tests/recorded_loop.rs`（4）、`state_is_always_pinned.rs`（3）、`crates/cli/tests/{live_args(8),no_execution(4)}.rs` | 31 |
| 真实链上 | `crates/pipeline/tests/record_corpus.rs --ignored`、`replay/tests/*` 的 live-RPC 测试、`data/evidence/m5/*` 的会话产物 | — |
| 产物层确定性/一致性 | 第 12、13 节的跨进程文件比对脚本（附 C 最后一条） | — |

按包实测：`evm-live` 34、`evm-metrics` 9、`evm-pipeline` 12(+1 忽略)、`evm-cli` 12、
`evm-opportunity` 93、`evm-chain` 17、`evm-replay` 16(+3 忽略)。

§26/§44/§45/§22 的四条守卫（`crates/cli/tests/no_execution.rs`）扫 production 源码（剥注释）：
禁止出现 `sendRawTransaction / eth_sendBundle / mev_send / flashbots / SequencerDirect / SigningKey /
PrivateSigner / private_key / secret_key / keystore`；禁止依赖 `alloy-signer / -network / -provider /
-transport / -rpc-client / foundry-evm`；禁止业务代码出现任何 `http(s)://`、`ws(s)://` 或字面 `91342`；
禁止 chain/live/pipeline/metrics/cli 里出现 `unsafe`（含 `unsafe impl Send`）。

## 16. Git Commits

四个提交，代码与文档分开，**不推送任何远端**：

| | commit | 内容 |
| --- | --- | --- |
| code | `7787ea9` | 4 个新 crate + chain/opportunity/replay/cli 的改动 + `fixtures/live-m5/arbitrage-window` + `data/live-m5` + `data/evidence/m5`（234 个文件，+26 223 行） |
| code | `d5a252c` | `crates/live/src/tracker.rs` 的 gap 边界测试与说明注释（写报告时实测发现的行为钉牢，判据行与 `7787ea9` 逐字相同）+ `data/evidence/m5/reproduce/` 两条复现产物（23 个文件，+796 行） |
| code | `8c9a7fe` | 同文件注释的一次自我更正：上一版把 `max()` 剪枝说成边界测试成立的原因，而剪枝从不 binding（`next_expected ≤ last_head + 1` 恒成立），测试真正钉住的是「head 与自己上一步 offering 比较」 |
| docs | 本提交（在 `git log` 里紧邻上一条之后） | 本文件 + `M5 Coding.md` + `M5 Coding补充说明.md` |

---

## 附 A：§55–§74 Acceptance A–T 逐条对照

| 条 | 任务书要求 | 状态 | 证据（本轮实测） |
| --- | --- | --- | --- |
| A1 | 四道关卡全过 | **PASS** | 第 15 节，350/0/5 |
| B | 真实 GIWA：connect / subscribe / receive + evidence | **PARTIAL** | connect + 121 块到达 + 全量证据（第 3 节）；subscribe = BLOCKED（`-32603` 原文入档） |
| C | 连续出块，无误判 gap | **PASS（附一条可读性缺陷）** | 121 块连续，`blocks_skipped=0`、`gaps_unrecovered=0`；判据严格按 §5；39 次 span=1 告警见第 5 节 / 局限 12 |
| D | gap → 恢复 → 继续 | **PASS** | 39 检出 39 恢复；未恢复即结束并命名区块（`source_loop.rs`） |
| E | 同块按 tx_index / log_index 确定排序 | **PASS** | 74 行（+9 行）非递减、去重后严格递增；多写入块 14 个（第 6 节） |
| F | 重复 event 只 apply 一次 | **PASS** | `duplicates=0`、`conflicts=0` + 3 个测试 |
| G | 真实日志 → ProtocolEvent → StateUpdate → Store，且与 RPC 回读一致 | **PASS** | `syncs_applied=73`（live）/5（parity），72 行带 before/after；`replay/tests/m1_replay.rs` 的 RPC 回读测试仍绿；状态写入产物与节点历史读回同值 |
| H | 图快照正确 | **PASS** | 每次重建都从 store 全量重建、目标块 = snapshot 自己的 position：累计 240 条边（live）/102 条（parity）；parity 的 44 次重建里 79 次 `NotAtTargetBlock` 跳过并记下池子真实位置（live 为 0，原因见第 4 节） |
| I | 机会来自真实历史 / live；live 无机会不得伪造；录制事件可用于验收但须标 replay | **PASS** | 2 个机会来自真实区块 37 191 169；live 会话如实 0（第 7 节结构原因）；replay 会话的 `source="replay"` 写在每条产物里 |
| J | 模拟用机会自己的区块，不用 latest | **PASS** | pin `37191169:0x56c628d7…`；`state_is_always_pinned.rs` 3 个测试；`--state-dump` 路径也带 `@37191169` |
| K | Accept / Reject / Unknown，绝不 Opportunity→Accept | **PASS** | 类型层只接受 `SimulationOutcome`；本轮 accept=0 reject=1 unknown=0 |
| L | relevant pool 变化 ⇒ 机会变 STALE 且不再被消费 | **PARTIAL** | 规则 + 14 个单元测试；真实窗口无发生（局限 4，含 4 462 条对照的假零排除）；§24 抑制分支无端到端测试（局限 5） |
| M | Replay == Live | **PASS** | 第 13 节逐文件差异表：语义 0 差 |
| N | 确定性：state_hash / graph / opportunity 相等 | **PASS** | 第 12 节：342 处差异全属时钟与身份；模拟指纹全等 |
| O | 断线 → 重连 → 对账 → 继续，不重复不漏块 | **PARTIAL** | 实现（`ws.rs:380` 指数退避 + 状态事件）+ 3 个内核测试（重投≠reorg、reconcile 后欠号不变、同数双哈希取首个）；本轮真实会话 `read_failures=0`，**未发生真实断线** |
| P | 六段延迟 + 分位数 | **PARTIAL** | 第 10 节映射表：5/6 有会话级聚合，`state_update_latency` 仅逐条 |
| Q | 代码审查证明无执行 | **PASS** | 4 个守卫测试 + 依赖清单（第 15 节）；本仓库无 signer/transport 依赖 |
| R | 报告不得混淆真实/fixture/replay/live | **PASS** | 第 3 节分类表；每条产物带 `session_id`，其前缀即 `websocket` / `http-poll` / `replay` |
| S | Flashblock 真实验证或如实 BLOCKED | **PASS（按 §73 第二种形态）** | 第 11 节 8 行表；`state_integration` BLOCKED，`end_to_end` PARTIAL，无「已完成」表述 |
| T | 模拟不阻塞摄取（集成测试或 benchmark） | **PASS** | 实测 21 221 ms 的模拟期间继续处理 26 个 canonical 块；`ingestion_never_waits_for_a_simulation_slot`（`state_is_always_pinned.rs`） |

## 附 B：与任务书的偏离（记录，不改范围）

1. **`--source` 显式化**：§47 只要求 `--rpc-url --ws-url --start-block --duration` 四个参数；本轮另加
   `--source / --replay-dir / --state-dump / --max-blocks / --evidence-dir / --registry-dir /
   --wrapped-native / 队列与阈值参数`。原因：§32/§67 的 parity 必须是**同一个二进制**跑两种世界，
   §63 要求录制事件可回放，§48/§49 要求证据目录可控。
2. **实时性用「同一连接轮询」代替推流**：端点不支持 `eth_subscribe`（附 A 的 B 条）。§3.1 的
   连接/重连/心跳/故障侦测按 §31 的精神保留为代码 + 测试，并在报告里记 BLOCKED，不当作已接通。
3. **`MarketEngine` 不看来源**：任务书没规定这条，但没有它 §9 就无法证成；实测方式是
   `engine.rs` 里 `SourceKind` 出现 0 次。
4. **§75 的输出形状**：`block=… / opportunity=… / simulation=… / risk=… / latency: …` 全部照打，
   少了「connected to GIWA / chain_id=91342」这两行字面横幅 —— 链身份是从端点 `eth_chainId` 现场读的、
   按 §44/§45 不许把名字或链号烧进代码，汇总里以 `session=… chain=91342 source=…` 的形式打印实测值。
5. **§70 的 `state_update_latency` 未做会话级聚合**：逐条 `block_to_state` / `decoded_to_state_updated`
   都在，缺的是分位数（附 A 的 P 条记 PARTIAL，第 14 节局限 13）。
6. **本轮写报告时新增 1 个测试**（`a_head_that_advances_one_at_a_time_is_lag_and_not_a_gap`，`d5a252c`）：
   我一度按「gap 告警偏噪」判断这是缺陷并改了判据，随后用假阴性验证证明改动是 no-op
   （`next_expected ≤ last_head+1` 恒成立，`max()` 剪枝从不生效），已把判据还原为与 `7787ea9`
   逐字相同的版本，只保留边界测试。第一版说明注释把「测试能成立」归因给那个从不 binding 的剪枝，
   这个归因本身是错的，由 `8c9a7fe` 更正：真正把「读取端落后」和「链上有洞」分开的是判据拿 head 跟
   它自己上一步 offering 比较。

## 附 C：复现 —— 报告里每个数字来自哪条命令

```bash
export CC=clang CXX=clang++ CXXFLAGS="-include cstdint"
export GIWA_RPC_URL=https://sepolia-rpc.giwa.io
export GIWA_WS_URL=wss://sepolia-rpc.giwa.io/ws
export GIWA_FLASHBLOCKS_URL=https://sepolia-rpc-flashblocks.giwa.io

# 四道关卡（第 15 节）
cargo fmt --all --check && cargo check --workspace --all-targets \
  && cargo test --workspace \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings

# 第 3 节的 live 会话（120 s；B/C/D/E/F/G/H/S 条的真实证据）
cargo run -q -p evm-cli --bin evm-mev-bot -- live --source websocket --duration 120 \
  --registry-dir data/protocols --registry-dir data/protocols-m3 \
  --wrapped-native 0x4200000000000000000000000000000000000006 \
  --evidence-dir data/evidence/m5/live

# 第 13 节的 parity 两条（M/N 条；本轮附 C 重跑产物在 data/evidence/m5/reproduce/）
cargo run -q -p evm-cli --bin evm-mev-bot -- live --source http-poll \
  --start-block 37191149 --max-blocks 50 \
  --registry-dir data/protocols --registry-dir data/protocols-m3 \
  --wrapped-native 0x4200000000000000000000000000000000000006 \
  --evidence-dir data/evidence/m5/parity-live
cargo run -q -p evm-cli --bin evm-mev-bot -- live --source replay \
  --replay-dir fixtures/live-m5/arbitrage-window --start-block 37191149 --max-blocks 50 \
  --state-dump fixtures/simulation-m4/dump-37191169.json \
  --registry-dir data/protocols --registry-dir data/protocols-m3 \
  --wrapped-native 0x4200000000000000000000000000000000000006 \
  --evidence-dir data/evidence/m5/parity-replay

# 第 12 节的确定性（同一目录跑两次）
for i in 1 2; do cargo run -q -p evm-cli --bin evm-mev-bot -- live --source replay \
  --replay-dir fixtures/live-m5/arbitrage-window --start-block 37191149 --max-blocks 50 \
  --state-dump fixtures/simulation-m4/dump-37191169.json \
  --registry-dir data/protocols --registry-dir data/protocols-m3 \
  --wrapped-native 0x4200000000000000000000000000000000000006 \
  --evidence-dir data/evidence/m5/determinism --quiet; done

# 第 7 节「结构原因」的两条 RPC 实测（真实节点，只读）
#   a) 该窗口有几个池子发 Sync、分别几个：eth_getLogs(from=0x39c7a29,to=0x39c7acd, topics=[Sync])
#   b) 这两个池子之后 3 000 块有没有任何日志：eth_getLogs(address=池子, 0x3978 8de..0x3979 099)
#      阳性对照：同形状去掉 address ⇒ 4 462 条 Sync
python3 - <<'PY'   # 形状与报告一致；body 由程序生成，避免手敲 hex
import json, subprocess
U="https://sepolia-rpc.giwa.io"; S="0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"
def q(p):
    b=json.dumps({"jsonrpc":"2.0","id":1,"method":"eth_getLogs","params":[p]})
    return json.loads(subprocess.run(["curl","-s","-X","POST","-H","content-type: application/json","-d",b,U],
                                     capture_output=True,text=True).stdout)["result"]
print(len(q({"fromBlock":hex(37480521),"toBlock":hex(37480642),"topics":[S]})))          # 253 条 / 8 个池子
print(len(q({"fromBlock":hex(37191170),"toBlock":hex(37194169),                          # 0 条（这两个池子静默）
             "address":"0x5bef6275607901dCd58160356660151BE0637440","topics":[S]})))
print(len(q({"fromBlock":hex(37191170),"toBlock":hex(37194169),"topics":[S]})))          # 4 462 条（对照）
PY

# 第 7 节：把 live 窗口 8 个活跃池解析成交易对（token0()=0x0dfe1681, token1()=0xd21220a7）
# 附 A 的 T 条：模拟在飞的 21 221 ms 里到达的 canonical 区块数
python3 - <<'PY'
import json
d="data/evidence/m5/parity-live/http-poll-91342-1790825443028/"
ev=[json.loads(l) for l in open(d+"events.jsonl") if l.strip()]
canon={e['Canonical']['number']: e['Canonical']['observed_at_unix_ms'] for e in ev if 'Canonical' in e}
a=canon[37191169]
print(sum(1 for n,t in canon.items() if a+1934 <= t <= a+23155))   # 26
PY

# 第 12/13 节用的产物比对（去掉时钟与身份字段后逐文件比）
python3 - <<'PY'
import json, os
DROP={'session_id','worker','ms'}
def norm(o):
    if isinstance(o,dict): return {k:norm(v) for k,v in o.items() if not (k in DROP or k.endswith('_ms'))}
    if isinstance(o,list): return [norm(x) for x in o]
    return o
def load(p): return ([norm(json.loads(l)) for l in open(p) if l.strip()] if p.endswith('.jsonl')
                     else [norm(json.load(open(p)))])
A="data/evidence/m5/parity-live/http-poll-91342-1790825443028"
B="data/evidence/m5/parity-replay/replay-91342-1790825517095"
for f in sorted(os.listdir(A)):
    if f.endswith(('.jsonl','.json')):
        print(f, json.dumps(load(A+'/'+f),sort_keys=True)==json.dumps(load(B+'/'+f),sort_keys=True))
PY

# 语料重录（可选，第 14 节局限 14；写出 4.1 MB 未入库目录）
cargo test -p evm-pipeline --test record_corpus -- --ignored
```

## 附 D：§88 的最终提问

> **M5 是否已经证明：GIWA Testnet 的实时链上状态，可以经过统一 State Semantics，进入实时
> Opportunity → Simulation → Risk 闭环？**

**已经证明，附一条明确的收窄。**

- 成立的形状：真实 GIWA 区块（37 191 150 → 37 191 199）经实时 RPC 摄取 → 真实日志解码 → 74/9 次
  可审计状态写入 → 每块图快照 → 4 个候选里定价出 2 个机会 → 1 个降级、1 个模拟（状态按该机会自己的
  `37191169:0x56c628d7…` 从节点现取）→ 1 个 `Reject`，全程落在同一份 `MarketEngine` 代码上，
  并且与「同一目录录制下来再喂给同一个二进制」的 replay 结果逐字节一致。
- 收窄的那一条：**刚刚产生的区块（live 窗口 121 块）上没有两池路线**，所以「机会→模拟→风险」这一段
  目前只在「实时读到的历史区块」上证成过；原因是可测的（全链 8 个活跃池分属 7 个交易对，
  无一对有两池；registry 覆盖 1 个活跃池），不是没跑。
- 因此 M5 之后的下一步不是 M6 执行，而是两件事的合并：**把活跃池子的发现扩到 registry 之外**
  （否则实时闭环永远只能靠历史区块举证）+ **等一个能被同一池子 Sync 覆盖的飞行中机会**，
  把 §24 的抑制分支从单元测试升级成链上证据。
