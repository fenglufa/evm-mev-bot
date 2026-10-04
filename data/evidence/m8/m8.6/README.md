# M8.6 — RPC 削减机会普查（诊断，不改行为）

## 先说结论（白话版）

- 这条热路径每跑一遍打 **246 次** RPC（3 把实测、每把 82 次；表里按调用点分成 44 个站点、9 个方法）。
- 其中 **42 次**是在重复问刚刚问过的东西。这是「物理重复」，是理论上限，不是省下来的钱。
- 今天真正可以安全停掉的 RPC 是 **0 次**。
- 结论：**`"NO_SAFE_RPC_REDUCTION_FOUND"`**。

为什么是 0：重复的那 42 次里，第二次提问**本身就是那道检查**（preflight 的余额闸门、build 的before-snapshot、fee/nonce 的独立复核、M8.5.1 判过的 eth_call 重估）。删掉它省一条网络请求，代价是删一道闸门。M8.6 拒绝用验证换 RPC——这不是没找到重复，是重复全部长在闸门上。

单位提醒：上面三个数都是「一次逻辑请求」，重试已经折进同一条记录里（本语料每行 `attempts` 都是 1，所以逻辑请求数 = 物理 HTTP 数 = 246）。「时长」在这套表里只做诊断展示，任何优先级、任何节省都不读它。

## 打个比方（同一件事换算成钱）

把每把运行想成一趟出门办事：全程要问 82 次「现在几点、这笔钱还在不在」。其中有 14 次是出门前已经问过、回来又问一遍——这就是那 42 次重复里的每一把。问题是：第二次问话不是随手问的，它是保安在门口核对工牌。你要省的不是问话，是让保安别核对。所以这套表把「问了两次」和「第二次可以不问」分成两列，前者是 42，后者是 0。

## 这套文件是什么

M8.6 的诊断产物：10 张表 + 本页。语料是 M8.4.2 那三把 live BuildOnly 运行已经提交下来的记录，加上 M8.4.3 的所有权矩阵、M8.4.4 的传播结论、M8.5.1 的 eth_call 结论。本阶段**没有打任何一条新RPC**（`new_rpc = 0`），也没有跑任何实验：需要实验才能回答的那 1 个问题被标成 `EXPERIMENT_REQUIRED` 留在表里。

## 每张表读什么

| 文件 | 回答什么 | 关键数字 |
|---|---|---|
| `rpc_surface.json` | §4：热路径上真实存在的调用点，每个的文件与行号 | 44 个站点，9 个方法，246 次请求 |
| `rpc-census.json` | §18：逐站点的次数、两种时长口径、身份、属主、验证职责 | 44 行，合计 246 次请求 |
| `rpc-reduction-candidates.json` | §6：每个削减问题的完整一行 | 18 个候选 |
| `reduction-matrix.json` | §19：十二列判定（两个节省分列） | 理论 42 / 安全 0 |
| `rejected-opportunities.json` | §20：被否决的伪优化与其理由 | 排除 15 个 |
| `priority-queue.json` | §21：由证据决定的排序（禁止预先写死） | 前 3 项：1. "chainid.connect_to_build"；2. "chainid.connect_to_preflight"；3. "l1fee.preflight_oracle"
| `information_flow.json` | §10：答案变成什么、谁持有、下一个阶段谁用它 | 10 个容器类型，44 条流 |
| `saving-kinds.json` | §7：五种节省口径，永不互相相加 | "physical_duplicate_saving"=42，"semantic_reuse_saving"=0，"verification_removal_saving"=0，"parallel_wall_time_saving"=0，"total_safe_rpc_saving"=0
| `negative-controls.json` | §25：六个必须让门禁变红的改动 | 全绿 = true |
| `final-verdict.json` | §32：结论块 | `"NO_SAFE_RPC_REDUCTION_FOUND"` |

## 五种口径为什么不能相加

```text
physical_duplicate_saving   = 42
semantic_reuse_saving       = 0
verification_removal_saving = 0
parallel_wall_time_saving   = 0
total_safe_rpc_saving     = 0
```

