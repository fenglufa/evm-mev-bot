# M12-F 持久化执行台账与进程重启恢复安全 — 完成报告

任务书：`docs/v0.1/M12F Coding.md`（任务书 §14 写作 `docs/v0.1/M12-F Coding.md`；仓库里 M12A–M12E 的任务书一律是无连字符的文件名，本轮沿用既有命名，指的是同一份文件）
基线：`HEAD = origin/main = d4d047f3163b7de4bd427831d88047be90b576a6`（M12-E 完成笔）
证据目录：`data/evidence/m12/f/`
门禁：四道串行，全部 rc=0（实测六栏见第 10 节；冷跑 clippy 曾以 rc=101 失败一次，如实记录在同一节）
判定：**M12_F = COMPLETE**（§15 十五条逐条判定见第 15 节）；`SELF_HOSTED_NODE = NOT_RUN`、`LOCAL_CANONICAL_RPC = NOT_VERIFIED`、`LOCAL_FLASHBLOCKS = NOT_VERIFIED`、`MULTI_NODE_HA = OUT_OF_SCOPE`、`REAL_PROFITABLE_ARBITRAGE = UNKNOWN` 全部维持。

---

## 1. 一页结论（白话版，先看这一段）

**改了什么。** 这轮之前，「这笔交易发到哪儿了」只记在进程的脑子里。M12-E 已经让一个不确定的答案在进程内是安全的（不重发、nonce 不释放），但那份安全随进程一起死：如果在「已经把交易交给节点」和「节点的回答回来」之间这个窗口里进程退出，重启的进程**什么都不知道** —— 不知道本地哈希、不知道占了哪个 nonce、甚至不知道自己曾经发过。于是它会把这个 nonce 当成空闲，重新分配、再发一笔：同一个编号的两笔交易。这轮做的事是把这些事实写在磁盘上，而且**写在发之前**：每一笔可能花钱的动作，先落一行、fsync 成功，才允许碰 socket；落盘失败就是停止，不退化成「先记在内存里」。

**一个比喻，附单位换算。** 把台账想成一间自己盖的小会计室，把每一次动作想成往账本上写一行字。旧行为：会计把「我要寄这封信」写在白板上 —— 断电白板就没了，第二天新会计看到空白板，就用**同一个编号**再寄一封（1 个意图 → 磁盘 0 行 → 网络最多 1 次额外 POST）。新行为：先在纸上写「意图」这行、墨干（fsync）才出门寄信，寄之前后各写一行；纸就是 `data/ledger/execution-journal-<chain_id>.jsonl`，一行 = 一个事实 = 1 个 `seq` + 1 个 keccak 校验和（32 字节摘要，写成 0x 开头 64 个十六进制字符）。新会计接班时只做一件事：从第一行往下折一遍，读出来什么状态就照什么状态守着 —— 读的过程一行都不写，所以同一本账读两遍答案相同。

**最高优先级那一格（任务书 §8 F2）实测结果。** 桩服务端完整读走并记录原始交易字节，然后丢弃响应；客户端得 `Unknown`；意图行与派发行都已在文件里；进程退出；新进程重新折这本账。实测：恢复出的状态 **awaiting_receipt**（未决），本地哈希、nonce、身份全部回来，重启后对同一笔的第二次提交次数 **0**，lane **held**。这些数字来自桩自己的 socket 到达计数与文件字节，不是「函数返回了预期错误」。

**九格（F1–F9）怎么落。** 40 行测量、13 个用例，每格都有真实断言：F1 意图落盘后崩溃 → 恢复 `possibly_in_flight`（不写成「确定没发」）；F3 派发后无答案 → `awaiting_receipt`；F4 节点 ack 后崩溃 → 恢复为「被接受」而**不是**「已进块」；F5 内存里看过回执但没落盘 → 记录回到「仍等待」；F6 十种损坏（截断/编辑/空文件/异链/重复 seq/新版 schema/双活 lane…）→ 14 行拒绝，拒绝后调用 0 次、发送 0 次、字节一个没改；F7 同一本账连开两次 → 行数与状态不变；F8 目录不可写 / 中途被清空 → 发送到达 0 次且不进成功态；F9 `receipt == null`、查询失败 → lane 不释放、不替换、`Unknown` 永不变成 `Rejected`。

**没做的事（任务书 §3.2 的十三条，逐条自查见第 8 节）。** 没部署节点，没连真实 GIWA / L1 RPC / Beacon，没签名也没广播任何交易，没自动重发 `Unknown`，没释放未知交易的 nonce lane，没做多节点/HA/故障转移，没实现完整自动对账或后台重试，没改 M10/M11 的业务设计，没接 Flashblocks Early Radar，没动 M12-E 的发送重试策略，没引入数据库或任何新外部基础设施。本轮唯一的 socket 是测试进程自己在 `127.0.0.1:0` 上起的桩。

**一句必须说清楚的话。** 本轮闭合的是**本地台账的跨进程持久化**：恢复成功只证明「这个进程知道自己上次做了什么」，它**不等于**「已确认节点上的交易状态」。一个恢复出来的 `Unknown` 是仍占着 nonce 的本地事实，不是链上结论；链上到底有没有这笔交易，本轮未验证、也无法用本地文件证明。

---

## 2. 基线与代码路径审计（§14 第 1 项）

| 项 | 实测 | 取证方式 |
| --- | --- | --- |
| 开工 HEAD / `origin/main` | 两者都是 `d4d047f3163b7de4bd427831d88047be90b576a6`，`git rev-list --left-right --count origin/main...main` 为 `0 0` | `git log`、`git rev-list` |
| 基线身份 | M12-E 的 `docs(m12e)` 完成笔，即任务书 §2 要求的「M12-E 最后一笔」 | `git log --format=%s -1` |
| 开工时工作树 | 只有任务书 `docs/v0.1/M12F Coding.md` 一项未跟踪，无其他改动 | `git status` |
| 工具链 | `rustc 1.96.1 (31fca3adb 2026-06-26)`、`cargo 1.96.1`、`Apple clang 17.0.0`、`Darwin 24.6.0 arm64`；仓库 `[toolchain] channel = "1.96.1"` | manifest `toolchain` 段 |
| 构建环境 | `CC=clang CXX=clang++ CXXFLAGS="-include cstdint"`（沿用仓库已验证配方，缺失会让 clippy 死在 rocksdb） | manifest `gates.env` |

