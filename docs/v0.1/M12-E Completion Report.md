# M12-E 交易提交结果不确定性与重试安全 — 完成报告

任务书：`docs/v0.1/M12E Coding.md`
基线：`HEAD = origin/main = 90f0b5cf8892b9b7c114732fa5ce596cd70dbf61`（M12-D 完成笔，任务书 §2 的「预期包含 `90f0b5c`」与此一致）
证据目录：`data/evidence/m12/e/`
门禁：三道串行，全部 rc=0（§10 六栏实测见第 10 节）

---

## 1. 一页结论（白话版，先看这一段）

**改了什么。** 这轮之前，「把交易发给节点」和「向节点查数据」走同一个出口，而那个出口自带一次自动重试。查数据重试没有代价，发交易重试有代价：如果节点已经收下这笔交易、只是回执在路上丢了，客户端会自动再发一遍 —— 同一个 nonce 上的两笔交易。这轮把发交易单独拿出来：只有一个出口，那个出口只发一次。

**一个比喻，附单位换算。** 把提交交易想成寄一封挂号信，把 nonce 想成信上编号。旧行为：邮差把信投进邮局，回执没回来，他默认「没寄出去」，于是用**同一个编号**再寄一封（1 次调用 → 最多 2 次 HTTP POST）。新行为：只寄一封；回执没回来就在账上记一笔「编号 N 可能已在路上」，这个编号从此被占住，不再寄第二封，也不改编号重写（1 次调用 → 恰好 1 次 POST）。查询类的信（`eth_call`、`eth_getBlockByNumber` 等）仍然允许重寄一次，这轮没动它 —— 实测对照在第 3.4 节：同一个「第一次连接断掉」的故障，读侧桩服务收到 **2** 次请求，发侧只收到 **1** 次。

**核心验收用例的实测结果**（任务书 §7.B，「服务端已接收请求，但客户端没有收到响应」）：桩服务完整读走并记录了原始交易字节，然后把响应丢弃。实测这一笔调用：桩服务收到 **1** 次 `eth_sendRawTransaction`（总到达 2 次，另 1 次是连接时的 `eth_chainId`）；客户端结果 **unknown**；本地预期哈希 **保留**；执行 lane **held**（未释放）；对同一 nonce 的**第二次 allocate 被拒绝**。四个断言全部由桩服务的 socket 计数与生命周期返回值实测得出，不是函数调用次数断言。

**七类情形怎么分（任务书 §4 的表）。** 哈希一致且格式正确 → accepted（只代表端点收了这笔字节，不代表上链）；哈希不是我们这笔 → unknown，两个哈希都写进理由；连接失败 / 超时 / 非 JSON / 非 2xx → unknown，不重发；JSON-RPC error → 只有当报错说的是**这笔字节本身永远不成立**（gas 低于自身内在成本、类型不支持、费用字段自相矛盾、负值、chain id 不对、方法不在白名单）才算 rejected；`already known`、`known transaction`、`nonce too low`、`replacement transaction underpriced` 一律 unknown，且证据门把「这四种措辞永远不能当无罪证明」写成了一条可执行的规则。

**没做的事（任务书 §8）。** 没部署节点，没连真实 RPC / Flashblocks / L1，没用真实私钥，没签名也没广播任何交易，没做多端点路由/故障转移，没实现自动重发，没建持久化台账，没重写 M10/M11，没把 M9.4 Radar 接进生产路径，没动策略和 PathFinder。本轮唯一的 socket 是测试进程自己在 `127.0.0.1:0` 上起的桩。

**仍然没解决的事。** 进程重启后没人记得那笔「不确定」的交易：本地哈希、lane 占用、追踪状态都活在内存里。这正是 M12-D §13.2 记下的缺口，任务书 §8 明确禁止本轮去建持久台账，所以它继续开放，并且**本轮的本地哈希追踪不称为跨进程持久化**。

---

## 2. 基线与前置审查（任务书 §2 的六项，全部先取证再动码）

| # | 任务书要求 | 实测结论 | 取证方式 |
| --- | --- | --- | --- |
| 1 | HEAD / `origin/main` / 工作树 | `HEAD = origin/main = 90f0b5c…`，`rev-list --left-right --count` 为 `0 0`；开工时工作树只有一项未跟踪：任务书本身 | `git log`、`git rev-parse`、`git status` |
| 2 | 实际交易提交调用链 | 生产侧三个调用点，全部经 `TransactionSubmitter::submit`：`crates/execution/src/stage.rs:828`、`crates/execution/src/deploy.rs:570`、`crates/execution/src/sequence.rs:1974`；三个点都落在 `GiwaSequencerDirect::submit`，那里是唯一的发送点 | grep `\.submit(` 限定 `crates/*/src` |
| 3 | `request_with()` 对各类错误的现有处理 | 基线版是一个 `for _ in 0..2` 的循环，只有 `terminal: true` 才提前退出。`CLASS_SEND_FAILED`（连接失败/超时）、`CLASS_NON_JSON`、`CLASS_HTTP_STATUS`（含 5xx）三条都是 `terminal: false` ⇒ **会再 POST 一次**；`CLASS_NODE_REJECTED`、`CLASS_DECODE_FAILED` 是 `terminal: true` ⇒ 不重发。发送当时就骑在这个循环上，这就是任务书 §1 引用的 M12-D 结论 | `git show HEAD:crates/chain/src/rpc.rs`，读 `one_attempt()` 的五个 return |
| 4 | `SubmissionOutcome` 的全部变体与消费者 | 三个变体：`Accepted{transaction_hash,endpoint,detail}`、`Rejected{reason,endpoint}`、`Unknown{reason,endpoint}`。src 侧 8 个文件出现该类型，真正按变体分支的是 `stage.rs`、`deploy.rs`、`sequence.rs`、`lifecycle.rs`、`evidence.rs`；释放判定只有 `proven_not_in_flight()` 一个闸门，追踪判定只有 `tracked_hash()` | grep `SubmissionOutcome`、`proven_not_in_flight`、`tracked_hash` 限定 src |
| 5 | 哈希 / nonce / 执行记录 / 回执在提交前后的流转 | nonce 由 `ExecutionLane::allocate` 预约；哈希由签名交易对象 `transaction.hash()` 得到；`resolve_submission(&outcome)` 返回 `LaneRelease`；记录状态经 `ledger.advance(ExecutionStatus::Submitted)`；回执由 `receipt(hash)` 单独读。也就是说「本地哈希」在请求之前就存在，不需要 RPC 返回 | 逐点读 `stage.rs:820-860`、`deploy.rs:565-595`、`lifecycle.rs:781` |
| 6 | 现有测试对「服务端已接收但客户端没收到响应」的覆盖 | 只有一处，而且它把风险**记成了事实**：`crates/execution/tests/endpoint_provenance.rs` 里 `an_answer_that_is_not_an_acknowledgement…` 断言 `stub.sends() == 2`，注释写着「传输层自己的一次重试在这里被实测而不是被假设」。即基线状态下这条路径确实发两封 | `git diff crates/execution/tests/endpoint_provenance.rs`，见第 3.5 节的前后对照 |

任务书 §2 末段要求「不要只依据 M12-D 报告中的描述直接编码」：上表第 3、6 两条都是从基线源码与基线测试断言里直接读出来的，与 M12-D §13.1 的转述一致，但取证来源是代码本身。

