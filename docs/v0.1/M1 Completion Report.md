# M1 Completion Report

结论（白话版）：M1 已经跑通。从 GIWA Sepolia（chain 91342）真实历史区块里读到的日志，经过
「解码 → 生成状态更新 → 写入状态库」这三步之后，得到了这个池子在那个区块当时的真实储备量
（reserve0 / reserve1），而且这个数字和直接向合约问一次 `getReserves()` 的答案完全一致。
同一份输入跑两遍，结果逐字节相同。状态：**COMPLETE**。

模板要求的事实值（十进制，来自链上原始数据，非估算）：

| 字段 | 值 |
| --- | --- |
| chain_id | 91342 |
| block | 37257255 |
| transaction | 0x0021b1b3197efe3219b2db23729dc50253e205eaed303f1d03070b1c240d71ae |
| log_index（块内全局） | 181 |
| pool | 0x3978e57bbceb7666d54a03551c03691f897f6092 |
| token0 | 0x304912af0ce0dd6479735634d567715107bdc0c6 |
| token1 | 0x4200000000000000000000000000000000000006 |
| reserve0 | 2849387467534192263206 |
| reserve1 | 141265148507902942209 |

---

## 1. Implementation

新增：

- `Cargo.toml`（workspace：5 个 crate）、`rust-toolchain.toml`（1.96.1）
- `crates/core`：领域类型 `ChainId / BlockNumber / TxIndex / LogIndex / TxHash / TokenId / PoolId /
  ProtocolId / PoolMeta / PoolState / PoolType / Fee / EvidenceRef / EvidenceSource`。金额一律 `U256`。
- `crates/chain`：`types.rs`（归一化 `ChainBlock / ChainTransaction / ChainReceipt / ChainLog / BlockData`，
  `ChainLog` 的 `Ord` 手写为 `(block, tx_index, log_index)`）、`adapter.rs`（`ChainAdapter` trait）、
  `rpc.rs`（`HttpChainAdapter`：JSON-RPC → 归一化类型，含 tx/receipt 对齐校验）、
  `recorded.rs`（`RecordedChainAdapter`：离线读 `block-<n>.json`，并校验文件自洽性）
- `crates/protocol`：`adapter.rs`（`ProtocolAdapter` trait）、`event.rs`（`SyncEvent / SwapEvent /
  PoolCreatedEvent`，每个事件自带 `LogPosition`）、`signatures.rs`（用 `alloy-sol-types` 现算 topic0）、
  `registry.rs`（`Registry / PoolAttestation / AttestationEvidence`，无证据的池子拒绝加载）、
  `v2.rs`（`V2Adapter`：只有被证实过的地址的 `Sync` 才会变成事件；`Swap` 只变成流量）
- `crates/state`：`update.rs`（`StateUpdate / UpdatePosition`——ProtocolEvent 与 Store 之间唯一的一扇门）、
  `store.rs`（`StateStore` trait + `InMemoryStateStore`，全部拒绝都是显式错误）、`snapshot.rs`
  （`BTreeMap` 快照，遍历顺序确定）、`error.rs`
- `crates/replay`：`pipeline.rs`（`ProtocolEvent → Vec<StateUpdate>`）、`engine.rs`
  （`ReplayEngine / ReplayReport`，对 `ChainAdapter` 泛型，所以实时与回放走同一份代码）
- `data/protocols/v2-giwap-sepolia.json`：真实池子的身份/代币/状态三组证据
- `fixtures/replay/`：7 个用例 8 个区块文件；`fixtures/real/block-37257255.json`（204 KB，
  153 笔交易 / 183 条日志，直接由 RPC 归一化后落盘，未手工转录）
- `crates/replay/tests/record_fixtures.rs`（生成器 + 抓取器）、`crates/replay/tests/m1_replay.rs`（验收测试）
- GIWA-specific 的东西只在两个地方：`data/protocols/v2-giwap-sepolia.json`（真实事实，数据不是代码）
  与测试/fixture 常量。5 个 crate 的非测试代码里，`91342`、池子/代币地址、`giwa` 字样
  的命中数为 **0**（脚本按 `#[cfg(test)]` 边界统计），所以 chain→protocol→state→replay
  这条链路对具体链没有任何硬编码。协议族代码（V2 形态）按 §33 限定在 `crates/protocol/src/v2.rs`。