**代码路径审计（以实际代码为准，不靠清单）。** 生产里能「建出一条会花钱的 lane」的入口，本轮之前有 3 个；本轮之后每一个上方都有一次台账读取，配对由 `crates/pipeline/tests/ledger_recovery.rs:every_production_entry_that_builds_a_lane_reads_a_ledger_above_it` 从源码扫描得出并打印：

| 生产入口 | lane 构造行 | 同函数内台账读取行 | 说明 |
| --- | --- | --- | --- |
| `crates/pipeline/src/runner.rs` | `:1350 ExecutionStage::connect(...)` | `:1347 ExecutionJournal::open(...)` | 完整 runner |
| `crates/pipeline/src/arbitrage.rs` | `:782 SequenceStage::connect_with_trace(...)` | `:401 ExecutionJournal::open(...)` | M12-D 审计过的 `arbitrage::run_once` |
| `crates/cli/src/lib.rs` | `:779 ExecutionStage::connect(...)` | `:776 ExecutionJournal::open(...)` | M12-D 审计过的 `cli::run_validation` |

扫描自身带正对照（三处构造名必须被扫到，扫不到就红）与负对照（`the_scan_reports_a_lane_built_without_a_ledger_read`），所以「扫到 0 处」不会伪装成通过。另有两条整表规则：`no_production_source_holds_a_memory_only_journal`（生产源码里不允许出现「只有内存、没有文件」的 journal 构造）和 `every_lane_constructor_takes_the_journal_as_a_required_argument`（四个 lane 构造器 `ExecutionStage::new/connect`、`SequenceStage::new/connect_with_trace` 的签名里 journal 是必填参数，不是可选项）。

## 3. 现有执行记录丢失的具体路径（§14 第 2 项）

审计结论写在新模块的头注释里（`crates/execution/src/journal.rs:1-47`），三条路径都指得到具体结构：

1. **lane 与 nonce 占用是 connect 时新建的内存对象。** `ExecutionLane { nonce: NonceAllocator }`（`crates/execution/src/lifecycle.rs:736`）由 `ExecutionStage::new` / `SequenceStage::new` 现场构造；M12-E 的「未知答案占住 nonce」规则（`lane.resolve_submission` → `LaneRelease::Held`）只活在这一个实例里。进程退出 ⇒ 占用消失，重启后 `NonceAllocator` 从节点的 `eth_getTransactionCount` 起算，看不出上一世占过哪个号。
2. **本地交易哈希与 `Unknown` 追踪只在内存。** M12-E 把哈希留在 `SubmissionOutcome` / 执行记录里，没有任何落盘点；任务书 §8 F2 正是这条窗口的最坏形状：请求已被节点完整接收、响应丢失、进程随后退出。
3. **执行记录本身没有跨进程的写入边界。** `ExecutionStage` / `SequenceStage` 的 `execution_id`、`opportunity_id`、pinned block 等都是本轮新建的事实行才第一次出现在磁盘上；这轮之前，重启的进程无法回答「这个 execution_id 上次走到哪」。

M12-E 报告第 1 节已把同一件事记为「仍然没解决的事」，并在 §8 里禁止那一轮去建台账；本轮就是补那一格。

## 4. 最终存储方案及选型理由（§14 第 3 项）

**方案：追加式 NDJSON 台账，一链一文件。**

- **位置与命名**：`data/ledger/execution-journal-<chain_id>.jsonl`；目录优先级是 CLI flag → `GIWA_EXECUTION_LEDGER_DIR` → `DEFAULT_LEDGER_DIR = "data/ledger"`（`journal.rs:69-94`，`crates/cli/src/lib.rs:975`）。刻意与 `--evidence-dir` 分开：台账是运行态，证据是交付物。
- **为什么不用数据库/外部服务**：任务书 §3.2 末条禁止引入新外部基础设施（除非先交方案待批）。本轮不需要查询能力，只需要「进程退出后字节还在、以及一行坏掉能被认出来」。
- **为什么一链一文件**：§6 问「能不能读到别的链的记录」，最诚实的答案是把答案做成结构 —— 文件里出现别的 `chain_id` 直接是一个故障词 `foreign_chain`，而不是「另一堆需要小心区分开的行」。
- **为什么追加而非原地更新**：唯一写入是 `O_APPEND` 追加一行；没有任何代码路径重写或删除已有行。这样「一条记录被改过」必然表现为校验和不匹配，而不是悄悄变成新事实。
- **恢复只读**：`ExecutionJournal::open` / `reload` 不追加（新建文件的开场行是唯一例外），所以 F7 的「同一本账连续启动两次」天然幂等，而不是靠一次补丁。

## 5. 数据模型和 schema 版本（§14 第 4 项）

**版本闸**：`pub const JOURNAL_SCHEMA_VERSION: u64 = 1`（`journal.rs:64`）。一个数字而不是字符串，v2 文件与 v1 读者只会在一个字段上不一致；读到不等于 1 就返回 `unsupported_schema`，**不迁移、不重新解释**。开场行 `journal_opened` 把版本与链写进文件，所以「这份文件比这个构建更早」是磁盘上的事实，不是从时间戳猜的。

**每行的字段**（`JournalEntry::to_line`，`journal.rs:481-560`）：`schema_version`、`seq`、`written_at_ms`、`fact`、`basis`、`chain_id`、`execution_id`、`idempotency_key`、`opportunity_id`、`sender`、`target`、`nonce`、`transaction_hash`、`pinned_block`、`pinned_block_hash`、`endpoint_class`、`position`、`status`、`detail`、`checksum`。时间戳用 Unix 毫秒墙钟（`journal_stamp`），不用 M8.1 的单调时钟 —— 单调时钟跨进程不可比，而本文件的全部意义就是跨进程比较。