---

## 3. E1 — 发送策略与读取策略分离（任务书 §3）

### 3.1 策略是参数，不是方法名表

`crates/chain/src/rpc.rs` 新增：

```rust
pub enum WirePolicy {
    RetryOnce, // 读：历史行为不变
    Once,      // 发：恰好一次 HTTP 请求
}

impl WirePolicy {
    fn attempts(self) -> usize {
        match self {
            Self::RetryOnce => 2,
            Self::Once => 1,
        }
    }
}
```

任务书 §3.2 明确「不要只通过字符串匹配 `eth_sendRawTransaction` 来决定是否重试」。所以循环的次数由调用方**显式选出的入口**决定，`rpc.rs` 里没有任何一处对方法名做匹配。理由写在该类型的文档注释里：靠方法名表推导策略，等于让下一个「发送形状」的新调用者在没人决定的情况下继承读侧的重试。

### 3.2 三个入口的落位

| 入口 | 策略 | 谁在用 |
| --- | --- | --- |
| `HttpChainAdapter::request`（trait 归一化路径） | `RetryOnce` | 全部状态读、`eth_chainId`、`eth_getTransactionReceipt` 等 |
| `pub async fn request_raw` | `RetryOnce` | `crates/chain/src/head.rs` 的单条 header 读取 |
| `pub async fn request_raw_once`（**本轮新增**） | `Once` | 唯一的生产发送点 |

发送点 `crates/execution/src/giwa/sequencer_direct.rs:370`：

```rust
.request_raw_once("eth_sendRawTransaction", json!([payload]))
```

`request_with()` 保留为 `request_attempting(…, WirePolicy::RetryOnce)` 的薄委托，其余逻辑（含 `terminal` 判定、`wire_error` 分类映射）未改；因此「明确的 JSON-RPC 错误仍与传输层错误区分」（§3.3 第二条）不依赖新代码。

### 3.3 trace 里的 attempt 数等于 socket 上的请求数

每完成一次尝试就往 `attempts` 里 push 一条 `RpcAttempt`，循环上限变成策略参数，于是 `Once` 下事件里最多只可能有一条 attempt。实测在 §9 项 7 那一把：

```
arrivals_in_order      = [eth_chainId, eth_sendRawTransaction, eth_getTransactionReceipt, eth_getTransactionReceipt]
socket_send_arrivals   = 1   trace_send_attempts    = 1
socket_receipt_arrivals= 2   trace_receipt_attempts = 2
total_arrivals         = 4   trace_total_attempts   = 4
dropped_events         = 0
```

这一行同时是对「不得把两次 HTTP 尝试伪装成一次」的直接检验：桩服务 counted，trace recorded，两者逐方法相等。

### 3.4 读侧未被改变的实测证据

| 用例 | 桩服务到达总数 | 其中发送 | 结论 |
| --- | --- | --- | --- |
| `the_read_side_still_asks_twice_when_the_first_socket_dies` | 3 | 0 | 1 次 `eth_chainId` + 同一方法 **2** 次读到达：读策略仍是 RetryOnce |
| `the_same_fault_counts_once_on_a_send_and_twice_on_a_read` | 4 | 1 | 同一把桩、同一故障形状：发送 1 次到达、读取 2 次到达。差值就是本轮建立的策略分离，而且是**同一次测量里**同时读出来的，不是跨把比较 |

### 3.5 M12-D 记下的那个风险，本轮前后对照

`crates/execution/tests/endpoint_provenance.rs` 的 `an_answer_that_is_not_an_acknowledgement…`：

- 基线（`HEAD`）：`assert_eq!(stub.sends(), 2, …)`，注释说明「第一次已被接收、响应没回来的话，字节会被交出去两次」。
- 本轮（工作树）：`assert_eq!(stub.sends(), 1, "M12-E §3: a send that never came back is not sent a second time. …")`。

这是任务书 §2 第 6 项「现有覆盖」的直接收口：同一条用例从**记录风险**变成**断言修复**。同一文件还有一处必要调整：该桩把 `Send::Refused` 答成 `-32003 nonce too low`，而 §4 之后这句措辞不再构成明确拒绝，于是改成 `-32003 intrinsic gas too low`（一个只关于这笔字节的事实），否则「收到明确拒绝 → 释放 lane」这条老断言会因为分类变保守而失效。

### 3.6 没有为了绕开重试而重写 Submitter

M10 的 `TransactionSubmitter` trait、`GiwaSequencerDirect` 的构造、执行状态机、`ExecutionLane` 都未重写：本轮在 trait 实现**内部**换了请求入口，并删掉一个字段（见 5.3）。生产 diff 的 6 个文件里没有新增状态机、没有新增服务（第 8 节自查表逐条）。

---

## 4. E2 — 提交结果的分类（任务书 §4）

### 4.1 任务书 §4 的七行，逐行的落位与实测

| 情形 | 本轮分类 | 实测证据（用例名 / 行值） |
| --- | --- | --- |
| 格式正确且与本地哈希一致的哈希 | `Accepted`，状态字 `submitted` | `an_acknowledgement_of_these_bytes_is_one_post_and_the_only_accepted_answer`：send_arrivals 1、`tracked_hash_equals_local=true` |
| 与本地哈希不一致的哈希 | `Unknown`，保留本地哈希，两个哈希都写进理由 | `a_hash_that_is_not_ours_is_one_post_and_never_an_acknowledgement`：send_arrivals 1、status_word unknown、`evidence_tracks_local_hash=true`、`hashes_agree=false`、`proven_not_in_flight=false` |
| 连接失败或超时 | `Unknown`，不重发 | `a_socket_closed_after_reading_every_byte_is_one_post_and_an_unknown_answer`；`a_response_that_comes_after_the_clients_own_timeout_is_one_post`（`client_waited_ms=20003`，`wait_was_the_adapters_own_timeout=true`，send_arrivals **1**） |
| 非 JSON 响应 | `Unknown`，不重发 | `a_body_that_is_not_json_is_one_post_and_an_unknown_answer`：send_arrivals 1 |
| 非 2xx HTTP | `Unknown`，不重发 | `an_http_status_that_is_not_a_refusal_is_one_post_and_an_unknown_answer`：send_arrivals 1（502 被读成「不知道」，不是「没送到」，正合 §4 的「默认不能证明请求未被接受」） |
| 无法明确解释的 JSON-RPC 错误 | `Unknown`，且**不因存在 `error` 字段就释放执行状态** | 走线层（`graded_at=wire`）17 行里 9 行 unknown，**9 行全部 `lane_release=held`**；另 1 行的 `error` 载荷读不出 code+message，仍是 unknown（`answered with an error payload this build cannot parse`，该行 `proven_not_in_flight=false`、没有 lane 字段可言）；`proven_not_in_flight` 在这 10 行上全为 false |
| 明确、可证明的拒绝 | `Rejected` | 走线测得 7 条 rejected 行，`lane_release=released`；依据见 4.2 |

### 4.2 `Rejected` 的依据：一张只列「这笔字节本身永远不成立」的短表

```rust
const DEFINITE_REFUSALS: [(&str, &str); 6] = [
    ("rpc method is not whitelisted", …),
    ("intrinsic gas too low", …),
    ("transaction type not supported", …),
    ("max priority fee per gas higher than max fee per gas", …),
    ("negative value", …),
    ("invalid chain id", …),
];
```

