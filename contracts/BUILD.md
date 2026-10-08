# ArbitrageExecutor — 编译与复现说明

M10 §4 的结论是本仓库原本没有任何 Solidity 工具链（详见
`docs/v0.1/M10 Semantic Audit.md` §1.2/§2），所以这里只做一件最小的事：一个固定版本的
`solc` 可执行文件 + 一条编译命令。没有引入 Foundry、Hardhat、npm 包或任何新框架。

## 1. 编译器固定版本

| 项 | 实测值 |
|---|---|
| 本地路径 | `target/tools/solc-0.8.37`（`/target` 被根 `.gitignore` 忽略，不进版本库） |
| 自报版本 | `0.8.37+commit.f401782d.Darwin.appleclang` |
| 文件字节数 | 32 260 112 |
| 本地 sha256 | `a27396e7732aa52e80ff89ad7bd8a2e46fec2a6dcc4ef20cd16e5e0c502d6821` |
| 本地 keccak256 | `3581bfdf7211ad70fe593bd43db6e355918886e22361f006f8a39f44627c7ae7` |

这两个摘要与上游清单里 `version == 0.8.37` 条目的 `sha256` / `keccak256` 字段**逐字相同**：

```bash
curl -sS https://binaries.soliditylang.org/macosx-amd64/list.json
# builds[] 里 version=="0.8.37" 的条目：
#   path        solc-macosx-amd64-v0.8.37+commit.f401782d
#   sha256      a27396e7732aa52e80ff89ad7bd8a2e46fec2a6dcc4ef20cd16e5e0c502d6821
#   keccak256   3581bfdf7211ad70fe593bd43db6e355918886e22361f006f8a39f44627c7ae7
```

上游用 keccak256（不是 sha256）描述二进制，所以两个摘要各自比了一次：只对上任何一个都不算证明。
本地 keccak 用 `openssl dgst -keccak-256` 计算；该实现的正对照是
`keccak256("")= c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470`
（Ethereum 空串摘要的公开值）与 `keccak256("abc")= 4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45`。

## 2. 编译命令（唯一一条）

```bash
target/tools/solc-0.8.37 --optimize \
  --bin --bin-runtime --abi --hashes --overwrite \
  -o <outdir> contracts/ArbitrageExecutor.sol
```

`contracts/artifacts/` 里四个文件就是这条命令的直接输出，未做任何改写：

| 文件 | 内容 | 字节数 | sha256（文件本身，即 hex 文本） |
|---|---|---|---|
| `ArbitrageExecutor.bin` | 部署字节码（hex 文本） | 15 130 | `69ae3d937671666e2fe1e05b86c88a0b4d51587fb052bcbb967c79609d9a2680` |
| `ArbitrageExecutor.bin-runtime` | 运行时字节码（hex 文本） | 14 694 | `5befa5296f79ba9e452b8360897de976fe76f5e23a1da7bf285af6a3dec96bb7` |
| `ArbitrageExecutor.abi` | ABI JSON | 8 132 | `393067317a1b7ab1b1272a6ee5737e99dfcf6b440c75ecf1c8b925ab59968722` |
| `ArbitrageExecutor.signatures` | 函数/错误/事件选择子表 | 1 969 | `49e2706ed8a3cccb632497daf1debf5a29ecf79c278b8596b33eb4075ad06437` |

源码：`contracts/ArbitrageExecutor.sol`，425 行 / 21 407 字节，
sha256 `efd02248238066bdee62660e4cac1ff296145fb3a2b3418673d842c355158acb`。

## 3. 解码后的字节码摘要（链上比对用的那一把）

`xxd -r -p` 把 hex 文本还原成真实字节，摘要算在还原后的字节上：

| 段 | 解码后字节数 | sha256 | keccak256 |
|---|---|---|---|
| creation（部署）| 7 565 | `9c85456eee82f9309d15b889c1e4b842a957a174fe97025fd63a1cd49a6f9cec` | `8c49d87cc6173d83a36c97fd1f919555143e431b4124fea08368054a676c7263` |
| runtime（部署后链上 code）| 7 347 | `6b221d2ece17958b972d0b25202073649acf7af73f5e8e6740db39d0a49b8ccf` | `917ed914c157e86958d0b2de9d85a62a3a62c381b49166e091e3b67255d42cca` |

```bash
xxd -r -p < contracts/artifacts/ArbitrageExecutor.bin-runtime > /tmp/runtime.bin
openssl dgst -keccak-256 /tmp/runtime.bin    # 期望 917ed914…
```

运行时 7 347 字节 < EIP-170 上限 24 576，所以部署不会被长度规则拒绝。
`keccak256(runtime)` 是 M10 §46/§47 要的 `bytecode_hash`：真实部署后用
`eth_getCode(executor, block)` 还原字节再算一次，两个数必须相同——这一条把
「Rust 侧 calldata 对应的 ABI/字节码 ≠ 链上实际部署的那份」钉死。

## 4. 确定性门禁（M10 §49 的合约侧）

同一条命令、同一台机器、两个不同输出目录，四次产物 `cmp` 全部逐字节相同；
把 `contracts/artifacts/` 与一次全新编译的产物直接 `cmp`，四个文件也全部相同：

```bash
for f in ArbitrageExecutor.{abi,bin,bin-runtime,signatures}; do
  cmp contracts/artifacts/$f <outdir>/$f && echo "CMP EQUAL $f"
done
```

上面那几条 `cmp` 是 M10-P1 期间**手工跑**的一次性测量，记录在这里；仓库里的任何测试都不会调用
`solc`（全仓库只有 `crates/cli/tests/` 的四处在 spawn 子进程，跑的是本项目的 CLI 二进制）。
日常 cargo 关卡验证的是另外两件事：`crates/execution/tests/executor_evidence_gate.rs` 从
`contracts/ArbitrageExecutor.sol` 源码重新推导出 selector、ABI 条目与字节码摘要，再和 solc 早已写好的
那四个产物文件逐字节对账；`crates/execution/tests/executor_giwa_live.rs` 里未 `#[ignore]` 的
`the_abi_identity_agrees_with_the_build_record` 把 §3 那把链上摘要和本文件对得上。
所以「产物 == 现编译」这一条的证据是**记录 + 源码↔产物对账**，不是每次关卡真去重编译。

## 5. 为什么没有 `--via-ir`

legacy 代码生成有 16 槽栈上限，本文件早期三次 `Stack too deep`（`execute` 主体、leg 循环、
`swap` 调用点）都是通过把阶段拆成 `_checkRoute` / `_pullInput` / `_checkSides` / `_pushInput` /
`_pullOutput` / `_settle` 解决的，而不是换编译器管线。代价是函数变多，收益是编译只需一条命令、
产物可用普通 `--optimize` 复现，不需要为一条合约引入 IR 管线及其不可复现的中间层。