**事实词表 `JournalFact`（9 个，`journal.rs:113-143`）**：`journal_opened`（文件开场行）、`ready_not_sent`、`send_intent_persisted`、`send_dispatched`、`endpoint_accepted`、`definite_refusal`、`outcome_unknown`、`receipt_observed`、`attention_required`。任务书 §4.2 要求恢复能区分 8 类事实，这里是 9 类（多出的一行属于文件本身）。节点 ack 与回执是两行，因为「被接受」不等于「已进块」（§2.3）；缺回执不等于缺交易。

**依据 `FactBasis`（3 个，`journal.rs:220-231`）**：`observed_local`（本进程做过的/拒绝做的）、`observed_node_answer`（节点说的话，引用而非解释）、`unconfirmed_inference`（从「没有答案」推出的结论）。§4.2 的「本地事实与推断分离」是**字段级**的：`endpoint_accepted` 的依据只能是 `observed_node_answer`，而任何由缺失推出来的状态必须走 `unconfirmed_inference`。

**恢复态 `RecoveredState`（3 个，`journal.rs:883-912`）**：`resolved`、`possibly_in_flight`、`awaiting_receipt`。它**从不写进文件**，只由事实序列折出来；`holds_lane()` 的定义是 `!matches!(self, Resolved)`，即两个未决态都占住 nonce。

**故意不写的东西（§5）**：不写签名交易原始字节（一行带原始字节 = 一张磁盘上的 bearer instrument）；不写端点 URL 或任何凭据（行里只有 `endpoint_class` 这个词，`detail` 字符串进入本模块前已按 `giwa` 清洗发送原因的方式洗过）；不写私钥/助记词。身份是真实的本地交易哈希（`transaction_hash` 为 0x + 64 个十六进制字符）。证据门的泄漏扫描规则会检查每张表的连接文本，并对这些禁令做「规则能开火」的自检（`the_leak_and_claim_rules_can_fire`）。

## 6. 原子写入、同步和损坏处理策略（§14 第 5 项）

**一次 append 的顺序（`journal.rs:1341-1420`）**：句内去重 → 计算 `seq` 与校验和 → **先把新行折进已有记录并检查冲突**（`merge` + `lane_occupancy`）→ 通过才 `write_all` → `flush` → `sync_all` → 才推进内存状态（`next_seq`、`appended`、`written`、`recovery.records`）。冲突检查放在字节出门之前，是为了 §9 的「失败的更新不得让内存与磁盘分叉」：被台账自己的不变量拒绝的那条记录，既不落盘也不改内存句柄状态。

**只有 fsync 的返回值算「这个事实已持久」。** 覆盖范围一句话讲清（§6 要求）：进程崩溃一定覆盖；在 `fsync` 真正到达介质的文件系统上，操作系统崩溃也覆盖；**不覆盖**磁盘缓存提前应答的硬件掉电，本模块任何地方都不宣称覆盖掉电。三种范围内可检测的是「尾巴断了」—— 最后一行没有换行符，或校验和与字节不符 —— 答案是拒绝，不是截断修复。

**校验和覆盖的是手工构造的 preimage**（`journal.rs:479-540`），每段带长度前缀并用 `|` 连接，不是对 JSON 对象求哈希：库可以改键序，一个键序可变的 map 不是任何校验和的对象。自由文本留在长度前缀段里，分隔符无法伪造边界。

**启动检测：`JournalFault` 20 个词（`journal.rs:595-726`），每个都带行号或路径**。`missing_genesis`、`emptied_externally`、`torn_tail`、`not_json`、`empty_line`、`missing_field`、`bad_field`、`unknown_fact`、`mismatched_basis`、`checksum_mismatch`、`unsupported_schema`、`foreign_chain`、`duplicate_sequence`、`sequence_out_of_order`、`conflicting_identity`、`conflicting_transaction_hash`、`two_unresolved_lanes`、`truncated_externally`、`replaced_externally`、`io`。

**fail-closed 的形状**：检测到损坏 ⇒ 拒绝打开（或拒绝追加），**不清空、不修复、不跳过最后一条解析失败的行、不建空台账继续跑**。句柄还额外记住打开时的 inode 与字节数，文件被外部替换（`replaced_externally`）或被截短（`truncated_externally`）都在下一次追加前被抓到。错误分两支：写不出去是 `ExecutionError::LedgerPersistence`，读回来不可信是 `ExecutionError::LedgerRecovery`（`crates/execution/src/error.rs:116-130`）—— 写侧坏了与记录坏了是两种诊断，调用方据此停止。

## 7. 启动恢复的实际调用路径（§14 第 6 项）

恢复发生在「stage 还不可能存在」的位置，这是 §7 的顺序要求：

```
ExecutionJournal::open(dir, chain_id, stamp)      // 读整个文件、逐行校验、折成记录、算出 restored_lane
        ↓ 失败即 map_err(PipelineError::Execution)  // 未读成功的台账 ⇒ 这条入口后面的一切都不可达
ExecutionStage::connect(.., journal)  /  SequenceStage::connect_with_trace(.., journal, ..)
        ↓
ExecutionStage::new / SequenceStage::new
        for (address, nonce, execution_id) in journal.restored_lane()   // stage.rs:415 / sequence.rs:1446
            ⇒ 把这些 nonce 预先压回分配器
```

三个生产入口的配对行号见第 2 节的表。`run_once` 与 `run_validation` 的读取点都在函数顶部、在任何 simulation 读、构造 plan 或发送之前；`ledger_recovery.rs` 里还有一格是从入口侧真的跑一遍：给一个由真实句柄写过、再被人为损坏（torn / edited / 双活 lane / 新版 schema）的台账文件，`arbitrage::run_once` 以 `PipelineError::Execution` 拒绝、故障词出现在错误文本里、桩 socket 一次都没被请求、文件字节保持原样。