入选标准是一个问题（写在源码注释里）：**这些字节有没有可能被上一次请求收走？** 一个自相矛盾、类型不支持、gas 低于自身内在成本的载荷在任何节点状态下都不可能被接受，所以对它的拒绝不可能是在描述「已有一笔在飞」。其余答案一律出局。匹配是**大小写不敏感的前缀**匹配，不是子串搜索：网关把拒绝裹进自己的措辞里、或这张表从没见过这条消息，都落到 `Uncertain`，猜错的方向是 lane 继续被占到回执读出来 —— 不会多一个 POST。

`code` 只挣到一支臂：`const METHOD_NOT_FOUND: i64 = -32601`。一个不提供 `eth_sendRawTransaction` 的端点是在拒绝**这个调用**，背后没有载荷进入交易池。其它 code 本仓库实测过的是包装层（M6 探针里 -32000 与 -32003 都装过池内答案），所以数字进证据行、不进判定。

任务书 §4 允许 `Rejected` 的前提是「现有实现及可引用的接口语义足以证明」。本轮把这条变成机器核对：证据门用 `the_definite_refusal_list_is_production_s_and_the_rows_cover_it` 从**生产源码**里解析出这张表（含数组长度），逐条要求证据里有对应行；表里有、行里没有 ⇒ 门红。也就是说 `Rejected` 的清单不是我抄在文档里的，是门从代码里读出来再对上的。

### 4.3 四条措辞：本轮明确「不是无罪证明」

`already known`、`known transaction`、`nonce too low`、`replacement transaction underpriced` 四句：

- 分类：全部 `Uncertain` ⇒ `Unknown`（classifier 层实测 4 行 `refusal_read=uncertain`；走线层实测 4 行 unknown + `held`）。
- 证据门规则 `never_absolved`：只要某行的消息文本等于这四句之一，`status_word` 就不能是 `rejected`，`lane_release` 就不能是 `released`，否则该行违规、门红。
- 负控制 `nc4_a_pool_answer_marked_refused_is_caught_twice`：把一条池内答案**同时**伪造成 rejected 和 released，门必须两次都抓到。实测：ok。

### 4.4 未能确认的语义（任务书 §4 末段要求列出）

以下措辞本轮**没有**给出结论，读作 unknown，理由是拿不出可引用的接口保证：

| 措辞 / 形状 | 实测分类 | 为什么不能确认 |
| --- | --- | --- |
| `insufficient funds` | uncertain / unknown | 无法区分「因为余额被拒」与「先前一笔已把余额搬走」 |
| `nonce too high` | unknown（走线实测） | 只说明账户当前 nonce 低于这笔，不排除同 nonce 的另一笔被收过 |
| `transaction gas limit exceeds block gas limit` | unknown（走线实测） | 是链上限制而非载荷自相矛盾，不满足 4.2 的入选问题 |
| `internal error`（-32603） | unknown | JSON-RPC 保留码，语义是「节点内部出问题」，对这笔字节的下落什么都不说 |
| 从未见过的 code（实测用 4242424） | unknown | 凭空给某个码赋 GIWA 专属语义正是 §4 禁止的事 |
| `internal: intrinsic gas too low, reported late in a wrapper sentence` | uncertain | 前缀匹配不成立 ⇒ 出局。这是**故意**的：一条把真拒绝埋在句子中间的消息，本轮宁可占到回执读出为止 |
| `method not found` 的文案出现在 `-32603` 下 | uncertain | code 臂只认 -32601；文案不足以单独授权（`Intrinsic Gas Too Low` 的大小写变体则**确实**判 definite，实测在案） |
| `error` 成员是裸字符串 / 没有 message / body 不是 JSON | 读不出 code+message ⇒ unknown | 由 `parse_rpc_error` 四种形状那一条实测（`rejects_a_bare_string`、`rejects_a_body_that_is_not_json`、`rejects_an_object_without_a_message` 均 true） |

没有虚构：`-32601` 与「不白名单」这句文案来自本仓库 M6 探针的实测记录 `data/evidence/m6/probe-method-whitelist.txt`，其余具体码值都不进判定。

---

## 5. E3 — 本地哈希与不确定下的追踪（任务书 §5 的七条）

| # | 要求 | 实现与实测 |
| --- | --- | --- |
| 1 | 本地预期哈希来自签名交易对象 | `sequencer_direct.rs:365` `let local_hash = transaction.hash();`，位置在发请求之前 |
| 2 | 请求发送前即可获得 | 同上；`payload` 由 `transaction.raw()` 十六进制编码得到，两者都在 `request_raw_once` 之前算完 |
| 3 | 无响应时调用者仍知道这笔可能的交易哈希 | 每条 `Unknown` 的 reason 都带 provenance；`SubmissionOutcome::tracked_hash(local)` 对三种变体都返回本地哈希（`submitter.rs` 内单测 `the_tracked_hash_is_ours_whichever_answer_came_back` 覆盖）；证据里带该字段的 31 行全部 `tracked_hash_equals_local=true` |
| 4 | 返回哈希必须与本地比较 | 比较发生在构造枚举**之前**：`Some(hash) if hash == local_hash => Accepted` |
| 5 | 不一致不得走正常 Accepted 路径 | `Some(hash) => Unknown`，reason 里同时写出 `it named {hash:#x}` 与 `these bytes hash to {local_hash:#x}`；实测见 4.1 第 2 行；`nc5_a_lane_released_on_an_unknown_answer_is_caught` 从证据侧再堵一次 |
| 6 | `Unknown` 必须让生命周期保留 lane 与追踪状态 | `lifecycle.rs:781` 与 `deploy.rs:581` 的释放闸门是 `proven_not_in_flight()`，该函数只对 `Rejected` 返回 true；§7.B 实测 `lane_release=held`、`outstanding_still_carries_the_nonce=true`、`second_allocate_refused=true` |
| 7 | 日志/证据能区分四个量 | 证据行字段分立：`local_hash`（本地预期）、`node_returned_hash`（节点返回，实测那行两个哈希都印出）、`status_word`（提交结果 submitted/rejected/unknown）、回执侧由 `an_acknowledgement_is_not_an_inclusion_and_a_receipt_binds_to_the_local_hash_only` 单独测 |

### 5.1 端点标识一律脱敏，且 provenance 是可核对的形状

每条结果都带 `({provenance})`，形如 `over endpoint rpc-f5033ec1c5f3c860`，来自 `submission_provenance(url)` + `evm_chain::endpoint_id`；所有 reason 文本先过 `without_the_endpoint(...)`，把 URL 与 URL+"/" 换成同一摘要。实测证据里 38 处 `rpc-<16 hex>` 摘要、形状全部合规、11 条敏感针（含真实 URL、JWT、API Key 字样）在 49 行中零命中；`nc6_a_row_that_carries_the_endpoint_url_is_not_evidence` 证明「把 URL 塞回某一行」会被门拒。§9.6 那一把是主动注入：让传输层错误消息里**带上**端点 URL，读回来的字符串仍然 `carries_the_url=false`、`carries_the_credential=false`、`keeps_the_nodes_words=true`。

### 5.2 `Accepted` 不等于 `Mined` / `Confirmed` / 获利

