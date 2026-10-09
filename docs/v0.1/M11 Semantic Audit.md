# M11 多跳决策链 — 语义审计底稿（把任务书里的每个词换成一个 `file:line`）

任务书：`docs/v0.1/M11 Coding.md`（§1–§56）
基线 commit：`250cd26`（M10 完成报告入库之后）
证据目录：`data/evidence/m11/`（16 个文件）
固定夹具目录：`fixtures/simulation-m11/`（13 个文件 = 9 份 probe 表 + 1 份 capture 说明 + 1 份状态 dump + 2 份夹具）

**本文件的成稿时序先说清楚**：它是在代码落地之后写的，所以每一条都是**对着当前工作树重新现量**得到的读数，不是写码之前的回忆。第 1 节记录的是一条被现量推翻的任务书前提——任务书写下时它成立，动工时已不成立。

规则沿用 M9.3/M9.4/M10：**每条断言带 `file:line` 或一条命令的输出；没有读数的地方写「未测」，不写成已证。**

---

## 1. §5.1 的前提被现量推翻：pathfinder 早就在 workspace 里

任务书 §5.1：

```text
crates/pathfinder 存在，但 root workspace 没有正式纳入。
M11.1 必须确认并修复："crates/pathfinder" 进入 workspace。
```

现量（三条命令，都在本轮跑过）：

```text
grep -n "pathfinder" Cargo.toml
  → 11:    "crates/pathfinder",
  → 36:evm-pathfinder = { path = "crates/pathfinder" }

git diff --stat -- Cargo.toml        → 空（本轮没碰过这个文件）
git log --oneline -S '"crates/pathfinder"' -- Cargo.toml
  → 6579a7d M9.3 代码：新建 evm-pathfinder（目标块图上的有界环搜索 2–3 跳）
```

结论：**members 早在 M9.3 那次提交就写进去了**。§5.1 要修的洞不存在。

M11 真正缺的是一条**依赖边**——`evm-opportunity` 此前不认识 `evm-pathfinder`，所以多跳层拿不到 `CycleCandidate` 这个类型。这条边的成本在 `Cargo.lock` 里正好是两行（本轮 diff 实测）：

```text
 evm-opportunity 的 dependencies: + "evm-pathfinder"
 evm-simulation   的 dependencies: + "evm-pathfinder"   ← dev-dependencies，见 §10
```

因此 M11.1 的完成判据按「依赖边已建 + §5.1 要求的四条命令全过」来记，**不按「补 members」来记**；把 members 写成 M11 的交付物会是一句假话。

---

## 2. §6 `CycleCandidate`：只读，实测零改动

§6 要求候选类型继续只表达 `chain_id / target_block / ordered edges / cycle identity / hop count`，并禁止加入 `amount / profit / gas / simulation / risk / calldata / execution`。

现量（`crates/pathfinder/src/candidate.rs`）：

```text
127: pub struct CycleCandidate {
128:     pub chain_id: ChainId,
133:     pub target_block: BlockNumber,
137:     pub start_token: TokenId,
141:     pub edges: Vec<EdgeId>,
144:     pub hop_count: usize,
145:     pub canonical_key: CanonicalKey,
146:     pub fee_status: FeeStatus,
```

七个 `pub` 字段对齐 §6 要求的五项：`chain_id` ↔ chain_id，`target_block` ↔ target_block，`edges` + `start_token` ↔ ordered edges，`canonical_key` ↔ cycle identity，`hop_count` ↔ hop count；多出来的 `fee_status` 是 M9.3 就有的费率举证状态，不落在禁止清单上。

禁止清单逐条核对的结果：那七个词（amount / profit / gas / simulation / risk / calldata / execution）**一个都不是这个 struct 的字段**，而 M11 给它们各自新建了载体（§4 的表）。`crates/pathfinder` 对本轮工作树的 diff 为 0 行（`git diff --stat -- crates/pathfinder` 空、`git status --short -- crates/pathfinder` 空）——搜索层一个字没改。

---

## 3. 深度硬顶：3 跳是搜索层的上限，4 跳只能停在 declared

