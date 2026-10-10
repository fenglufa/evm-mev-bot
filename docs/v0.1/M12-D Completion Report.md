# M12-D 离线执行安全与状态恢复审计 — 完成报告

里程碑：M12-D（D1 readiness 执行隔离 → D2 状态生命周期 → D3 端点用途与提交真实性 → D4 证据门缺口 → D5 Pending/Radar 边界）
任务书：`docs/v0.1/M12D Coding.md`（§1–§12）
前置：`docs/v0.1/M12-A Repo Audit.md`、`docs/v0.1/M12-B Completion Report.md`、`docs/v0.1/M12-C Single-Node Deployment and Verification Plan.md`、`docs/v0.1/M12-C Completion Report.md`、`data/evidence/m12/b/`
证据目录：`data/evidence/m12/d/`（5 个文件：`manifest.json` + `state-lifetime-controls.json` + `endpoint-provenance.json` + `policy-table-consistency.json` + `radar-status.json`）
基线：`HEAD = origin/main = df6d119`（`git rev-list --count origin/main..HEAD` = 0）；开工时工作树唯一的未提交内容是任务书本身（`?? docs/v0.1/M12D Coding.md`）
判定：`M12-D = COMPLETE`，且 `SELF_HOSTED_NODE = NOT_RUN`、`LOCAL_CANONICAL_RPC = NOT_VERIFIED`、`LOCAL_FLASHBLOCKS = NOT_VERIFIED`、`MULTI_NODE_HA = OUT_OF_SCOPE`（任务书 §12 要求这四条在完成时仍然成立）

---

## 1. 一页结论（白话版，先看这一段）

1. ** readiness 闸门现在挡在两条会花钱的路上，不再只挡在「看行情」的路上。**
   M12-B 的闸门只住在 `runner::build_canonical` 里（live 启动的那条路径）。本轮把它挂到了
   另外两个入口：一键套利 `arbitrage::run_once` 和单笔校验 `cli::run_validation`——这两条是
   真会签名、真会发的入口。实测生产里 `gate_readiness` 有 4 个调用点、lane 的
   `connect` 有 3 个调用点，**每一个 connect 上面都已经有闸门**（`crates/pipeline/tests/readiness_isolation.rs`
   就是扫这张表的，它还自带一个正控制：故意把闸门删掉，扫描必须报红而不是默默通过）。

2. **「节点没答上来」这句报错，按构造进不了证据。**
   本轮把 session 摘要里的 `rpc_url` / `ws_url` / `flashblocks_url` 三个明文键删掉了，
   只留 digest（`rpc-` + 16 位十六进制）。带 URL 的唯一字符串是 readiness 的
   「没判明」detail，而它只有一个证据侧读者（`ReadinessFacts::describe`，`runner.rs:1358`），
   那个写入点挂在 `build_canonical(...).await?` 之后——闸门拒绝时先报错退出，session 目录都没建。
   所以 `Ok`  ⟹ 判定必然是 `Ready` ⟹ 那句带 URL 的话写不进 `status.jsonl`。
   它现在只到操作员的 stderr（`PipelineError::NodeNotReady`），那是 §3 要求「必须告诉操作员为什么停」的地方。

3. **提交侧不再自称「公共 HTTP RPC」。**
   lane 的两个 connect 点以前按 URL 形状自己猜端点类别，猜出来写进证据。现在两处一律记
   `EndpointKind::Unknown`（词面 `unknown`），并且在报错/回执话术里点名「这条链上我实际拨的那只 socket」
   （`over endpoint rpc-<digest>`）。**读取侧的用途标签（`EndpointPurpose`）和提交侧的类别标签（`EndpointKind`）
   现在是两套词汇，有一条测试专门守「读侧标签不许跨进提交侧」。历史证据不回头改：7 个已提交文件里
   104 处 `public_http_rpc` 原样留着。

4. **重启失效那张表，现在有整表一致性门守着——改一个字段就红。**
   M12-B 自己记录过「证据门只重解析锚点，不比对整表，模型字段变了但没触发 refresh 也可能照样绿」。
   本轮复核确认这条描述属实，并补上了缺口：以测试文件里的 `POLICY` 常量 + 生产源码为期望值，
   对已提交的 24 行 × 受审计列逐列比对；身份是语义键 `state_class@node_event`（不是「排序后第几行」）；
   行号那一列被明确排除并写明理由。种进表的缺陷全部被抓到，两个「看似等价」的控制也没有误报。
   **生成器和校验器不共读同一份已生成结果**——这是 §6 点名的循环自证，有专门一条测试证明校验侧
   读的是模型和源码，不读那份 json。

5. **已提交交易的追踪：节点重启不会丢，进程重启目前没有账本。**
   「Pending 视图被清空」不会删掉发送中那笔的追踪，也不会让 lane 以为自己干净了（有测试）。
   但执行账本和 nonce 通道是**进程内**的：生产里只在两处构造 `Ledger`，也没有任何生产代码
   反序列化 `ExecutionRecord`。所以进程在「发出」和「回执」之间重启，看不见自己发过什么，
   只被链上事实（pending 视图仍领先于 confirmed 视图）拦住；池子一旦排空，它能重新推导出同一串字节再发一次。
   这一条**记在证据里没顺手改**：补它要建第二套交易执行状态机，正是 §4 和 §9 禁止的东西。

6. **Radar 的真实状态：存在、有测试、生产里没有一处构造它。**
   M9.4 的 Early Radar 那套词汇（`EarlyRadar`/`PreconfLink`/帧源）在 4 个下游 crate 里 0 命中，
   在能产出二进制的路径上 0 个构造点（同 4 个针在测试里有 17 个构造点，所以这个 0 是「扫得到东西的扫描」）。
   候选那一支确实接进了 runner（`runner.rs:1415`），但它只做一件事：把候选写进证据文件；
   只有 sealed block 那一支才走 state engine 和 dispatch。因此本轮**不说 Radar 端到端可用**，
   §7 那句「准确记录其真实状态」按字面执行。

7. **这一轮全程离线，而且这句话有数字。**
   没部署节点、没连真实 RPC、没签名、没广播。可核对的实测，口径写在数字旁边：
   `crates/*/src` 里含 `eth_sendRawTransaction` 的**非注释行数 8 → 8 一个没变**
   （含注释的原始行数 15 → 16，多的那一行是 `sequencer_direct.rs` 里的一句 `///` 文档注释）；
   src 侧签名面 needles 按「出现次数（含注释）」口径 HEAD 与工作树逐个相等
   （`.sign(` 4、`SigningKey` 4、`k256` 66、`may_broadcast` 5，全部零增量）——
   §11.3 那张表用的是另一个口径（整个 workspace 的**非注释行数**，`.sign(` 在那里是 7 而不是 4，
   `SigningKey` 是 5 而不是 4），两处各说各的口径，不互相矛盾。
   本轮 7 个测试文件（5 个新建 + 2 个原有）里带协议的 URL 字符串字面量共 **6** 处，host 全是 `127.0.0.1`
   （`http://127.0.0.1:{port}` 3 处、`http://127.0.0.1:{}/{TOKEN}` 1 处、
   `http://127.0.0.1:8545/token/abc123` 与 `:8546` 各 1 处）；
   同批文件里裸 `127.0.0.1` 字样 21 处、`localhost` 字样 9 处，指向公网主机的 **0** 处。

一句话结论：**M12-D 把 M12-B 引入的单节点安全能力收到了「防线挂在真正被执行的那条路径上、
证据里不含凭证、策略表变化能被门抓到」这个程度；没有部署节点，没有新增 RPC 方法，
没有改变 M9–M11 的执行与策略语义。**

---

## 2. 本轮检查范围与审计计划（任务书 §2）

### 2.1 基线与前置审查

| 项目 | 实测值 | 怎么读的 |
| --- | --- | --- |
| `HEAD` | `df6d1198dd2c9d46fbc6faf04a55ff479f846c0f` | `git rev-parse HEAD` |
| `origin/main` | 同上 | `git rev-parse origin/main` |
| `origin/main..HEAD` 领先提交数 | 0 | `git rev-list --count` |
| 开工时工作树 | 只有任务书一个未跟踪文件 | `git status --short` |
| 任务书预期的 `d873d55` / `df6d119` | 两笔都在 `main` 上 | `git log --oneline -3` |

阅读清单按 §2 逐项读完：M12-A 审计报告（缺陷编号 D1–D6 出自其 §16）、M12-B 完成报告与
`data/evidence/m12/b/`（含 manifest 的 `out_of_scope_findings_recorded_not_fixed`）、M12-C 方案与报告、
以及 M10/M11 的执行、签名、回执、多通道实现。

### 2.2 审计计划（先给计划，再动代码）

§2 明确要求「不要仅凭 M12-B 完成报告就认定代码一定符合报告中的描述」。执行方式是：每条不变量
先只从源码建立「谁调用谁」的事实表，再拿测试文件核对是否真有代码守着，最后才决定要不要改。

| # | 安全不变量（§1 原文口径） | 源码入口 | 测试入口 | 已有覆盖（本轮之前） | 计划里标为「尚无证据支持的结论」 |
| --- | --- | --- | --- | --- | --- |
| I1 | 节点未就绪时不允许进入交易执行路径 | `runner::gate_readiness:1118`、`build_canonical:926/1263`、`arbitrage::run_once:373`、`cli::run_validation:730` | `pipeline/tests/readiness_startup.rs`（M12-B）、新增 `readiness_isolation.rs`、`validation_gate.rs` | 只有 `build_canonical` 一条路径有闸门和测试 | 「闸门覆盖全部花钱入口」——需要按 connect 点全仓扫描才有证据 |
| I2 | RPC/WS/Pending 异常不能被误判为「没有套利机会」 | `readiness::judge`→`Unverified`、`withheld()`、`PipelineError::NodeNotReady` | 新增 `readiness_isolation.rs` 的报错形状用例、M12-B `chain/tests/readiness_gate.rs` | 判定函数层已有；「拒绝不会变成一张空候选表」没在入口层测过 | 真实节点的超时/断连分布（本轮不联网，无法测） |
| I3 | 失效的 Canonical/Pending 视图不能继续支撑旧候选或旧模拟 | `execution/src/gate.rs`（区块身份判定）、`live/src/flashblocks.rs`（pending 视图） | M10 `stage_matrix.rs`、M12-B `node_reset_pending.rs`、新增 `state_lifetime_recovery.rs` | 进程内失效有覆盖 | 「旧候选在新视图下仍被接受」的真实窗口长度 |
| I4 | 已提交交易的追踪状态不能因节点重启而丢失 | `execution/src/lifecycle.rs`、pending 视图清理路径 | M10 `stage_matrix.rs`（timeout≠failure）、新增 `state_lifetime_recovery.rs` | 节点侧清理有覆盖 | **进程重启后的持久追踪**：生产里没有可回放的存储 |
| I5 | 端点用途标记不能与实际提交路径矛盾 | `submitter::EndpointKind`、`stage.rs:350`、`sequence.rs:1492`、`sequencer_direct.rs` | 新增 `endpoint_provenance.rs`（8 用例）、M12-B `live_args.rs`/`endpoint.rs` 标签用例 | 读取侧标签有覆盖；提交侧类别是「按 URL 自己猜」的 | 提交到底落在谁手里——无法从代码证明，只能保守记 `unknown` |
| I6 | 关键安全策略变化时证据门必须能检测 | `pipeline/tests/node_reset_policy.rs` 的 `POLICY` + `assemble()`、`data/evidence/m12/b/node-reset-policy.json` | M12-B 的 11 条（结构 + 锚点）、本轮新增 8 条（整表） | 只比结构与锚点，**不比字段值** | 生成器与校验器是否循环自证（§6 点名，必须独立证明） |

另外两条 §3/§7 的硬约束写进计划里当检查项：**不新增逐事件/逐候选/逐交易的 `eth_syncing` 请求**、
**不新增 RPC 方法**（除审计证明现有设计无法安全修复时才允许）。

### 2.3 按这个计划实际查出来的偏差（不是复述 M12-B 报告）

1. **闸门的位置与 M12-B 报告的描述不一致。** 报告说 readiness 挡在「会产生真实执行动作的阶段」之前，
   源码里当时只对 `build_canonical` 成立；`arbitrage::run_once` 和 `run_validation` 会构造 lane、会签名，
   却不过闸门。→ 本轮修（D1）。