`submitter.rs` 的文档注释与 `Accepted` 分支都只声明「端点返回了与这笔字节一致的哈希」；状态机把 `Submitted` 与后续回执阶段分开，`an_acknowledgement_is_not_an_inclusion…` 实测：ack 之后 receipt 读仍然只按**本地哈希**去要，`Accepted` 本身不推进到任何「已确认」语义。

### 5.3 `SubmissionOutcome` 字段的实际调整

删掉一个字段：`Accepted { hash_matches_local: bool }`。这是 §5「按实际消费者做最小调整」的落点 —— 该字段唯一的消费者是测试与 `evidence.rs`，而它存在的前提（先建 `Accepted` 再回看哈希是否匹配）正是 §5 第 5 条禁止的路径：比较必须先于变体构造。删除后的连带改动全部在测试侧：`endpoint_provenance.rs`、`executor_deploy.rs`、`executor_lifecycle.rs`、`lane_matrix.rs`、`sequence.rs`、`stage_matrix.rs`、`state_lifetime_recovery.rs`（五个文件各 ±1 行，另两个是本轮前后对照的主角，见 3.5）。生产侧只有 `evidence.rs` 少 2 行、`giwa/mod.rs` 多 2 行（re-export 分类函数）。

---

## 6. E4 — 不确定结果的处置（任务书 §6 的八条）

| # | 要求 | 实测 / 结构性证明 |
| --- | --- | --- |
| 1 | 不自动重发 | 发送入口只有一次尝试；§7.A 七案 send_arrivals 全为 1；证据门规则 `one_post_per_answer` + `arrival_arithmetic` 逐行核对 |
| 2 | 保留该交易的本地哈希 | §5 第 3 条；证据里带 `tracked_hash_equals_local` 字段的 31 行（`request_counts` 14 + `classification` 17）**全部为 true** |
| 3 | 保留现有 nonce lane 与执行追踪状态 | `LaneRelease::Held`，`outstanding_still_carries_the_nonce=true` |
| 4 | 不当成失败交易立即释放 nonce | 闸门 `proven_not_in_flight()` 只对 `Rejected` 为真；`nc5` 从证据侧堵住「unknown 却 released」 |
| 5 | 不因一次 receipt 为 null 断言从未被接受 | 回执路径未改（`sequencer_direct.rs` 的 `receipt()` 仍把 null 读作 Pending）；本轮没有为「不确定」新增任何断言性读取 |
| 6 | 不因一次 `eth_getTransactionByHash` 查不到就安全地再造一笔 | 本轮代码里没有该调用；发送侧无「查不到 ⇒ 可重发」的推理路径 |
| 7 | 不自动递增 nonce | `second_allocate_refused=true`：同一 nonce 在 held 期间第二次 allocate 被拒，因此不存在「换个号重发」的入口 |
| 8 | 不自动重签一笔替代交易 | 同上；签名只在 allocate→build 阶段发生一次，本轮未新增签名路径 |

三个生产调用点对 `Unknown` 的处理都是**阻塞**而非**继续**：`stage.rs` 返回 `Halt::Blocked`（理由串里明写「nothing is resent and the lane stays held」），`deploy.rs` 返回 `ExecutionError::SubmissionUnknown`，`sequence.rs` 走同一条未知分支。允许把 lane 交回去的只有 `Rejected`。

§6 末段的两条禁令：本轮没有新建第二套交易状态机，没有实现 reconciliation 服务，也没有实现自动重发 —— 第 8 节的自查表逐条列了证据。

---

## 7. E5 — 故障注入、测试矩阵与证据（任务书 §7 A/B/C/D）

### 7.1 两个新文件

| 文件 | `#[test]`/`#[tokio::test]` 数 | 角色 | 门禁实测 |
| --- | --- | --- | --- |
| `crates/execution/tests/send_uncertainty.rs` | 16 | 测量：对 loopback 桩发起真实 HTTP 请求，把 socket 计数与结果写成原始行 | 16 passed / 0 failed / 0 ignored，20.25 s |
| `crates/execution/tests/send_uncertainty_evidence.rs` | 15 | 证据门：从生产源码重解析锚点与拒绝表，校验 49 行、重建两张表并逐字节比对，含 9 条负控制 | 15 passed / 0 failed / 0 ignored，0.25 s |

### 7.2 测量 → 发布物的数据流（谁写了谁）

```
1) M12E_EVIDENCE_DIR=data/evidence/m12/e cargo test -p evm-execution --test send_uncertainty -- --test-threads=1
        → measured-rows.jsonl（49 行原始测量，逐行一次真实 socket 事件）
2) cargo test -p evm-execution --test send_uncertainty_evidence（不带 REFRESH）
        → 读那 49 行，用 17 条规则校验；把行**重新组装**成两张表并与已提交文件逐字节比对
3) M12E_SEND_UNCERTAINTY_REFRESH=1 同一命令
        → 先跑校验、只要有一行破坏规则就拒绝出版（「拒收红行」守卫），否则写文件后 panic
4) python3 /tmp/m12e_manifest.py
        → 只读第 3 节之后的门禁日志 + 已提交证据 + 工作树 diff，生成 manifest.json
```

时间戳（UTC，本地 CST = UTC+8）：原始行 18:55:48、两张表 18:55:57 ⇒ 门禁运行 19:05:45–19:11:08 ⇒ manifest 19:13:53。**门禁运行没有改写测量文件**：三道门跑完后 `data/evidence/m12/e/` 里除 manifest 外 mtime 不变，这一点由 mtime 实测（02:55 本地）与第 12 节的证据状态共同支撑。

### 7.3 §7.A 七案的实测表（计数来自 socket）

| 任务书 §7.A | 用例 | 总到达 | 其中 send | 结果 |
| --- | --- | --- | --- | --- |
| 1 有效且匹配的哈希 | `an_acknowledgement_of_these_bytes_is_one_post_and_the_only_accepted_answer` | 2 | 1 | submitted |
| 2 读完字节后关闭连接 | `a_socket_closed_after_reading_every_byte_is_one_post_and_an_unknown_answer` | 2 | 1 | unknown |
| 3 延迟响应、客户端超时 | `a_response_that_comes_after_the_clients_own_timeout_is_one_post` | 2 | 1 | unknown（等了 20003 ms，就是适配器自己的 20 s 超时） |
| 4 返回非 JSON | `a_body_that_is_not_json_is_one_post_and_an_unknown_answer` | 2 | 1 | unknown |
| 5 返回 HTTP 502 | `an_http_status_that_is_not_a_refusal_is_one_post_and_an_unknown_answer` | 2 | 1 | unknown |
| 6 返回明确 JSON-RPC 错误 | `a_json_rpc_error_is_one_post_whichever_way_it_is_read` | 2 | 1 | 三种答案各留一行：rejected ×1（`intrinsic gas too low`）、unknown ×2（`nonce too low`、`already known`） |
| 7 返回不匹配的哈希 | `a_hash_that_is_not_ours_is_one_post_and_never_an_acknowledgement` | 2 | 1 | unknown |

七案留下 9 行（第 6 案一案三行，见上表），全部 send_arrivals=1。「总到达 2」= `eth_chainId`（适配器连接时学 chain id）+ 1 次发送；证据行里 `methods` 数组把到达顺序逐条写清，规则 `arrival_arithmetic` 要求 `total_arrivals` 与 `send_arrivals` 之差与 methods 里的非发送方法数一致 —— 所以「2」不是一个含糊的总数，而是可展开的明细。