## 8. nonce lane 恢复与并发保护（§14 第 7 项）

**恢复。** `restored_lane()` 返回未决记录占住的 `(address, nonce, execution_id)` 三元组，构造器逐条回放给分配器，所以重启的进程在第一次分配 nonce 之前就已经「欠着」上一世的那格。占住与释放只由 `holds_lane()` 决定：`possibly_in_flight` 与 `awaiting_receipt` 占住，`resolved` 释放 —— 而 `resolved` 只能由「读到了回执」或「读到了确定性拒绝」这两次**读**达成。

**并发保护（不重写 M11 调度算法）。** 台账在写之前做一次 fold：若合并后存在两条未决记录，直接返回 `two_unresolved_lanes` 并拒绝这条 append（`journal.rs:1352-1359`），所以单进程内不可能出现「两条 lane 同时未决」的文件状态；跨进程的两个句柄在同一文件上会在 `seq` 上相撞并被 `duplicate_sequence` 抓住（F6 的 `two_handles_on_one_file_collide_at_the_sequence_number_a_reader_checks` 实测）。§9 的「durable-before-remembered」由 `lane_held_unwritten(&ExecutionError) -> LaneRelease`（`lifecycle.rs:755`）实现：落盘失败的那次运行，报告里的 lane 词从 `Released` 改写为 `Held`，因为文件里已有的行还在替它占着这格 nonce —— 共 4 处调用点（`stage.rs:934`、`stage.rs:1024`、`sequence.rs` 两处）。

**明确不做。** 不自动重发、不用替代交易绕过被占住的 nonce（恢复出的 hold 会拒绝一次被替换的尝试并写一行 `attention_required`，即 F9 的 `a_recovered_hold_refuses_a_substituted_attempt_and_writes_the_need_for_a_human_once`），不做后台对账。恢复的全部产出是「占住这格 + 回答提问」，等人来关。

## 9. F1–F9 的实际测试结果（§14 第 8 项）

测量全部来自 `crates/execution/tests/crash_recovery.rs`（13 个用例，真实文件 + 自己数到达次数的桩提交器），在 `M12F_EVIDENCE_DIR` 指定目录时逐行写出 `measured-rows.jsonl`；发布表由 `crates/execution/tests/ledger_evidence.rs` 从这些行重新装配。

| 格 | 用例（真实测试名） | 实测结果 | 行数 |
| --- | --- | --- | --- |
| F1 | `a_crash_between_the_intent_line_and_the_socket_leaves_the_nonce_possibly_in_flight` | 意图行落在第 2 行、`dispatch_seq = null`（文件里确实没有派发行）；恢复 `possibly_in_flight`、`never_recovered_as = resolved`；`nonce_held = 0`、恢复条目 1、重启后重提交 0 次、需人工关注行 1 | 3 |
| F2 | `an_unknown_answer_survives_the_restart_as_unknown_and_never_as_a_refusal` | 桩读走全部字节后丢响应；三行按 `send_intent_persisted → send_dispatched → outcome_unknown` 的顺序落在文件里（`intent_precedes_dispatch = true`，共 4 行，拒绝行 0）；发送到达 1 次；重启后 `awaiting_receipt`、lane held、重提交 0 次，且文件里从未写过 `definite_refusal` | 3 |
| F3 | `a_dispatch_line_with_no_answer_after_it_is_recovered_as_still_awaiting` | 派发行在第 3 行、答案行不存在 ⇒ `awaiting_receipt`；不写成「确定未发送」；`hashes_agree = true`（本地算的哈希 == 文件记的哈希） | 2 |
| F4 | `a_node_acknowledgement_is_recovered_as_an_acknowledgement_and_never_as_inclusion` | 节点 ack 哈希一致；恢复为 ack，**不**报告进块或成功；追踪所需信息（哈希/nonce/身份）保留 | 3 |
| F5 | `a_receipt_read_in_memory_that_never_reached_the_file_leaves_the_record_awaiting`；`a_ledger_replaced_under_a_run_refuses_the_next_line_instead_of_forking_memory_against_it` | 内存看过回执但没落盘 ⇒ 记录回到等待、身份不丢、不视为未发送；文件被换 ⇒ 下一次 append 拒绝 | 6 |
| F6 | `a_damaged_ledger_refuses_to_open_and_the_run_never_reaches_the_endpoint`；`two_handles_on_one_file_collide_at_the_sequence_number_a_reader_checks` | 14 行拒绝、13 种损坏；open 处 12 次、append 处 2 次；拒绝之后调用 0 次、发送 0 次、字节未改 | 11 |
| F7 | `a_restarted_process_reads_the_same_record_twice_and_refuses_a_finished_attempt_as_duplicate` | 同一本账读两遍答案相同；已完结的尝试再写被判重复；恢复过程不签名、不发送 | 3 |
| F8 | `a_ledger_emptied_before_the_intent_line_stops_the_send_and_the_next_process_refuses_it`；`a_ledger_path_that_cannot_be_created_stops_the_run_before_anything_is_read` | 不可写/被清空 ⇒ 发送到达 **0** 次（行里 `send_arrivals = 0`、`sends_after_refusal = 0`、`calls_after_refusal = 0`）、不进成功提交态、不降级为内存模式、错误是 `LedgerPersistence` 且调用方可识别 | 4 |
| F9 | `an_accepted_transaction_whose_receipt_never_arrives_keeps_its_nonce_across_a_restart`；`a_recovered_hold_refuses_a_substituted_attempt_and_writes_the_need_for_a_human_once` | `receipt == null` 不释放 lane（`holds_lane = true`、`refusal_lines_in_file = 0`）；查不到 ≠ 未被接受；查询失败不转 `Rejected`；被替换的尝试 `substitute_send_arrivals = 0`、第三个进程 `third_process_send_arrivals = 0`、文件里意图行仍只有 1 行；跨 2 次重启后仍未决并写一行 `attention_required` | 5 |