2. **session 摘要把端点 URL 原文写进证据。** §5 的特别检查点名「端点 URL、API Key、JWT 不得进入公开证据」，
   而 M12-B 的设计是把 URL 和 digest 都写进同一个对象。→ 本轮删掉明文键，只留 digest（D3）。
3. **lane 自己按 URL 猜端点类别并把它写进提交记录。** 与「不得把读取端点标签直接复用为提交端点标签」
   相邻的一条：提交侧也不该由字符串形状推出类别。→ 两处 connect 统一 `Unknown`（D3）。
4. **M12-B 自述的 D4 缺口属实。** manifest 写「只重新解析锚点，未对整张表做完整一致性检查」，
   复核 `the_committed_table_still_resolves_against_the_source` 的实现，确认字段值确实不比对。→ 本轮补整表门（D4）。
5. **M12-B manifest 里 `public_http_rpc` 的分文件计数与我重测的结果不同**（旧记录 1,1,2,1,6,38,1，
   重测为 2,3,2,1,42,48,6 / 7 文件 / 104 处）。本轮证据以重测数为准，并把它记为「旧记录不再引用」。

---

## 3. 修改的生产文件与测试文件（任务书 §11.2）

### 3.1 生产文件（`git diff --numstat HEAD` 实测）

| 文件 | +行 | -行 | 属于 |
| --- | --- | --- | --- |
| `crates/pipeline/src/runner.rs` | 41 | 22 | D1（闸门改可见 + 三入口共用一份定义）、D3（session 摘要去明文 URL） |
| `crates/pipeline/src/arbitrage.rs` | 25 | 2 | D1（一键套利入口接闸门） |
| `crates/cli/src/lib.rs` | 13 | 0 | D1（单笔校验入口接闸门） |
| `crates/pipeline/src/config.rs` | 19 | 2 | D1（两个花费入口的 freshness 策略取 `NotJudged` 的理由写在类型旁） |
| `crates/execution/src/giwa/sequencer_direct.rs` | 45 | 6 | D3（答案话术点名 socket、错误侧 URL→digest） |
| `crates/execution/src/submitter.rs` | 24 | 1 | D3（`EndpointKind::Unknown` + 词面 + `may_broadcast` 口径） |
| `crates/execution/src/stage.rs` | 11 | 10 | D3（connect 处不再自己推类别） |
| `crates/execution/src/sequence.rs` | 4 | 1 | D3（同上，序列 lane） |
| `crates/execution/src/giwa/mod.rs` | 3 | 1 | D3（新 helper 的导出） |
| `crates/live/src/flashblocks.rs` | 15 | 2 | D5（pending 视图与 Radar 的边界注释/命名口径） |
| **合计** | **200** | **47** | 10 个文件 |

生产侧**没有新增文件**，也没有新增 struct/enum 变体之外的抽象；`Unknown` 是既有枚举的第 5 个值。

### 3.2 测试文件

| 文件 | 状态 | 行数 | 测试数 | 属于 |
| --- | --- | --- | --- | --- |
| `crates/pipeline/tests/readiness_isolation.rs` | 新增 | 993 | 9 | D1 |
| `crates/cli/tests/validation_gate.rs` | 新增 | 451 | 5 | D1 |
| `crates/execution/tests/state_lifetime_recovery.rs` | 新增 | 1126 | 7 | D2 |
| `crates/execution/tests/endpoint_provenance.rs` | 新增 | 725 | 8 | D3 |
| `crates/pipeline/tests/pending_radar_boundary.rs` | 新增 | 516 | 5 | D5 |
| `crates/pipeline/tests/node_reset_policy.rs` | 修改 | +1197 | 11 → 19（+8） | D4 |
| `crates/pipeline/tests/readiness_startup.rs` | 修改 | +40 / -37 | 10（数量不变） | D1（把断言改成读新的三入口表） |

本轮新增测试函数 **42 个**（`grep -cE '#\[(tokio::)?test\b'` 逐文件实测）：5 个新文件里的 34 个
（9+5+7+8+5）加上 `node_reset_policy.rs` 的 +8（11 → 19）。
`readiness_startup.rs` 的用例数不变（10 → 10，只是断言改写）。
没有删掉任何既有测试；`#[ignore]` 属性数**全仓 29 → 29**（按行首属性计数，排除字符串与注释里的字面命中），
本轮没有新增被忽略的用例。

### 3.3 证据文件

`data/evidence/m12/d/` 5 个文件（任务书 §11）：`manifest.json`、`state-lifetime-controls.json`（D2）、
`endpoint-provenance.json`（D3）、`policy-table-consistency.json`（D4，由测试自己写出）、
`radar-status.json`（D5）。本轮重生成过的历史证据**只有一类，而且只动了行号**：
M12-D 在 `crates/pipeline/src/arbitrage.rs` 净插 23 行、`crates/execution/src/sequence.rs` 净插 3 行、
`crates/execution/src/stage.rs` 净插 1 行（§3.1 的 numstat），把 **三张**证据门逐行重新解析源码时读的锚点
顶下去了——M8.5.1（6 处）、M8.6（142 处）、M8.4.3（25 处），合计 **11 张表 173 个行号值**。
「行号值」不是「键名等于 `line` 的值」：键名叫 `"line"` 的 102 个，另外 71 个落在 7 个同样指向源码行的键上
（`source_line` 24、`asked_at` 12、`decision_line` 10、`consumer_source_line` 9、`producer_source_line` 8、
`first_decision` 5、`consumer_decision_line` 3；这 71 个全部在 M8.6 的表里，另两门的 31 个都是 `"line"`），
按类型则是 153 个整数 + 20 个 `"文件:行号"` 字符串。§12.2 有逐键名的分解。
叙述、判定、计数、token、文件名、键集合一个字都没动，每一处都能用 `git diff` 逐行核对；
分组、交叉核对与刷新入口在 §12.2，第一条红的取证过程在 §11.7，
沿用的是 M12-B 处理同一类漂移的做法（那次动的是 `data/evidence/m8/state-ownership/` 的 3 种位移、
落在 7 个 `"line"` 字段值上——numstat 3+3+1，commit d417f5c 的说明写的是那 3 种位移）。
唯一被外部进程重写过的已提交证据是 `data/evidence/m10/manifest.json`（M10 的
`executor_evidence_gate` 在任何 workspace 测试运行时会重印它的 `git_commit`），
本轮在提交前把它还原回去，并记在 manifest 的 `known_staleness` 里。

---

## 4. 六条安全不变量的结论与证据（任务书 §11.3）

| # | 不变量（§1 原文） | 结论 | 证据 |
| --- | --- | --- | --- |
| 1 | 节点未就绪时，不允许进入交易执行路径 | **成立（本轮补强）**。闸门在 4 个生产调用点，lane 的 3 个构造点全部在闸门下游；被拒时 `Err` 出 `run`，session 目录、事件循环、lane 都不建 | `crates/pipeline/src/runner.rs:970/1005`、`crates/pipeline/src/arbitrage.rs:373`、`crates/cli/src/lib.rs:730`（闸门本体在 `runner.rs:1118`）；测试 `readiness_isolation.rs` 9 条（含全仓 connect 扫描 + 该扫描的负控制）、`validation_gate.rs` 5 条 |
| 2 | RPC/WS/Pending 异常不能被误判为「没有套利机会」 | **成立**。`judge` 对无可用回答只产出 `Unverified`，`is_ready()` 唯一读法是 `Ready`；拒绝是 `Err`，不是「跑完但候选为空」 | `crates/chain/src/readiness.rs:199`（`judge`）/`:156`（`is_ready`）/`:207`、`:224`、`:335`（三处 `Readiness::Unverified` 返回）、`crates/pipeline/src/error.rs:79`（`NodeNotReady`）；测试 `a_node_that_will_not_or_cannot_answer_is_not_ready`、`a_refusal_is_an_error_and_not_a_route_that_found_nothing`；M12-B `readiness_gate.rs` 13 条 |
| 3 | 失效的 Canonical/Pending 视图不能继续支撑旧候选或旧模拟 | **成立**。旧块上下文的候选在 `connect`/发送前被拒；重新 pin 块会开一次新执行并保留旧记录（不覆盖）；pending 视图回退按 §4 表失效 | `state_lifetime_recovery.rs` 前 3 条 + `the_identity_a_restart_rederives_names_the_execution_the_evidence_recorded`；`data/evidence/m12/d/state-lifetime-controls.json`（含 5 轮 mutation control，每轮只有被摘掉那道守卫的那条测试红） |
| 4 | 已提交交易的追踪状态不能因节点重启而丢失 | **节点重启：成立。进程重启：不成立，已记录为范围外缺陷**。清空 pending 视图不删追踪、并让 lane 保持占用（不判失败）；但账本与 nonce 通道是进程内的，生产里 0 处反序列化 `ExecutionRecord` | 测试 `a_emptied_pending_view_after_a_send_deletes_no_tracking_and_holds_the_lane`、`no_production_path_resumes_execution_from_a_record`；证据同上文件 `durable_state_limits`（含风险与建议里程碑） |
| 5 | 端点用途标记不能与实际提交路径相矛盾 | **成立**。提交侧不再声明任何「谁在运行这个端点」：两处 connect 一律 `Unknown`；读侧标签（`EndpointPurpose`）在生产代码里 0 次出现在 execution crate；答案话术只点名 socket digest；错误串里的 URL 被替换成同一 digest | `stage.rs:350`、`sequence.rs:1492`、`sequencer_direct.rs`（`submission_provenance` / `without_the_endpoint`）；测试 `endpoint_provenance.rs` 8 条；`data/evidence/m12/d/endpoint-provenance.json` |
| 6 | 关键安全策略发生变化时，证据门必须能检测 | **成立**。24 行 × 受审计列逐列比对已提交表；缺失/重复/多余行、字段值变化、死锚点都会红；两把生成器与校验器不共读同一份产物 | `node_reset_policy.rs` 新增 8 条；`data/evidence/m12/d/policy-table-consistency.json`（`planted_defects` / `controls` / `independence` 三块） |

---

## 5. D1 — Readiness 与执行隔离（任务书 §3）

### 5.1 §3 七条路径的落位

| §3 要追踪的路径 | 生产位置 | 测它的用例 |
| --- | --- | --- |
| 启动初始化 | `runner.rs:970`（WS 分支）、`:1005`（HTTP 轮询分支），都在 registry 读取、lane connect、证据写入、第一块之前 | `a_syncing_node_holds_the_route_before_it_reads_a_block`（并断言被拒时一次块读都没发生） |
| Canonical RPC 不可用 | `HttpChainAdapter::connect` 失败 → `PipelineError::Chain` | `a_node_that_will_not_or_cannot_answer_is_not_ready` |
| `eth_syncing` 返回同步进度对象 | `readiness::judge` → `Syncing` | 同上用例（桩回答进度对象）+ M12-B `readiness_gate.rs` |
| `eth_syncing` 返回错误/超时/无效响应 | `judge` → `Unverified`（`readiness.rs:207`） | `a_node_that_will_not_or_cannot_answer_is_not_ready`、`a_refusal_is_an_error_and_not_a_route_that_found_nothing` |
| RPC/WS 断连与重连 | WS 有真重连环（`chain/src/ws.rs`）；preconf 环在读取预算耗尽时结束本次会话（`pipeline/src/preconf_loop.rs`） | 本轮不新增：§3 的口径是「一次进程级启动问一次，下一个重检点是下一次 `run`」，`a_route_run_after_recovery_asks_again_and_is_admitted` 就是这条的测试 |
| 节点高度落后 / Canonical 视图回退 | `HeadFreshnessPolicy::AgainstReference` → `HeadBehindReference` | M12-B `readiness_gate.rs`；本轮把两个花费入口显式设为 `NotJudged` 并写明理由（见 5.3） |
| readiness 恢复后的重新放行 | `gate_readiness` 只在 `Ready` 时 `Ok` | `a_route_run_after_recovery_asks_again_and_is_admitted`、`an_admitted_validation_gets_past_the_gate_and_stops_on_the_next_read` |

### 5.2 「闸门真的在执行路径上」怎么证明的

不靠叙述，靠一张表：`readiness_isolation.rs:686` 起的扫描把 `crates/**/src/**` 里所有
构造 lane 的点列出来（实测 `ExecutionStage::connect` 2 处：`cli/lib.rs:741`、`runner.rs:1339`；
`SequenceStage::connect*` 1 处：`arbitrage.rs:759`），再对每一处要求它所在函数上游能找到
`gate_readiness(`。这条扫描自带负控制：`the_lane_scan_reports_an_ungated_entry_instead_of_passing_it`
把闸门从模板里删掉一次，扫描必须报错而不是空过。

