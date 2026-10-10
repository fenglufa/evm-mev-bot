# M12-D Coding Agent Task — 离线执行安全与状态恢复审计

## 1. 目标

在不部署 GIWA 节点、不连接真实 RPC、不签名或广播交易的前提下，审计并收尾 M12-B 引入或确认的单节点安全能力。

本轮目标是确认现有实现是否满足以下原则：

* 节点未就绪时，不允许进入交易执行路径。
* RPC、WS、Pending 数据异常不能被误判为“没有套利机会”。
* 失效的 Canonical/Pending 视图不能继续支撑旧候选或旧模拟结果。
* 已提交交易的追踪状态不能因节点重启而丢失。
* 端点用途标记不能与实际提交路径相矛盾。
* 关键安全策略发生变化时，证据门必须能够检测变化。

## 2. 基线与前置审查

先确认当前 `origin/main`、HEAD、工作树状态，并阅读：

* `docs/v0.1/M12-A Repo Audit.md`
* `docs/v0.1/M12-B Completion Report.md`
* `docs/v0.1/M12-C Single-Node Deployment and Verification Plan.md`
* `docs/v0.1/M12-C Completion Report.md`
* `data/evidence/m12/b/` 下的现有证据
* M10、M11 相关的执行、签名、回执和多通道实现

预期基线包含 M12-C 的两笔提交 `d873d55` 和 `df6d119`，但必须以实际远端状态为准。

先输出一份简短的审计计划，列明：

1. 要检查的安全不变量。
2. 对应的源码入口和测试入口。
3. 已有测试覆盖。
4. 尚无证据支持的结论。

不要仅凭 M12-B 完成报告就认定代码一定符合报告中的描述。

## 3. D1 — Readiness 与执行隔离

沿真实调用路径追踪 readiness 状态：

* 启动初始化。
* Canonical RPC 不可用。
* `eth_syncing` 返回同步进度对象。
* `eth_syncing` 返回错误、超时、无效响应。
* RPC/WS 断连与重连。
* 节点高度落后或 Canonical 视图回退。
* readiness 恢复后的重新放行。

检查 readiness 是否真正位于执行路径的必经位置，而不是只存在于启动日志或独立状态对象中。

使用 Mock、Replay 或确定性测试验证：

* 未就绪时，Signer 不被调用。
* 未就绪时，Submitter 不被调用。
* readiness 检查失败不会被转换成空候选或零机会。
* 不会静默回退到未经授权的公共 RPC。
* 恢复后必须重新满足放行条件。

不要新增逐事件、逐候选或逐交易的 `eth_syncing` 请求。除非审计发现现有设计存在无法安全修复的结构性问题，否则不新增 RPC 方法。

## 4. D2 — 节点重启与状态失效

沿现有状态生命周期追踪：

* Canonical block-pinned candidate。
* Simulation 结果及其关联的区块标识。
* Pending view、Pending 交易集合与 Radar 状态。
* 已提交但尚未确认的交易。
* Nonce 与资金预留。
* 节点重启、断连、Pending 回退及恢复。

必须区分：

1. 可以失效并重新计算的候选或模拟数据。
2. 必须保留的已提交交易追踪信息。
3. 不能重复执行的 Nonce 或资金预留操作。

测试至少覆盖：

* 节点重启后，旧区块上下文的候选不能直接继续执行。
* Pending 视图回退后，依赖旧视图的结果按既定策略失效。
* 已提交交易的追踪状态不会因 Pending 清理而被删除。
* 重连不会重复预留资金或重复分配 Nonce。
* 恢复流程不会绕过 readiness gate。

优先复用 M10、M11 现有生命周期和数据结构，不新建第二套交易执行状态机。

## 5. D3 — 端点用途与提交路径真实性

沿完整交易路径检查：

* `--rpc-url` 的读取用途。
* Executor 的模拟与提交路径。
* Signer 和 Submitter 的实际依赖。
* `RETH_ROLLUP_SEQUENCERHTTP` 相关配置及潜在公共端点依赖。
* Canonical RPC、Flashblocks 和提交端点的用途标签。
* 证据输出中的 endpoint provenance。

特别检查：

* `localhost` 不应自动等价于“交易最终提交到本地节点”。
* 本地 Canonical RPC 与公共 sequencer 可能同时存在。
* 不得把读取端点标签直接复用为提交端点标签。
* 未知端点应标记 `Unknown` 或等效状态。
* 端点 URL、API Key、JWT 和其他凭证不得进入公开证据。

如果现有实现无法准确证明提交目的地，应采用保守标签并阻止不符合配置要求的执行，而不是猜测端点用途。

本轮不实现多端点路由、故障转移或 HA。

## 6. D4 — `node_reset_policy` 证据门缺口

M12-B 已将以下问题记录为本轮范围外事项：