```text
crates/pathfinder/src/config.rs
  17: pub const MIN_MAX_HOPS: usize = 2;
  20: pub const MAX_MAX_HOPS: usize = 3;
  29: if self.max_hops < Self::MIN_MAX_HOPS || self.max_hops > Self::MAX_MAX_HOPS { … }
  40: Self::new(Self::MAX_MAX_HOPS)          // default()

crates/pathfinder/src/candidate.rs
  168: if edges.len() > crate::config::PathFinderConfig::MAX_MAX_HOPS { … }   // assemble 里直接拒
```

推出三件事，每一件都已在证据里落实：

1. **不存在**可被端到端驱动的四跳 `CycleCandidate`——不是「还没找到」，是搜索层的构造函数拒绝装配。
2. 所以 `data/evidence/m11/pricing/declared_4hop.json` 是手画图上算出来的价；REVM 从未执行过一条四腿路线。
3. 这一条同时写进 `manifest.json` 的 `not_measured[0]`（`why` 字段就指着 `config.rs` 与 `candidate.rs` 这两个行号），门禁有一相会核对「该项必须存在」。

---

## 4. 三级证明语义：`Estimated` 不等于 `Simulated`

| 层 | 载体（现量行号） | 它凭什么算这一级 |
| --- | --- | --- |
| 定价（Estimated） | `crates/opportunity/src/multihop.rs:486 price(route, input) -> Price`、`:366 MultiHopQuote` | 常数乘积折叠，全程 U256，不执行任何字节码 |
| 搜索（Estimated） | `crates/opportunity/src/multi_optimizer.rs:224 optimize`、`:201 search_domain` | 在发布域上离散取值，与暴力 oracle 对照 |
| 仿真（Simulated） | `crates/simulation/src/multihop.rs:376 SimulatedOpportunity`、`:250 executor_run`、`:289 SimulationStatus` | REVM 在 pinned 真状态上**实际跑过**，gas 与 delivered 是跑出来的 |
| 计划绑定 | `crates/execution/src/multihop_plan.rs:305 plan_from_simulation`、`:462 executable_plan`、`:64 MultihopPlanContext`、`:190 MultihopBinding` | hash 对不上就拒，不做二次序列化 |
| 风险判定 | `crates/risk/src/multihop.rs:216 MultihopAcceptance`、`:244 MultihopRiskDecision`、`:256 check()` | 只对已发布的数字做比较 |

市场断言只有两型，来自 M7 而不是 M11 新造：

```text
crates/execution/src/market.rs
  29: pub enum MarketKind {
  33:     RealMarket { attested_by: String },
  37:     ControlledFixture { proves: String },
```

§41 由此锁定：**`CONTROLLED_FIXTURE` 永远不构成真实市场判定；`REAL_PROFITABLE_ARBITRAGE` 恒为 `UNKNOWN`，不得写 0**——0 是一个判定（「已确认没有」），而 M11 只做到「没问这个问题」。

代码侧的两型与证据侧的拼写不是同一件事，这里分清楚（现量）：`MarketKind::name()` 只会打印 `"REAL_MARKET"` 或 `"CONTROLLED_FIXTURE"`，而三份 pricing 行里的 `market_claim.kind` 用了**三个更细的拼写**，各一份——`REAL_MARKET_POOLS_ON_A_CONTROLLED_FIXTURE_STATE`（录制对，池子是真的、executor 与余额是画的）、`CONTROLLED_FIXTURE`（声明三角形）、`DECLARED_SYNTHETIC_GRAPH`（四跳手画图，chain 7）。这两个非 `MarketKind` 的字符串是写入器自己选的标签（`crates/simulation/tests/multihop_evidence.rs:1688` `:1704`）。门禁的相 9 对每个拼写各自要求一组必填字段（`.../multihop_evidence_gate.rs:4801–4810`：前两者要 `proves` / `does_not_prove`，录制那份要 `attested_by` / `forbidden_reading`，`REAL_MARKET` 要 `attested_by`），**遇到没有规则的拼写就记一条 note**，所以标签拼写扩了而门禁没同步时，暴露的是 note 而不是绿灯。结论：§11 第 2 条的锁法要说的是「证据里的每一条市场断言必须落到门禁有字段要求的那几个拼写之一」，不是「必须等于 `MarketKind` 的两个名字」。

