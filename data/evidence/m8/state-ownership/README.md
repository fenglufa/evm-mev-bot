# M8.4.3 — 状态所有权与生命周期诊断（证据）

§1 的问题：一次链上读取由谁产生、谁拥有、属于哪个生命周期阶段、什么让它失效，下游因此能消费什么。 本目录对 11 类状态、23 条阶段边、以及 M8.4.2 记录的 42 个重复候选各答一遍。它是诊断：没有新增缓存、没有跨 stage 复用、没有改 RPC 次数或顺序、没有删除任何执行前检查（只记录，不消除）。

## 结论

- 11 类状态逐类声明，`ownership_status`：proven 4 / partially_proven 7 / unknown 0；其中命名不出 owner 的 2 类。
- 23 条边的判决（§17 的四个选项，没有第五种）：A 2、B 5、C 16、D 0。
- M8.4.2 的 42 个 exact-duplicate 候选，按本模型的四级判定：**0** 条到达 `safe_to_reuse_now`。
- 拒绝的原因逐行点名，沉默的行 0；命中最多的一档是 42 / 42 行，并列在这一档的原因有 7 个：`answer_bytes_not_recorded, block_hash_not_recorded, contract_lists_missing_proof, different_lifecycle_scope, invalidation_rule_not_proven, same_value_different_source, state_movement_not_detectable`。
- 声明里写明「共享这个值会让一项检查消失」的类别：5。freshness 规则 proven 的类别 7，invalidation 规则 proven 的类别 3。

四级判定是四个不同的问题（§10 的三条不等式）：`same value ≠ same state identity`、`same state identity ≠ same lifecycle ownership`、`reusable in principle ≠ safe to reuse now`。下面每一行把四级分开打印，读一张表就能看到它们各自停在哪儿。

## §4 的 11 类状态（`ownership-matrix.json`）

| state_kind | owner | scope | authority | block_tag_semantics | ownership_status | duplicate | semantically_equivalent | reusable_in_principle | safe_to_reuse_now |
|---|---|---|---|---|---|---|---|---|---|
| pool_reserves | state_store | update_position | node_canonical_logs | number | partially_proven | false | null | true | false |
| contract_code | simulation_state_provider | simulation | node_at_height | number | proven | false | null | true | false |
| storage_slot | simulation_state_provider | simulation | node_at_height | number | proven | false | null | true | false |
| native_balance | no_owner_in_code | head_moment | node_at_height | tag | partially_proven | true | false | false | false |
| nonce | execution_lane | pending_account | node_at_height | tag | partially_proven | true | false | false | false |
| fee_parameters | chain_adapter | head_moment | node_at_height | absent | partially_proven | true | false | false | false |
| block_header | node | block | node_at_height | number | proven | true | true | true | false |
| chain_identity | chain_adapter_connection | chain | first_answer_at_connect | none | proven | true | true | true | false |
| eth_call_result | no_owner_in_code | block | node_at_height | number | partially_proven | false | false | false | false |
| simulation_result | simulation_state_provider | simulation | not_applicable | number | partially_proven | false | null | true | false |
| transaction_intent | sequence_stage | attempt | configuration | number | partially_proven | null | null | true | false |

`holds` 的三种写法各有含义：`true` 是这一级成立，`false` 是这一级被点名拒绝，`null` 是本轮证据答不了这一级 （§13 不许用编造的数据填空）。每行的 `freshness_rule`、`invalidation_rule`、`missing_proof` 和已解析的 `evidence_refs` 都在 `ownership-matrix.json` 里。

## §6 的重点链路：preflight 产生的 18 对（其中 preflight → build 12 对）

§6 禁止把一类状态的结论推及全部，所以这张表一条流一行。preflight → build 上有实测重复的流：`eth_getTransactionCount` 3 对判 C、`eth_getBalance` 3 对判 B、`eth_maxPriorityFeePerGas` 3 对判 C、`eth_getBlockByNumber` 3 对判 C，合计 12 对，与 `stage-dependency-matrix.json` 的 `measured_pairs_preflight_to_build` 一致。同一批边里还有一行只提问、不带重复对：`s6.gas_limit.not_a_chain_read`（fee_parameters，判 C）。判决并不相同这一事实本身就是结论：一条流的判决推广不到同类另一条，也推广不到它的下一步。