`node_reset_policy` 的证据门只重新解析锚点，未对整张证据表执行完整一致性检查；模型字段发生变化时，如果没有触发 refresh，证据门可能仍然通过。

本轮需要：

1. 复核 M12-B 报告及 manifest 中的原始描述。
2. 找到实际证据生成器、锚点提取逻辑和测试。
3. 确定整表的权威数据源。
4. 为策略表建立可重复的对照检查。
5. 验证改变 `disposition`、`strength` 或其他受审计字段后，证据门会失败。
6. 验证正常生成的证据能够通过检查。
7. 验证缺失行、重复行、额外行和字段值变化都会被检测到。

证据生成器与校验器不得仅通过共同读取同一份已经生成的结果来形成循环自证。

尽可能从模型、源码结构或确定性的策略定义中独立提取期望值，并与持久化证据逐项比较。

如果当前结构无法建立可信的独立期望值，明确报告原因，不要用弱化断言的方式伪造完整验证。

## 7. D5 — Pending 与 Radar 数据边界

复核 M12-B 已处理的 Pending 响应形状兼容性，以及 M9.4 Radar 的真实调用关系。

至少检查：

* 完整交易对象与交易 Hash 列表的解析行为。
* 缺字段、错误字段类型和未知结构。
* 断连、重连及重复 Pending 事件。
* Radar 观察结果与 Canonical StateStore 的边界。
* Pending 候选进入后续模拟或执行前的重新验证。

不得因为 Pending 数据可解析，就认定它可以直接驱动执行。

不得将 Early Radar 直接接入 Signer 或 Submitter。

如果 Radar 当前尚未接入运行链路，准确记录其真实状态，不得声称其端到端功能已经通过。

## 8. 测试要求

使用现有测试框架与 Mock/Replay 机制。

优先补充以下负向测试：

* readiness 失败时的执行隔离。
* 重启前候选在恢复后被拒绝。
* 已提交交易追踪在重启清理后仍保留。
* Nonce/资金预留不会重复。
* 未知端点用途不能被标为 Local。
* 公共提交端点不会被描述为本地提交。
* `node_reset_policy` 的关键字段变化会使证据校验失败。
* Pending 响应不兼容时 fail-closed。
* 断连恢复后必须重新满足执行条件。

不得使用真实私钥、真实资金或真实链上交易。

## 9. 范围限制

禁止：

* 部署 GIWA 节点或启动数据同步。
* 访问真实 GIWA RPC、Flashblocks 或 L1 服务。
* 进行真实签名、广播或资金操作。
* 新增 HA、多节点、负载均衡或自动故障转移。
* 重写 M10 Executor、Signer、ReceiptTracker。
* 重写 M11 Multi-Lane。
* 将 M9.4 Radar 扩大成新的执行框架。
* 为了增加测试数量而大规模重构无关代码。
* 修改或重新生成无关历史证据。
* 将未执行的真实环境测试标为通过。

发现但不属于本轮的缺陷，记录文件、函数、风险、证据和建议后续里程碑，不顺手扩大任务。

## 10. 串行验证门禁

按顺序执行，不得并行运行 Cargo 门禁：

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace -- --test-threads=1`

必须解析完整日志，而不只是检查进程退出码。

报告：

* 命令与退出码。
* 实际执行时间。
* 测试目标数。
* passed、failed、ignored 数量。
* 警告和错误摘要。
* 是否存在超时或未完成的命令。

`ignored` 不得计入 `passed`。

另外执行：

* 生产代码 diff 审查。
* 新增 RPC 调用预算审查。
* 签名与广播路径审查。
* 敏感信息扫描。
* 证据生成与校验的负向测试。
* M10/M11 回归测试。

## 11. 交付物

按仓库现有惯例组织提交，至少交付：

* 必要的代码与测试修复。
* `docs/v0.1/M12-D Completion Report.md`
* `data/evidence/m12/d/manifest.json`
* 对 `node_reset_policy` 证据门的正向与负向测试证据。
* 实际运行的门禁日志或可追溯摘要。

完成报告必须包含：

1. 本轮检查范围。
2. 修改的生产文件与测试文件。
3. 每个安全不变量的结论及证据。
4. 测试门禁的完整统计。
5. 未解决问题与明确的后续范围。
6. 与 M12-C 部署方案的衔接。
7. 本轮未部署节点、未连接真实 RPC、未签名、未广播的声明。

所有结论必须能追溯到代码、测试或生成的证据。

## 12. 完成后的状态

M12-D 完成只代表离线安全收尾完成，不代表真实节点已经通过验证。

继续保持：

* `SELF_HOSTED_NODE = NOT_RUN`
* `LOCAL_CANONICAL_RPC = NOT_VERIFIED`
* `LOCAL_FLASHBLOCKS = NOT_VERIFIED`
* `MULTI_NODE_HA = OUT_OF_SCOPE`

完成后停止并等待审查，不自动部署节点，也不自行进入真实环境验证。
