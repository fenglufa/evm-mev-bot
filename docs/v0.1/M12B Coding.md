# M12-B Coding Agent Task — 单节点就绪闸门与 RPC 接入整理

## 1. 目标

基于已提交的 `docs/v0.1/M12-A Repo Audit.md` 和 `data/evidence/m12/audit_manifest.json`，实施最小必要的单节点安全改造。

M12-B 的目标不是部署节点，也不是实现多节点 HA，而是让机器人具备以下能力：

1. 在节点未同步或状态不确定时，明确拒绝启动或暂停运行。
2. 让运行证据能够区分本地节点与公共 endpoint。
3. 明确节点重启后哪些状态必须失效。
4. 修复本地 Flashblocks 接入前已经识别的配置和测试缺口。
5. 保持现有 M9–M11 策略、REVM、Executor、Signer 和 Multi-Lane 的语义不变。

## 2. 前置审计

开始编码前：

1. 读取 `docs/v0.1/M12-A Repo Audit.md` 全文。
2. 读取 `data/evidence/m12/audit_manifest.json`。
3. 核实当前 HEAD 与工作树状态。
4. 重新查看报告 §12、§13、§16 的 G1–G4、D1–D6。
5. 检查当前代码是否已经修复其中任何问题；不要盲目按报告旧行号修改。
6. 输出一份简短的实施前核对表，再开始修改。

如果实际代码与审计报告不同，以当前代码为准，并在报告中记录差异。

## 3. B1 — 节点同步就绪闸门（P0）

增加一个最小的 readiness check，使用标准 JSON-RPC `eth_syncing`。

### 要求

* 使用现有 ChainAdapter / RPC 客户端。
* 不新建第二套 HTTP client。
* 在启动阶段执行一次。
* 在既有节点连接恢复流程中，按明确规则重新检查；不要在每次行情事件上调用。
* `eth_syncing = false` 表示该 RPC 声明当前不处于同步中。
* `eth_syncing` 返回同步进度对象时，节点尚未就绪，禁止进入会产生真实执行动作的阶段。
* JSON-RPC error、超时、断连、非法响应和无法解码的结果均不能视为 ready。
* 不允许把 readiness 失败转换为“没有套利机会”。
* 不允许在本地 endpoint 失败时静默切换回公共 endpoint。
* 不得在未验证节点状态时将缺失的 block、reserve 或 receipt 解释为零值。

### 重要语义

`eth_syncing = false` 只证明节点报告自身不处于同步状态，不保证节点一定追上网络最新高度。

因此：

* 不要将 `false` 解释为“节点已经完全健康”。
* 如果当前已有 head height 信息，明确记录其来源。
* 节点头高的新鲜度策略必须可配置或有明确默认值，不能在没有依据时硬编码一个看似精确的容差。
* 如果无法判断是否足够新，应失败关闭或暂停真实执行。

### 测试

至少覆盖：

* `false` → readiness 通过。
* 同步进度对象 → readiness 拒绝。
* JSON-RPC error → 拒绝。
* 超时 → 拒绝。
* 断连 → 拒绝。
* 非 JSON 响应 → 拒绝。
* 错误数据类型 → 拒绝。
* readiness 未通过时，Signer / Submitter 不得被调用。
* readiness 重试不得造成无限循环或无界 RPC 调用。

每个测试都必须检查真实返回结果和调用次数，而不是只检查没有 panic。

### RPC 调用预算

这是 M12-B 唯一明确批准新增的 RPC 方法。

* `eth_syncing` 仅在启动和规定的恢复节点执行。
* 记录实际调用次数。
* 不在热路径、候选搜索、金额优化或每笔交易中重复调用。
* 不为 metrics、日志或证据额外增加 RPC。
* 后续必须在真实本地节点上验证响应形状。

## 4. B2 — Endpoint 身份与证据标记

当前证据可以记录 endpoint 摘要，但无法可靠区分该端点是本地节点还是公共服务。

添加最小、明确的 endpoint 用途标记，例如：

* `LocalCanonicalRpc`
* `PublicCanonicalRpc`
* `LocalFlashblocksRpc`
* `PublicFlashblocksRpc`
* `Unknown`

具体枚举可以按当前类型系统调整，但必须做到：

1. 标签来源明确，不允许根据 `localhost`、端口号或 URL 字符串自行猜测 Flashblocks 能力。
2. 端点身份和 endpoint digest 分开记录。
3. 现有脱敏规则保持不变。
4. 不能在日志或证据里泄露 API key、token 或 URL 敏感参数。
5. 公共 endpoint 不得被标记为本地。
6. `Unknown` 必须作为合法状态，不能静默默认为本地。

只修改必要的数据类型和证据输出，不改写整个 metrics 或 evidence 系统。

## 5. B3 — 节点重启后的状态失效规则

为单节点重启、重新连接或链头倒退制定明确的状态失效策略。

至少审查：

* 当前 GraphSnapshot。
* canonical head / block identity。
* pending / Flashblocks 视图缓存。
* 尚未完成的候选与仿真结果。
* Ready 但尚未提交的 ExecutablePlan。
* nonce reservation。
* capital reservation。
* 已提交但尚未确认的交易。

基本原则：

* 任何依赖旧 canonical block identity 的候选或仿真结果，不能仅因为重新连上 RPC 就继续执行。
* 旧 pending 视图在 reset 后必须按明确规则失效。
* 已提交交易不能因节点重启而直接当作失败或重新提交。
* 已提交但状态未知的交易必须沿用现有 transaction hash / receipt 生命周期进行恢复。
* 不允许通过简单清空全部状态破坏正在进行的交易跟踪。
* 不允许恢复后重复占用同一 nonce 或同一笔资本。