`Signer` 侧不需要另外证明：生产里 `Signer::new` 的构造点是 0（lane 的 `connect` 是唯一入口），
所以「未就绪时 Signer 不被调用」由「未就绪时 connect 不发生」推出，这一点写在测试注释里而不是当成事实。

### 5.3 本轮在 §3 的两条禁令下做了什么、没做什么

- **没新增 RPC 方法**（§3 原文：除非证明现有设计有无法安全修复的结构性问题）。
  `gate_readiness` 用的还是 M12-B 的 `eth_syncing`；方法名针在 diff 里 0 增量。
- **没新增逐事件/逐候选/逐交易请求**。`gate_readiness` 的生产调用点 4 个，全是进程级启动；
  仓库里没有任何循环调用它（`the_fix_adds_no_rpc_method_and_no_second_gate` 同时守这两件事）。
- 两个花费入口传 `HeadFreshnessPolicy::NotJudged` 且不带 observed head：它们在自己读过高度**之前**就要过闸，
  用那个高度判鲜度就不是「前置」了。这是刻意记录的取舍，写在 `config.rs` 的类型旁，
  也写在 `runner.rs` 的 doc 里；它不等于宣称这两个入口检查了高度落后。
- **§3 的「不会静默回退未经授权的公共 RPC」**：`a_held_route_contacts_no_other_endpoint` 用桩计数断言
  被拒那次运行除了配置的那一个端点外没有拨过别的地址。

### 5.4 报错形状（§1 第 2 条不变量的实现侧）

`PipelineError::NodeNotReady { endpoint, detail }`（`crates/pipeline/src/error.rs:79`，本轮未改）。
`detail` 来自 `Readiness::withheld_because()`：`Syncing`/`HeadBehindReference`/`Unverified` 三句话各不相同，
所以「节点在同步」「节点高度落后」「节点没答」在操作员屏幕上是三条不同的原因，
而不是同一个「没机会」。

---

## 6. D2 — 节点重启与状态失效（任务书 §4）

### 6.1 §4 要求区分的三类状态

| 类别 | §4 口径 | 本轮实现/证据 |
| --- | --- | --- |
| 可以失效并重算 | 候选、模拟结果 | `a_candidate_carried_into_a_restarted_node_is_refused_before_a_send`（旧块上下文直接被拒，且拒在发送前）；`re_pinning_a_candidate_after_a_restart_starts_a_new_execution_and_keeps_the_old_record`（重 pin 是新执行，不覆盖旧记录） |
| 必须保留 | 已提交交易的追踪信息 | `a_emptied_pending_view_after_a_send_deletes_no_tracking_and_holds_the_lane`（pending 排空删的是视图，不是追踪；lane 保持占用，不判失败） |
| 不能重复 | nonce / 资金预留 | `a_restarted_process_never_allocates_the_nonce_still_in_flight_and_spends_the_next`、`a_restarted_process_stops_when_the_balance_it_read_cannot_cover_the_bill`；M11 那两条（`multihop_lanes.rs:1212/:555`）是既有覆盖，本轮引用不重做 |

### 6.2 复用而非新建

§4 要求「优先复用 M10、M11 现有生命周期和数据结构，不新建第二套交易执行状态机」。
实测遵守情况：D2 没有新增生产文件、没有新增状态机类型；改动集中在既有函数的注释与判定分支，
以及 4 处**只在测试侧**的守卫。`no_production_path_resumes_execution_from_a_record` 反过来证明
「没有第二套状态机」不是口头话：它扫生产源码，要求没有任何生产路径反序列化 `ExecutionRecord`，
并配了三个正控制（真实的 deserializer、真实的单向证据写、只住在 `#[cfg(test)]` 下的针）。

### 6.3 mutation 轮次与还原

5 轮 mutation（`state-lifetime-controls.json` 的 `mutation_controls`）：每轮摘掉一道生产守卫，
只有对应那一条测试红，其它全绿；每轮之后把文件还原。本轮结束时的还原核对是 `git diff HEAD --numstat`
对这些文件返回空（`crates/execution/src/gate.rs`、`lifecycle.rs`、`nonce.rs`、`intent.rs` 等全部 clean），
这一点在 §12 的门禁数据旁边再列一次。没有一轮 mutation 计时，所以证据里不给它们配时长。

### 6.4 记录而未修的缺陷（§4 的第 4 类）

`durable_state_limits`：进程重启后的追踪需要可回放的存储。风险写在证据里：
「池子排空后可能重新推导出同一串字节并再发一次」；未修理由：需要第二套执行状态机，§4/§9 禁止；
建议里程碑：M12-D 之后的持久化轮次，给 ReceiptTracker 一个可重放的存储。

---

## 7. D3 — 端点用途与提交路径真实性（任务书 §5）

### 7.1 §5 的五条特别检查，逐条判定

| §5 检查项 | 判定 | 证据 |
| --- | --- | --- |
| `localhost` 不应自动等价于「交易最终提交到本地节点」 | 成立 | 提交侧类别不再由 URL 形状推出（`stage.rs:350`、`sequence.rs:1492` 一律 `Unknown`）；读取侧的 locality 判定由 M12-B 的 `endpoint.rs` 三条用例守着（`no_url_shape_declares_anything`、`silence_is_not_a_claim_about_locality`），本轮的 `the_read_sides_label_never_crosses_into_the_submission_path` 把它挡在 execution crate 之外 |
| 本地 Canonical RPC 与公共 sequencer 可能同时存在 | 成立 | `both_halves_of_a_run_name_the_one_socket_and_a_second_socket_changes_only_the_digest`：同一运行里读与发各指一处时，digest 各自变、类别都不变，不存在「把读侧端点当成提交端点」的推导 |
| 不得把读取端点标签直接复用为提交端点标签 | 成立 | `EndpointPurpose::` 在 `crates/execution/src` 的**非注释代码行里 0 命中**，全仓 `crates/*/src` 里 `#[cfg(test)]` 之上的 `EndpointKind::` 引用**只有 2 处**，且两处都是 `Unknown`（`stage.rs:350`、`sequence.rs:1492`）。正控制证明扫描看得见东西：`chain/src/endpoint.rs` 有命中；`PublicHttpRpc` 也仍在仓库里，但全部落在定义/词面（`submitter.rs:48`、`:61`）、说明注释（`:29`）或测试模块内（`submitter.rs:176/:194/:205`，`evidence.rs:349/:386`，`lifecycle.rs:1374/:1384`，四者的 `#[cfg(test)]` 分别在 `:235`、`:932`、`:168`） |
| 未知端点应标记 `Unknown` 或等效状态 | 成立 | `EndpointKind::Unknown`（词面 `unknown`）；`may_broadcast()` 对 `Unknown` 返回 true 是刻意的——它只表示「这只 socket 能承载广播」，不表示「我们知道它是谁」 |
| 端点 URL、API Key、JWT 和其他凭证不得进入公开证据 | 成立（路径见 7.3） | URL→`rpc-<digest>`（`without_the_endpoint` + `submission_provenance`）；session 摘要去掉 3 个明文 URL 键；实测某类答案里 URL 出现次数 1 → 0（只记计数，不把 URL 抄进证据）。口径说清：本轮没有任何真实凭证；测试里那个**故意的**假 token `abc123` 只以「桩 URL 形状模板」的形式出现在 `endpoint-provenance.json` 的脱敏说明块里（§11.4 逐条列出） |

### 7.2 保守标签与「不符合配置就阻止」

§5 要求「无法准确证明提交目的地时采用保守标签并阻止不符合配置要求的执行，而不是猜测」。
本轮把两件分开的事都做了：`Recorded` 端点被要求提交时，在 connect 阶段就被拒（
`execution_that_does_not_fit_the_configuration_is_refused_before_the_socket`，
拒绝话术点名它实际拿到的类别，不编造目的地）；`Unknown` 只描述 socket，不承诺目的地。

### 7.3 本轮改动之外仍然存在的三个洞（如实记录）

1. **网关只 echo URL 的 path**：桩端把 path 回显进 body，因此 query 里的密钥形式本轮没测到 scrub 效果；
   `without_the_endpoint`（`sequencer_direct.rs:575`）是按整串 URL 替换的，path-only 回显时替换不会命中。
2. **读取侧错误串未 scrub**：`read_error`（`sequencer_direct.rs:583`，20 个调用点）把节点错误原样带上，
   本轮能到达的那些都以日志行结束、不进证据行。全仓扫一遍会把生产 diff 扩到缺陷类之外。
3. **accepted 分支原样插入节点的 result 值**：发送的 result 是哈希，只有第 1 条那种网关回显
   才可能把 URL 带回来，所以它与第 1 条是同一条不对称的两个面。

这三条写在 `endpoint-provenance.json` 的 `holes_left_open`，不顺手扩大任务。

### 7.4 red → green 过程里的一次自我纠正

D3 测试第一次跑是 8 条里 2 条红。其中一条是**真缺陷**（提交记录里仍带 URL，失败点 `:277:9`），
另一条是**我的过度断言**（`:514:5`：那条断言要求扫描的针在某个文件里存在，而该文件的命中其实住在
`#[cfg(test)]` 之下）。两次都记在 `endpoint-provenance.json` 的 `red_then_green`，
因为「测试变绿」不区分是修了缺陷还是放宽了断言。

### 7.5 历史证据不回头改

7 个已提交文件里的 104 处 `public_http_rpc`（分文件 2/3/2/1/42/48/6，扫描范围 1382 个 tracked 文件）
原样保留。本轮的词表变更是**只向前**（forward-only）的：新运行写 `unknown`，旧运行写它当时写的东西。

---

## 8. D4 — `node_reset_policy` 证据门缺口（任务书 §6）

### 8.1 §6 七项要求的落位

| §6 | 做了什么 |
| --- | --- |
| 1 复核 M12-B 的原始描述 | `data/evidence/m12/b/manifest.json` 的 `out_of_scope_findings_recorded_not_fixed[4]` 原文确认；并复现它的失败模式：旧门只比行数/schema/锚点，比字段值不 |
| 2 找到生成器/锚点提取/测试 | 生成器 = `node_reset_policy.rs` 的 `assemble()`；锚点 = `(anchor_file, anchor_token)` 两把 resolver；产物 = `data/evidence/m12/b/node-reset-policy.json` |
| 3 确定整表权威数据源 | 测试文件里的 `POLICY` 常量 + 它指向的生产源码，不是那份 json |
| 4 可重复对照检查 | `the_committed_table_matches_the_model_column_by_column`：行身份 `state_class@node_event`（语义键），24 行 × 受审计列逐列 |
| 5 改 `disposition`/`strength` 会失败 | `every_planted_defect_in_the_policy_table_is_reported` + `the_audited_columns_cover_every_column_the_generator_emits`（保证「受审计列」不等于「除了一列什么都不是」） |
| 6 正常生成的证据通过检查 | `the_assembled_table_is_the_one_the_evidence_file_carries`（既有那条 M12-B 用例仍绿，说明补门没有把已提交表判死） |
| 7 缺失/重复/多余行与字段值变化都被检测 | 三类行缺陷 + 字段变化都在 planted 用例里；`only_the_display_column_is_excluded_from_the_comparison` 明确排除的只有行号那一列并说明理由 |

### 8.2 循环自证怎么破掉的

§6 原文：「证据生成器与校验器不得仅通过共同读取同一份已经生成的结果来形成循环自证」。
`the_gate_reads_the_model_and_the_source_and_nothing_else` 断言校验侧的输入只有两样：
本文件里的常量表 + 锚点指向的生产源码；`expected_side_reads_the_artifact = false` 写进证据。
更强的那条是 `both_copies_that_agree_on_a_dead_anchor_still_fail`：把同一个缺陷同时种进两份副本，
只有「拿源码重解析锚点」这一路能报出来——两份产物互相点头是抓不到的。

### 8.3 结论

M12-B 记为范围外的这条缺口本轮闭合：`node_reset_policy` 现在对**字段值**敏感，
且这张表的期望值不来自它自己生成的 json。已提交表没有被重写（`verdicts.policy_table_changed = none`），
`M12B_NODE_RESET_POLICY_REFRESH` 没有被使用。

---

## 9. D5 — Pending 与 Radar 数据边界（任务书 §7）

### 9.1 Radar 的真实状态（§7 第三条禁令要求如实记录）