| edge | state_kind | method | consumer | pairs | decision | consumer 自带职责 | 保留的安全约束 |
|---|---|---|---|---|---|---|---|
| s6.nonce.preflight_to_build | nonce | eth_getTransactionCount | build | 3 | C | true | the §26 nonce leg — the number the node will accept next |
| s6.native_balance.preflight_to_before_snapshot | native_balance | eth_getBalance | build | 3 | B | true | not a gate: the before-snapshot is the baseline the real asset delta is measured against afterwards |
| s6.fee.preflight_to_build | fee_parameters | eth_maxPriorityFeePerGas | build | 3 | C | true | the §26 fee leg — a moved fee changes the profit the sequence is being sent for |
| s6.fee.intra_preflight_head_pair | fee_parameters | eth_maxPriorityFeePerGas | preflight | 3 | C | true | the head-versus-pin fee comparison inside preflight |
| s6.header.nonce_track_tag_preflight_to_build | block_header | eth_getBlockByNumber | build | 3 | C | true | resolving the tags the nonce leg speaks, at the moment that leg runs |
| s6.header.nonce_track_tag_intra_preflight | block_header | eth_getBlockByNumber | preflight | 3 | B | false | nothing of its own: the second ask is the same question asked again a few calls later |
| s6.gas_limit.not_a_chain_read | fee_parameters | null | build | 0 | C | true | the ceiling keeps a limit inside a block a node could actually include |

「哪些值可能复用，但某些验证动作仍然必须保留」就在这一节：判 A 的边只说明下一轮可以设计一个受控实验，不说明今天可以共享；§6 的「不能为了减少 RPC 而删除执行前安全检查」由 `safety_constraint_served` 一列逐行说明。

## §6–§9 的全部边（`stage-dependency-matrix.json`）

| section | edge | producer → consumer | state_kind | decision | measured_pairs |
|---|---|---|---|---|---|
| 6 | s6.chain_id.connection_to_preflight | unstamped → preflight | chain_identity | B | 3 |
| 6 | s6.chain_id.connection_to_build | unstamped → build | chain_identity | C | 3 |
| 6 | s6.nonce.preflight_to_build | preflight → build | nonce | C | 3 |
| 6 | s6.native_balance.preflight_to_before_snapshot | preflight → build | native_balance | B | 3 |
| 6 | s6.fee.preflight_to_build | preflight → build | fee_parameters | C | 3 |
| 6 | s6.fee.intra_preflight_head_pair | preflight → preflight | fee_parameters | C | 3 |
| 6 | s6.header.nonce_track_tag_preflight_to_build | preflight → build | block_header | C | 3 |
| 6 | s6.header.nonce_track_tag_intra_preflight | preflight → preflight | block_header | B | 3 |
| 6 | s6.header.observation_to_build_fee_input | observation → build | block_header | A | 3 |
| 6 | s6.header.observation_to_build_binding_gate | observation → build | block_header | C | 3 |
| 6 | s6.header.observation_to_preflight_binding | observation → preflight | block_header | C | 3 |
| 6 | s6.header.observation_to_preflight_fee_input | observation → preflight | block_header | A | 3 |
| 7 | s7.header.observation_to_simulation_pin_read | observation → simulation | block_header | C | 3 |
| 6 | s6.gas_limit.not_a_chain_read | preflight → build | fee_parameters | C | 0 |
| 7 | s7.pool_reserves.finding_to_simulation | opportunity → simulation | pool_reserves | C | 0 |
| 7 | s7.block_identity.finding_pin_is_verified | opportunity → simulation | block_header | C | 0 |
| 7 | s7.opportunity_staleness.decided_before_repricing | opportunity → build | transaction_intent | C | 0 |
| 8 | s8.pool_reserves.snapshot_lacks_a_hash | state_store → graph | pool_reserves | B | 0 |
| 8 | s8.canonical_state.store_is_not_an_evm_source | graph → simulation | storage_slot | C | 0 |
| 9 | s9.native_balance.build_gate_leg | simulation → build | native_balance | C | 3 |
| 9 | s9.transaction_intent.fields_agree_by_construction | simulation → build | transaction_intent | B | 0 |
| 9 | s9.simulation_result.override_dependence_is_refused | simulation → build | simulation_result | C | 0 |
| 9 | s9.transaction_intent.multi_step_plan_is_refused | simulation → build | transaction_intent | C | 0 |

判决词表：A = controlled_experiment_definable — a next-phase controlled experiment is definable; this says nothing about sharing the value today；B = contract_design_first — the shape to design is a contract, not a cache；C = must_refetch — the consumer's read is its own duty；D = insufficient_evidence — this build cannot answer the question yet。9 条边没有实测重复对，它们的问题来自 §7–§9 的代码事实，而不是本表测到的那 42 对重复；表里 `measured_pairs` 为 0 的行不假装被测量过。