按表的行数分布：`recovery_results` 9、`persistence_boundary` 9、`damage_controls` 14、`lane_recovery` 8，合计 40 行、13 个用例。跨表的关键上界（都是行里实测，未测到的一律 `null` 而不是 0）：任一恢复读到的最多台账行数 5、任一恢复里的记录数最大 1、一条记录活过的重启次数最大 2、任何用例的发送到达最多 **1** 次（9 行合计 5 次）、重启后重提交最多 **0** 次、恢复出的 lane 条目最多 1 条、拒绝之后的调用与发送均为 0。lane 表 8 行里 `held` 5、`idle` 1、另 2 行不报这个字段（用例没有读它，宁可缺字段也不编一个词）。

## 10. 所有质量门禁的完整统计（§14 第 9 项）

串行四道，构建环境同上。manifest 的 `gates` 段每个数字都由 `/tmp/m12f_manifest.py` 从日志解析整份文件得出（不是只看退出码），日志本身在 `/tmp/m12f_gate*.log`。

| 门 | 命令 | rc | 日志/target 数 | passed/failed/ignored | warnings | 耗时 | 超时 | 是否改历史证据 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 fmt | `cargo fmt --all -- --check` | 0 | 输出 0 行 | — | — | 3s | 否 | 否 |
| 2a 预热清理 | `cargo clean -p <16 个 workspace 包>` | 0 | 16 包全部 rc=0 | — | — | 16s（02:31:15Z → 02:31:31Z） | 否 | 否 |
| 2b clippy（冷） | `cargo clippy --workspace --all-targets -- -D warnings` | **101** | 2 行 error | — | 1 条 lint | 31s | 否 | 否 |
| 2c clippy（修复后重跑） | 同上 | 0 | 重新分析 3 个 unit | — | **0** | 18s（自报 17.33s） | 否 | 否 |
| 3 仓库既有证据/门禁 | 7 条 `cargo test -p … --test …`（清单见下） | 全 0 | 22 个 target | 239 / 0 / 1 | 0 | 84s | 否 | **是**，见 10.3 |
| 4 workspace 串行 | `cargo test --workspace --no-fail-fast -- --test-threads=1` | 0 | 138 个 target 全部有结果行（105 integration + 17 unittest + 16 doctest），0 个 target 无结果行 | **1824 / 0 / 29** | 0 | 247s | 0 次 | **是**，见 10.3 |

**门 3 的七条命令（逐条 rc 全 0）**：`evm-chain --test readiness_gate`（41s）、`evm-cli --test validation_gate`（1s）、`evm-discovery --test evidence_gate --test pathfinder_evidence_gate --test reconstruction_evidence_gate`（17s）、`evm-execution --test block_context_evidence --test executor_evidence_gate --test ledger_evidence --test multihop_evidence_gate --test send_uncertainty_evidence`（3s）、`evm-live --test preconf_evidence_gate`（2s）、`evm-pipeline` 的 8 个证据目标（18s）、`evm-simulation` 的 3 个证据目标（2s）。

**10.1 clippy 冷跑为什么先红。** 2b 的 101 是新代码里的一个真实 lint：`crates/pipeline/tests/ledger_recovery.rs` 里损坏表的内联类型触发 `type_complexity`。修法是把那个类型命名（`type Damage = (&'static str, fn(&str) -> String, &'static str);`），**不是加 `#[allow]`**；随后重跑 2c，0 warning。这一段按实际发生记录，不写成「一次通过」。

**10.2 为什么 warm clippy 不算数。** clippy 对缓存过的 crate 不重新分析任何东西，所以门 2 前逐个 `cargo clean -p` 清了 16 个包 —— 这是 M12-E 起的既有做法，本轮沿用。

**10.3 门禁跑动过程中被测试改写的历史证据（两处，都处理干净）。**
- `data/evidence/m10/manifest.json`：`crates/execution/tests/executor_evidence_gate.rs` 在任何一次 workspace 测试运行时都会重印它的 `git_commit` 字段（本次 diff 4 行）。处理方式按仓库既有惯例：**`git checkout --` 还原为已提交字节**，行为本身作为「记录未修」条目写进 manifest 的 `known_staleness`，不改 M10 的门。提交前后各查一次：`git diff -- data/evidence/m10/manifest.json` 为空。
- 11 张 M8 表格：本轮在 `runner.rs` / `arbitrage.rs` / `cli/lib.rs` 等文件插入行，使锚点行号移动。刷新前逐叶核对：193 个变化叶，键全部落在行位置词表（`line`、`source_line`、`decision_line`、`asked_at`、`first_decision`、`consumer_source_line`、`consumer_decision_line`、`producer_source_line`）内，没有任何语义字段被改。**逐项说明在 manifest 的 `cross_milestone_anchor_refresh.per_file`**（每文件的变化叶数 + 前/后样例）。

**10.4 本轮没有为了让门通过而做的事。** 未删除任何测试，未扩大 `#[ignore]`（新四个目标里 ignore 计数为 0，`scope.ignored_added_by_this_round = 0`），未降低任何门禁阈值，未手改历史证据掩盖回归。`--no-fail-fast` 与 `--test-threads=1` 都是仓库既有规则（后者因为 `target/pipeline-tests/` 装配 scratch 跨进程共享）。

## 11. 证据门及负控制结果（§14 第 10 项）

**目录**：`data/evidence/m12/f/` = `measured-rows.jsonl`（40 行原始测量）+ `recovery-results.json`（9 行）+ `persistence-boundary.json`（9 行）+ `damage-controls.json`（14 行）+ `lane-recovery.json`（8 行）+ `manifest.json`（19 个顶层键）。