`radar-status.json` 的 `status_of_the_radar`：**存在、有测试、生产里没有一处构造它**。
实测（全部由 `pending_radar_boundary.rs` 现扫）：

| 测量 | 值 | 非空证明 |
| --- | --- | --- |
| 被扫描的 src 文件数 | 137 | 扫描本身报出文件数 |
| Radar 词汇（11 个针）在 4 个下游 crate 的命中 | 0 | 同词汇在 `crates/live` 有 78 处命中 |
| Radar 构造针（4 个）在生产代码（会进二进制的路径）的构造点 | 0 | 测试里有 17 个构造点（`PreconfLink::new` 5 / `EarlyRadar::new` 4 / `PollingFrameSource::new` 6 / `ReplayFrameSource::new` 2） |
| runner 里候选那一支（`runner.rs:253`）的长度与禁词 | 131 字符，禁词 0（dispatch、on_canonical、Signer、submitter 等） | 同一函数里 Canonical 那一支确实调用 `engine.on_canonical` 与 `dispatch` |
| 下游对 pending 三个方法名的读取 | 0 | `crates/live` 自己读（Radar 的输入），命中非 0 |

有一处计数是我自己先写错、重测后改的：注释里写「Measured 13」，而该门用的递归目录遍历实际数到 17
（第一版复算用非递归 glob 得到 16）。修正记录在 `corrections_found_by_re_measuring`，
断言下限保持 `>= 8`（是余量，不是猜测）。

### 9.2 §7 三条禁令

- **不因为 Pending 可解析就认定它能驱动执行**：可解析那一半由 M12-B 的
  `pending_shape_compat.rs` 证明（8 条），本轮不动它；本轮证明的是「它离执行还有一整个 crate」——
  `the_signer_and_the_radar_are_two_crates_that_never_meet_in_a_manifest` 读两个 crate 的
  `Cargo.toml` 依赖边，要求 `evm-live → evm-execution` 这条边不存在。
- **不把 Early Radar 接进 Signer 或 Submitter**：本轮没接（`crates/live` 无新增依赖，
  上面那条依赖边测试是它的守卫）。
- **Radar 未接入链路就不声称端到端通过**：任务书 §12 的四条判定保持，`LOCAL_FLASHBLOCKS = NOT_VERIFIED` 未变。

另外如实记录一条容易被误读的事实：候选来源**确实**接在 `runner.rs:1415`（由 `config.flashblocks_url` 门控），
它做的唯一一件事是写候选证据文件；把它说成「Radar 已接入运行链路」和说成「完全没接线」都不准确。

### 9.3 结构性门的两次 mutation 对照

- mut-1：在 `crates/metrics/src/lib.rs` 里种一个 `PreconfLink` 构造 → 门在 `:225` 报红。
- mut-2：在 runner 的候选分支里种一个 `"m12d_plant": "dispatch"` → 门在 `:362` 报红。
- 两轮之后都还原（`crates/metrics/src/lib.rs` 在 `git status` 里 clean，`runner.rs` 只含 M12-D 自身改动，
  还原用 `/tmp/runner.rs.bak` + `diff -q`，**没有对该文件使用 `git checkout`**，因为它带着本轮的生产改动）。

`crates/pipeline/tests/pending_radar_boundary.rs` 这个 D5 文件在主体写成之后又被补写过一次。
补写的内容本轮**不能**用 git 复核（它是本轮新建、当时尚未提交的文件），所以这一条不建立在
「那只是注释/文档行」这种说法上，而是直接看二进制：权威轮的门 3 跑完之后，
`target/debug/deps/pending_radar_boundary-0b5793f7ea84984f` 的 mtime 是 **14:47:41Z**，
落在门 3 的运行窗口（14:46:33 → 14:51:06Z）之内，而门 2 之前清过 16 个包的产物（§12.1）——
也就是说 §12.3 里那 5 个通过用例是**这份文件重新编译之后**跑出来的，不是旧二进制。
更早那把 12:28:12Z 的单目标复跑（`/tmp/m12d_d5_recheck.log`，`test result: ok. 5 passed`）
写在这最后一次补写（文件 mtime 12:45:24Z）之前，本轮不拿它当证据。
manifest 的 `gates.per_target_re_run` 记的是六个文件「声明用例数 = 权威门禁日志里的实跑通过数」逐个相等
（含这一个 5/5），并写明 `done_separately: false`——权威轮之后没有为这六个文件另打一把门禁。

### 9.4 不重复既有覆盖

§58 的 `crates/live/tests/preconf_isolation.rs` 与 M12-B 的 `pending_shape_compat.rs` 已在仓库里，
本轮**没有**为它们再造用例（记在 `coverage_already_present_before_d5`）。

---

## 10. §8 九项负向测试的逐项落位（任务书 §8）

| # | §8 要求的负向测试 | 本轮用例 |
| --- | --- | --- |
| 1 | readiness 失败时的执行隔离 | `readiness_isolation.rs`：`a_syncing_node_holds_the_route_before_it_reads_a_block`、`a_node_that_will_not_or_cannot_answer_is_not_ready`、`a_held_route_contacts_no_other_endpoint`；`validation_gate.rs` 前 4 条 |
| 2 | 重启前候选在恢复后被拒绝 | `state_lifetime_recovery.rs`：`a_candidate_carried_into_a_restarted_node_is_refused_before_a_send`（+ `re_pinning_...` 说明重新 pin 是「新执行 + 保留旧记录」，不是放行旧候选） |
| 3 | 已提交交易追踪在重启清理后仍保留 | `state_lifetime_recovery.rs`：`a_emptied_pending_view_after_a_send_deletes_no_tracking_and_holds_the_lane` |
| 4 | Nonce / 资金预留不会重复 | `state_lifetime_recovery.rs`：`a_restarted_process_never_allocates_the_nonce_still_in_flight_and_spends_the_next`、`a_restarted_process_stops_when_the_balance_it_read_cannot_cover_the_bill`；策略表侧 `node_reset_policy.rs: a_reservation_refuses_a_second_claim_by_name` |
| 5 | 未知端点用途不能被标为 Local | `endpoint_provenance.rs`：`a_conservative_label_still_describes_a_socket_and_a_read_only_one_still_refuses_to_send`（`Unknown.name() == "unknown"`、`Recorded` 不许广播）；读取侧那半由 M12-B 的 `endpoint.rs` 4 条守着 |
| 6 | 公共提交端点不会被描述为本地提交 | `endpoint_provenance.rs`：`an_accepted_send_names_the_socket_and_not_who_runs_it`、`the_evidence_row_for_a_send_says_unknown_and_carries_no_url`、`the_read_sides_label_never_crosses_into_the_submission_path` |
| 7 | `node_reset_policy` 关键字段变化使证据校验失败 | `node_reset_policy.rs`：`every_planted_defect_in_the_policy_table_is_reported`、`the_committed_table_matches_the_model_column_by_column`、`both_copies_that_agree_on_a_dead_anchor_still_fail`、`the_audited_columns_cover_every_column_the_generator_emits` |
| 8 | Pending 响应不兼容时 fail-closed | 既有：M12-B `pending_shape_compat.rs`（含「只有哈希的帧被拒而不是报空池集」）。本轮补的是边界侧：`the_run_loop_records_a_candidate_and_acts_only_on_a_sealed_block`、`the_lanes_only_pending_read_is_a_nonce_and_is_named_as_one` |
| 9 | 断连恢复后必须重新满足执行条件 | `readiness_isolation.rs`：`a_route_run_after_recovery_asks_again_and_is_admitted`、`every_production_entry_that_builds_a_lane_passes_the_gate_above_it`、`the_lane_scan_reports_an_ungated_entry_instead_of_passing_it`（这条是前一条的负控制） |

§8 的另一句「不得使用真实私钥、真实资金或真实链上交易」：全程遵守。所有端点是 127.0.0.1 桩或
配置字面量；唯一使用的密钥是合成的 scalar-1 测试密钥（既有 fixture，本轮没引入新密钥）。

---

## 11. 任务书 §10 的六类专项审查（全部在门禁运行期间以只读扫描完成）

任务书 §10 在三道命令之外还点名六项审查。这一节逐项给结论与口径，
所有数字都是本轮用脚本现量（对比基线 = `git archive HEAD` 解到 `/tmp/headscan` 的那一份源码树），
不是引用上一里程碑的表。

### 11.1 生产代码 diff 审查

`git diff --numstat HEAD -- 'crates/*/src/*'` → **10 个文件，+200 / −47**（逐文件见 §3.1）。
判定口径不是「改动少」而是「改动落在哪条禁令上」：

- M10 的 Executor、Signer、ReceiptTracker 与 M11 的 Multi-Lane 状态机**没有被重写**：
  这五个模块在 diff 里只出现两类最小动作——把已有 `match` 分支的标签来源换成既有 `EndpointKind`
  （`stage.rs` +11/−10、`sequence.rs` +4/−1）、给已有提交答案话术加 socket 指纹（`sequencer_direct.rs` +45/−6）。
  没有新的状态机、没有新的 lane、没有改交易构造或 gas 计算的任何一行。
- 任务书 §9 列出的 10 条禁令里与生产代码直接相关的三条（不新增 HA/多节点/负载均衡/自动故障转移；
  不重写 M10/M11；不把 Radar 扩成执行框架）在 diff 里的体现是**新增文件数 = 0**（生产侧），
  新枚举值只有 `EndpointKind::Unknown` 一个。

### 11.2 新增 RPC 调用预算审查

- 新增行里的 `"eth_…"` 形状字符串只有 **1 个**：`"eth_pendingRewind"`，位置
  `crates/pipeline/tests/node_reset_policy.rs:1845`，它是 D4 门**自己种的缺陷值**
  （往 `verdicts.new_rpc_methods_in_this_policy` 里填一个新方法名，验证门会报红），
  不是任何一处请求。生产侧新增 RPC 方法 = **0**。
- 逐事件/逐候选/逐交易的 `eth_syncing` 请求 = **0**：本轮没有增加闸门位置，
  只是把已有的启动/恢复两处判据改成三入口共用同一份 `gate_readiness`（见 §5.3）。
- 证据文件 `verdicts.new_rpc_methods_in_this_gate = []`（`policy-table-consistency.json`）与之一致。

### 11.3 签名与广播路径审查

以「非注释行里出现该方法名/调用形状」为口径，比较 HEAD 树与工作树：

| 探针 | HEAD | 工作树（既有文件） | 5 个新测试文件 |
| --- | --- | --- | --- |
| `eth_sendRawTransaction` | 40 | 40（逐文件 delta 为空） | 9 |
| `.sign(` | 7 | 7 | 1 |
| `SigningKey` | 5 | 5 | 0 |
| `sign_hash` / `recoverable` / `to_bytes()` | 0 / 2 / 8 | 0 / 2 / 8 | 0 |

结论：**生产侧的签名与发送面一字未动**；新增的 9+1 处全部在新测试文件里，
且都是 127.0.0.1 桩或纯字符串断言（`endpoint_provenance.rs` 6 处是把「答案话术必须点名 socket」
写成正反两侧的探针）。整个里程碑没有产生过一笔真实交易（见 §15）。

### 11.4 敏感信息扫描

扫描对象 = 本里程碑全部新增行，共 **5421 行**（manifest `scans.secrets.scanned_lines`），
由两部分相加：工作树里跟踪文件的 `+` 行 1610 行（生产与测试代码 1437 + 刷新的 M8 证据表 173；
两份未提交的文档不在 `git diff` 范围内），加上 5 个新测试文件的全文 3811 行。

- PEM 头 `BEGIN … PRIVATE KEY`：**0**
- JWT 形状 `eyJ….`：**0**
- 64 位十六进制字面量 `0x[0-9a-f]{64}`：**0**
- 非 127.0.0.1/localhost 的 URL：**1 个值** —— `http://candidate.test`，一个文档用途的假域名。
  它在新增行里只出现在 `crates/live/src/flashblocks.rs` 的 `#[cfg(test)] mod tests` 内（两处：
  构造桩 source 的第 658 行，和下面的断言），而本轮的改动恰好是把它从明文降级成摘要：
  断言从 HEAD 的 `assert_eq!(rows["endpoint"], json!("http://candidate.test"))`
  改成 `rows["endpoint_id"] == endpoint_id("http://candidate.test")`（digest），
  并新增一条 `rows.get("endpoint").is_none()` 断言，证明配置的 URL 不进记录。
  同一个字面量在 HEAD 就存在于 `crates/live/tests/node_reset_pending.rs:84`，本轮未动。
  除此之外新增行的 host 全是 127.0.0.1 的桩端口。