---

## 5. §28 边界：Risk 与 Lane 都拿不到传输能力（结构核对，不是口头承诺）

`crates/risk/Cargo.toml` 的 `[dependencies]` 整块现量：

```text
evm-core, evm-simulation, alloy-primitives, async-trait, serde, serde_json
```

没有 `evm-chain`、没有 `evm-live`、没有任何 http/ws 客户端 ⇒ Risk **在类型层面就无法**发 RPC、签名或提交；§28「Risk 永不可达 Signer / RPC send / Submitter」是依赖图的性质，不是一句注释。

`crates/execution/src/lanes.rs` 的 import 面（63–68 行）：

```text
std::cmp::Reverse · std::collections::BTreeMap · std::fmt
alloy_primitives::{Address, B256, U256} · serde::Serialize
```

账本只持有数字与标识，没有任何 provider / signer 句柄。

词面命中已扣自指：`lanes.rs` 里 `submit` 命中 3 处，分别是第 11 行 §36 状态机的**文档句**，与 `:146` `:147` 两个状态名字符串 `"submitting"` / `"submitted"`——零方法调用。**「submit」在这个 crate 里是一个状态名，不是一个动作。**

---

## 6. M10 复用面：没有新建 Executor / Signer / Receipt 框架

计划语义全部直接取 M10 已提交的 `crates/execution/src/arbitrage.rs`（行号现量）：

```text
578 new · 611 canonical_text · 639 plan_hash · 646 calldata · 652 to_call · 666 calldata_hash · 678 route_id
```

M11 侧不做第二次序列化。门禁的重算口径因此可以只有一行：
`keccak256(canonical_text)` 与 `keccak256(calldata bytes)`，两个数字与发布值同位并排打印——§43 要的「plan hash / calldata hash 独立可验」就是这两个数字，不是 `println!("PASS")`。

---

## 7. Lane 状态机：箭头只有一处定义

```text
crates/execution/src/lanes.rs
   88: pub enum LaneState { … }
  193: pub fn allows(self, next: LaneState) -> bool      // §32 的表全在这里
  271: pub enum LaneFailure { … }
  514: pub struct CapitalDomain { … }
  787: pub struct LaneLedger { … }
 1019: pub fn reserve_for( … )                            // §36：未被点名的 lane 拿不到预留
```

`allows` 是唯一的箭头来源（本轮读码确认 `Ready => matches!(next, Reserved | Rejected | Expired | Cancelled)`、`Reserved => matches!(next, Submitting | Rejected | Expired | Cancelled)`）。

门禁对 lane 阶段**不重放脚本**（重放等于再抄一份可能的错误），只重算三类「错误无法幸存」的不变量：每步资本两半相加等于容量、每条箭头必须在 `allows` 的表内、一个 nonce 不能同时属于两条 lane。

---

## 8. 真实侧：`real/` 三行为什么只能是 UNKNOWN

三条独立的原因，每条都有产物：

1. **录制里唯一的真实三角环不能被执行**。它所在池的 runtime 代码不含 swap 分发分支；实测在 `crates/simulation/tests/triangle_probe.rs`（12 道），结论记在 `fixtures/simulation-m11/probe-37224031-three-leg-slots.json`。⇒ `manifest.json` 的 `not_measured[1]`。
2. **M11 没有拿到签名/广播授权**，且 §2 禁止新建 executor contract。⇒ `not_measured[2]`。
3. **M10 的真实证据只有 M10 那一套**（2-hop 直连序列），它证明的是执行机制，不是 M11 的多跳计划。

`real/` 三行的 `evidence_that_does_exist_for_the_mechanism` 一共指向 6 个磁盘目标，本轮脚本逐个 `os.path.exists` 核对：`missing = 0`。