**门的形状**：`crates/execution/tests/ledger_evidence.rs` 18 个测试，**自己不测量** —— 它读回 crash 目标写的行，把每张发布表按同一套装配函数从行重新生成，逐字节比对提交的文件；因此一行与代码不一致、或一张表被手改偏离它的行，都是红的。词表（9 个事实词、3 个恢复态词与 `holds_lane`、20 个故障词）从生产源码解析，不复制粘贴；解析器带非空断言（词表为空即红）与去重断言（同一个词不能代表两个变体）。锚点用 `文件:行 + token` 定位并断言唯一命中，arity 检查把「这一格指的是一个位置」本身当成被验证的主张。

**七条负控制（§10 要求检查器必须失败；全部 `ok`）**：

| 编号 | 种下的假 | 被抓 |
| --- | --- | --- |
| NC1 | 把恢复出的 `Unknown` 改写成已闭合记录 | `nc1_an_unknown_recovered_as_a_closed_record_is_caught` |
| NC2 | 把占用的 lane 改写成空闲（两处独立规则） | `nc2_a_lane_rewritten_as_idle_is_caught_twice` |
| NC3 | 一次尝试记成两次提交 | `nc3_two_submissions_for_one_attempt_is_caught` |
| NC4 | 哈希不一致伪装成一致 | `nc4_a_hash_disagreement_is_caught` |
| NC5 | 损坏台账写成「已成功打开」 | `nc5_a_damaged_ledger_rewritten_as_opened_is_caught` |
| NC6 | 发送跑在意图行之前 | `nc6_a_send_that_outran_the_intent_line_is_caught` |
| NC7 | 不支持的 schema 伪装成可读 | `nc7_an_unsupported_schema_disguised_as_readable_is_caught` |

另有三条元规则：`the_leak_and_claim_rules_can_fire`（泄漏与越权主张的词表规则本身能被触发，防止「规则写了但永不开火」）、`a_second_copy_of_the_measurement_is_not_a_measurement`（同一次测量抄两份不算两次）、`a_second_pass_that_answers_differently_is_caught`（两遍装配答案不同即红）。**检查器不读 manifest**（`evidence.gate_reads_the_manifest = false`），所以结论不可能是自己喂自己的；未测到的最大值写 `null`（`an_unmeasured_maximum_stays_null`）。

`manifest.json` 每个数字都读自某个日志、某张已提交表、工作树 diff 或某个源文件；它不记录自己的 sha256；`git_commit` 记的是**基线** `d4d047f…`（读取方式逐项写在 `git_commit_reading`）。

## 12. 变更文件和四笔提交（§14 第 12 项）

**第 1 笔 `44526a2` — feat(m12-f)**，12 个文件，3,682 增 / 50 删：
`crates/execution/src/journal.rs`（新，2,738 行，含 25 个单元测试）、`error.rs`、`lib.rs`、`stage.rs`、`sequence.rs`、`lifecycle.rs`、`nonce.rs`、`deploy.rs`、`crates/pipeline/src/{arbitrage.rs,config.rs,runner.rs}`、`crates/cli/src/lib.rs`。

**第 2 笔 `4140bfa` — test(m12-f)**，12 个文件，5,479 增 / 42 删：
新目标 `crates/execution/tests/crash_recovery.rs`（13 测试）、`crates/execution/tests/ledger_evidence.rs`（18 测试）、`crates/pipeline/tests/ledger_recovery.rs`（7 测试）；连带字段/构造签名更新的既有测试 9 个：`crates/cli/tests/validation_gate.rs`、`crates/execution/tests/{executor_deploy,executor_giwa_live,executor_lifecycle,sequence,stage_matrix,state_lifetime_recovery}.rs`、`crates/pipeline/tests/readiness_isolation.rs`、`crates/simulation/tests/historical_gate.rs`。

**第 3 笔 `64a0956` — evidence(m12-f)**，17 个文件：
`data/evidence/m12/f/` 六个新文件（2,304 行）+ 11 张 M8 表格的锚点行号刷新（193 行改，全部为行位置）。

**第 4 笔 — docs(m12-f)**：本报告 + 任务书 `docs/v0.1/M12F Coding.md`。这一笔就是承载本文件的那一笔，因此不在这里写它自己的 hash；前三笔的 hash 如上，本文件写在它们之后提交。

一处口径需要在报告里说明（不改已提交字节）：manifest 的 `scope.production_added` 记 **3,683**，比 `git show --numstat` 的 **3,682** 多 1 行；差在 `journal.rs` 的行数统计口径 —— builder 对未跟踪新文件按行列表计数（把结尾换行算作一行），提交后 numstat 按实际增加行数计。两处都在描述同一件事，取哪一个都可复核。

## 13. 未解决问题及明确不支持的恢复场景（§14 第 13 项）

**本轮明确不支持（按构造就不做，不是没测）**：
1. **掉电级持久性**。`sync_all` 之后的承诺止于「本进程不再有能力撤销这次写」；磁盘缓存提前应答的硬件掉电不在保证范围内，头注释与 manifest `not_claimed` 都这么写。
2. **跨机/跨目录的台账一致性**。一链一文件、单机本地目录；没有复制、没有故障转移、没有多节点视图（§3.2 明确 OUT_OF_SCOPE）。
3. **自动重发、替代交易、后台对账**。恢复出的未决记录只占 lane 并写一行 `attention_required`，等人关。
4. **台账清理/轮转**。文件只追加，本轮没有压缩、归档、按块高截断的路径 —— 长期运行的体积是个已知待办。
5. **锁**。两个句柄同时写同一本账靠 `seq` 冲突被检出（F6 实测），不是靠文件锁预防；这不是一个「安全并发写」的实现，而是一个「不安全并发写会被发现」的实现。

**词表覆盖的诚实边界**：`JournalFault` 20 个词里，崩溃矩阵（40 行） drove 13 个词至少一次；另外 7 个词中，`two_unresolved_lanes` 与 `conflicting_transaction_hash` 由 `journal.rs` 自己的单元测试驱动（`two_unresolved_executions_in_one_file_are_a_fault_and_not_a_choice`、`a_live_handle_refuses_a_second_unresolved_execution_before_writing_it`、`two_different_hashes_for_one_step_of_one_execution_are_a_conflict`）；剩下 `not_json`、`missing_field`、`bad_field`、`sequence_out_of_order`、`conflicting_identity` 五个词**本轮没有任何测试把它们驱动过** —— 它们有抛出点、有诊断文案，但没有一行证据。`damage-controls.json` 的 `fault_words_observed` 把这五个记为 0，读表的人能直接看见这个空白。补这五格需要在 F6 用例里再加三种手工行形状，属于后续工作。