- `api[_-]?key|secret|token|bearer|jwt` 词面命中 44 行 = 9 行注释散文（说明「配置 URL 是本轮唯一可能带
  key/JWT 的地方」）+ 35 行标识符：`anchor_token`（D4 门的列名）、`TOKEN = "abc123"`
  （**故意假**的对照值，用来断言它出现在证据里就报错）、`from_secret_bytes(&TEST_SCALAR)`
  （既有合成 scalar-1 测试密钥）、`input_token` / `mid_token`（字段名）。
  逐条分类见 `endpoint-provenance.json` 与 manifest 的 `scans`。
- 证据文件本身：`data/evidence/m12/d/` 的 4 张表里「某次真实配置过的端点 URL」= **0**，
  端点在表里只以 `rpc-<digest>` 出现，非回环 host 也 = 0。
  要说全的话：这 4 张表里有 **2 处 URL 形状的文本**，都在 `endpoint-provenance.json` 的
  `url_leak_measurement` 块里，是**讲脱敏这件事的说明文字本身**——一条是取证用的 grep 模式
  `http://127\.0\.0\.1:[0-9]*`，一条是桩 URL 的形状模板 `http://127.0.0.1:{ephemeral port}/token/abc123`；
  两条都是回环地址，且该块的正文自己就写明「记的是计数，故意不记那条带凭证形状路径段的 URL」。
  两条断言（`endpoint_provenance.rs:278`、`:503` 的 `!line.contains(credential)` /
  `!text.contains(credential)`，`credential ∈ {TOKEN, "127.0.0.1"}`）管的是它们各自管辖的对象——
  组合出来的发送行与写进 submissions 的行——不是整份 JSON 的每个字符串，所以这两处说明文本
  在断言范围之外，这一点写在这里而不是含糊成「文件里 0 处 URL」。
  另外 `manifest.json`（第 5 个文件，汇总而非证据表）里出现的 `http://candidate.test`
  是 `scans.secrets.non_loopback_urls` 这条扫描结论本身，不是任何一张表的证据行。

### 11.5 证据生成与校验的负向测试

四类「让门变红」的对照，全部当场做过并还原：

| 门 | 种缺陷方式 | 结果 |
| --- | --- | --- |
| D4 整表一致性 | 26 个 planted defect（25 个只改产物副本，1 个同时改两份副本使其互相点头） | 26 抓到、0 漏；2/2 对照行为符合预期 |
| D2 状态生命周期 | 5 轮 mutation（删追踪、放行旧候选、重复 nonce 等） | 5/5 变红，还原后 `git diff --numstat` 与预期一致 |
| D5 结构性边界 | 2 轮 mutation（在 metrics 生产码种 `PreconfLink` 构造；在 runner 候选分支种 `"dispatch"`） | 2/2 变红（`:225`、`:362`），还原干净且未用 `git checkout` |
| D3 端点真实性 | red→green 记录在案：`:277:9` 抓到**真生产缺陷**（传输错误原文带着 URL 进证据），`:514:5` 抓到**我自己的过度断言**并撤回 | 修复前 1 次整 URL 泄漏 → 修复后 0 次 |

一句话概括这四类对照的意义：**每次「门是绿的」都配了一次「故意弄坏它，看它是不是真的会红」**，
所以 §12 的绿灯不是自证。

### 11.6 M10 / M11 回归

M10、M11 的既有测试目标在 §12.3 那把 workspace 门禁里整体跑完：**19 个目标、257 passed、0 failed**
（M10 六个 56 passed，M11 十三个 201 passed），逐目标数字与归属方法在 §12.4。

### 11.7 门禁跑出来的第一条红：M8.5.1 的三处锚点漂移（本轮造成，已按门自己的机制刷新）

第一次跑 §12 的第三道门时，`cargo test --workspace` 停在
`crates/pipeline/tests/eth_call_semantics_evidence.rs::the_committed_directory_is_a_reassembly_of_the_model_and_the_records`，
`10 passed; 1 failed`，而 cargo 的 fail-fast 把整轮截断在 71 个测试目标上——
所以权威的那一轮改用 `--no-fail-fast -- --test-threads=1`，日志必须覆盖全部目标才算解析完（见 §12.1）。

这条红不是flaky，是**本轮自己的改动造成的锚点漂移**，逐字取证如下：

- M12-D 往 `crates/pipeline/src/arbitrage.rs` 净插了 23 行（numstat 25 增 / 2 删，见 §3.1），
  插在 M8.5.1 证据门**逐行重新解析源码**的两个锚点之前：
  `"getReserves_blockTimestampLast": self.last_synced.to_string(),` 从 266 行移到 268 行，
  `async fn price_legs` 从 1465 行移到 1488 行。两个位置都当场以 `git show HEAD:…` 与工作树对照确认。
- 门按 token 重新解析出 268 / 1488，与已提交字节不符 ⇒ 逐字节比较变红。三个表各受影响 2 处，
  合计 6 个 `"line"` 值：`lifecycle-contracts.json`、`ownership-matrix.json`、`reuse-verdicts.json`。
- 刷新用的是门自带的 `M851_ETH_CALL_SEMANTICS_REFRESH=1`（该门设计里唯一的改写入口），
  刷新后的 `git diff` 实测就是那 6 行、且全是 `"line"` 字段——
  没有任何 note、判定、计数、token 或文件名被改动，`git diff --stat` 显示 3 files changed, 6 insertions(+), 6 deletions(-)。
- 刷新后不带环境变量重跑该目标：`11 passed; 0 failed`。断言本身一个字都没放松，
  它仍然是「提交目录 = 一次装配的字节」。

这与任务书 §9「不得修改或重新生成无关历史证据」不冲突：被重写的锚点之所以动，
是**本轮生产改动直接造成的**（相关而非无关），而门提供刷新入口就是为了这种情况；
只重新解析、不重写叙述。M12-B 处理过同一类问题（那次动的是 `data/evidence/m8/state-ownership/` 的
7 个 `"line"` 字段值、3 种位移，numstat 3+3+1，
写在它的 evidence 提交说明里），本轮沿用同一做法并把范围如实记在
`data/evidence/m12/d/manifest.json` 的 `known_staleness`。

这一条红之后，同一类漂移又在两门上出现（M8.6 与 M8.4.3，因为本轮也净插了 `sequence.rs` 3 行、
`stage.rs` 1 行），三门合起来的分组与逐处交叉核对在 §12.2。

---

## 12. 三道串行门禁的实测（任务书 §10 的六栏）

三道命令**串行**执行，构建环境沿用已经验证过的那一份（少了它 clippy 会死在 rocksdb，见 §16 第 0 步）：

```bash
export CC=clang CXX=clang++ CXXFLAGS="-include cstdint"
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast -- --test-threads=1
```

任务书 §10 要求「必须解析完整日志，而不只是检查进程退出码」，所以三条各把 stdout+stderr 重定向到独立文件，
退出码从包装脚本写进汇总文件的 `G*_EXIT=` 行读，并且 manifest 生成脚本会把这些行**再解析一遍**
（`gates.exit_code_provenance`）。本轮最早那两把（出缺陷的那把 clippy 与停在 71 个目标的那把 test）跑在
有包装行之前，它们的退出码在产物里已经无从恢复，所以它们的成败只从日志正文里的 `error:` / `FAILED` 行判定；
这正说明为什么权威那批必须把退出码写进文件而不是凭记忆。
`/tmp` 下的原始日志不进仓库，但解析出来的每一个数字都进了 `data/evidence/m12/d/manifest.json` 的 `gates` 块，
而且写文件的脚本在任何断言失败时直接不落盘。

### 12.1 第 1、2、6 栏：命令与退出码 / 实际执行时间 / 有没有超时或未完成的命令

| 步 | 命令 | exit | 墙钟（UTC，包装脚本自报） | 产物与日志里读到什么 |
| --- | --- | --- | --- | --- |
| clean | `cargo clean -p <16 个 workspace 包>`（逐包） | 0 | 460 s，14:38:12 → 14:45:52 | cargo 自报 `Removed 1015754 files, 89.5GiB total` |
| 门 1 | `cargo fmt --all -- --check` | 0 | 3 s，14:45:52 → 14:45:55 | `/tmp/m12d_gate1_fmt6.log` = **0 字节**：没有待格式化的差异 |
| 门 2 | `cargo clippy --workspace --all-targets -- -D warnings` | 0 | 38 s，14:45:55 → 14:46:33（cargo 自报 `Finished … in 37.52s`） | `/tmp/m12d_gate2_clippy6.log`：15 行 `Checking` + 1 行 `Compiling` = **16 个包**；以 `warning`/`error` 开头的行 **0** |
| 门 3 | `cargo test --workspace --no-fail-fast -- --test-threads=1` | 0 | 273 s，14:46:33 → 14:51:06 | `/tmp/m12d_gate3_test6.log`：133 行 `test result: ok.`；见 §12.3 |

三闸合计 314 s，加上 clean 共 774 s（`RUN_START` → `RUN_END`，同一份 `/tmp/m12d_gates_summary6.txt`）。
**没有超时，也没有未完成的命令**：三道各自的 `START/EXIT/END` 三个戳齐全、首尾相接（前一道 END 与下一道 START 同秒），
最后一道的 END 与 `RUN_END` 同秒。门 2 那 16 行里有一行是 `Compiling` 而不是 `Checking`——clippy 必须先跑带
build script 的 crate 的构建脚本才能分析它（`evm-pipeline`），它仍然是在 `-D warnings` 下被分析的；
这一点也写进了 manifest 的 `why_one_line_says_compiling`。

**门 2 之前那次 clean 是本轮加的一步，理由不是仪式感。** 同一批的门 2 原本是 warm run：日志里 0 个 crate 行，
即 cargo 一个包都没重新分析。按包核对「每个包最后一次被 clippy 分析是在哪一把日志里」的结果是
16 个包里有 11 个，其最后一次分析发生在一把因无关诊断中止的跑上，所以「这道门本轮到底审过多少代码」
陈述不出来（该核对写在 manifest 的 `previous_warm_run.why_replaced`）。结论是不能用解释凑过一道门，
于是清掉这 16 个包的产物重跑；依赖产物不在清除范围内，所以门 3 的 16 行 `Compiling` 是本仓库 16 个 crate
的测试产物，而不是依赖重编。这道门的覆盖面随后用 mtime 而不是用文字陈述：工作树里（排除 `target/`、`.git/`）
凡文件名匹配 `.rs` / `Cargo.toml` / `Cargo.lock` / `build.rs` 的文件共 **279 个**，逐个拿 mtime 与门 2 日志的
mtime 相比，晚于它的集合为空（字段 `clippy_inputs_touched_after_the_authoritative_run: []`），
所以这道门的结论对应的就是被提交的那棵树。

本轮一共留下 5 份 fmt 日志（全部 0 字节）、5 份 clippy 日志、5 份 test 日志；权威的是上表这一批。
其余的为什么不作权威，任务书要求逐条交代，这里不隐藏：

| 产物 | 角色 | 为什么不算权威 |
| --- | --- | --- |
| `gate3_test.log` | 门 3 第 1 把 | 命令没有 `--no-fail-fast`：停在 M8.5.1 的锚点红（`test result: FAILED. 10 passed; 1 failed`），cargo 默认在第一个失败目标后收摊，整份日志只有 70 行 ok + 1 行 FAILED = **71 个目标**，其余 **46 个测试二进制**（117 − 71）与**全部 16 个 doc-test 目标**根本没执行。这一把是「只看 exit code 不够」的实证 |
| `gate3_test3.log` | 定向复跑 | 只有 2 行统计（`evm-chain` 单测 56、`tests/readiness_gate.rs` 13），不是 workspace 门禁 |
| `gate3_test4.log` | discovery 跑 | 加上 `--no-fail-fast` 之后第一次全覆盖：133 目标、1725 passed、**5 failed**（`rpc_reduction_evidence` 14/3、`state_ownership_evidence` 10/2），exit 101（包装行 `gate3 test exit=101`，在 `/tmp/m12d_gates_summary3.txt`）。它的价值是把 fail-fast 藏住的另外两张漂移门照了出来（§12.2）；它跑在刷新之前，不能当验收 |
| `gate3_test5.log` | 刷新后的绿跑 | 133 目标 / 1730 passed / 0 failed / 29 ignored，与权威那把逐个数字相同；作废的原因是**同一批**的门 2 是 warm run（0 个 crate 行），三道门必须作为同一时刻的树与同一份缓存状态一起成立，所以整批在 clean 之后重跑 |
| `gate2_clippy.log` | 缺陷证人 | 报出 `doc_lazy_continuation` 那条 clippy 缺陷（本轮修了）：日志末尾是 ``error: could not compile `evm-pipeline` (test "pending_radar_boundary") due to 1 previous error`` 加 `warning: build failed`。这一把跑在包装脚本记录退出码之前，它的退出码在产物里已经无从恢复，所以它的成败只能从日志正文的 `error:` 行判定 |
| `gate2_clippy2.log`、`gate2_clippy3.log`、`gate2_clippy5.log` | 增量跑 | 分别只有 6、1、0 个 crate 行；`gate2_clippy5.log` 就是上面那张 warm 的 |