修改：

- `crates/chain/src/recorded.rs`：`validate()` 把录制数据当不可信输入处理——允许 receipt/log 在文件里
  任意乱序，但要求内部索引彼此自洽（同块重复 log_index、log 与所属 receipt 的 tx 字段不符、
  log_index 顺序与 tx_index 顺序矛盾、跨链/跨块串号，全部拒绝）。
- `crates/protocol/src/signatures.rs`：`Burn` 声明按链上实际日志修正为
  `Burn(address,uint256,uint256,address)`（第三个 indexed 地址只能从数据里看出来；
  常见声明 `Burn(address,uint256,uint256)` 的 topic0 `0x49995e5d…` 在 95,670 笔交易的语料里
  出现 0 次，而 `0xdccd412f…` 出现 172 次），并把无法举证的 `sync()` selector 断言移除。
- `crates/replay/src/engine.rs`：`ReplayReport` 增加 `rejected_syncs / rejections`。空储备属于市场事实、
  不是流水线故障，因此拒绝那一条陈述并记录原因，其余任何拒绝仍然中止本次 replay。

---

## 2. Protocol Fact

Protocol：

- 结论：这个池子表现为 **V2 形态的恒定乘积对**（代码里的协议名 `v2-compatible`）。
- 依据（全部在 chain 91342 上实测，见第 3 节）：`getReserves()` 返回 3 个 word
  （reserve0 / reserve1 / blockTimestampLast），`token0()` `token1()` 各自返回一个地址，
  `name()` 返回 `"Giwap LPs"`，同一个地址自己发时 `Sync / Swap / Mint / Burn / Transfer`
  这一族事件都观察到过。
- 没有预设的东西：没有假设存在 Factory，没有假设手续费，没有从地址形状、命名或某一条
  Transfer 推断过协议身份。
- 关键反例（这正是 M1 要求警惕的）：在对 `/Volumes/superfs/giwa-mev` 既有证据文件
  （`data/evidence/v0.4.3.1/semantic-events.json`：54,686 笔交易、95,670 条收据日志、
  2,575 个不同合约地址）的重新普查里，`Sync` 形状的日志共 **2,345** 条，只来自三个地址：
  `0xad153c84…` 1,629 条、本池 `0x3978e57b…` 698 条、`0xcaafb95f…` 18 条。
  占多数的 `0xad153c84…` 全语料里只有 3 种 topic0（Sync + 两个别的），**没有任何
  Swap / Mint / Burn / Transfer 伴随**，并且对它调 `token0()` 直接
  `execution reverted`（block 37257255 实测）。
  **topic0 相同 ≠ 协议相同**，所以解码器对未证实地址一律返回 `Ok(None)`，而不是「先当成池子」。
  对照：本池在语料里有 6 种 topic0，Swap 353 / Mint 173 / Burn 172 / Approval 173 / Transfer 517，
  而全语料的 Mint(173) 与 Burn(172) 都来自这一个地址。

Pool：

```text
chain_id 91342
address  0x3978e57bbceb7666d54a03551c03691f897f6092
```

Token0：

```text
0x304912af0ce0dd6479735634d567715107bdc0c6      （该池子 token0() 在该区块的返回值）
```

Token1：

```text
0x4200000000000000000000000000000000000006      （该池子 token1() 在该区块的返回值）
```

Reserve 恢复来源：`Sync(uint112,uint112)`，
topic0 `0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1`（1 个 topic + 64 字节数据），
由池子本身发出。`Swap` 的四段金额只被解释为流量，代码里不存在把 Swap 变成 reserve 的路径。

---

## 3. Evidence

Pool evidence（身份，`data/protocols/v2-giwap-sepolia.json` → `evidence.identity`）：

- `EthCall` `getReserves()` → selector `0x0902f1ac`，在 block 37257255 返回 3 个 word。
- `EthCall` `name()` → `"Giwap LPs"`。
- `ChainLog` block 37257255 / tx `0x0021b1b3…` / log 180：池子自身发出的 LP `Transfer(address,address,uint256)`。
- `ChainLog` 同上 tx / log 182：`Mint(address,uint256,uint256)`（2 个 topic + 64 字节）。