**仍然开放的环境事实**（与本轮无关，但影响真实运行）：`data/ledger/` 未在 `.gitignore` 里。本轮没有任何实跑，所以工作树里不存在这个目录，也没有产物被提交；一旦真跑，运行态台账文件会以未跟踪项出现在 `git status` 里，需要按仓库对 `fixtures/live-m5/corpus/` 的既有做法处理。本轮未擅自改 `.gitignore`（§3.2：不扩大改动面）。

**没有解决的原始问题（一句话）**：节点侧的真实状态仍未验证 —— 台账能证明「我们知道曾经发过」，证明不了「链上发生了什么」。下一格是把恢复后的未决记录与一次真实（且仍受 M12-E 单次发送策略约束的）receipt 查询对上，这需要真实广播窗口，须用户点名授权。

## 14. 与真实节点部署之间仍然存在的差距（§14 第 14 项）

| 差距 | 本轮状态 | 要闭合需要什么 |
| --- | --- | --- |
| 节点本身 | `SELF_HOSTED_NODE = NOT_RUN`。方案文档 M12-C 已有单节点部署与验证计划，本轮一行未执行 | 按 M12-C 部署，并跑它的验收清单 |
| 真实 RPC | `LOCAL_CANONICAL_RPC = NOT_VERIFIED`、`LOCAL_FLASHBLOCKS = NOT_VERIFIED`。本轮所有 HTTP 交互是测试进程自己在 `127.0.0.1:0` 上起的桩 | 一个可信端点 + 真实 `eth_sendRawTransaction` / `eth_getTransactionReceipt` 往返 |
| 签名与广播 | 未使用真实私钥、未签名、未广播（`REAL_PROFITABLE_ARBITRAGE = UNKNOWN`） | 花钱窗口，须用户点名授权 |
| 节点对重复 nonce 的真实行为 | **未验证，本轮不作断言**。桩证明了「客户端这侧只发一次、恢复后不重发」；节点在 pending 池里怎么处理同 nonce 的两笔，仍是 M12-A/M12-B 记录过的外部假设 | 真实节点上的受控实验 |
| 崩溃窗口的真实成因 | 测试用「在指定事实之后截断文件」与「句柄内注入 io 故障」模拟；真实 SIGKILL、断电、容器被驱逐的时序分布没有测量 | 生产环境的进程监管与恢复演练 |
| 台账与节点视图的对账 | 明确不做（§3.2 禁止完整自动对账）。当前只有 `attention_required` 这一行人写的入口 | 一份人工核对程序 + 后续里程碑的只读对账 |

## 15. §15 完成标准逐条判定（15 条）

| # | 判据 | 判定 | 证据在哪 |
| --- | --- | --- | --- |
| 1 | 已确认 M12-E 基线及关键实现 | ✅ | 第 2 节表：`HEAD = origin/main = d4d047f`、`0 0` |
| 2 | 发送前必须成功持久化 | ✅ | `journal.rs:1341` 的 append 顺序 + 第 6 节；三个入口的配对行号在第 2 节 |
| 3 | 持久化失败时实际网络发送次数为 0 | ✅ | F8 两格实测到达 0 次（`persistence-boundary` / `damage-controls` 行）|
| 4 | `Unknown` 交易重启后仍可识别 | ✅ | F2：`awaiting_receipt` + 本地哈希保留（第 9 节表） |
| 5 | 本地哈希、nonce、lane 占用正确恢复 | ✅ | F2/F9 的 `restored_lane` 回放；`lane-recovery` 表 8 行 |
| 6 | 进程崩溃窗口不会自动重广播 | ✅ | 跨表上界 `max_resubmissions_after_restart = 0` |
| 7 | `receipt == null` 不直接释放未知交易的 lane | ✅ | F9 + `holds_lane()` 的词表门 + NC2 |
| 8 | 台账损坏或 schema 不兼容时 fail-closed | ✅ | F6 的 14 行拒绝、NC5、NC7 |
| 9 | 重复恢复幂等 | ✅ | F7 + 「恢复只读」构造性保证 |
| 10 | F1–F9 均有真实离线测试证据 | ✅ | 第 9 节表（40 行 / 13 用例，全部真实文件 + 桩到达计数） |
| 11 | 证据门及负控制通过 | ✅ | 第 11 节：18 个门测试 + NC1–NC7 全 ok |
| 12 | M10/M11 及完整串行质量门禁通过 | ✅ | 第 10 节四道门 + 第 16 节回归 310/0/6 |
| 13 | M10 历史证据未被污染 | ✅ | `git diff -- data/evidence/m10/manifest.json` 为空；行为记录在 `known_staleness` |
| 14 | 四笔提交已推送，工作区干净 | **条件式**（本报告所在的一笔即是第四笔；推送发生在本文件写完之后，所以此处不预打 ✅） | 前三笔 hash 在第 12 节；第四笔 hash 与 `git status` / `git log origin/main..HEAD` 的实测结果只能由推送后的终端输出证明，第 18 节末尾给了复核命令 |
| 15 | 报告明确说明未部署节点、未连真实 RPC、未签名、未广播 | ✅ | 第 1 节末段、第 14 节表、manifest `standing_verdicts` |

**M12_F = COMPLETE**，范围限定为「本地台账的持久化与重启恢复」。这一句不包含任何关于节点上交易状态的结论。

十五条判据里前十三项与第十五项由已落盘的代码、测试输出和证据表支撑；第十四项（四笔提交与推送）只能在本报告这笔提交之后才成立，因此写作条件式而非 ✅。