一条时长上的实话：同一条门 3 命令，本轮三把全覆盖跑分别是 1679 s（`test4`）、616 s（`test5`）、273 s（`test6`，权威）。
这个跨度由外部卷上的编译状态与并发扫描决定，各把的自报运行时间合计都是 105 s 量级（§12.3），
所以时长只用于说明「门禁不廉价」，不作为性能结论。

### 12.2 三张证据门的锚点漂移：11 张表、173 个行号值

「173 个行号值」的口径：这 173 个改动值全部指向某一行源码，但**不全是键名叫 `line` 的字段**。
按键名逐个现量（`git show HEAD:<表>` 与工作树逐键对照，脚本口径同下第 1 个读数）：

| 键名 | 值的类型 | 位移值数 | 落在哪些表 |
| --- | --- | --- | --- |
| `line` | 整数 | 102 | 全部 11 张表 |
| `source_line` | 整数 | 24 | `rpc-census`、`rpc_surface`（各 12） |
| `asked_at` | `"文件.rs:行号"` 字符串 | 12 | `information_flow` |
| `decision_line` | 整数 | 10 | `rpc-census`、`rpc_surface`（各 5） |
| `consumer_source_line` | 整数 | 9 | `rpc-reduction-candidates` |
| `producer_source_line` | 整数 | 8 | `rpc-reduction-candidates` |
| `first_decision` | `"文件.rs:行号"` 字符串 | 5 | `information_flow` |
| `consumer_decision_line` | `"文件.rs:行号"` 字符串 | 3 | `rpc-reduction-candidates` |
| **合计 8 个键名** | 153 整数 + 20 字符串 | **173** | |

那 71 个非 `line` 键全部在 M8.6 的表里；M8.5.1（6）与 M8.4.3（25）受影响的字段键名都是 `line`。


| 门（证据目录） | 守它的测试目标（权威轮实跑） | 被改写的已提交表（位移值数） | 合计 | 刷新入口（门自己公布的唯一改写口） |
| --- | --- | --- | --- | --- |
| M8.5.1 `data/evidence/m8/m8.5.1/` | `eth_call_semantics_evidence`（11 passed） | `lifecycle-contracts` 2、`ownership-matrix` 2、`reuse-verdicts` 2 | **6** | `M851_ETH_CALL_SEMANTICS_REFRESH=1` |
| M8.6 `data/evidence/m8/m8.6/` | `rpc_reduction_evidence`（17 passed） | `information_flow` 34、`rpc-census` 34、`rpc-reduction-candidates` 40、`rpc_surface` 34 | **142** | `M86_CENSUS_REFRESH=1` |
| M8.4.3 `data/evidence/m8/state-ownership/` | `state_ownership_evidence`（12 passed） | `lifecycle-contracts` 7、`ownership-matrix` 5、`reuse-verdicts` 6、`stage-dependency-matrix` 7 | **25** | `M843_STATE_OWNERSHIP_REFRESH=1` |
| **合计 11 张表** | 3 个目标，40 passed | | **173** | |

按造成漂移的生产文件分组（同一张表里不同锚点吃到不同的位移，所以一个文件可以出现两次）：

| 门 | 生产文件 | 该锚点观察到的位移（行） | 位移值数 |
| --- | --- | --- | --- |
| M8.5.1 | `crates/pipeline/src/arbitrage.rs` | 2 | 3 |
| M8.5.1 | `crates/pipeline/src/arbitrage.rs` | 23 | 3 |
| M8.6 | `crates/pipeline/src/arbitrage.rs` | 2 | 6 |
| M8.6 | `crates/pipeline/src/arbitrage.rs` | 23 | 44 |
| M8.6 | `crates/execution/src/sequence.rs` | 3 | 92 |
| M8.4.3 | `crates/pipeline/src/arbitrage.rs` | 23 | 3 |
| M8.4.3 | `crates/execution/src/sequence.rs` | 3 | 13 |
| M8.4.3 | `crates/execution/src/stage.rs` | 1 | 9 |
| **合计** | 3 个文件 | | **173** |

「位移 2 / 位移 23」是同一个文件的两个不同锚点各自吃到的位移，不是两个互相矛盾的数：
`arbitrage.rs` 在前面插了一段（把 266 行的锚点顶到 268，位移 2），后面又净插一批
（把 1465 行的锚点顶到 1488，从文件开头累计的位移是 23）。§3.1 的 numstat 是文件级的
+25/−2（净 +23）、+4/−1（净 +3）、+11/−10（净 +1），与此一致。

**这 173 处由三个互相独立的读数共同核对，脚本里断言三者相等：**

1. JSON 键级对照：把 `git show HEAD:<表>` 与工作树的表逐键走一遍，每张表的键集合前后完全相同，
   且每一个不同的值都能解析成行号（整数，或 `"文件:行号"` 字符串）⇒ 173 处。
   生成脚本里这两条分别是每张表的 `check("<表>: the refresh changed no shape")` 与
   `check("every value the refreshes changed is a line number that moved")`，后者把解析不出行号的值
   收进 `non_numeric` 并断言它为空。
2. `git diff -U0 -- data/evidence/m8` 的成对 `−`/`+` 行数 ⇒ 173 对；脚本用一条列出上面那 8 个键名的正则
   逐对匹配，要求键名与文件名相同、行号确实变了，匹配不上的进 `raw_bad`，
   由 `check("the raw diff classifies every changed line as a line reference")` 断言为空，
   再由 `check("the raw diff and the JSON walk count the same rewrite")` 断言 173 对 = 173 个 `−` = 173 个 `+` = 173 个 JSON 值。
3. `git diff --numstat -- data/evidence/m8` ⇒ **11 个文件、173 插入 / 173 删除**，
   逐文件为 2+2+2（M8.5.1）、34+34+40+34（M8.6）、7+5+6+7（M8.4.3）；
   脚本按目录聚合后与第 1 个读数的 `per_gate` 比相等
   （`check("the JSON walk's per-directory counts equal the numstat line counts")`），
   并对每个文件断言增删行数相等。

一句话概括这三读数的意思：**每一处改动都是「一个行号换了值」，一行没增、一行没删。**
没有动过的是叙述、判定、计数、token、列名、文件名、键名——manifest 里
`what_was_not_touched` 记的就是这一条，并且它由第 1 个读数的键集合比较得出，不是声明。
（该字段原文写作「only integers that name a source line differ」，是生成脚本里的措辞：
173 个里有 20 个是 `"文件:行号"` 字符串而非整数。数值本身与上表一致，只是那句话把类型说窄了；
manifest 已由脚本断言落盘、且脚本只能在提交前的工作树上重跑，故此处以报告更正措辞，不回改产物。）

为什么这不属于任务书 §9 禁止的「修改或重新生成无关历史证据」：三张表的行号之所以必须动，
是**本轮生产改动直接造成的**（相关历史），而不是本轮顺手重跑了别人的证据生成器；
处理方式也只有「通过门自己公布的刷新入口重新解析一次源码」，没有任何一处是手填的。
M12-B 在 commit d417f5c 里对同一类漂移做过同一处理（当时动的是 `data/evidence/m8/state-ownership/`
的 7 个 `"line"` 字段值），本轮沿用并把三门合起来的范围记在 manifest 的 `cross_milestone_anchor_refresh`
与 `known_staleness`。**断言本身一条都没放松**：三门在权威轮里都是**不带刷新变量**跑的
（`gates.test.test_detail_per_refreshed_target` 下三个目标各自的 `refresh_variable_set_for_this_run: false`），
实跑 11、17、12 全绿；带变量重跑时会红一条，
那是 refresh 模式下「改写发生在校验之后」的既有设计，取证写在 §11.7。

另外一类外部改写只有一处：`data/evidence/m10/manifest.json` 的 `git_commit` 会被 M10 的
`executor_evidence_gate` 在任何 workspace 测试运行时重印。本轮在提交前用 `git checkout --` 还原它，
缺陷本身按 §9 末段记为 out-of-scope finding 7，不改 M10 的代码。
门禁之后 `git status --porcelain -- data/evidence` 应该只看到 11 个 `M data/evidence/m8/...`
加一个 `?? data/evidence/m12/d/`，看到别的就说明有东西被意外重写了。

### 12.3 第 3、4、5 栏：测试统计（整份日志解析，不看 exit code）

| 口径 | 值 | 怎么读出来的 |
| --- | --- | --- |
| 测试目标数 | **133** | `Running …` 117 行 + `Doc-tests …` 16 行；与 `test result:` 统计行 1:1 配对后断言相等 |
| 目标构成 | 17 个 `unittests` + 100 个 `tests/*.rs` + 16 个 doc-test | 三类相加 = 133，脚本里断言 |
| passed | **1730** | 133 行求和；分侧为 680（单测）+ 1050（集成）+ 0（doc-test） |
| failed | **0** | 133 行状态词集合只有 `{ok}`，且求和为 0 |
| ignored | **29** | 求和；**没有计入 passed**，明细单列（见下） |
| measured / filtered out | 0 / 0 | 命令里没有 filter 参数 |
| doc-test 那一侧为什么是 0 | 16 个 doc-test 目标每行自报 `running 0 tests` | 仓库里没有文档测试用例；16 个目标计进目标数、0 计进 passed，不让它们伪装成测试 |
| 编译器警告与错误 | **0 行** | 门 3 与门 2 两份日志里以 `warning` / `error` 开头的行各 0 条 |
| 每目标自报运行时间合计 | **105.25 s**（门 3 墙钟 273 s） | 差值是 clean 之后 16 个 crate 的测试产物编译时间 |

两处自指必须写明，否则统计是自己造出来的：

- 整份日志里 **14** 行以 `test result::` 开头——全部落在 `evm-simulation` 的单元测试目标里，
  那是一个名为 `result` 的测试模块的用例行（例如 `test result::tests::a_fingerprint_covers_every_field ... ok`），
  **不是**统计行。解析按 `test result: <状态>. <数字> passed;` 的完整形状匹配，这 14 行被排除，
  `len(results) == len(targets) == 133` 才成立（同一口径下裸 `grep -c '^test result:'` 给的是 147，是错的）。
- 29 个 ignored **全部住在既有目标里**（17 个文件：`*_live`、`*_probe`、`record_*`、`m1_replay`、
  `real_chain`、`real_market_fee`、`triangle_probe`、`multihop_{capture,e2e,evidence}`、`executor_revm` 等），
  本轮新增数为 0：`git diff -U0 -- crates` 的新增行里 `#[ignore` 命中 **0** 条（断言），
  且全仓 `#[ignore]` 属性行 29 → 29（§3.2）。这 29 个是需要真实节点或真实密钥的那批，
  与门禁是否通过无关；29 个测试名字逐个列在 manifest 的 `ignored_test_names`。

### 12.4 里程碑回归（§11.6 承诺那张表）

归属不是手工挑的：对每个 `tests/*.rs` 用 `git log --diff-filter=A` 找到「新增它的提交」，
再从提交信息里取里程碑号；本轮 5 个新文件还没进 git，所以它们必然不在既有归属里——
这个缺口正好用来核对「100 = 95 个既有 + 5 个本轮新增」是断言出来的而不是凑出来的。