Token evidence：

- `EthCall` `token0()` → `0x304912af…`；`token1()` → `0x4200…0006`（均取自 block 37257255）。
- `ChainLog` log 177（token0 的 `Transfer`）与 log 179（token1 的 `Transfer`），
  与池子的 `Sync` 在同一笔交易里——两条真实转账在两个不同地址上，与 `token0()/token1()` 的答复对上。

State evidence：

- `ChainLog` block 37257255 / tx `0x0021b1b3…` / **log 181**：`Sync(uint112,uint112)`
  reserve0=2849387467534192263206 reserve1=141265148507902942209。
- `EthCall` `getReserves()` 在同一区块的交叉核对（只做校验，不是状态的来源）。

同一区块里这个池子的完整日志形状（从落盘的真实区块文件读出，9 条）：
log 146/147 `Transfer`、150 `Sync`、151 `Burn`、174 `Sync`、175 `Swap`、
180 `Transfer`、181 `Sync`、182 `Mint`。三次 `Sync` 分别在 log 150 / 174 / 181，
最终状态必须是 181——这也是同块排序测试用的数字。

`Fee` 字段是 `null`：`eth_getCode`（同一区块）拿到 14,931 字节字节码，里面
`03e5` 出现 **0 次**（既没有 V2 的 `6103e5` PUSH2 300，也没有 32 字节的 300 常量），
所以手续费没有被任何证据支持——宁可留空，不写进身份。
（同样说明它不是原样照搬的 Uniswap V2 部署：`Burn` 多带一个 indexed 地址，见第 2 节。）

---

## 4. Real Replay

Chain：

```text
91342  (eth_chainId = 0x164ce, https://sepolia-rpc.giwa.io)
```

Block：

```text
37257255  (0x2388027)
hash          0xf7830370a520d0301ebee9ef59134e25421164ae35cc4794fb5a4d1766c9890b
parent_hash   0x9391d021a6317e4821ce0fc4a26b4deee99cb86b3ec6a08393081bc89c26318c
timestamp     1790602371
153 transactions / 183 logs / 26 个不同 topic0
```

Transaction：

```text
0x0021b1b3197efe3219b2db23729dc50253e205eaed303f1d03070b1c240d71ae   (tx_index 152)
```

Log：

```text
全局 log_index 181（该笔交易的 6 条日志为 177…182）
```

回放结果（`crates/replay/tests/m1_replay.rs` → `the_real_block_replays_to_the_pools_real_reserves`）：

```text
blocks = 1        logs = 183
sync_events = 3   swap_events = 1        （本区块 Sync 形状日志共 3 条，全部来自被证实的池子；
                                          Swap 形状日志共 1 条，也来自它）
registrations = 1 syncs_applied = 3      rejected_syncs = 0
pools = 1         synced_pools = 1
快照位置 = block 37257255 log 181
```

同一测试文件里还有一条 `#[ignore]` 的实时对照
（`the_live_provider_reproduces_the_recorded_state`）：直接连 RPC 回放同一个区块，
`ReplayReport` 与 `StateSnapshot` 与读落盘文件的运行**完全相等**——即离线回放与实时链路同源同果。

---

## 5. Recovered State

```text
pool      0x3978e57bbceb7666d54a03551c03691f897f6092   (chain 91342)
token0    0x304912af0ce0dd6479735634d567715107bdc0c6
token1    0x4200000000000000000000000000000000000006
fee       unknown (None)
protocol  v2-compatible
pool_type ConstantProduct

reserve0  2849387467534192263206
reserve1  141265148507902942209
block     37257255
log_index 181
```

这两个数是状态库里该池子的最终储备：同一个区块里三次 `Sync`（log 150 / 174 / 181）按块内全局
log_index 依次写入，后一次覆盖前一次，最后落定在 181。整条链路没有任何一步用到 `f64`——
全仓库（含测试）搜 `f64 / f32` 为零命中，储备类型是 `U256`。

---

## 6. Validation