前三个是「本可以省」的不同角度，第四个是时长（M8.3.3 的事，跟请求条数不同单位），第五个是**前四个里真正有证据支撑的那一小撮**——它是第一个的子集，不是四个的总和。把任何一个加进第五个都是 §24 第七条要拦的错。

## 怎么复现

```bash
export CC=clang CXX=clang++ CXXFLAGS="-include cstdint"
M86_CENSUS_REFRESH=1 cargo test -p evm-pipeline --test rpc_reduction_evidence -- --test-threads=1
```

不带那个环境变量时，同一个门做的是**字节比对**：把模型 + 记录重新装配一遍，与已提交的文件逐字节比。差异只能来自模型或记录的变化——表里没有墙上时钟，所以两次装配必然一模一样（§25 的 NC5/NC6 就是这条的反证实验）。

⚠️ `target/pipeline-tests/` 不能并发写：这两个门必须 `--test-threads=1`，并且整个 workspace 测试独占一个日志跑。

## 门禁

- `crates/pipeline/tests/rpc_reduction_evidence.rs`：§24 的十四条（总数可回算、逐方法/逐阶段可回算、候选数与重复数可回算、安全节省可回算、理论与安全不混、验证职责不许忽略、被否决必须有 blocker、优先级不许由时长排序位置推出、身份必须是业务字段、不许用时长排序后的行号、时间字段只能自证、不许改证据把候选改绿）+ 字节稳定 + 锚点解析。
- `crates/pipeline/tests/rpc_reduction_recompute.rs`：绕开表、直接从原始记录重算，再逐表对账；跑 §25 六个负控制；`new_rpc = 0` 的核算（本阶段一个 RPC 都没打，靠的是只读已提交的目录）。

## 这套证据不包含什么

- 不包含任何新的链上请求：246 次请求全部来自已提交的记录。
- 不包含签名、广播、真实套利（三把语料本身就是 `build-only`）。
- 不包含 M8.5.1 已判死的 eth_call 复用（继承 `REUSE_BLOCKED`，不重做）。
- 不包含 M8.4.4 已测过的块上下文传播（继承「安全但净省 0」，不重新列为候选）。
- 不包含任何 cache / batch / multicall / prefetch / 并发改动 / 安全门放宽。
- 不包含把 26 个「没有候选点名」的站点硬塞进优先级队列：那些行的 priority 明确写成 `no_candidate_names_this_site`，而不是借用 §14 的五个标签。

## 结论块（§32 格式）

```text
M8.6 RESULT

label = "NO_SAFE_RPC_REDUCTION_FOUND"

current_hot_path_rpc_count = 246
theoretical_reducible_rpc = 42
safe_reducible_rpc = 0

P0 = 0
P1 = 3
P2 = 0
P3 = 0
REJECT = 15

new_rpc = 0
signatures = 0
broadcasts = 0
real_arbitrage = 0
```

## 下一步（不是建议动手，是说明证据缺口）

- `"l1fee.preflight_oracle"`："§17's Candidate 1: does the same calldata at a different height answer the same question?" —— 记为 `"EXPERIMENT_REQUIRED"`，本阶段不执行（未来要单独授权）。
要让结论改变，必须先出现下面任何一条证据：

- produce record rows in which one pair of asks has all five of M8.4.2's reuse conditions resolved met, or a `safe_to_reuse` true in the published candidate record — the two kinds in `CREDITING_PROOFS`
- name the check that would still bear each duty §12 assigns, for every consumer whose read is itself the check
- show a numbered height and a tag resolving to the same answer for one site, which §4.2 of M8.4.2's rules currently refuses by construction
- settle the oracle's cross-height question by the controlled experiment §26 defers to M8.6.x / M8.7


按 §33：如果剩下的 RPC 大多是有业务语义的独立验证，那么 M8 的主要优化方向就不该再是「消 RPC」，而是 RPC 延迟、endpoint 架构、连接策略、provider 选择、地域与 sequencer 邻近度、请求调度、执行路径重设计。这些都**必须等 M8.6 之后**再定，且都不在本阶段的允许范围内。