| 里程碑 | 集成测试目标数 | passed | failed | ignored |
| --- | --- | --- | --- | --- |
| M10（Executor：`executor_deploy/revm/lifecycle/evidence/evidence_gate/giwa_live`） | 6 | 56 | 0 | 2 |
| M11（Multi-Lane 与多跳：`multihop*`、`multi_optimizer`、`triangle_probe`） | 13 | 201 | 0 | 12 |
| M12-B（`readiness_gate`、`readiness_startup`、`node_reset_policy`、`node_reset_pending`、`pending_shape_compat`） | 5 | 55 | 0 | 0 |
| 本轮新增（`readiness_isolation`、`validation_gate`、`state_lifetime_recovery`、`endpoint_provenance`、`pending_radar_boundary`） | 5 | 34 | 0 | 0 |
| M1–M9.4 其余 | 71 | 704 | 0 | 15 |
| **集成目标合计** | **100** | **1050** | **0** | **29** |

M12-B 那一行的 55 里已经含本轮改写的两个文件：`node_reset_policy` 从 11 个用例长到 19 个（§3.2 的 +8，
文件从 1128 行到 2325 行），`readiness_startup` 保持 10 个用例（改的是注释与断言：+40/−37 行）。
两个文件都是**在既有目标里加用例或改断言**，没有新增测试目标，所以目标数仍然是 5。

结论按任务书 §10 的最后一类专项审查（M10/M11 回归）来说：M10 的 6 个目标与 M11 的 13 个目标
在同一把门禁里跑完，257 passed / 0 failed，被禁掉的只有那 14 个需要真实环境的用例（它们本轮之前就是 ignored）。
`crates/execution/src/*` 的本轮改动（D3 的 `EndpointKind::Unknown` 等）没有让任何一个 M10/M11 用例改变状态。

---

## 13. 未解决问题与明确的后续范围（任务书 §11.5、§9 末段）

任务书 §9 末段要求：发现但不属于本轮的缺陷要记**文件、函数、风险、证据、建议后续里程碑**，
不顺手扩大任务。下面 7 条是本轮审计发现后**只记录、未修改生产代码**的全部条目。
前两条是这一轮审计出来的实质风险，后面 5 条是范围纪律下的既有缺口。

### 13.1 发送走的是带一次重试的通用 POST 助手（最高优先，真实广播窗之前必须处理）

- **文件 / 函数**：`crates/chain/src/rpc.rs` 的 `request_with`（重试循环在 `:212` 的 `for _ in 0..2`），
  失败分类在 `one_attempt`（`:304`）。
- **调用者**：全仓唯一的生产发送点 `SequencerDirectSubmitter::submit`
  （`crates/execution/src/giwa/sequencer_direct.rs:346`，请求在 `:360` 的
  `request_raw("eth_sendRawTransaction", …)`）→ `request_traced`（`rpc.rs:168`）→ `request_with`。
  `submitter.rs:177` 的那处 `eth_sendRawTransaction` 只是标签字符串，不是第二个发送点。
- **风险**：三类失败被标成非终结因而**会重发**——
  `CLASS_SEND_FAILED`（`:311`，请求没能送达或响应没回来）、
  `CLASS_NON_JSON`（`:322`，节点已经收了但回了非 JSON）、
  `CLASS_HTTP_STATUS`（`:330`，非 2xx，例如网关 502）。
  后两类正好是「节点已接受、响应在回程丢了」的形状：第二次 POST 会把同一笔原始交易再送一次。
  进程内的 nonce 台账（D2 那条）拦不住它，因为**发出请求的是同一个进程、同一个 nonce**。
  JSON-RPC 错误负载与「没有 result」是终结的（`:339`、`:347`），这两类不会重发。
- **证据**：本轮读了这条路径并把它记进 `endpoint-provenance.json` 的 `holes_left_open`；
  本轮**没有**为它写测试，因为要让测试有意义就得改发送路径本身（§9 禁止重写 M10 的 Submitter 一类改动）。
- **建议里程碑**：真实环境验证（M12-E）之前的第一件事——把发送与读取的重试策略分开
  （发送要么不重试，要么重发前先查 pending 视图/收据确认前一次是否落地），
  并配一条「重发前必须先看链上是否已有该 nonce」的负向测试。

### 13.2 执行台账是进程内的，没有可回放的持久层

- **文件 / 函数**：`crates/execution/src/sequence.rs`、`crates/execution/src/stage.rs`
  各构造一份 `Ledger`；生产代码里没有任何地方反序列化 `ExecutionRecord`
  （`crates/live/src/*` 的 persistence 只服务 discovery 侧）。
- **风险**：进程在「已发送」与「已确认」之间重启，新进程看不见自己刚发的那笔；
  此时唯一拦它的是链上事实（pending 视图领先于 confirmed 视图）。
  如果那一刻池子恰好空了，它能把同一笔字节重新推导出来再发一次。
- **证据**：`state-lifetime-controls.json`（D2）的 `durable_state_limits[0]`，
  含 `state_lifetime_recovery.rs:942` 那条断言与 mut-4 的发现（§30 的去重是**每本台账各自**的去重）。
- **为什么本轮不修**：闭合它需要新建一个持久化执行存储，也就是任务书 §4/§9 禁止的第二套交易执行状态机。
- **建议里程碑**：M12-D 的真实环境验证之后，单独立一个持久化里程碑，
  把 ReceiptTracker 改成「能从一个存储里被回放起来」。

### 13.3 其余 5 条（既有缺口，本轮确认仍在，未扩大改动）

| # | 缺口 | 位置 | 本轮为什么不动 |
| --- | --- | --- | --- |
| 3 | 网关只回显 URL 的 **path** 部分时，脱敏不生效（key/JWT 常在 path 里） | `sequencer_direct.rs` 的 `without_the_endpoint` 只替换配置的整串 URL 及其加斜杠的前缀 | 改成删消息内容就等于改写节点的原文，这是 M12-B 立下的同类约束；M12-B 在 trace 侧记过同一条不对称，处理方式一致 |
| 4 | 读取侧错误串未脱敏：`read_error` 直接把传输层原文拼进 `ExecutionError` 显示文本 | `sequencer_direct.rs` 的 `read_error` | 本轮能到达的 `read_error` 调用点都以日志行结束、不进证据行；全仓扫一遍会把生产 diff 扩到缺陷类之外 |
| 5 | accepted 分支原样插入节点返回的 result 值 | `sequencer_direct.rs` 的 accepted 分支 | 发送的 result 是哈希；只有第 3 条那种网关回显才可能带 URL |
| 6 | M9.4 Radar 的入口没接进运行链路（`EarlyRadar` / `PreconfLink` 生产构造点 = 0） | `crates/live/src/preconf*.rs`、`crates/metrics/src/lib.rs` | §7 明令「如实记录其真实状态，不得声称端到端已通过」；§9 又禁止把 Radar 扩成执行框架，接线不是本轮动作。本轮做的是把它钉成结构性门（D5） |
| 7 | `crates/execution/tests/executor_evidence_gate.rs` 在任何 workspace 测试运行时重印 `data/evidence/m10/manifest.json` 的 `git_commit` | 该测试文件 | 与 §9「不重新生成历史证据」相冲突。本轮的处理是**每次门禁后把它还原**并记进 manifest 的 `known_staleness`；真正的修法（把字段钉成常量，或写进 scratch 目录）需要 M10 门的所有者同意改动，不属本轮范围 |

### 13.4 明确不在本轮范围的东西

多端点路由、故障转移、HA、多节点、负载均衡（§5、§9 点名禁止）；
部署节点或启动同步；任何真实签名/广播/资金操作；
`node_reset_policy` 之外的其他证据门整表化（本轮只闭合 §6 点名的这一张）；
把 Radar 接进执行链路。

---

## 14. 与 M12-C 部署与验证方案的衔接（任务书 §11.6）

`docs/v0.1/M12-C Single-Node Deployment and Verification Plan.md`（811 行）是本轮开工前读过的
四份前置文档之一。M12-D 不修改 M12-C 的交付物（那是上一里程碑的已提交文档，改动它等于替它的
作者重新下结论），下面这张表是**衔接记录**：M12-C 里因本轮而变旧的条目、变化的方向、
以及下一次修订该方案时的动作。同一份表写进 `data/evidence/m12/d/manifest.json` 的
`out_of_scope_findings_recorded_not_fixed` 与 `known_staleness`。

| M12-C 的位置 | 它当时写的 | 本轮之后的事实 | 修订动作 |
| --- | --- | --- | --- |
| §11 第 9 行、附录 B.4 第 1 行 | 缺陷：提交车道硬编 `EndpointKind::PublicHttpRpc`（`stage.rs:348`、`sequence.rs:1489`），本任务只记录不修复 | **已闭合**：两处现在传 `EndpointKind::Unknown`（`stage.rs:350`、`sequence.rs:1492`）；M12-C 建议借用的 `receipt_provenance` 现在在 `sequencer_direct.rs:543`，本轮另加 `submission_provenance`（`:561`） | §16 第 7 项「D4 残留仍未修复」的记录过期；下次修订把它改成「已由 M12-D 闭合，验收时证据应显示 `unknown`」 |
| §11 第 5 行 | `gate_readiness()` 在 `runner.rs:1104-1126`，调用点 `:970`（WS）/`:1005`（HTTP 轮询） | 定义移到 `:1118` 并改成 `pub`；调用点从 2 个变成 **4 个**（新增 `arbitrage.rs:373` 的一键套利、`cli/lib.rs:730` 的单笔校验） | §11 第 5 行的「未来节点验收」要扩成三个花费入口：同步中启动时 route run、arbitrage run、single validation 都必须退出且都不产生「无机会」结论 |
| §6.6 第 348 行 | 会话记录里打印 `rpc_url` / `ws_url` / `flashblocks_url` 与三者的 digest（`runner.rs:1652-1673`） | **三个明文键已删除**（HEAD 的 `:1652`、`:1653`、`:1654`），现在只剩 `*_purpose` + `*_endpoint_id`（`:1683`、`:1688`–`:1695`） | 操作手册里的核对步骤改成「在会话记录里 grep `rpc-<hex>` 摘要」，不要再写「确认 URL 出现在记录里」；这条变更是**只向前**的，旧会话文件仍带 URL |
| §11 第 7 行 | `endpoint_id()` 在 `rpc_trace.rs:938-941`，证据里只出现 `rpc-<hex>` | 仍成立（现 `:938`），本轮把它从读取侧推广到了提交侧 | 无需改，可以升格为「两侧同一条规则」 |
| §5.4 第 281 行 | 跨机场景：`EndpointPurpose` 五个字面量里没有「私网远端」取值，只记录不改代码 | 仍然如此——本轮没给读取侧词表加值（§9 范围纪律）。提交侧本轮的答案是不再猜：一律 `Unknown` | 保留为机器人侧待办；真实部署若是跨机，报告里必须显式写「不是本机节点」 |
| §9.3、§10 | 机器人这一侧吃什么；Flashblocks/WS 验收矩阵 | 本轮实测：M9.4 Radar 在生产代码里**没有构造点**（D5 的 t2 = 0），唯一会把 Radar 指向真实端点的 harness 是 `#[ignore]` 的 | §9/§10 的 Flashblocks 验收不能宣称「通过 run link 达成」；要么用 M9.4 的忽略 harness 单独驱动并写明，要么记 `NOT_RUN` |
| §12 故障注入 F-03 | 「重启失效」注入（节点重启） | 本轮把两件事分开了：**节点重启**（M12-C F-03 覆盖的）与**进程重启**（台账是进程内的，见 §13.2；发送重试见 §13.1） | F-03 之外建议加一条「已发送未确认时重启机器人进程」的注入项 |
| §13 阶段 9、§16 第 9 项 | 端到端第 9 阶段（真实交易）本轮不执行 | 仍未执行，本轮进一步给出该阶段的**前置条件**：§13.1 的发送重试不修，第一笔真实提交就带双发风险 | 修订时把「发送与读取的重试策略已分离」写进阶段 9 的准入条件 |
| §14 状态词表 | `NOT_RUN` / `NOT_VERIFIED` / `BLOCKED` / `NEEDS_CONFIRMATION` 等 | 本轮沿用同一批词，未新增状态词；新增证据目录 `data/evidence/m12/d/` 与 M12-C 规划的 `m12/c/` 分开 | 无需改 |

---

## 15. 未部署节点 · 未连接真实 RPC · 未签名 · 未广播（任务书 §11.7）

四条禁令的遵守情况按「本轮实际跑过什么」来交代，不按意图。