RPC cross-check:
PASS

```text
eth_call {to: 0x3978e57b…, data: 0x0902f1ac} @ 0x2388027 返回
  reserve0  0x00…009a7731d505416ac026 = 2849387467534192263206
  reserve1  0x00…0007a872a01cf45b9c01 = 141265148507902942209
  blockTimestampLast 0x6aba6c83 = 1790602371  ← 与该区块 header timestamp 相同
```

也就是「池子自己的最后一次 `Sync`」与「直接问合约」在同一个区块上三元一致
（两个储备 + 一个时间戳）。`eth_call` 只出现在这里的初始化/交叉核对，
以及身份确认阶段；每块储备的来源始终是 `Sync` 日志。

其他验证：

- 确定性：`two_runs_of_the_same_input_are_identical` —— 同一输入跑两遍，`ReplayReport` 与
  `StateSnapshot` 逐字段相等；逐块回放与整段区间回放结果一致。
- 生成器可复现：两次 `--ignored` 重跑（含重新向 RPC 抓取真实区块）后，9 个 fixture 文件的
  SHA-256 与提交版本逐一相同 ⇒ 落盘数据没有被手工改过。
- 顺序：文件里 receipt 数组顺序被打乱（磁盘上为 `[3,1,0,2]`）的用例，
  最终储备仍是 log 181 的值；倒序喂块被 `StateError::Regression` 拒绝且不改动任何状态；
  区间倒挂被 `ReplayError::InvertedRange` 拒绝；缺块是错误而不是「跑得短一点」。
- 无效数据：`Sync(0,0)` 被 `StateError::InvalidReserves` 拒绝并记入报告，池子保留前一次有效状态；
  形状不对的 `Sync`（attested 池子发出）中止本次 replay 并给出原因；
  未证实地址发出的合法形状 `Sync` 被忽略，既不进状态库也不注册成池子。
- 无 panic：5 个 crate 的非测试代码里 `unwrap() / expect() / panic!` 命中数为 0（脚本按
  `#[cfg(test)]` 边界统计）。

---

## 7. Tests

cargo fmt:
PASS（`cargo +1.96.1 fmt --check`，退出码 0）

cargo check:
PASS（`cargo check --workspace --all-targets`，无 warning）

cargo test:
PASS — 52 passed / 0 failed / 3 ignored
　　`evm-core` 6，`evm-chain` 9，`evm-protocol` 14，`evm-state` 9，
　　`evm-replay` 集成测试 `m1_replay.rs` 14 passed + 1 ignored（实时对照），
　　`record_fixtures.rs` 2 ignored（生成与抓取，已单独手工跑通）。
　　注：`cargo test` 默认不跑那 3 个 `#[ignore]`；它们各自的通过结果在本报告第 4、6 节有记录。

cargo clippy:
PASS（`cargo clippy --all-targets --all-features -- -D warnings`）

Fixture 覆盖（`fixtures/replay/`，要求 ≥5，实际 7 个用例 8 个区块）：

| 用例 | 要回答的问题 |
| --- | --- |
| `swap_only` (block 200) | 只有 Swap + 两条 Transfer：不得产生任何储备 |
| `single_sync` (210) | 一次权威 Sync ⇒ 储备出现，并带上 (block, log) 位置 |
| `multiple_sync` (220, 221) | 跨块演进，后者胜出 |
| `same_block_ordering` (230) | 同块 4 次 Sync + 1 次 Swap，receipt 数组故意乱序 ⇒ 结果按 log_index |
| `invalid_reserves` (240) | `Sync(0,0)` 被显式拒绝并记录原因，状态不前移 |
| `malformed_log` (250) | attested 池子的畸形 Sync ⇒ 中止并说明，不静默跳过 |
| `unattested_emitter` (260) | 未证实地址的合法形状 Sync ⇒ 忽略，不能变成池子 |

---

## 8. Known Limitations

1. 只证实了**一个**池子。没有 Factory/`PairCreated` 的证据，所以新池子无法自动进入状态——
   目前只能往 `data/protocols/*.json` 增加一份带证据的 attest。自动发现属于后续里程碑。