## §5 的生命周期（`lifecycle-contracts.json`）

55 行 = 11 类 × 5 步（acquired, validated, published, consumed, invalidated_or_expired）。状态分布：not_applicable=3, partially_proven=11, proven=33, unknown=8。没有锚点的行 10 —— 它们只能是 `unknown` 或 `not_applicable`，门禁不允许一行正面的断言不带出处。

## 实测：M8.4.2 候选的四级判定（`reuse-verdicts.json`）

输入是 `data/evidence/m8/cross-stage/reuse-candidates.json` 的 78 对（42 exact / 30 同目标不同块 / 6 块身份不明），只有 exact duplicate 进入本表：42 个。映射到 §4 的 5 类状态，映射不到任何一类的 0 个。

| tier | true | false | null |
|---|---|---|---|
| duplicate | 42 | null | null |
| semantically_equivalent | null | 21 | 21 |
| reusable_in_principle | null | 21 | 21 |
| safe_to_reuse_now | null | 42 | null |

拒绝逐条点名：answer_bytes_not_recorded=42, block_hash_not_recorded=42, block_number_not_comparable=21, consumer_names_no_height=21, contract_check_would_stop_existing=21, contract_declares_not_reusable=15, contract_lists_missing_proof=42, different_lifecycle_scope=42, freshness_rule_not_proven=12, invalidation_rule_not_proven=42, no_owner_in_code=6, ownership_not_proven=15, producer_names_no_height=21, same_value_different_source=42, state_movement_not_detectable=42。记录自己给出的 `safe_to_reuse` 与本模型判定不一致的行 0 行 —— 两个结论各说各话时表把两列并排放 （§14 不许把「测试给出相同结果」读成「生产可安全复用」，也不许用模型的拒绝去改写记录）。

`*_term` 两列抄的是记录自己的 `block_form` 词（number / tag / absent），不是模型造的词：两侧都是 tag 的行 9，单侧 tag 的行 0，两侧词不同的行 0；记录发出的词照原样抄在 `source_record`：`latest` 6 行、`pending` 3 行。模型在 `src/` 下的代码不写标签的那个词 —— §20 的禁令由 `crates/pipeline/tests/state_is_always_pinned.rs` 逐个 `src/` 文件扫描，一份诊断文件不豁免。

## 这套证据回答不了什么

四级判定的最后一级问的是「消费方读的那一刻，链上状态有没有动」和「答案事后由谁持有」。前者要比对两个时刻的状态，这段代码没有这种机制；后者在 RPC 调用记录里没有对应字段。本轮被禁止改生产语义（§3），所以这两项在表里始终是 `null`，不是 `false`。同理，M8.4.2 的记录里没有两次答案的字节，因此每一行的 `same_value` 都是 `null`：同一个请求被证明问了两次，不等于同一个答案被拿到了两次。

本轮没有为补齐这些字段再跑一次实验（§11）。要真正回答它们，需要一段能被重复运行的真实执行窗：签名与广播都不在本轮授权范围内。

## 怎么读锚点

每一行都带 `evidence_refs`：`source` 为 `code` 的指向 `crates/pipeline/src/state_ownership.rs` 之外某个文件生产区间的唯一一行，`test` 指向一个测试文件里的唯一一行，`run_record` 指向一份已提交的证据文件。行号由 `crates/pipeline/tests/state_ownership_evidence.rs` 现场解析，不是手抄的；代码搬家而表没刷新，门禁就会失败。

本目录的 5 份文件由一次装配写出；重新生成要显式设 `M843_STATE_OWNERSHIP_REFRESH=1`。运行记录在 `data/evidence/m8/cross-stage/`，本目录只读不写它们。

## 文件

- `README.md`
- `ownership-matrix.json`
- `lifecycle-contracts.json`
- `stage-dependency-matrix.json`
- `reuse-verdicts.json`

测量这些记录的三把 live run 的配置与代码版本，逐把抄在 `reuse-verdicts.json` 的 `assembled_from` 里（build `"f0031ad8c74d8b2c0ed11ff45bfbec6d29b60805"`、模式 `"build-only"`、`state_read_reuse` true、配置并发 1、真实套利 false）；本目录没有为它们再跑一次，也没有跑任何新实验。

判决共 42 行，每行至少点出一个拒绝原因：没有任何 blocker 的行 0 行。