优先复用 M10/M11 现有生命周期和 reservation 逻辑，不另建一套执行框架。

本阶段无需做真实节点重启实验；必须提供可复现的单元/集成测试。真实节点重启验证保留为后续基础设施验收项。

## 6. B4 — Flashblocks 接入前兼容性修正

根据 M12-A 审计中 D5：

当前 pending 请求与 Radar 解码器对交易对象形状的预期存在不一致。

要求：

1. 核对 `HeadReader::pending_raw()` 的真实 JSON-RPC 参数。
2. 核对 `preconf_decode` 支持的交易对象格式。
3. 明确选择：

   * 修改请求参数以获取完整交易对象；或
   * 扩展解码器以支持当前真实响应形状。
4. 选择前必须评估响应体大小、调用延迟和现有语义。
5. 不得仅通过放宽校验让未知格式静默通过。
6. 对不支持的形状必须 fail-closed，并产生可诊断的错误。
7. 使用已有 replay fixtures 覆盖完整交易对象、hash-only 交易、缺失字段、类型错误和未知字段。
8. 统计新增测试的 RPC 调用数；默认使用 mock / replay，不访问真实节点。

本阶段不把 Early Radar 直接接入真实执行路径。

本阶段也不宣称本地 Flashblocks 已验证。

## 7. B5 — 环境变量名称统一

根据审计 D2，统一生产与测试中 Flashblocks endpoint 的环境变量名称。

要求：

* 先检查全部生产、测试、文档和忽略测试中的使用位置。
* 选择一个规范名称。
* 同步更新生产配置、测试、运行文档和示例。
* 移除旧名称，或在必要时提供明确的迁移错误提示。
* 不允许生产与测试继续使用两个拼写却没有说明。
* 保持 `flashblocks_url` 与 canonical `rpc_url` 的语义独立。
* 不因为统一名称而默认启用 Flashblocks。

## 8. B6 — 消除重复的间隔默认值

根据审计 D3，当前 CLI 和 live 模块对部分间隔分别定义默认值。

要求：

* 先核对当前值和语义。
* 统一使用现有配置来源。
* 不改变既有有效配置的运行行为。
* 为默认值、显式配置和无效配置添加测试。
* 不以此为理由重构所有配置系统。

## 9. D1、D4 与其他问题

* D1：修正 Flashblocks chain mismatch 错误信息中的字段角色颠倒。保持原有 fail-closed 比较逻辑不变。
* D4：检查执行证据中的 endpoint 类型标签，避免把本地读请求错误标成公共 RPC。
* 保留公共 Sequencer endpoint 仍在使用这一事实，除非已经有证据证明提交路径改变。
* 不要为了让证据显示“全本地”而改写标签。
* 其他发现若超出上述范围，只记录，不顺手扩大任务。

## 10. 明确不做

* 不实现 HA。
* 不实现多 endpoint router。
* 不实现负载均衡。
* 不实现备用节点。
* 不修改 M9.3 PathFinder 的语义。
* 不修改 M10 Executor 合约。
* 不重写 Signer、Submitter、ReceiptTracker。
* 不重写 M11 Multi-Lane。
* 不部署真实节点。
* 不宣称本地 canonical RPC 已验证。
* 不宣称本地 Flashblocks 已验证。
* 不进行真实资金交易。
* 不新增签名或广播。
* 不把新的 RPC 调用加入每笔交易热路径。
* 不修改无关模块。
* 不进行与 M12 无关的格式化或大规模重构。

## 11. 测试与门禁

必须串行执行，避免共享测试目录引起干扰：

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace -- --test-threads=1`

必须解析完整日志，不能只凭 exit code 判定。

报告实际测试目标数、passed、failed、ignored、耗时和警告。不能把 ignored 计入 passed。

另外执行：

* M12 新增测试清单及调用次数核对。
* 生产代码 panic / unwrap 风险扫描。
* 密钥与敏感信息扫描。
* 生产 diff 范围审查。
* RPC 新增调用清单审查。
* endpoint 标记与脱敏测试。
* 真实交易路径未被意外触发的证明。

不得用 mock 测试结果声称官方节点支持某个方法。

## 12. 交付物

新增或更新：

* `docs/v0.1/M12-B Completion Report.md`
* `data/evidence/m12/b/manifest.json`
* 与新增功能对应的测试文件。

报告必须明确记录：

* 本次代码变更范围。
* 每个修复对应的缺陷编号。
* 新增 RPC 调用及调用预算。
* 测试结果和可复核命令。
* 未完成的真实节点验证项。
* 是否触及 M9–M11 的生产行为。
* 真实节点和本地 Flashblocks 仍未验证的事实。

建议按代码、测试、证据、文档分开提交；具体提交边界按实际改动决定，不要为了凑提交数拆分无关文件。

## 13. 完成标准

M12-B 可以完成的条件：

* 就绪闸门行为确定且 fail-closed。
* readiness 失败时不会进入真实执行阶段。
* endpoint 身份标签可靠且脱敏。
* 节点重启失效规则有测试覆盖。
* pending / Radar 解码器形状问题得到明确修正。
* 环境变量命名统一。
* 重复间隔默认值已收敛。
* D1/D4 中纳入范围的证据标签问题得到修正。
* 全部串行门禁通过。
* 证据与报告口径一致。
* 无未批准的新增生产 RPC。
* 无签名、广播或真实资金交易。

即使以上全部完成，也必须继续保持：

* `SELF_HOSTED_NODE = NOT_RUN`
* `LOCAL_CANONICAL_RPC = NOT_VERIFIED`
* `LOCAL_FLASHBLOCKS = NOT_VERIFIED`

不能把代码层面的 readiness 支持等同于真实节点已验证。