任务书 §7.A 的要求「记录桩服务实际收到的请求次数，不能只断言客户端函数被调用一次」的落点：`send_arrivals` 与 `total_arrivals` 由桩自己的 accept/read 循环累加，写进 `methods` 与两个计数；客户端一次函数调用的断言在这套证据里没有任何位置。

### 7.4 §7.B 核心验收用例的完整实测行

用例：`the_endpoint_took_the_bytes_and_the_answer_never_arrived_sends_once_and_holds_the_lane`

```json
{
  "fault": "§7.B: the endpoint read and recorded the raw bytes, and no answer came back",
  "methods": ["eth_chainId", "eth_sendRawTransaction"],
  "total_arrivals": 2, "send_arrivals": 1, "unconsumed_shapes": 1,
  "status_word": "unknown", "proven_not_in_flight": false,
  "tracked_hash_equals_local": true,
  "local_hash": "0x16d1f0bb41a80d306fed87471da7d911d4a779884ca3ba6ce0cd3cba559ace47",
  "lane_release": "held",
  "evidence_marks_bytes_as_sent": true, "evidence_outcome": "unknown",
  "evidence_tracks_local_hash": true,
  "outstanding_still_carries_the_nonce": true, "second_allocate_refused": true,
  "answer_line": "no answer from eth_sendRawTransaction (over endpoint rpc-54ad3aecafd23cb3): rpc request failed: error sending request for url (rpc-54ad3aecafd23cb3)"
}
```

七项要求逐项对上：桩完整读走并记录原始字节（`evidence_marks_bytes_as_sent=true`，且 `unconsumed_shapes=1` 表示这笔发送载荷已被读走、没有留在 socket 缓冲区里）；桩模拟接受后丢弃响应；客户端得 unknown；同一次调用没有第二次 POST（send_arrivals=1）；本地预期哈希被保留（`tracked_hash_equals_local=true`、`evidence_tracks_local_hash=true`）；lane 不因响应丢失而释放（`lane_release=held`）；额外一条 `second_allocate_refused=true` 把「同 nonce 不能再开第二笔」也测了。

**这条测试没有声称的任何事**：它不证明交易进入了区块，不证明节点真的接受了它，只证明「在端点已读走字节且回执丢失的情况下，我们的客户端只发一次、并占住这个 nonce」。

### 7.5 §7.C 分类实测（33 行，按 grading 层次分三档）

| 层次 | 行数 | 内容 |
| --- | --- | --- |
| `wire`（穿过桩 socket 的完整链路） | 17 | 7 行 rejected/released + 9 行 unknown/held + 1 行 unknown（`error` 载荷读不出 code+message） |
| `classifier`（直接问 `read_send_refusal`，没有 socket 挡路） | 15 | 8 行 definite + 7 行 uncertain |
| `payload reader`（直接问 `parse_rpc_error`） | 1 | 四种 `error` 形状：能读出 code+message / 裸字符串拒读 / body 非 JSON 拒读 / 缺 message 拒读 |

`refusal_reads = {definite: 8, uncertain: 7}`；`lane_releases = {held: 9, released: 7}`；`never_absolves = [already known, known transaction, nonce too low, replacement transaction underpriced]`；`definite_refusal_messages_from_production` 是生产表里的全部 6 条。

### 7.6 §7.D 生命周期回归（3 案）

| 用例 | 实测 |
| --- | --- |
| `the_read_side_still_asks_twice_when_the_first_socket_dies` | 读侧仍 2 次到达 ⇒ 策略未变 |
| `the_same_fault_counts_once_on_a_send_and_twice_on_a_read` | 同一次测量里 send=1、read=2 ⇒ 分离成立且不跨把比较 |
| `an_acknowledgement_is_not_an_inclusion_and_a_receipt_binds_to_the_local_hash_only` | submitted 不等于确认；回执只按本地哈希追踪 |

M10/M11 侧的既有回归见第 11 节（10 个目标全绿）。

### 7.7 证据门里的 17 条规则与 9 条负控制

规则（`validate()` 的失败标签）：`row_shape`、`case_is_a_test`、`fault_names_the_book`、`arrival_arithmetic`、`one_post_per_answer`、`read_attempts_follow_the_read_policy`、`in_flight_word`、`lane_release_word`、`local_hash_shape`、`classifier_recomputed`、`classification_reading`、`never_absolved`、`trace_matches_socket`、`no_endpoint_leak`、`single_run`、`every_test_recorded`、`definite_list_covered`。

其中三条值得说明：

- `classifier_recomputed` / `classification_reading`：门用**生产源码**里重新解析出的 `read_send_refusal`、`parse_rpc_error`、`DEFINITE_REFUSALS` 把每一行分类重算一遍，与行里记录的读法比较；行与代码不一致即红。分类证据不是「我说是 rejected」，是「门按当前代码重算也是 rejected」。
- `case_is_a_test` + `every_test_recorded`：双向暴露核对 —— 每行的 `case` 必须能在测试源码里找到一个同名 `#[test]`，且每个测试都必须至少留下一行。这样「删掉一个测试」与「凭空多写一行」都会红。
- `single_run`：同一次测量里同一 (case, fault, graded_at) 不允许出现两次，防止把重复记录当样本量。

负控制（全部实测 ok）：`nc1` 桩上多一次 POST ⇒ 到达规则红；`nc2` 伪造一个不存在的测试名 ⇒ 红；`nc3` 某测试不再记录 ⇒ 表短了；`nc4` 池内答案被写成 rejected **且** released ⇒ 两次都抓；`nc5` unknown 上释放 lane ⇒ 红；`nc6` 行里带上 URL ⇒ 不再是证据；`nc7` trace 与 socket 不一致 ⇒ 红；`nc8` 同一把测两遍 ⇒ 不算两次测量；`nc9` 从发布表读回的行仍是合法行（重建可逆）。

### 7.8 两张发布表是原始行的重组，不是第二份手写数据

`the_published_tables_are_the_rows_re_assembled` 用同一套 `assemble()` 从 49 行重建 `request-counts.json` 与 `classification.json` 的字节，再与磁盘上已提交的文件比较。表头里除 `schema`/`task_book`/说明字段外的每个统计数（`rows`、`cases`、`max_send_arrivals_in_any_case`、`status_words`、`refusal_reads`、`lane_releases`、`wire_policy`、12 条 anchors）都由行推导；`wire_policy.once_attempts=1 / retry_once_attempts=2` 是门从 `crates/chain/src/rpc.rs :: WirePolicy::attempts` 现读的，不是抄的。

---

## 8. §8 范围外事项自查（13 条禁令，逐条）