## 16. M10 / M11 回归（§14 第 11 项）

从门 4 的日志按 target 头部逐段解析（`manifest.m10_m11_regression` 存了 21 条）：M10 执行侧 10 个目标 + M11 多跳侧 11 个目标，合计 **310 passed / 0 failed / 6 ignored**。其中被本轮签名改动直接牵动的四张表：`executor_deploy` 12/0/0、`sequence` 44/0/0、`stage_matrix` 17/0/0、`executor_lifecycle` 9/0/0；M11 的调度算法与 lane 状态机未被改写（`multihop_lanes` 35/0/0、`multihop_risk` 40/0/0、`multihop_negative_controls` 13/0/0）。6 个 ignored 都是既有需要真实端点或真实历史的目标，本轮未新增、未移除任何一个。

## 17. 范围外事项自查（任务书 §3.2 的十三条，逐条）

| 禁令 | 自查结果 | 取证 |
| --- | --- | --- |
| 不部署 GIWA 节点 | 未部署 | `standing_verdicts.node_deployed = false` |
| 不连真实 GIWA / L1 RPC / Beacon | 未连接 | 唯一 socket 是测试进程内的 `127.0.0.1:0` 桩；`real_rpc_connected = false` |
| 不发起真实签名或广播 | 未发起 | `transaction_signed_for_broadcast = false`、`transaction_broadcast = false` |
| 不自动重发 `Unknown` | 未实现 | 无重发路径；`max_resubmissions_after_restart = 0` |
| 不通过释放 lane 绕过未知交易 | 未绕过 | `holds_lane()`；NC1/NC2；F9 的替换尝试被拒 |
| 不新增多节点/HA/故障转移 | 未新增 | `multi_node_ha = OUT_OF_SCOPE` |
| 不实现完整自动对账或后台重试 | 未实现 | 恢复的产出只有「占住 + 回答」；第 13 节第 3 项 |
| 不修改 M10/M11 业务设计来规避恢复问题 | 未修改 | 第 16 节回归 0 failed；M11 调度算法 diff 不涉及 |
| 不接入 Flashblocks Early Radar | 未接入 | 本轮 diff 不含 `preconf`/radar 文件 |
| 不修改 M12-E 的发送重试策略 | 未修改 | 台账是发送的前置，不改单次发送规则；`send_uncertainty_evidence` 门仍 rc=0 |
| 不引入数据库或新外部基础设施 | 未引入 | `journal.rs` 只用 `std::{fs, io, path, collections}`、工作区里已有的 `serde_json`、`alloy_primitives::keccak256` 与既有 `evm_metrics::unix_ms`；本轮没有任何 `Cargo.toml` 被改动 |
| （§11）无浮点金融计算 / 热路径不加 RPC / 不引 LLM / 仅内存非生产降级 | 全部满足 | 台账不含算术；append 是本地写；生产里不存在无文件的 journal |

## 18. 复核命令（只读，可在任何工作树执行）

```bash
# 0) 构建环境（缺它 clippy 会死在 rocksdb）
export CC=clang CXX=clang++ CXXFLAGS="-include cstdint"

# 1) 四道串行门禁（门 2 前必须清这 16 个包，否则 warm run 不重新分析任何东西）
cargo fmt --all -- --check
for p in <16 workspace 包>; do cargo clean -p "$p"; done
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast -- --test-threads=1

# 2) 本轮的三个新目标（串行；测量必须在同一把里跑）
cargo test -p evm-execution --test crash_recovery -- --test-threads=1
cargo test -p evm-execution --test ledger_evidence -- --test-threads=1
cargo test -p evm-pipeline --test ledger_recovery -- --test-threads=1

# 3) 重新装配证据表（会重写 data/evidence/m12/f/，比对即验证「表 == 行」）
M12F_EVIDENCE_DIR=data/evidence/m12/f \
  cargo test -p evm-execution --test crash_recovery -- --test-threads=1
M12F_LEDGER_REFRESH=1 \
  cargo test -p evm-execution --test ledger_evidence -- --test-threads=1

# 4) 发送到达次数与恢复状态的实测分布（整表读取，不用词面 grep）
python3 - <<'PY'
import json
rows=[json.loads(l) for l in open('data/evidence/m12/f/measured-rows.jsonl')]
from collections import Counter
print(Counter(r['section'] for r in rows))
print(Counter(r.get('recovered_state') for r in rows if 'recovered_state' in r))
PY

# 5) 三个生产入口的 ledger/lane 配对（现量，勿引用本报告行号）
grep -n "ExecutionJournal::open\|ExecutionStage::connect\|SequenceStage::connect_with_trace" \
  crates/pipeline/src/runner.rs crates/pipeline/src/arbitrage.rs crates/cli/src/lib.rs

# 6) 历史证据是否被本轮无关修改
git diff --stat -- data/evidence/m10/        # 必须为空
git show --numstat 64a0956 | grep data/evidence/m8   # 只有行位置变化

# 7) 第十四判据（只在推送之后成立，推送前执行会显示"未推送"）
git log --oneline -4 --format='%h %s'         # 四笔 m12-f：feat / test / evidence / docs
git status --porcelain                       # 必须为空
git rev-list --left-right --count origin/main...main   # 推送后必须为 0 0
```

## 19. 交付与停止

四笔提交（前三笔 hash 见第 12 节）之后立即推送 `origin/main` 并停止：不开始 M13、不部署节点、不接真实 RPC、不进入任何花钱的窗口。判定维持：

- `M12_F = COMPLETE`（范围：本地执行台账的持久化与重启恢复）
- `SELF_HOSTED_NODE = NOT_RUN`
- `LOCAL_CANONICAL_RPC = NOT_VERIFIED`
- `LOCAL_FLASHBLOCKS = NOT_VERIFIED`
- `MULTI_NODE_HA = OUT_OF_SCOPE`
- `REAL_PROFITABLE_ARBITRAGE = UNKNOWN`