2. 手续费未证实（`fee = None`）。任何依赖手续费的定价计算在 M1 阶段拿不到这个数。
3. 非 V2 的 `Sync` 形状发射者被有意忽略而不是解码：语料里占 `Sync` 日志 1,629/2,345 的
   `0xad153c84…`，以及 18 条的 `0xcaafb95f…`。要接它们需要各自的身份证据，不能靠 topic0。
4. `token1` `0x4200…0006` 只有「它是该池 token1() 的返回值」这一层证据；
   它的 `symbol()/name()` 没有取证，所以报告里不称其为 WETH。
5. 本地既有历史文件里的 `log_index` 是**每笔交易内**的序号，不是块内全局序号——实测该语料
   54,686 笔交易 / 95,670 条日志，`log_index` 取值只有 0…251（而且是字符串），
   所以它不可能是块内全局索引，两者不可混用。真实全局索引只能来自 RPC
   （本项目的 fixture 与 registry 证据里的 150/174/177/179/180/181/182 都是 RPC 给出的全局值）；
   合成 fixture 直接写全局索引，并且 `RecordedChainAdapter::validate` 会拒绝
   「全局索引与交易顺序矛盾」的录制文件。
6. 被拒绝的空储备 `Sync` 之后，池子仍保留上一次有效储备（可能已经过期）。
   M1 没有「把池子标记为失效」这种更新类型；这是后续里程碑需要补的语义。
7. 没有 Flashblocks 处理，因此 §15 的 `enum ChainEvent` 也还没有建立——M1 只有
   “完整 RPC 区块”这一种输入，为一个变体做抽象就是提前抽象（§38）。协议层看到的
   仍然只是规范化的 `ChainLog`，将来加 Flashblocks 不需要动 protocol/state/replay。
   §16 的条件不触发：不存在把单个 frame 当完整区块的路径，
   且 `RecordedChainAdapter` 明确拒绝服务 `eth_call`。
8. 回放是逐块串行的，没有预取/并发；`ReplayReport.rejections` 无长度上限（异常链上可能增长）。
9. `InMemoryStateStore` 不落盘。M1 的立场是状态必须能从日志重建，所以没引入数据库。
10. 快照的储备只到「最后一次 Sync」的粒度，flashblock/子块级别的中间态不在 M1 范围内。

---

## 9. M1 Status

**COMPLETE**

对照 §36 的完成定义（逐条，均可复跑）：

| 条件 | 状态 | 证据 |
| --- | --- | --- |
| A. Chain 能读真实历史 block/receipt/logs | ✅ | `fixtures/real/block-37257255.json` 由 `HttpChainAdapter::get_block_data` 抓取 |
| B. 至少一个真实协议的真实 Pool | ✅ | `v2-compatible` @ `0x3978e57b…`（第 2、3 节） |
| C. 至少确认 pool / token0 / token1 | ✅ | `the_committed_registry_attests_the_real_pool_with_evidence` |
| D. 从权威 state event 恢复 reserve0/reserve1 | ✅ | `the_real_block_replays_to_the_pools_real_reserves` |
| E. 同块按 transaction_index + log_index 确定性处理 | ✅ | `a_blocks_last_log_is_its_last_word`（乱序文件）、`chain/src/recorded.rs` 校验 |
| F. 相同输入相同结果 | ✅ | `two_runs_of_the_same_input_are_identical` + fixture SHA-256 重跑一致 |
| G. 所有 unit/integration tests 通过 | ✅ | 52 passed / 0 failed |
| H. 至少一个真实历史区块的 PoolState 验证成功 | ✅ | block 37257255，第 5、6 节 |
| I. 结果可追溯 Pool→Evidence→Block→Transaction→Log→State | ✅ | `data/protocols/v2-giwap-sepolia.json` 的三组 `EvidenceRef` |

M1 期间明确没有实现（按任务书的禁止清单）：Graph、套利识别、Bellman-Ford/环搜索、
机会排序、REVM、模拟、bundle、私有中继、bribe、signer、nonce、执行、mempool、V3/V4、
清算、三明治、NFT/IAM/AI MEV、Web Dashboard、微服务、PostgreSQL/Redis/Kafka/ClickHouse。