1. **未部署 GIWA 节点，未启动任何数据同步。** 本轮执行过的构建与验证类命令只有四条 cargo 命令：
   `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、
   `cargo test --workspace --no-fail-fast -- --test-threads=1`，以及门 2 之前那一次
   `cargo clean -p <16 个包>`（只删构建产物，理由与覆盖面见 §12.1）；其余全是只读扫描
   （git/grep/python 的读文件与统计）。没有 `docker`、没有 `compose`、没有快照下载、没有同步日志；
   对外动作只有四笔提交与其后按任务书授权的那一次 `git push`，都记在 §17。
2. **未访问真实 GIWA RPC / Flashblocks / L1。** 本轮 5 个新测试文件 + 2 个被改的测试文件里，
   带协议的 URL 字面量 host 全是 `127.0.0.1`（桩服务自己起的临时端口，6 处，口径见 §7 第 7 项）；
   文档用途的假域名 `candidate.test` **不在**这批文件里，它在被本轮修改的
   `crates/live/src/flashblocks.rs` 的 `#[cfg(test)] mod tests` 内（HEAD 就有），
   本轮把引用它的那条断言从明文降级为摘要（§11.4）。
   §11.4 的扫描口径给出全集。所有会打真实端点的 harness 仍然是 `#[ignore]` 的
   （全仓 `#[ignore]` 属性数 29 → 29，本轮没有解除任何一条，也没有新增）。
   JSON-RPC 的每一次回答都来自仓库内桩或录制 fixture。
3. **未签名真实资金交易，未广播。** §11.3 的对照显示：既有文件里 `eth_sendRawTransaction`
   的非注释行数 40 → 40（逐文件 delta 为空），`.sign(` 7 → 7；新增的 9 + 1 处全部位于
   5 个新测试文件，指向 127.0.0.1 桩。测试里唯一用到的密钥是仓库既有的合成 scalar-1 测试密钥，
   没有引入新密钥、没有读环境变量里的密钥（本轮 diff 的新增行里密钥类环境变量的**名字**
   只出现在说明文字中，值从未出现）。
4. **没有产生任何一笔链上交易，也没有把未执行的东西标为通过。**
   `SELF_HOSTED_NODE = NOT_RUN`、`LOCAL_CANONICAL_RPC = NOT_VERIFIED`、
   `LOCAL_FLASHBLOCKS = NOT_VERIFIED`、`MULTI_NODE_HA = OUT_OF_SCOPE` 四条判定原样保持
   （同时写进 `manifest.json` 的 `standing_verdicts`）。§4 的六条不变量结论全部是
   **代码 + 本地双桩 + 证据门**层面的结论，没有一条声称是「对某个真实节点的测量」。

一句话：本轮证明的是「闸门在没有真实节点时也拦得住、并且拦了会说什么」，
不是「节点就绪时长什么样」。前者是这一轮的任务书要的，后者只能在 M12-C 方案真跑起来的那一轮拿。

---

## 16. 复核命令（只读，可在任何工作树执行）

```bash
# 0) 构建环境（缺了它 clippy 会死在 rocksdb，见 M12-B 的记录）
export CC=clang CXX=clang++ CXXFLAGS="-include cstdint"

# 1) 三道串行门禁本身（门 2 之前先清这 16 个包的产物，否则 warm run 一个包都不重新分析，见 §12.1）
cargo clean -p evm-chain -p evm-cli -p evm-core -p evm-discovery -p evm-execution -p evm-graph \
  -p evm-live -p evm-metrics -p evm-opportunity -p evm-pathfinder -p evm-pipeline -p evm-protocol \
  -p evm-replay -p evm-risk -p evm-simulation -p evm-state
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast -- --test-threads=1
#   --no-fail-fast 是必需的：没有它，cargo 会在第一个失败目标后收摊，
#   本轮第一把就这样停在 71 个目标上（另有 46 个测试二进制 + 16 个 doc-test 目标没跑），见 §12.1 的替代表。
#   退出码从包装日志的 G*_EXIT= 行读，不要信任务完成通知。

# 2) 本轮 6 个测试目标：权威轮是整仓串行跑，六个文件已经在里面跑过一遍；
#    下面的单跑命令用于复现单个目标，manifest 的 gates.per_target_re_run 记录的是
#    「声明数 vs 权威门禁日志里的实跑通过数」逐个相等（9/5/7/8/5/19），不是另外打了一把门禁。
cargo test -p evm-pipeline --test readiness_isolation   -- --test-threads=1
cargo test -p evm-pipeline --test node_reset_policy      -- --test-threads=1
cargo test -p evm-pipeline --test pending_radar_boundary -- --test-threads=1
cargo test -p evm-execution --test state_lifetime_recovery -- --test-threads=1
cargo test -p evm-execution --test endpoint_provenance   -- --test-threads=1
cargo test -p evm-cli --test validation_gate             -- --test-threads=1

# 3) 生产 diff 与「发送/签名面没有变大」的对照
git diff --numstat HEAD -- 'crates/*/src/*'          # 10 个文件，+200 / -47
# 发送面的口径要挑对：git grep 的原始计数会把文档注释算进来（本轮 sequencer_direct.rs 原始 +1，
# 那一条是 /// 说明行），非注释行的口径才是 §11.3 表里的 40 -> 40。三行原始值应满足
# tracked-only 比 HEAD 多 1（那 1 行是文档注释），含新测试文件比 tracked-only 多 12（新文件里的
# 字面量：validation_gate 2、endpoint_provenance 9、state_lifetime_recovery 1；其中 3 行是注释，非注释口径是 9）。
git grep -c eth_sendRawTransaction HEAD -- crates | awk -F: '{s+=$NF}END{print "HEAD raw:",s}'
git grep -c eth_sendRawTransaction -- crates | awk -F: '{s+=$NF}END{print "WT raw 仅 tracked:",s}'
git grep -c --untracked eth_sendRawTransaction -- crates | awk -F: '{s+=$NF}END{print "WT raw 含 5 个新测试文件:",s}'
# 提交车道不再硬编类别：src 文件里第一个 #[cfg(test)] 之前、且不在注释行上的 EndpointKind::
# 引用只有 2 处，两处都是 Unknown（stage.rs:350、sequence.rs:1492）——和 manifest 的
# scans.production_endpoint_kind_sites 是同一个口径。剩下的 PublicHttpRpc 命中在枚举定义
# （submitter.rs:48）、词面映射（:61）、说明注释（:29）和 cfg(test) 之下的测试模块里。
# 注意：`git grep 'EndpointKind::' -- 'crates/*/src'` 这种写法跑不出结果（pathspec 少了一层），
# 加了 /*.rs 又会把测试模块的行一起带进来，所以这里用按文件截断到第一个 #[cfg(test)] 的 awk。
for f in $(git ls-files 'crates/*/src/*.rs'); do
  awk '/#\[cfg\(test\)\]/{exit} /EndpointKind::/ && $0 !~ /^[[:space:]]*\/\// {printf "%s:%d: %s\n", FILENAME, NR, $0}' $f
done
git grep -n 'PublicHttpRpc' -- crates/execution/src

# 4) 证据侧脱敏：新证据里不该有「数字 host + 数字端口」形状的端点 URL（本轮实测输出 no plaintext endpoint）
#    口径注意：这一条只拦真端点形状；`endpoint-provenance.json` 里另有 2 处 URL 形状的说明文本
#    （grep 模式与桩形状模板，都是回环地址），逐条交代见 §11.4。
grep -RnoE 'http://[0-9.]+:[0-9]+' data/evidence/m12/d/ || echo "no plaintext endpoint"
grep -c 'rpc-' data/evidence/m12/d/endpoint-provenance.json   # 实测 2

# 5) D4 整表一致性门的期望值来源（它读常量与源码，不读自己写的 json）
grep -n 'CONSISTENCY_FILE\|expected_side_reads_the_artifact' crates/pipeline/tests/node_reset_policy.rs

# 6) 门禁之后必须还原的历史证据
git status --porcelain -- data/evidence   # 只应看到本轮 m12/d/ 的新文件
git checkout -- data/evidence/m10/manifest.json 2>/dev/null || true
```

---

## 17. 判定

| 判据 | 结论 | 依据（可点开的产物） |
| --- | --- | --- |
| §1 的六条安全不变量 | **5 条成立；第 4 条按子情形分裂**：节点重启侧成立，进程重启侧不成立且按范围纪律只记录不修 | §4 表；§6.4、§13.2 |
| D1 readiness 与执行隔离补到真正花钱的入口 | 成立 | §5；`readiness_isolation.rs` 9 条含全仓 connect 扫描与它的负控制 |
| D2 节点重启的状态失效 | 成立（复用 M12-B 的 24 行表，不新建机制） | §6；`state_lifetime_recovery.rs` 7 条；`state-lifetime-controls.json` 的 5 轮 mutation |
| D3 端点用途标签与提交路径真实性 | 成立（localhost 不再被当成「本地提交」；读侧标签与提交侧词汇分离；无法证明目的地时记 `Unknown` 并阻止不符合配置的执行） | §7；`endpoint_provenance.rs` 8 条；`endpoint-provenance.json` |
| D4 证据门缺口（整表一致性 + 不循环自证） | 成立 | §8；`node_reset_policy.rs` 的 +8 条；`policy-table-consistency.json` 的 `planted_defects` / `controls` / `independence` |
| D5 Pending 与 Radar 的数据边界 | 成立（Radar 仍未接入生产，按 §9.1 如实记录，未把它写成执行框架） | §9；`pending_radar_boundary.rs` 5 条；`radar-status.json` |
| §8 的九项负向测试 | 逐项落位，且每项都指明它挂在哪个入口上 | §10 |
| §9 的十条禁令 | 全部遵守；改动面按 §3.1 的 numstat（10 个生产文件、+200/−47）核对为「未扩大」；本轮发现但不属于本轮的 7 条按文件/函数/风险/证据/建议里程碑记在 §13 | §11.1–§11.6、§13、§15 |
| §10 的三道串行门禁与六栏 | 全绿：fmt exit 0（0 字节日志）、clippy exit 0（clean 之后重新分析 16 个包、0 警告行）、test exit 0（133 目标 / 1730 passed / 0 failed / 29 ignored，ignored 未并入 passed）；六栏逐个取自日志正文，超时与未完成命令为 0 | §12.1、§12.3；manifest 的 `gates` 块 |
| §10 的六类专项审查 | 生产 diff、新增 RPC 预算、签名与广播路径、敏感信息、证据生成/校验的负向测试、M10/M11 回归（19 目标 257 passed / 0 failed）各成一节 | §11.1–§11.6、§12.4 |
| §11 的交付物 | 齐：代码与测试修复、本报告、`data/evidence/m12/d/manifest.json`、`node_reset_policy` 门的正向与负向证据、门禁日志的可追溯摘要（原始 `/tmp` 日志的解析结果全部进 manifest，manifest 每个数字都由生成脚本断言后才落盘） | §3.3、§12、`how_this_file_was_built` |
| §9「不得修改或重新生成无关历史证据」 | 成立：被改写的只有**本轮生产改动直接顶动**的三张门、11 张表 173 个行号值（键名 `line` 的 102 个，其余 71 个在 7 个同样指向源码行的键上），且三个独立读数一致（JSON 键级 / `-U0` 成对行 / numstat）；叙述、判定、计数、token 一字未动 | §12.2；manifest 的 `cross_milestone_anchor_refresh` |
| 「不得把未执行的真实环境测试标为通过」 | 成立：本轮所有结论都在代码 + 本地桩 + 证据门层面，四条真实环境判定原样保持 | §15、下面的判定块 |

任务书 §12 要求完成时仍然成立的四条，逐条抄在这里，没有任何一条因为门禁全绿而被改写：

```
SELF_HOSTED_NODE      = NOT_RUN
LOCAL_CANONICAL_RPC   = NOT_VERIFIED
LOCAL_FLASHBLOCKS     = NOT_VERIFIED
MULTI_NODE_HA         = OUT_OF_SCOPE
```

同一份内容也写在 `data/evidence/m12/d/manifest.json` 的 `standing_verdicts`（含一条 note：
本轮证明的是「闸门在没有真实节点时也拦得住、拦了会说什么」，不是「节点就绪时长什么样」）。

**交付与停止点。** 代码、测试、证据、文档分四笔提交后 `git push`（任务书允许的推送范围），
随后**停止并等待审查**：不部署节点、不接真实 GIWA RPC / Flashblocks / L1、不签名、不广播、
不自行进入真实环境验证。真实环境的验证入口是 `docs/v0.1/M12-C Single-Node Deployment and
Verification Plan.md`，衔接关系与本轮留下的三条必须先处理项在 §13、§14。