```text
data/evidence/m10/real/giwa_execution.json        ← 32 项对账行在这个文件的 reconciliation_32 里
data/evidence/m10/real/giwa_failure.json
data/evidence/m10/real/giwa_ladder_steps.json
data/evidence/m10/real/preconditions.json
data/evidence/m11/controlled/2hop/chain.json
data/evidence/m11/controlled/3hop/chain.json
```

（这里踩过一次坑：最初把 M10 路径嵌成 `previous_milestone` 子对象，而门禁相 9 只读顶层字符串，于是空文本被计入「有名字的目标」。现在的口径是**平铺键 + 写入期断言同标签不重复**。）

---

## 9. 跨文件连接一律用语义键（位置键按缺陷处理）

| 连接 | 用的键 | 现量位置 |
| --- | --- | --- |
| pricing 行 ↔ chain 行的 pricing 摘要 | `published.identity`（`RouteIdentity` 的最小旋转） | `multihop.rs:101` `:288` |
| 一次仿真 ↔ chain 行的 simulation 摘要 | `published.simulated_opportunity.identity_hash` | `simulation/src/multihop.rs:575` |
| lane ↔ 资本 / nonce | 候选 id 尾段 = `plan_hash` | `arbitrage.rs:639` |

理由与 M8.4.2 那次教训相同：**「排序后的第几行」「阈值切出的列表长度」这类位置身份，会在无关改动后指向另一个对象**，于是门禁既能假绿也能假红。本轮 chain 行的 id 尾段就是 plan hash，读者拿它可以直接定位到计划本身。

---

## 10. 写入器与门禁的口径分歧：本轮 32 条漂移的根因清单

写入器（`crates/simulation/tests/multihop_evidence.rs`，装配全部 16 个文件）与门禁（`crates/execution/tests/multihop_evidence_gate.rs`，只读、不写文件、用库 API 重跑 REVM）第一次对跑时报出 32 条不一致。

这 32 的口径（首轮门禁的输出，逐字誊录在此；那份运行日志在 `target/` 下、不入库且在提交前已删除，现量算法：门禁表头行「`32 published figure(s) did not survive recomputation:`」，与该行之后、`note: run with` 之前以 `[` 开头的行数一致）：
按相位分 —— `[lanes] 20`、`[unknown] 6`、`[risk] 3`、`[inventory] 2`、`[wrote nothing] 1`。
去重后是 **31 个不同的字段**：`manifest total_bytes` 被 phase 1（inventory）和 phase 11（wrote nothing）各报一次，因为两道都独立读同一个数——重复的是报告行数，不是缺陷数。
逐条定位后归为六类，**每一类都改的是口径或快照，没有一处是为了让门禁变绿而改数字**：

1. **`total_bytes` 的两个口径**。写入器统计「被摘要的那批文件」，门禁统计「整棵树」。修法不是取其一，而是新增 `total_bytes_scope` 把口径写成文字（且**不含任何数字字面量**——否则字段宽度一改，文案即假而门全绿），同时让门禁**两个数字都打印**，使「差一个 manifest」这件事可见而非隐含。
2. **`file_digests` 覆盖数**。原来断言相等，实际是 `digested + 1 == declared` 的关系（manifest 不能摘要自己，与 `self_digest` 同理）。改成关系判断。
3. **chain 行拷贝 stage 对象的语义**。`market_claim` 拷成**指针文本**（指向 pricing 文件），`what_this_is` 各文件自述；其余字段整对象拷贝。之前门禁把「文本不同」当漂移。
4. **NC12 正向臂与 NC15 到期缺 `lane_after` 快照**。这一条是**写入器缺陷**，不是生产缺陷：生产的 `ledger.end()` 确实把 lane 移到 `Expired`（磁盘终态已核）。修法是补快照，门禁侧同时新增规则「 granted 一个 pair 却不显示持有它的 lane = 报」。
5. **门禁 lane 相把规则挂在了 `control` 字段上**。§34 的承诺关于「是否发布了拒绝」，于是判据应挂在 `refusal` 字段；改挂在 `refusal` 后，另设 `control_positive_arm` 承接正向臂。已结算 lane 的资本不在 lane 上、活在池的另一半边，所以留痕的载体是 `CapitalDomain::settled_input`（`crates/execution/src/lanes.rs:543` 读、`:582` 在 commit 时累加），写入器把同一个数放进池快照与终态（`crates/simulation/tests/multihop_evidence.rs:1187` `:1515`），门禁按这个字段收口（`crates/execution/tests/multihop_evidence_gate.rs:3917` `:4135`）。这条现量核对过：证据目录里 `"claimed"` 出现 **0** 次，早期底稿写的「lane 账本加 `claimed`」是错的字段名。
6. **身份解析取错数字**。`ChainId(` 标签之后的那串数字才是链号；原先取「下一个 `(` 之后的数字」，读到的是后面某个字段的链号。