| 禁令 | 本轮是否触碰 | 依据 |
| --- | --- | --- |
| 部署 GIWA 节点 | 否 | 本轮没有运行任何节点进程；仓库历史里官方节点从未在本机跑起来过 |
| 启动节点或同步链上数据 | 否 | 同上；无新命令、无新脚本 |
| 连接真实 RPC / Flashblocks / L1 | 否 | 测试唯一 socket 是测试进程内 `TcpListener::bind("127.0.0.1:0")`；生产代码本轮未被执行 |
| 使用真实私钥或真实资金 | 否 | 全部用 §40 的合成标量-1 密钥（M6 已证该地址无资金） |
| 签名并广播真实交易 | 否 | 本轮签名字节只进 loopback；仓库真实提交历史仍是 M7 的两笔验证交易 |
| 多端点路由 / 故障转移 / HA / 多节点 | 否 | 未新增端点集合、未新增选择逻辑 |
| 实现自动重发策略 | 否 | 本轮删掉的是发送侧重发能力，没有加回任何重试计划 |
| 实现完整的持久化执行台账 | 否 | 见第 15 节，该缺口继续开放 |
| 重写 M10 Executor / Signer / ReceiptTracker | 否 | 三个模块未改动；`sequencer_direct.rs` 只换发送入口与分类 |
| 重写 M11 Multi-Lane | 否 | `lifecycle.rs` 的分配/释放结构未变，本轮只改了释放闸门的判据语义（原本就走 `proven_not_in_flight`） |
| 把 M9.4 Radar 接入生产执行路径 | 否 | 未接线 |
| 扩大到策略 / PathFinder / 套利算法 | 否 | `crates/pathfinder`、`crates/opportunity`、`crates/pipeline` 本轮零 diff（第 10.5 节的文件清单） |
| 把本轮哈希追踪称为跨进程持久化 | 否 | 报告与证据文件都写作进程内字段；`what_this_table_is_not` 里明写「重启即忘」 |

---

## 9. §9 九项安全与质量审计（逐条回答，答案指向测量）

1. **所有 `eth_sendRawTransaction` 生产调用点。** 扫描 `crates/` 下全部非测试 `.rs`、丢弃注释行：命中 2 处，其中 1 处在 `submitter.rs` 的 `#[cfg(test)]` 单元测试模块里（`submitter.rs:189`，是断言用的字符串），生产请求点**只有 1 处**：`crates/execution/src/giwa/sequencer_direct.rs:370`。manifest 的 `audit_9[0]` 把两个计数与 `behind_a_cfg_test_module` 标记都写了出来。
2. **绕开一次性路径的旁路。** 无。该结论有两层：上面那一处扫描 + 门 `the_send_path_anchors_resolve_and_no_submission_takes_the_retrying_entry` 断言发送锚点解析到 `request_raw_once` 且不存在第二处使用 `request_raw` 发交易的站点。
3. **仍有提交路径复用通用重试循环。** 无。`WirePolicy::Once` 只允许 1 次尝试（`attempts()` 现读），`request_with` 只作为 `RetryOnce` 的委托存在；证据侧 14 行「有答复的发送」每行 `send_arrivals` 都等于 `once_attempts`。
4. **`Unknown` 被转成 `Rejected` / `Accepted` / 空机会的路径。** 生产代码里没有：三个调用点对 `Unknown` 分别返回 `Halt::Blocked` / `SubmissionUnknown` / 阻塞分支，只有 `Rejected` 通向释放 lane 的失败路径。`request-counts.json` 16 行里带 `status_word` 的 14 行为 unknown 10、rejected 2、submitted 2（另 2 行是纯读侧与 trace 那把，没有提交结果可言），unknown 保持 unknown，未出现在被改写的方向上。
5. **哈希不匹配仍释放 nonce lane 的路径。** 结构上不可能：不匹配与缺失哈希都构造 `Unknown`，而释放只看 `proven_not_in_flight()`（仅 `Rejected` 为真）。证据门 `lane_release_word`、`never_absolved`、`nc5` 三处堵住反向记录。
6. **错误与 trace 是否泄露端点 URL / API Key / JWT。** 无泄露：49 行里 38 处 `rpc-<16 hex>` 摘要、形状全部合规、11 条敏感针零命中；§9.6 两把是**主动注入**（让传输层错误消息里带上 URL），读回仍 `carries_the_url=false`、`carries_the_credential=false`，且 `keeps_the_nodes_words=true`（脱敏没有吃掉节点的原话）。trace 侧的 scrub 继承 M12-B（`rpc_trace.rs` 本轮 8/8 行变化全是锚点重解析后的注释与行号，无行为改动）。
7. **trace attempt 数与真实 HTTP 请求数是否一致。** 一致：见 3.3 那一行（1/1、2/2、4/4、`dropped_events=0`），门规则 `trace_matches_socket` 逐行要求 `trace_send_attempts == socket_send_arrivals`。
8. **新增代码是否引入额外 RPC 调用。** 没有新方法：`git diff -U0 -- '*/src/*'` 的增删行里出现的 `eth_` 字面量只有 `eth_sendRawTransaction`（本来就有）。逐候选 / 逐交易的额外探测：**0** —— §7.A 每把的到达数都是 2（chainId + 发送），没有第三种方法混进来。
9. **是否有测试或构建过程意外修改历史证据。** 有两处，都按仓库既有惯例处理，见第 12 节：`data/evidence/m10/manifest.json` 的 `git_commit` 被 `executor_evidence_gate` 重印 ⇒ 提交前还原为已提交字节；10 个 M8 表格因本轮移动了 `rpc.rs` 的行而只剩锚点行号变化 ⇒ 逐叶核对确认「仅行位置」后随证据笔提交。

---

## 10. §10 三道串行门禁实测（六栏齐全，解析整份日志而非退出码）

### 10.1 第 1、2 栏：命令、退出码、耗时、UTC 窗口

运行环境：`CC=clang CXX=clang++ CXXFLAGS="-include cstdint"`（缺它 clippy 会死在 rocksdb，见 M12-B 的记录）。严格串行，一条起、一条落、日志各自留存，无并行 cargo。

| 顺序 | 命令 | rc | 墙钟 | UTC 窗口 | 日志 |
| --- | --- | --- | --- | --- | --- |
| 预备 | `cargo clean -p` × 16 个工作区包 | 0 | 13 s | 19:05:47 → 19:06:00 | `/tmp/m12e_gate2_clean.log` |
| 门 1 | `cargo fmt --all -- --check` | 0 | 2 s | 19:05:45 → 19:05:47 | `/tmp/m12e_gate1_fmt.log`（0 字节、0 行） |
| 门 2 | `cargo clippy --workspace --all-targets -- -D warnings` | 0 | 28 s | 19:06:00 → 19:06:28 | `/tmp/m12e_gate2_clippy.log` |
| 门 3 | `cargo test --workspace --no-fail-fast -- --test-threads=1` | 0 | 280 s | 19:06:28 → 19:11:08 | `/tmp/m12e_gate3_test.log` |

**为什么门 2 之前要清 16 个包**：warm clippy 一个包都不重新分析，其日志无法说明这道门的曝光量。清理只覆盖工作区包（依赖保持缓存），清理列表与 `Cargo.toml` 成员解析出的包名**逐一对齐**（manifest `cleaned_every_workspace_package=true`，16/16）。冷跑日志里 `Checking`/`Compiling` 单元 16 条、去重后包名 16 个，自报 `Finished dev profile … in 27.17s`；`warning`/`error` 行 **0** 条（`-D warnings` 下任何一条都会让 rc 非 0）。

### 10.2 第 3、4 栏：测试目标总数与三态计数（ignored 不计入 passed）

解析用的正则要求 `^test result: (ok|FAILED)\.`。这个限定不是装饰：`evm-simulation` 里有一个名为 `result` 的测试模块，其用例行形如 `test result::tests::… ... ok`，宽正则会把 14 条模块用例行当成目标结果行（宽 `^test result:` 计 149，严格计 135）。

| 指标 | 实测 |
| --- | --- |
| 测试目标总数 | **135**（`Running`/`Doc-tests` 头部 135 条，结果行 135 条，`targets_without_a_result_line = []`） |
| 目标种类 | unittests 17、集成测试 102、doc-tests 16 |
| `passed` | **1761** |
| `failed` | **0** |
| `ignored` | **29**（单列，不并入 passed；本轮**没有新增**任何 `#[ignore]`，`ignored_added_by_this_round=0`） |
| `measured` / `filtered out` | 0 / 0 |
| 各目标运行时间合计 | 128.53 s（墙钟 280 s，差值是编译与目标间开销） |

29 个 ignored 全是既有目标（示例：`capture_contiguous_corpus`、`capture_real_block`、`capture_the_real_three_hop_cycle_state`、`historical_census_is_reproducible_and_lands_in_the_graph`、`m11_cycle_state_files_are_written`），它们要么需要真实广播窗口、要么需要一次性取态写盘，属 M8.1/M11 记录的既有状态。`sequencer_direct_probe` 那一个目标本轮结果是 `passed 0 / ignored 1`，报告把它记为 ignored 而不是通过。

### 10.3 第 5 栏：警告、超时、未执行目标

`cargo test` 日志里 `^warning: ` 行 **0**；测试超时提示行（`has run for`）**0**；`^thread .* panicked` **0**；`--no-fail-fast` 且 `filtered out` 合计 0 ⇒ 没有目标被跳过执行；135 个头部与 135 条结果行一一配对，无孤儿。

### 10.4 第 6 栏：新增负向测试各自的实际执行结果

本轮写了 31 个测试（16 + 15），在门禁日志里逐个查名，**31/31 全部 `ok`，0 个 NOT IN LOG**。九条证据门负控制：`nc1`…`nc9` 全部 ok（名单见 7.7）。生产侧分类负控制与走线用例逐条 ok；两处「前后对照」`endpoint_provenance` 从 asserts 2 改成 asserts 1 后仍 ok（8 passed / 0 failed）。

### 10.5 生产 diff 与发送路径调用次数审计

| 文件 | + | − | 角色 |
| --- | --- | --- | --- |
| `crates/chain/src/rpc.rs` | 103 | 13 | `WirePolicy`、`request_raw_once`、循环次数化参数 |
| `crates/chain/src/rpc_trace.rs` | 8 | 8 | 锚点重解析后的注释/行位移，无行为改动 |
| `crates/execution/src/giwa/sequencer_direct.rs` | 196 | 18 | 一次性发送 + 哈希比较 + 保守分类 |
| `crates/execution/src/submitter.rs` | 37 | 19 | 删 `hash_matches_local`、`proven_not_in_flight`、`tracked_hash` 文档 |
| `crates/execution/src/giwa/mod.rs` | 2 | 1 | re-export `parse_rpc_error`/`read_send_refusal`/`RefusalRead` |
| `crates/execution/src/evidence.rs` | 0 | 2 | 适配删除的字段 |
| 合计 | **346** | **61** | 6 个生产文件 |

测试侧：改 7 个既有文件（`endpoint_provenance.rs` 20/12、`lane_matrix.rs` 8/12、其余五个各 −1 行，全是删除字段的连带）、新增 2 个文件（31 个测试函数）、新增 3 个证据文件。策略/机会/路径相关 crate（`pathfinder`、`opportunity`、`pipeline`、`simulation`、`live`、`metrics`、`risk`、`replay`、`graph`、`state`、`protocol`、`core`、`discovery`、`cli`）**零 diff**。

发送路径调用次数：一次 `submit()` = 1 次 `eth_sendRawTransaction` 到达 + 1 次既有 `eth_chainId`（适配器构造时那次，本轮未新增）。`request-counts.json` 发布表共 **16 行** = §7.A 9 行（七案，第 6 案三行）+ §7.B 1 行 + §7.D 3 行 + §9.6 2 行 + §9.7 1 行；§7.C 的 33 行属另一张表（`classification.json`），不带到达计数。这 16 行里 **send_arrivals > 1 的行数为 0**（`max_send_arrivals_in_any_case = 1`；§9.7 那行的计数字段名是 `socket_send_arrivals`，值同样为 1）。读侧 `max_read_arrivals_in_any_case = 2` —— 这两个数一起写在同一张表头，正是 §3.3「读侧不变、发侧一次」的数值形态。

---

## 11. M10 / M11 回归（任务书 §7.D 最后一条）

门禁日志里逐个查名的 10 个目标，全部 failed=0：

| 目标 | passed | ignored | 秒 |
| --- | --- | --- | --- |
| `tests/endpoint_provenance.rs` | 8 | 0 | 0.06 |
| `tests/executor_deploy.rs` | 12 | 0 | 0.12 |
| `tests/executor_evidence_gate.rs` | 1 | 0 | 0.52 |
| `tests/executor_lifecycle.rs` | 9 | 0 | 0.15 |
| `tests/lane_matrix.rs` | 14 | 0 | 0.14 |
| `tests/real_validation_receipt.rs` | 7 | 0 | 0.01 |
| `tests/sequence.rs` | 44 | 0 | 1.22 |
| `tests/sequencer_direct_probe.rs` | 0 | 1 | 0.00 |
| `tests/stage_matrix.rs` | 17 | 0 | 0.11 |
| `tests/state_lifetime_recovery.rs` | 7 | 0 | 0.21 |

ReceiptTracker、Signer、M11 Multi-Lane 与状态机（`lifecycle.rs`/`sequence.rs`/`stage.rs`/`deploy.rs`）的生产代码未被本轮改动，其上测试保持原样通过；M11 的 multi-lane 目标也在门 3 的 135 个目标里跑绿（`failed` 全局为 0）。

---

## 12. 历史证据的处理（任务书 §9 项 9、§10 末段）

| 文件 | 变化 | 处理 |
| --- | --- | --- |
| `data/evidence/m10/manifest.json` | 1 行：`git_commit` 被 `crates/execution/tests/executor_evidence_gate.rs` 重印 —— 任何一次 `cargo test --workspace` 都会做这件事 | 按仓库既有惯例**还原为已提交字节**（`git checkout --`），并把该行为作为「记录未修」条目写进 manifest 的 `out_of_scope_findings_recorded_not_fixed`，不在本轮改 M10 的门 |
| 10 个 M8 表格（`m8.5.1/call-surface.json`、`negative-controls.json`、`m8.6/information_flow.json`、`rpc-census.json`、`rpc-reduction-candidates.json`、`rpc_surface.json`、`state-ownership/` 四表） | 共 44 个叶值 | 逐叶 JSON 结构 diff 核对：路径形状与叶数完全相等，每个差异值都落在 `line` / `source_line` / `adapter_line` / `producer_source_line` / `asked_at` 五个「行位置」键之一（manifest 的 `every_changed_leaf_is_a_line_position=true`，程序核对，不是叙述）。原因是本轮在 `rpc.rs` 里插了行 |
| `data/evidence/m12/d/endpoint-provenance.json` | 未改 | 其提交侧 `public_http_rpc` 措辞被 §5 的 `over endpoint <digest>` 取代；后续里程碑不回头改历史证据，只记录 |
| 手工构造 `Rejected { reason: "nonce too low" }` 的 5 处测试夹具 | 未改 | 它们不经过分类器，因此不与生产路径矛盾；但按 §4 这是一个不好的新测试模型，报告点名而不顺手改 |
| `data/evidence/m12/e/` | 新增 | 本轮自有证据目录，与历史目录无冲突 |