每类的验证方式相同：**备份 → 篡改（假指针 + `total_bytes` 各改 1）→ 门禁必须报出 → 逐字节还原（sha256 相同）→ 门禁再次全绿**。这条负对照留了脚本与还原记录，本轮跑出的报出条数与漂移内容一一对应。

---

## 11. 由本审计锁定、下游不得静默改的接口

1. `CycleCandidate` 的七个字段（§2 的「七字段 ↔ §6 五项要求」映射）与三跳硬顶（§3）。
2. `MarketKind` 只有两型；证据里的市场断言必须落到**门禁第 9 相有字段要求的那几个拼写**之一（§4 已列出代码侧两个 `name()` 与 pricing 侧三个 `market_claim.kind` 拼写的差异）。
3. Risk 与 Lane 的依赖面里不得出现传输层（§5）。
4. plan / calldata hash 只由 M10 的 `canonical_text` 与 `calldata` 决定（§6）。
5. lane 的合法箭头只由 `LaneState::allows` 定义（§7）。
6. 证据文件里的数字口径必须写成**关系或文字 scope**，不得写成死数字字面量；跨文件身份必须是语义键（§9、§10）。
7. 写入器只装配不判定，门禁只重算不写盘；`real/` 三行 verdict 恒为 `UNKNOWN`，`rpc_count` 恒为 0（除非有人真的授权了一次 live 抓取，那要另开一个里程碑）。

---

## 12. §50 的密钥形状门：口径、阳性对照与它明确看不见的东西

门禁第 10 相（`crates/execution/tests/multihop_evidence_gate.rs` 的 `boundaries()`）对 16 个文件的 JSON 逐个走 `strings_under`，谓词是 `is_secret_shape`：**长度恰为 64、全部为十六进制字符、且不含大写字母**。谓词被抽成函数，是为了让同相的负对照测试能调**真正在跑的那一条规则**，而不是抄一份。

现量口径（本轮脚本读 `data/evidence/m11/**/*.json` 的全部字符串）：

| 形状 | 目录里的条数 | 门的处置 |
| --- | --- | --- |
| 64 位纯小写 hex | 0 | 命中即报漂移（§50 要抓的形状） |
| 含大写字母的 64 位 hex | 0 | **不报**——规则要求全小写 |
| `0x` + 64 位小写 hex（66 字符） | 104（分布在 `plan_hash` 41、`keccak256` 25、`calldata_hash` 8 等 14 个键下） | **不报**——这是发布摘要的写法 |

因此两条边界是**规则的选择而非遗漏**：把 `0x` 前缀纳入会一次性制造 104 条假证据；把大写纳入目前没有可测收益（该类条数为 0）。这两条都写进了同文件的 `#[cfg(test)] mod secret_shape_control`：植入串在运行期构造（`"1".repeat(64)`，源码里绝不出现 64 位 hex 字面量——`crates/cli/tests/no_execution.rs:190` 会全文扫描 `crates/execution/tests/`），植入后扫描必须恰好报 1 条且路径指向植入位置，植入 `0x` 摘要形状则必须仍报 0 条。

**这条门看不见的东西要说明白**：一笔真写在证据里的 `0x` 前缀私钥，与摘要无法靠形状区分。本轮没有为它加键名白名单（改动面会扩到整个相 10 的口径），依据是写入器只写它自己的字段、且源码侧另有 M6 的字面量守卫；把它列为已知边界，不列为已证安全。