---

## 13. 任务书 §11 要求报告说明的八件事（索引）

1. 发送与读取重试策略如何分离 → 第 3 节（`WirePolicy` + `request_raw_once`，实测对照在 3.4）。
2. 哪些提交结果归类 `Unknown` → 第 4 节 4.1 与 4.4（七情形 + 未确认语义清单）。
3. 本地预期哈希如何在不确定结果下保留 → 第 5 节（`tracked_hash`；证据里带 `tracked_hash_equals_local` 的 31 行全部为 true）。
4. 「已接收但响应丢失」测试的实际结果 → 第 7.4 节整行 JSON（1 次 POST、哈希保留、lane held、第二次 allocate 被拒）。
5. JSON-RPC 错误分类依据及未能确认的语义 → 第 4.2、4.3、4.4 节。
6. 所有测试门禁的完整统计 → 第 10 节六栏（135 目标 / 1761 passed / 0 failed / 29 ignored / 0 警告 / 0 超时 / 0 未执行）。
7. 本轮没有部署节点、没有连接真实 RPC、没有签名或广播真实交易 → 第 1 节与第 8 节自查表；manifest 的 `standing_verdicts` 七个 false。
8. 进程重启后的持久化执行台账尚未解决 → 第 15 节第一条；manifest `process_restart_persistence = "UNSOLVED"`。

---

## 14. 任务书 §12 的十一条完成标准（逐条判定）

| # | 标准 | 判定 | 指向 |
| --- | --- | --- | --- |
| 1 | `eth_sendRawTransaction` 不再经过自动重试的通用读取路径 | **满足** | 3.2、9 项 2/3 |
| 2 | 一次提交调用最多发出一次 HTTP POST | **满足** | 7.3 七案、`max_send_arrivals_in_any_case=1` |
| 3 | 读取侧原有重试行为保持通过回归测试 | **满足** | 3.4（读 2 次）、10.5、11 节 |
| 4 | 本地交易哈希在提交前确定，并在 `Unknown` 情况下可追踪 | **满足** | 5.1、7.4 |
| 5 | 哈希不匹配不能作为正常成功处理 | **满足** | 4.1 第 2 行、5 第 5 条、`nc5` |
| 6 | 无响应、非 JSON、HTTP 错误和不确定 JSON-RPC 错误不触发自动重发 | **满足** | 7.3 案 2–6、7.5 |
| 7 | `Unknown` 不被误当明确拒绝，也不随意释放 nonce lane | **满足** | 4.3、6、`never_absolved` |
| 8 | 「服务端已接收但响应丢失」的负向测试通过 | **满足** | 7.4（门禁日志里 `ok`，20.25 s 那把） |
| 9 | M10/M11 回归与全部串行门禁通过 | **满足** | 第 10、11 节（rc 全 0，failed 全 0） |
| 10 | 历史证据没有被无关修改 | **满足** | 第 12 节：M10 还原、M8 仅行位置且逐叶核对、M12-D 与夹具未动 |
| 11 | 完成报告和 manifest 的统计相互一致 | **满足** | 本报告每个数字都取自 `data/evidence/m12/e/manifest.json` 或它解析的三份日志/两份表；复核命令在第 16 节 |

---

## 15. 未解决问题与明确的后续范围（不顺手扩大）

1. **跨进程持久化（最高优先，与 M12-D §13.2 同一个洞）。** 本地哈希、lane 占用、追踪状态都是内存字段；重启即忘，下一轮可能在同一 nonce 上重发。任务书 §8 禁止本轮建台账，故移交独立里程碑。
2. **回执侧对 held lane 没有时限。** `Unknown` 之后 lane 一直被占到回执读出为止，没有 deadline、没有向操作员的升级动作。这不是本轮的缺陷修复对象（§6 只要求「不要当成失败释放」），但值得和一个真实广播窗一起设计。
3. **哈希不匹配只以文字承载。** 两个哈希写进 reason 字符串，下游无法把「mismatch」当机器值分支。§5 要求字段只按实际消费者调整，本轮没有消费者，所以不加。
4. **`without_the_endpoint` 是精确串替换，不是前缀树脱敏。** URL 被拆词、或同主机报出不同 path 时可能残留片段；实测 11 条针零命中，故记录未修。
5. **端点摘要含端口，跨里程碑不可 join。** 桩服务用 ephemeral port，所以本轮 38 处摘要彼此不同（`distinct=31`）。本轮从不跨把/跨运行比较摘要，只核对形状与唯一性。
6. **`executor_evidence_gate` 每次跑都重印自己的 manifest。** 历史门的设计；本轮还原字节而非改 M10。

明确不在本轮范围：自动重发策略、reconciliation 服务、多端点/HA、真实环境验证。若将来要在不确定下重发，必须另立任务并单独证明重发条件、哈希一致性、nonce 所有权与端点行为（§6 原话）。

---

## 16. 复核命令（只读，可在任何工作树执行）

```bash
# 0) 构建环境（缺它 clippy 会死在 rocksdb）
export CC=clang CXX=clang++ CXXFLAGS="-include cstdint"
cd /Volumes/superfs/evm-mev-bot

# 1) 三道串行门禁（门 2 前清这 16 个包，否则 warm run 不重新分析任何东西，见 10.1）
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast -- --test-threads=1

# 2) 只看这轮的两个新目标（测量必须在同一把里重跑，见 7.2）
M12E_EVIDENCE_DIR="$PWD/data/evidence/m12/e" \
  cargo test -p evm-execution --test send_uncertainty -- --test-threads=1
cargo test -p evm-execution --test send_uncertainty_evidence -- --test-threads=1

# 3) 日志解析：135 个目标、三态计数（宽正则会误计 evm-simulation 的 result 模块，见 10.2）
grep -cE '^test result: (ok|FAILED)\.' /tmp/m12e_gate3_test.log
awk '/^test result: (ok|FAILED)\./{p+=$4; f+=$6; i+=$8; n++} END{print n, p, f, i}' /tmp/m12e_gate3_test.log

# 4) 发送路径的调用次数（一次 submit = 一次 send 到达）
python3 -c "import json;t=json.load(open('data/evidence/m12/e/request-counts.json'));\
print(t['max_send_arrivals_in_any_case'], t['max_read_arrivals_in_any_case'], t['wire_policy'])"

# 5) 历史证据是否被本轮无关修改
git diff --stat data/evidence/m10 data/evidence/m8
```

---

## 17. 交付的四笔提交

| 笔 | 内容 |
| --- | --- |
| 1 | 生产修复：6 个文件，346 增 / 61 删 |
| 2 | 测试与故障注入：2 个新文件（31 个测试）+ 7 个既有测试文件的字段连带 |
| 3 | 证据：`data/evidence/m12/e/`（49 行原始测量 + 2 张发布表 + manifest）+ 10 个 M8 表格的锚点行号刷新 |
| 4 | 文档：本报告 + 任务书 `docs/v0.1/M12E Coding.md` |

完成后停止，等待审查。本轮未部署节点、未连接真实 RPC、未签名或广播真实交易，也不会进入真实环境验证。
