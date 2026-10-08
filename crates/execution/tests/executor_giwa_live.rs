//! §57's ladder, run against a live GIWA node.
//!
//! This file is the only thing in the milestone that puts bytes on a network. Everything
//! else in M10 — the contract, the plan, the calldata, the REVM runs, the lifecycle ladder —
//! is decided offline against a scripted endpoint, and a scripted endpoint can only say what
//! the code *would* do. §57 asks what the chain answers.
//!
//! ```text
//! GIWA_RPC_URL=<the endpoint this session is configured for> \
//! cargo test -p evm-execution --test executor_giwa_live -- --ignored --nocapture
//! ```
//!
//! The key comes from `GIWA_EXECUTION_PRIVATE_KEY` and is read once (§19/§35): it is never
//! printed, never written into an evidence file, and never a literal in this repository. The
//! endpoint URL is never a literal either (§5's "configure, don't bake in") — it arrives from
//! the environment, and the value that was used is recorded in the evidence directory.
//!
//! ## What the run does, in order
//!
//! ```text
//! deploy the executor            (creation, to: None, value: 0)
//! allowlist two pairs            (setPairAllowed ×2)
//! allowlist two tokens           (setTokenAllowed ×2)
//! wrap native into WETH          (the one step with msg.value — §34)
//! approve the executor           (ERC20 approve, exactly the principal)
//! read reserves at a pinned head (the route's market facts, from the chain)
//! simulate the route in REVM     (three runs on one shared pinned cache: a profit floor, a
//!                                  one-wei floor, and §58's floor no market can pay)
//! execute through the lifecycle  (ExecutablePlan → intent → build → sign → submit → receipt)
//! reconcile balances             (§31/§32: native, both ERC20s, L2 bill and L1 fee)
//! execute with a floor too high  (§58: included, status 0, no partial state)
//! ```
//!
//! ## What it is allowed to claim
//!
//! A receipt with `status = 1` is the executor primitive working on a live chain. It is not a
//! profit: §52 requires an observed route *and* a measured balance delta *and* a proven
//! denomination, and §59 requires the two verdicts to stay separate — `M10 code/tests
//! COMPLETE` can be true while `real profitable arbitrage = NOT_PROVEN`. The amounts are
//! deliberately tiny (§57's "所有金额保持极小"), which is also why the run is cheap enough to
//! repeat: the principal is 1e14 wei of WETH and every step's ceiling is bounded by
//! [`CALL_GAS`] / [`CREATION_GAS`] times the fee the node itself quoted.
//!
//! Nothing here adds a hot-path read (§51): every RPC this file issues belongs to the operator
//! session or to the plan's own pricing, and the reconciliation reads the receipts the ladder
//! already returned plus the balances a controlled test cannot do without.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{address, keccak256, Address, Bytes, U256};
use serde_json::{json, Value};

use evm_chain::{CallRequest, ChainAdapter, HttpChainAdapter};
use evm_core::{BlockNumber, ChainId, Fee};
use evm_execution::{
    Abilities, AmountDerivation, ArbitrageExecutionPlan, BuildPolicy, ChainHead, DeployPolicy,
    Deployer, EndpointKind, ExecutablePlan, ExecutionBinding, ExecutionKey, ExecutionMode,
    ExecutionSetup, ExecutionStage, FeePolicy, Freshness, GasPolicy, GiwaSequencerDirect,
    MarketKind, PlanLeg, PlanValidity, ProfitDenomination, ProfitPolicy, ReceiptPolicy,
    SenderFunding, Signer, SimulationContext, SimulationOutcome, TransactionType, PRIVATE_KEY_ENV,
};
use evm_metrics::{Clock, Metrics};
use evm_opportunity::math::swap_exact_in;
use evm_protocol::{CallReturn, ExecutorCall, ExecutorLeg, V2Call};
use evm_simulation::executor::{run as executor_run, ExecutorRun};
use evm_simulation::gas::GasPricing;
use evm_simulation::request::EvmRules;
use evm_simulation::state::{BlockPin, RpcStateProvider, StateProvider};

const CHAIN: u64 = 91_342;

/// The two pools that share a token pair on this chain, and the pair itself. Chain data, not
/// configuration: these are the addresses `data/evidence/m8/optimization/raw/baseline/run-001/
/// route-91342-37607876-1790952994619/preflight.json` recorded from the node, and every one of
/// them is re-read from the chain below before it is trusted.
const POOL_A: Address = address!("2a3ceafba30f6626170cbb0cd67392efb94bd9a4");
const POOL_B: Address = address!("5b3c1e3fb6a97c0130ae015ff10f53a1a30c353e");
const TOKEN_0: Address = address!("07d4af6e2bc8dd82beb06b4fd279df4c9028f26f");
const WETH: Address = address!("4200000000000000000000000000000000000006");

/// §57's "所有金额保持极小": 0.0001 of the wrapped native asset, the same principal size M7's
/// controlled sequence used.
const INPUT_WEI: u128 = 100_000_000_000_000;
const CREATION_GAS: u64 = 5_000_000;
const CALL_GAS: u64 = 600_000;
/// Long enough for a testnet block to be sealed and read back, short enough that a stuck step
/// is discovered in this run rather than in the next one.
const RECEIPT_ATTEMPTS: usize = 30;
const RECEIPT_POLL: Duration = Duration::from_millis(1_000);
/// How many of one simulation's independent state reads may be outstanding at the node at
/// once (M8.3.3's knob). One REVM run on RPC state is tens of sequential `eth_call`s, and the
/// whole gap between "priced" and "sent" in this ladder is that run's wall time on a
/// ~1-block-per-second chain — so the reads M8.3.3 proved safe to batch go out batched.
const SIM_READ_CONCURRENCY: usize = 8;
/// §8's window as this ladder declares it, and the two measurements behind it: with three cold
/// REVM runs at concurrency 1 the price-to-send gap was 33 sealed blocks; with one shared
/// pinned read cache at concurrency 4 it was 14 (`real/giwa_execution.json` records the gap of
/// the run being read, beside this bound, every time). The bound is that second number with
/// room, not a number chosen to make the check pass — the check itself is what §8's gate is
/// for, and `tests/executor_lifecycle.rs`'s stale-plan case proves it fires. A wider window
/// cannot buy a silent worse fill: the contract asks exact outputs (§14), so a market that
/// moved inside the window reverts rather than fills badly.
const MAX_BLOCK_AGE: u64 = 20;

/// §47's ABI identity: which compiler produced the artifact and which file the bytes came
/// from. The compiler's self-reported version is written here; the artifact digest is read
/// out of `contracts/BUILD.md`'s table at run time, because §17's key scan reads this crate's
/// files whole and a contiguous 64-hex-digit token is exactly the shape of a private key. The
/// record is the source of truth either way, and `the_abi_identity_agrees_with_the_build_record`
/// is what stops the record and the evidence from drifting apart unnoticed.
const SOLC_VERSION: &str = "solc 0.8.37+commit.f401782d";

/// The sha256 `contracts/BUILD.md` records for `contracts/artifacts/ArbitrageExecutor.bin`.
fn bin_sha256_from_build_record() -> String {
    let text = std::fs::read_to_string(workspace_root().join("contracts/BUILD.md"))
        .expect("contracts/BUILD.md is the record §47's identity quotes");
    for line in text.lines() {
        if !line.starts_with("| `ArbitrageExecutor.bin` |") {
            continue;
        }
        let cell = line
            .split('|')
            .map(str::trim)
            .rfind(|cell| !cell.is_empty())
            .unwrap_or_default()
            .trim_matches('`')
            .to_string();
        if cell.len() != 64 || !cell.chars().all(|c| c.is_ascii_hexdigit()) {
            panic!("the build record's bin row does not carry a digest: {cell}");
        }
        return cell;
    }
    panic!("contracts/BUILD.md has no `ArbitrageExecutor.bin` artifact row");
}

/// The identity string the deployment writes into every §60 artifact.
fn abi_version() -> String {
    format!(
        "{SOLC_VERSION}, ArbitrageExecutor.bin sha256 {}",
        bin_sha256_from_build_record()
    )
}

/// §48's replayability, in two file reads: the identity this script would write is the one
/// `contracts/BUILD.md` records, and it is the one the committed evidence already carries. A
/// digest that differs between the two is a different contract, and §52 forbids calling that a
/// success — so this refuses quietly rebuilding evidence against bytes nobody described.
#[test]
fn the_abi_identity_agrees_with_the_build_record() {
    let composed = abi_version();
    let path = workspace_root().join("data/evidence/m10/contract/deployment.json");
    let published: Value = serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display())),
    )
    .expect("the deployment evidence is json");
    let recorded = published["abi_version"]
        .as_str()
        .unwrap_or_else(|| panic!("{} carries no abi_version", path.display()));
    assert_eq!(
        recorded, composed,
        "the build record's digest and the digest the committed evidence published are not the \
         same contract"
    );
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/execution sits two levels below the workspace root")
        .to_path_buf()
}

fn write_evidence(relative: &str, value: &Value) {
    let path = workspace_root().join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|error| panic!("{}: {error}", parent.display()));
    }
    let text = serde_json::to_string_pretty(value).expect("evidence is serializable");
    std::fs::write(&path, format!("{text}\n"))
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    println!("wrote {}", path.display());
}

fn env_required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} is required: this target drives a \
        live node (§57), and no endpoint or key is baked into the source (§5, §19)"
        )
    })
}

/// One balance from an evidence row, which stores wei as a decimal string so a reader can
/// compare it against a receipt without a 256-bit parser. `None` is an unread balance, and
/// every downstream delta keeps it as `null` rather than as zero (§52).
fn decimal(value: &Value) -> Option<U256> {
    value
        .as_str()
        .and_then(|text| U256::from_str_radix(text, 10).ok())
}

/// `after - before`, spelled as a signed decimal string: a movement the other direction reads
/// as a negative number instead of a wrapped 256-bit quantity.
fn delta(before: Option<U256>, after: Option<U256>) -> Option<String> {
    let (before, after) = (before?, after?);
    Some(if after >= before {
        format!("{}", after - before)
    } else {
        format!("-{}", before - after)
    })
}

/// The positive half of [`delta`], kept as a number rather than as text: a movement the other
/// direction is not a gain, and a gross that cannot be formed stays `null` instead of becoming
/// a wrapped 256-bit quantity.
fn gain(before: Option<U256>, after: Option<U256>) -> Option<U256> {
    let (before, after) = (before?, after?);
    (after >= before).then_some(after - before)
}

/// Lifted addition, so one unread balance propagates as `null`.
fn sum(a: Option<U256>, b: Option<U256>) -> Option<U256> {
    Some(a? + b?)
}

/// The node, reached twice on purpose: the chain adapter serves the plan's reads and the
/// REVM state provider, the Giwa adapter serves the four execution abilities. One HTTP
/// endpoint, two typed views of it, so no read path is shared with the send path.
struct Node {
    url: String,
    chain: Arc<HttpChainAdapter>,
    adapter: Arc<GiwaSequencerDirect>,
}

impl Node {
    async fn connect(url: &str) -> Result<Self, String> {
        let chain = HttpChainAdapter::connect(url)
            .await
            .map_err(|error| format!("connect {url} as a chain reader: {error}"))?;
        let adapter = GiwaSequencerDirect::connect(
            url,
            CHAIN,
            ExecutionMode::Submit,
            EndpointKind::PublicHttpRpc,
        )
        .await
        .map_err(|error| format!("connect {url} for execution: {error}"))?;
        Ok(Self {
            url: url.to_string(),
            chain: Arc::new(chain),
            adapter: Arc::new(adapter),
        })
    }

    fn abilities(&self) -> Abilities {
        Abilities {
            submitter: self.adapter.clone(),
            fees: self.adapter.clone(),
            nonces: self.adapter.clone(),
            chain: self.adapter.clone(),
        }
    }

    /// The head as one atomic answer: number and hash out of the same header, which is the
    /// only shape §8's pin can take. A caller-supplied head is re-verified by the deployer
    /// before it signs, so a stale number costs a read and no gas.
    async fn head(&self) -> Result<ChainHead, String> {
        let number = self
            .chain
            .latest_block()
            .await
            .map_err(|error| format!("latest block: {error}"))?;
        let block = self
            .chain
            .get_block(number)
            .await
            .map_err(|error| format!("block {}: {error}", number.0))?;
        Ok(ChainHead {
            number: block.number.0,
            hash: block.hash,
        })
    }

    async fn call(&self, at: u64, to: Address, data: Bytes) -> Result<Bytes, String> {
        self.chain
            .call(
                BlockNumber(at),
                &CallRequest {
                    to,
                    data: data.clone(),
                },
            )
            .await
            .map_err(|error| {
                format!(
                    "eth_call at {at} on {to:#x} ({len} bytes): {error}",
                    len = data.len()
                )
            })
    }

    /// One 32-byte amount word back from a view call, decoded by the protocol crate's own
    /// decoder so the selector and the return shape are checked together.
    async fn word(&self, at: u64, to: Address, call: &V2Call) -> Result<U256, String> {
        let raw = self.call(at, to, call.encode()).await?;
        match call
            .decode_return(&raw)
            .map_err(|error| format!("decode {signature}: {error}", signature = call.signature()))?
        {
            CallReturn::Amount(amount) => Ok(amount),
            other => Err(format!(
                "{} answered {other:?}, which is not one amount word",
                call.signature()
            )),
        }
    }

    /// One address word back — `token0()`/`token1()`, which the protocol crate decodes as an
    /// address and not as an amount, so it cannot share [`Node::word`].
    async fn address_word(&self, at: u64, to: Address, call: &V2Call) -> Result<Address, String> {
        let raw = self.call(at, to, call.encode()).await?;
        match call
            .decode_return(&raw)
            .map_err(|error| format!("decode {signature}: {error}", signature = call.signature()))?
        {
            CallReturn::Address(address) => Ok(address),
            other => Err(format!(
                "{} answered {other:?}, which is not one address word",
                call.signature()
            )),
        }
    }

    /// A pool's own three answers: which two tokens it trades and what reserves it reports.
    async fn reserves(&self, at: u64, pool: Address) -> Result<Value, String> {
        let token0 = self.address_word(at, pool, &V2Call::Token0).await?;
        let token1 = self.address_word(at, pool, &V2Call::Token1).await?;
        let raw = self.call(at, pool, V2Call::GetReserves.encode()).await?;
        let reserves = match V2Call::GetReserves
            .decode_return(&raw)
            .map_err(|error| format!("getReserves at {pool:#x}: {error}"))?
        {
            CallReturn::Reserves(reserves) => reserves,
            other => return Err(format!("{pool:#x} getReserves answered {other:?}")),
        };
        Ok(json!({
            "pool": format!("{pool:#x}"),
            "token0": format!("{token0:#x}"),
            "token1": format!("{token1:#x}"),
            "reserve0": reserves.reserve0.to_string(),
            "reserve1": reserves.reserve1.to_string(),
            "block_timestamp_last": reserves.block_timestamp_last.to_string(),
            "read_at_block": at,
        }))
    }

    async fn allowance(
        &self,
        at: u64,
        token: Address,
        owner: Address,
        spender: Address,
    ) -> Result<U256, String> {
        // `allowance(address,address)` is 0xdd62ed3e. The protocol crate's `V2Call` has no
        // variant for it, so the selector is written here — and it is not trusted: the value
        // has to come back as exactly the approval the ladder just mined, and the executor
        // contract reads the same slot itself inside the REVM run below. A wrong selector
        // cannot produce both of those.
        let mut data = Vec::with_capacity(68);
        data.extend_from_slice(&[0xdd, 0x62, 0xed, 0x3e]);
        data.extend_from_slice(&[0u8; 12]);
        data.extend_from_slice(owner.as_slice());
        data.extend_from_slice(&[0u8; 12]);
        data.extend_from_slice(spender.as_slice());
        let raw = self.call(at, token, Bytes::from(data)).await?;
        if raw.len() != 32 {
            return Err(format!(
                "allowance({owner:#x}, {spender:#x}) at {at} returned {} bytes, not one word",
                raw.len()
            ));
        }
        Ok(U256::from_be_slice(&raw))
    }

    /// The wallet-side row §31/§32's reconciliation needs: one block, four numbers, two
    /// accounts. Every field is a read, so an absent read is a `null` in the evidence and
    /// never a zero.
    async fn wallet(&self, at: u64, operator: Address, executor: Address) -> Result<Value, String> {
        Ok(json!({
            "block_number": at,
            "native_wei": self.chain.get_balance(BlockNumber(at), operator).await.map_err(|e| format!("{e}"))?.to_string(),
            "weth_operator_wei": self.word(at, WETH, &V2Call::BalanceOf { owner: operator }).await?.to_string(),
            "weth_executor_wei": self.word(at, WETH, &V2Call::BalanceOf { owner: executor }).await?.to_string(),
            "token0_operator_wei": self.word(at, TOKEN_0, &V2Call::BalanceOf { owner: operator }).await?.to_string(),
            "token0_executor_wei": self.word(at, TOKEN_0, &V2Call::BalanceOf { owner: executor }).await?.to_string(),
            "weth_allowance_to_executor_wei": self.allowance(at, WETH, operator, executor).await?.to_string(),
        }))
    }
}

/// One REVM run of the route, in the two shapes §57 and §58 need.
struct Simmed {
    outcome: evm_simulation::executor::ExecutorOutcome,
    call: ExecutorCall,
}

/// The state source the ladder's simulations share: one pinned block, one read cache, and
/// [`SIM_READ_CONCURRENCY`] independent reads outstanding. The three runs below differ only in
/// the floor inside the calldata, so the second and third hit the first run's cache — the
/// point of sharing it is that the wall time between pricing and sending is what §8's
/// freshness check measures, and re-reading the same state over HTTP is what made it 33 blocks.
fn provider_at(node: &Node, head: &ChainHead) -> Arc<dyn StateProvider> {
    Arc::new(RpcStateProvider::with_state_read_concurrency(
        node.chain.clone() as Arc<dyn ChainAdapter>,
        BlockPin::new(BlockNumber(head.number), head.hash),
        true,
        SIM_READ_CONCURRENCY,
    ))
}

async fn simulate(
    provider: Arc<dyn StateProvider>,
    head: &ChainHead,
    executor: Address,
    operator: Address,
    legs: Vec<ExecutorLeg>,
    min_final_amount: U256,
    tip_wei: u128,
) -> Result<Simmed, String> {
    let call = ExecutorCall::Execute {
        legs,
        input_token: WETH,
        amount_in: U256::from(INPUT_WEI),
        min_final_amount,
        recipient: operator,
    };
    let spec = ExecutorRun {
        chain_id: ChainId(CHAIN),
        priced_at: BlockNumber(head.number),
        // The engine refuses a run whose name is not the provider's own identity string, so
        // this quotes the provider rather than restating it. Which block and which header hash
        // the run actually read is proved below by `ExecutorOutcome::block`, not by this label.
        state_source: provider.source(),
        executor,
        operator,
        call: call.clone(),
        gas_limit: CALL_GAS,
        rules: EvmRules::Prague,
        pricing: GasPricing::Eip1559 {
            priority_fee_per_gas: tip_wei,
            provenance: format!(
                "the tip this session's fee reading quoted from the node ({tip_wei} wei/gas); \
                 the base fee comes from the pinned header, not from here"
            ),
        },
        // §58's one permitted state edit is the operator's native balance. The real operator
        // account is already funded, so a real run declares no endowment at all.
        endowment: None,
    };
    let outcome = executor_run(provider, &spec)
        .await
        .map_err(|error| format!("REVM refused to answer: {error}"))?;
    Ok(Simmed { outcome, call })
}

#[tokio::test]
#[ignore = "broadcasts real transactions on a live chain (§57); needs GIWA_RPC_URL and \
             GIWA_EXECUTION_PRIVATE_KEY, and spends testnet gas on every run"]
async fn the_ladder_runs_on_giwa() {
    let url = env_required("GIWA_RPC_URL");
    let key_text = env_required(PRIVATE_KEY_ENV);
    let report = run_ladder(&url, &key_text)
        .await
        .expect("the ladder stopped: see the printed reason");
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("serializable")
    );
}

async fn run_ladder(url: &str, key_text: &str) -> Result<Value, String> {
    let node = Node::connect(url).await?;
    // The key text is read from the environment once, by the caller of this function. It is
    // parsed here into an address and then re-parsed for each session's signer, because
    // [`ExecutionKey`] owns a signing key and is not cloneable — a key that could be copied
    // freely would be a key that could be handed to a log.
    let key = ExecutionKey::from_hex(key_text).map_err(|e| format!("the key: {e}"))?;
    let operator = key.address();
    let signer = || -> Result<Signer, String> {
        let key = ExecutionKey::from_hex(key_text).map_err(|e| format!("the key: {e}"))?;
        Ok(Signer::from_key(ExecutionMode::Submit, key))
    };

    let policy = DeployPolicy {
        creation_gas_limit: CREATION_GAS,
        call_gas_limit: CALL_GAS,
        tx_type: TransactionType::DynamicFee,
        fee: FeePolicy::BaseFeeHeadroom { headroom_blocks: 2 },
        receipt: ReceiptPolicy {
            attempts: RECEIPT_ATTEMPTS,
            between_attempts: RECEIPT_POLL,
        },
    };
    let mut deployer = Deployer::new(node.abilities(), signer()?, policy.clone(), CHAIN)
        .map_err(|e| format!("the operator session refused to open: {e}"))?;
    let verified_chain = deployer
        .verify_chain()
        .await
        .map_err(|e| format!("§7's third opinion: {e}"))?;
    if verified_chain != CHAIN {
        return Err(format!("the endpoint answers for chain {verified_chain}"));
    }
    if deployer.sender().map_err(|e| e.to_string())? != operator {
        return Err("the session's sender and the parsed key disagree".to_string());
    }

    let artifact = workspace_root().join("contracts/artifacts/ArbitrageExecutor.bin");
    let hex_text =
        std::fs::read_to_string(&artifact).map_err(|e| format!("{}: {e}", artifact.display()))?;
    let creation_code = hex::decode(hex_text.trim())
        .map_err(|e| format!("{} is not hex: {e}", artifact.display()))?;

    let mut steps: Vec<Value> = Vec::new();
    // Pre-flight: the two pools answer before anything is spent. Nothing here trusts the
    // addresses at the top of this file — a pool that does not report the pair it is named
    // for is a reason to stop before the creation transaction, not a row in the evidence.
    let mut preflight = Vec::new();
    let probe_head = node.head().await?;
    for pool in [POOL_A, POOL_B] {
        let row = node
            .reserves(probe_head.number, pool)
            .await
            .unwrap_or_else(|error| {
                json!({
                    "pool": format!("{pool:#x}"),
                    "error": error,
                    "read_at_block": probe_head.number,
                })
            });
        let ok = row["error"].is_null();
        preflight.push(row);
        if !ok {
            return Err(format!(
                "pool {pool:#x} did not answer the pre-flight reads at block {}: see the \
                 preflight rows in the evidence",
                probe_head.number
            ));
        }
    }
    for row in &preflight {
        let pool = row["pool"].as_str().unwrap_or_default();
        if row["token0"] != json!(format!("{WETH:#x}"))
            && row["token1"] != json!(format!("{WETH:#x}"))
        {
            return Err(format!(
                "pool {pool} does not trade the chain's wrapped native asset"
            ));
        }
        if row["token0"] != json!(format!("{TOKEN_0:#x}"))
            && row["token1"] != json!(format!("{TOKEN_0:#x}"))
        {
            return Err(format!("pool {pool} does not trade {TOKEN_0:#x}"));
        }
    }
    // §57's preconditions, written before a single wei is spent. If the ladder stops at a
    // later rung, this file is still the record of what the chain looked like when it started
    // — which is the half of the evidence a run that failed would otherwise not leave behind.
    let native_before = node
        .chain
        .get_balance(BlockNumber(probe_head.number), operator)
        .await
        .map_err(|e| format!("balance of {operator:#x} at {}: {e}", probe_head.number))?;
    write_evidence(
        "data/evidence/m10/real/preconditions.json",
        &json!({
            "what_this_is": "§57's preconditions for the controlled ladder, read before the \
                             creation transaction was signed",
            "chain_id": CHAIN,
            "endpoint": node.url,
            "endpoint_source": "GIWA_RPC_URL at run time; no URL is baked into the source (§5)",
            "key_source": format!("`{PRIVATE_KEY_ENV}`, read once by this test; the value is \
                                   never printed, stored or written to evidence (§19/§35)"),
            "operator": format!("{operator:#x}"),
            "head_at_first_step": {
                "number": probe_head.number,
                "hash": format!("{:#x}", probe_head.hash),
            },
            "native_balance_wei": native_before.to_string(),
            "planned_principal_wei": INPUT_WEI.to_string(),
            "gas_ceiling_wei": {
                "creation": CREATION_GAS.to_string(),
                "call": CALL_GAS.to_string(),
            },
            "receipt_policy": {
                "attempts": RECEIPT_ATTEMPTS,
                "between_attempts_ms": RECEIPT_POLL.as_millis().to_string(),
            },
            "pools": [format!("{POOL_A:#x}"), format!("{POOL_B:#x}")],
            "input_token": format!("{WETH:#x}"),
            "second_token": format!("{TOKEN_0:#x}"),
            "preflight_reads": preflight.clone(),
        }),
    );
    // §57's first rung: the creation. The head is whatever the node calls latest right now;
    // the deployer re-verifies it against its own hash before signing.
    let head = node.head().await?;
    let deployment = deployer
        .deploy(head, &creation_code, operator, &abi_version())
        .await
        .map_err(|e| format!("the creation did not resolve: {e}"))?;
    let executor = deployment
        .deployed()
        .ok_or_else(|| "the receipt carried no contract address".to_string())?;
    if !deployment.address_proved() {
        let predicted = deployment.predicted_address;
        let reported = deployment
            .deployed()
            .map(|a| format!("{a:#x}"))
            .unwrap_or_default();
        return Err(format!(
            "the receipt's address {reported} is not the predicted {predicted:#x}"
        ));
    }
    steps.push(deployment.step.to_json());
    write_evidence(
        "data/evidence/m10/contract/deployment.json",
        &deployment.to_json(),
    );

    // Rungs 2..5: the allowlists. Both must hold before a route can be priced, because a
    // plan against a contract that does not accept its pool is a plan the contract refuses.
    for (label, call) in [
        (
            "allow-pair-a",
            ExecutorCall::SetPairAllowed {
                pair: POOL_A,
                allowed: true,
            },
        ),
        (
            "allow-pair-b",
            ExecutorCall::SetPairAllowed {
                pair: POOL_B,
                allowed: true,
            },
        ),
        (
            "allow-token-0",
            ExecutorCall::SetTokenAllowed {
                token: TOKEN_0,
                allowed: true,
            },
        ),
        (
            "allow-weth",
            ExecutorCall::SetTokenAllowed {
                token: WETH,
                allowed: true,
            },
        ),
    ] {
        let step = deployer
            .call(
                node.head().await?,
                executor,
                call.encode(),
                label.to_string(),
            )
            .await
            .map_err(|e| format!("{label}: {e}"))?;
        let json = step.to_json();
        steps.push(json);
        if !step.succeeded() {
            return Err(format!("{label} did not succeed: {}", step.detail));
        }
    }

    // Funding: §34's ERC20-only route still needs its input token, and the only honest way to
    // get WETH is the deposit that wraps native value — the one step of the ladder with
    // `msg.value`, and the reason [`Deployer::funding_call`] is a separate method.
    let wrap = deployer
        .funding_call(
            node.head().await?,
            WETH,
            V2Call::Deposit.encode(),
            U256::from(INPUT_WEI),
            "wrap-principal".to_string(),
        )
        .await
        .map_err(|e| format!("the wrap: {e}"))?;
    steps.push(wrap.to_json());
    if !wrap.succeeded() {
        return Err(format!("the wrap reverted: {}", wrap.detail));
    }

    let approve = deployer
        .call(
            node.head().await?,
            WETH,
            V2Call::Approve {
                spender: executor,
                value: U256::from(INPUT_WEI),
            }
            .encode(),
            "approve-executor".to_string(),
        )
        .await
        .map_err(|e| format!("the approval: {e}"))?;
    steps.push(approve.to_json());
    if !approve.succeeded() {
        return Err(format!("the approval reverted: {}", approve.detail));
    }

    // The state the plan is priced against has to be the state the approval produced, so the
    // pin is taken after that receipt, and the wallet row proves the two funding steps landed.
    let head = node.head().await?;
    let before = node.wallet(head.number, operator, executor).await?;
    let principal =
        U256::from_str_radix(before["weth_operator_wei"].as_str().unwrap(), 10).unwrap();
    // The ladder is a sequence of transactions, not an atomic reset: re-running it on a wallet
    // an earlier run already funded leaves that run's WETH sitting here. The check is therefore
    // "the wrap landed", which is what the rung proves, and the surplus is recorded rather than
    // silently folded into the principal — §31's deltas read the same `before` row either way.
    if principal < U256::from(INPUT_WEI) {
        return Err(format!(
            "the wallet holds {principal} wei of WETH after the wrap, short of the {INPUT_WEI} \
             the ladder sent — the funding steps and the balance read disagree"
        ));
    }
    let granted = U256::from_str_radix(
        before["weth_allowance_to_executor_wei"].as_str().unwrap(),
        10,
    )
    .unwrap();
    if granted != U256::from(INPUT_WEI) {
        return Err(format!(
            "the allowance reads {granted}, not the {INPUT_WEI} the approval transaction sent"
        ));
    }
    let wallet_note = if principal > U256::from(INPUT_WEI) {
        format!(
            "the operator held {} wei of WETH before this run's wrap and {} wei after it; the \
             surplus is an earlier ladder run's residue, untouched by this one. The principal \
             this run prices and sends is {INPUT_WEI} wei, and every delta below is read \
             against this row.",
            principal - U256::from(INPUT_WEI),
            principal
        )
    } else {
        "the wallet holds exactly this run's wrapped principal".to_string()
    };

    let reserve_a = node.reserves(head.number, POOL_A).await?;
    let reserve_b = node.reserves(head.number, POOL_B).await?;
    let fee = Fee::new(997, 1000).expect("0.3% is a fraction");
    let round_trip = |first: &Value, second: &Value| -> Option<(U256, U256)> {
        let side = |row: &Value, weth: bool| -> U256 {
            let token0 = row["token0"].as_str().unwrap();
            let is_weth0 = token0 == format!("{WETH:#x}");
            let key = match (weth, is_weth0) {
                (true, true) | (false, false) => "reserve0",
                _ => "reserve1",
            };
            U256::from_str_radix(row[key].as_str().unwrap(), 10).unwrap()
        };
        let hop1 = swap_exact_in(
            side(first, true),
            side(first, false),
            fee,
            U256::from(INPUT_WEI),
        )
        .ok()?;
        let hop2 = swap_exact_in(side(second, false), side(second, true), fee, hop1).ok()?;
        Some((hop1, hop2))
    };
    let ab = round_trip(&reserve_a, &reserve_b);
    let ba = round_trip(&reserve_b, &reserve_a);
    let (buy, sell, ask1, ask2) = match (ab, ba) {
        (Some(x), Some(y)) => {
            if y.1 > x.1 {
                (POOL_B, POOL_A, y.0, y.1)
            } else {
                (POOL_A, POOL_B, x.0, x.1)
            }
        }
        (Some(x), None) => (POOL_A, POOL_B, x.0, x.1),
        (None, Some(y)) => (POOL_B, POOL_A, y.0, y.1),
        (None, None) => {
            return Err("neither direction prices a round trip at this head".to_string())
        }
    };
    let legs = vec![
        ExecutorLeg {
            pool: buy,
            token_in: WETH,
            token_out: TOKEN_0,
            amount_in: U256::from(INPUT_WEI),
            amount_out: ask1,
            min_amount_out: ask1,
        },
        ExecutorLeg {
            pool: sell,
            token_in: TOKEN_0,
            token_out: WETH,
            amount_in: ask1,
            amount_out: ask2,
            min_amount_out: ask2,
        },
    ];
    let tip = 1_000_000u128;
    let sim_provider = provider_at(&node, &head);

    // Three REVM answers on the same pinned head and the same legs, sharing one read cache:
    // the one that demands a profit, the one that only demands the route complete, and §58's
    // floor no market can pay. §59's separation lives in the first two rows.
    let profit_floor = simulate(
        Arc::clone(&sim_provider),
        &head,
        executor,
        operator,
        legs.clone(),
        U256::from(INPUT_WEI) + U256::from(1u8),
        tip,
    )
    .await?;
    let open_floor = simulate(
        Arc::clone(&sim_provider),
        &head,
        executor,
        operator,
        legs.clone(),
        U256::from(1u8),
        tip,
    )
    .await?;
    // §58's attribution run: the identical route with a floor no market can pay. The named
    // contract error is what lets the real `status = 0` be called a min-output failure
    // rather than an unexplained one.
    let too_high = simulate(
        Arc::clone(&sim_provider),
        &head,
        executor,
        operator,
        legs.clone(),
        U256::from(INPUT_WEI) * U256::from(100u128),
        tip,
    )
    .await?;

    let sim_row = |label: &str, sim: &Simmed| -> Value {
        json!({
            "label": label,
            "describe": sim.outcome.describe(),
            "succeeded": sim.outcome.succeeded(),
            "status": format!("{:?}", sim.outcome.status),
            "contract_error": sim.outcome.contract_error.clone(),
            "revert_kind": sim.outcome.revert_kind,
            "delivered": sim.outcome.delivered.map(|d| d.to_string()),
            "gas_used": sim.outcome.gas_used,
            "gas_limit": sim.outcome.gas_limit,
            "charge": format!("{:?}", sim.outcome.charge),
            "block": sim.outcome.block.number.0,
            "block_hash": format!("{:#x}", sim.outcome.block.hash),
            "calldata_len": sim.outcome.calldata_len,
            "calldata_hash": format!("{:#x}", keccak256(sim.outcome.calldata.as_ref())),
            "min_final_amount": sim.outcome.min_final_amount.to_string(),
            "reserves": sim.outcome.reserves.iter().map(|row| json!({
                "pool": format!("{:#x}", row.pool),
                "before": [row.before.reserve0.to_string(), row.before.reserve1.to_string()],
                "after": [row.after.reserve0.to_string(), row.after.reserve1.to_string()],
                "changed": row.changed(),
            })).collect::<Vec<_>>(),
            "balances": sim.outcome.balances.iter().map(|row| json!({
                "token": format!("{:#x}", row.token),
                "holder": format!("{:#x}", row.holder),
                "before": row.before.to_string(),
                "after": row.after.to_string(),
                "changed": row.changed(),
            })).collect::<Vec<_>>(),
        })
    };

    let binding = ExecutionBinding {
        chain_id: CHAIN,
        executor,
    };
    let plan_of = |sim: &Simmed, floor: U256| -> ArbitrageExecutionPlan {
        let delivered = sim.outcome.delivered.unwrap_or(U256::ZERO);
        ArbitrageExecutionPlan::new(
            CHAIN,
            executor,
            operator,
            operator,
            WETH,
            U256::from(INPUT_WEI),
            vec![
                PlanLeg {
                    pool: buy,
                    token_in: WETH,
                    token_out: TOKEN_0,
                    amount_in: U256::from(INPUT_WEI),
                    amount_out: ask1,
                    min_amount_out: ask1,
                    derivation: AmountDerivation::PlanInput,
                },
                PlanLeg {
                    pool: sell,
                    token_in: TOKEN_0,
                    token_out: WETH,
                    amount_in: ask1,
                    amount_out: ask2,
                    min_amount_out: ask2,
                    derivation: AmountDerivation::PreviousLegOutput,
                },
            ],
            floor,
            PlanValidity {
                simulated_at_block: BlockNumber(head.number),
                max_block_age: MAX_BLOCK_AGE,
                provenance: format!(
                    "§8's window for this run: the route was priced at canonical block {} and \
                     sent against the head the submitter re-verifies before signing. The bound \
                     is {MAX_BLOCK_AGE} blocks, which is the ladder's own measured \
                     price-to-send latency (three REVM runs sharing one pinned read cache); the \
                     gap this run actually took is recorded beside it in \
                     real/giwa_execution.json, and the contract asks exact outputs (§14) so a \
                     market that moved inside the window reverts rather than fills worse.",
                    head.number
                ),
            },
            SimulationContext {
                correlation_id: format!("m10-giwa-{buy:#x}-{sell:#x}-{INPUT_WEI}"),
                block_number: BlockNumber(head.number),
                block_hash: head.hash,
                state_fingerprint: format!("pinned-block-{}-{}", head.number, head.hash),
                simulation_id: keccak256(format!(
                    "m10-real|block={}|calldata={}",
                    head.number, sim.outcome.calldata_len
                )),
                outcome: if sim.outcome.succeeded() {
                    SimulationOutcome::Succeeded {
                        gas_used: sim.outcome.gas_used,
                        // The limit this run was executed under, carried forward because it — not
                        // the burn — is what the send limit is resolved against (§13/EIP-150).
                        proved_gas_limit: sim.outcome.gas_limit,
                        final_amount: delivered,
                    }
                } else {
                    SimulationOutcome::Reverted {
                        revert: sim
                            .outcome
                            .contract_error
                            .clone()
                            .unwrap_or_else(|| format!("{:?}", sim.outcome.status)),
                    }
                },
                funding: SenderFunding::RealState {
                    source: format!(
                        "§57's controlled funding: this run wrapped {INPUT_WEI} wei of the \
                         chain's native asset into WETH at {} and approved the executor for \
                         the same amount. The pools were not touched.",
                        wrap.block()
                            .map(|(n, _)| n.to_string())
                            .unwrap_or_else(|| "an unread block".to_string())
                    ),
                },
                market: MarketKind::RealMarket {
                    attested_by: format!(
                        "getReserves()/token0()/token1() read from the live node at canonical \
                         block {} ({:#x}); reserves quoted in this evidence file",
                        head.number, head.hash
                    ),
                },
            },
            ProfitPolicy {
                denomination: ProfitDenomination::TokenSettled {
                    token: WETH,
                    reason: "the route round-trips into WETH, so the floor is in WETH wei and \
                             the gas bill is not part of it (§34)"
                        .to_string(),
                },
                required_final_balance: floor,
                provenance: "§12's floor as the plan states it, mirrored by the contract's \
                             minFinalAmount in the same calldata"
                    .to_string(),
            },
        )
    };

    let executable = ExecutablePlan::new(plan_of(&open_floor, U256::from(1u8)), &binding)
        .map_err(|e| format!("the plan the simulation blessed did not validate: {e}"))?;

    // §13's gas policy for this run, spelled out rather than left buried in `Default`, because
    // the evidence has to name the margin the on-wire limit was resolved with.
    let build_policy = BuildPolicy {
        expected_chain_id: CHAIN,
        ..Default::default()
    };
    let declared_gas_margin = match build_policy.gas {
        GasPolicy::SimulationGasPlus { margin } => margin,
        GasPolicy::Configured { gas_limit } => {
            return Err(format!(
                "the build policy would resolve the limit to the configured {gas_limit} instead \
                 of to a simulation measurement, and §13 refuses that for an arbitrage intent"
            ))
        }
    };
    let mut stage = {
        let setup = ExecutionSetup {
            mode: ExecutionMode::Submit,
            fee: FeePolicy::BaseFeeHeadroom { headroom_blocks: 2 },
            build: build_policy,
            receipts: ReceiptPolicy {
                attempts: RECEIPT_ATTEMPTS,
                between_attempts: RECEIPT_POLL,
            },
        };
        ExecutionStage::new(node.abilities(), signer()?, setup, CHAIN, Clock::new())
            .map_err(|e| format!("the lifecycle stage refused to open: {e}"))?
    };

    let send_head = node.head().await?;
    let freshness: Freshness = executable.freshness_at(BlockNumber(send_head.number));
    let mut metrics = Metrics::default();
    let report = stage
        .on_arbitrage_plan(&executable, &binding, freshness.clone(), &mut metrics)
        .await;

    // §31's reconciliation: read the wallet at the block the receipt names, not at latest.
    // The block comes from the record the stage closed, which is the same object the ledger
    // holds — so the number read here and the number in the evidence cannot drift apart.
    let record = report.record.clone().or_else(|| {
        report
            .execution_id
            .as_ref()
            .and_then(|id| stage.ledger().get(id).cloned())
    });
    let mined_block = record.as_ref().and_then(|r| r.execution_block);
    let after = match mined_block {
        Some(block) => node.wallet(block, operator, executor).await?,
        None => json!({
            "reason": "the execute attempt produced no bound receipt, so there is no block to \
                       reconcile at; this is an unread outcome, not a zero"
        }),
    };

    // §32's output leg needs a window this transaction owns on its own. The pair above spans
    // the pinned block to the receipt block, which also covers every block the transaction
    // waited to be mined; this read brackets exactly the execute block, so its delta is the
    // one a route can be credited with. `None` when there is no receipt block, and the row
    // then stays `null` rather than borrowing the wider window's number.
    let pre_execute = match mined_block {
        Some(block) if block > 0 => Some(node.wallet(block - 1, operator, executor).await?),
        _ => None,
    };

    // §31's row, spelled with §31's field names. Every one of them is a read: the receipt
    // fields come from the record the lifecycle closed, the balances come from the two wallet
    // rows above, and anything the chain did not answer stays `null` rather than becoming a
    // zero (§52's rule as applied to a field list).
    let weth_before = decimal(&before["weth_operator_wei"]);
    let weth_after = decimal(&after["weth_operator_wei"]);
    let weth_at_block_start = pre_execute
        .as_ref()
        .and_then(|w| decimal(&w["weth_operator_wei"]));
    let token0_before = decimal(&before["token0_operator_wei"]);
    let token0_after = decimal(&after["token0_operator_wei"]);
    let weth_delta = delta(weth_before, weth_after);
    let token0_delta = delta(token0_before, token0_after);
    let row_31 = json!({
        "chain_id": CHAIN,
        "executor_address": format!("{executor:#x}"),
        "operator_address": format!("{operator:#x}"),
        "tx_hash": record.as_ref().and_then(|r| r.transaction_hash).map(|h| format!("{h:#x}")),
        "block_number": record.as_ref().and_then(|r| r.execution_block),
        "receipt_status": record.as_ref().map(|r| r.status.name()),
        "gas_used": record.as_ref().and_then(|r| r.gas_used),
        "effective_gas_price": record.as_ref().and_then(|r| r.effective_gas_price).map(|p| p.to_string()),
        "input_asset": format!("{WETH:#x}"),
        "input_amount": INPUT_WEI.to_string(),
        "output_asset": format!("{WETH:#x}"),
        "output_amount": weth_delta.clone(),
        "before_balance": weth_before.map(|b| b.to_string()),
        "after_balance": weth_after.map(|a| a.to_string()),
        "balance_delta": weth_delta.clone(),
        "token0_balance_delta": token0_delta.clone(),
        "l1_fee": record.as_ref().and_then(|r| r.l1_fee).map(|f| f.to_string()),
        "l2_fee": record.as_ref().and_then(|r| r.l2_fee).map(|f| f.to_string()),
        "total_fee": record.as_ref().and_then(|r| r.total_fee).map(|f| f.to_string()),
        "nonce": record.as_ref().map(|r| r.nonce),
        "realized_profit": record.as_ref().and_then(|r| r.realized_profit).map(|p| p.to_string()),
        "realized_profit_status": record.as_ref().and_then(|r| r.profit_status).map(|s| format!("{s:?}")),
        "note": "§31: realized_profit is the record's own field, derived from the receipt and \
                 the balance reads, and is null when the two halves cannot be added in one \
                 denomination (§12/§14: the route settles in WETH and the bill is paid in the \
                 chain's native asset). The WETH balance delta and the fee columns are printed \
                 beside it so a reader can do the arithmetic and see why it is not summed here. \
                 output_amount is that delta — the operator's net WETH movement across the \
                 window from the pinned block to the receipt block — so it is not the gross the \
                 route paid out; §32's output leg prints the gross, the principal leg, and the \
                 one-block window beside it.",
    });

    // The limit on the bytes that were actually signed. The record's `gas_limit` is the
    // measurement the intent carried — the record is opened before the build resolves §13's
    // policy — so the on-wire number has to come from the signed-transaction evidence, or the
    // run does not claim to know it.
    let signed_limit = report.signed.as_ref().map(|e| e.gas_limit);
    if report.sent && signed_limit.is_none() {
        return Err(
            "bytes went to a node but the attempt carries no signed-transaction evidence: this \
             run cannot name the gas limit it sent, and §52 would rather stop than describe a \
             transaction it cannot identify"
                .to_string(),
        );
    }
    if let Some(limit) = signed_limit {
        // §13/EIP-150, checked against the real bytes: the send limit is this run's proved
        // simulation limit plus the declared margin, and therefore above the gas it burned. A
        // limit built from the burn is what starved the deepest nested call in §57's first two
        // attempts — same calldata, same state, `UniswapV2: TRANSFER_FAILED`.
        if limit <= open_floor.outcome.gas_used {
            return Err(format!(
                "the transaction was sent at a limit of {limit} against a simulation burn of {}: a \
                 limit at or below the burn can leave a nested frame short of gas under EIP-150, \
                 which is the failure this ladder is here to not repeat",
                open_floor.outcome.gas_used
            ));
        }
        let expected = open_floor.outcome.gas_limit + declared_gas_margin;
        if limit != expected {
            return Err(format!(
                "the on-wire limit {limit} is not the proved simulation limit {} plus this run's \
                 declared margin {declared_gas_margin}; §13's policy resolves out of exactly those \
                 two numbers, so a third value means the build did not use this run's measurement",
                open_floor.outcome.gas_limit
            ));
        }
    }

    // §32's three-way comparison: what REVM answered at the pinned block, what the receipt
    // charged, and what the balances actually moved. Where the two disagree this row says
    // which of §32's five causes it is, and where a number is absent it says null.
    //
    // The subtraction has to be done in one denomination first. REVM's `delivered` is the
    // gross WETH the route pays out; the operator's balance delta is net of the principal the
    // route pulled in at the start of the same transaction. Those two numbers differ by that
    // principal by design, so the principal goes into the row as its own leg and the
    // comparison is gross against gross, inside the one block this transaction owns.
    let principal_in = Some(U256::from(INPUT_WEI));
    let in_block_delta = delta(weth_at_block_start, weth_after);
    let wait_window_delta = delta(weth_before, weth_at_block_start);
    let gross_out_in_block = sum(gain(weth_at_block_start, weth_after), principal_in);
    let gross_difference = delta(gross_out_in_block, open_floor.outcome.delivered);

    // A successful receipt whose gross does not line up is a reconciliation this run cannot
    // attribute: either the principal leg did not move as the plan says, or something outside
    // this transaction touched the operator's WETH inside the execute block. §32 asks for an
    // explanation of a difference, not a number edited into agreement — so the run stops and
    // names the reads instead of publishing a row whose halves do not belong to each other.
    if record.as_ref().map(|r| r.status.name()) == Some("included") {
        match gross_difference.as_deref() {
            None => {
                return Err(
                    "the receipt says the route was included, but one of the three reads the \
                     output leg needs is absent (the balance at the block before the execute \
                     block, the balance at the receipt block, or REVM's delivered amount), so \
                     §32's reconciliation cannot be formed"
                        .to_string(),
                )
            }
            Some("0") => {}
            Some(other) => {
                return Err(format!(
                    "the route paid out {} gross WETH inside the execute block against REVM's \
                     delivered amount of {other} short of it, with receipt.status = 1: the \
                     delta of a wallet this ladder holds alone should be exactly the principal \
                     plus what the simulation returned, so a difference here is a read to \
                     chase, not a number to reconcile by hand (§32)",
                    gross_out_in_block.unwrap_or(U256::ZERO)
                ))
            }
        }
    }
    let row_32 = json!({
        "route": {
            "simulation_calldata_hash": format!("{:#x}", keccak256(open_floor.outcome.calldata.as_ref())),
            "executed_calldata_hash": format!("{:#x}", executable.calldata_hash()),
            "identical_by_construction": executable.calldata_hash()
                == keccak256(open_floor.outcome.calldata.as_ref()),
            "explanation": "the plan's calldata is the bytes REVM ran (§23/§37), so a difference \
                            here would mean the lifecycle rebuilt the route rather than sent it",
        },
        "input": {
            "simulation_amount_in": open_floor.outcome.amount_in.to_string(),
            "chain_balance_input": INPUT_WEI.to_string(),
            "chain_balance_input_means": "the principal this plan instructed the contract to \
                                          pull, which is the same constant the simulation was \
                                          given — the read that credits it to the chain is the \
                                          output leg's arithmetic: the operator's balance moved \
                                          by the gross less this figure",
        },
        "output": {
            "simulation_delivered_gross": open_floor.outcome.delivered.map(|d| d.to_string()),
            "principal_transferred_in": INPUT_WEI.to_string(),
            "actual_weth_delta_pinned_to_receipt": weth_delta.clone(),
            "actual_weth_delta_in_the_execute_block": in_block_delta.clone(),
            "external_movement_in_the_wait_window": wait_window_delta.clone(),
            "actual_gross_out_in_the_execute_block": gross_out_in_block.map(|g| g.to_string()),
            "difference": gross_difference.clone(),
            "difference_means": "REVM's gross delivered amount minus the gross the operator's \
                                 WETH balance actually gained inside the execute block (that \
                                 block's net delta plus this plan's principal). Both halves are \
                                 gross WETH out of the route, which is why the net delta is not \
                                 an operand here and why the pinned-to-receipt delta is printed \
                                 as its own row: it is net of the principal and covers the \
                                 blocks the transaction waited in. `external_movement_in_the_\
                                 wait_window` is the check that the two windows agree.",
            "cause_if_different": "state drift: the plan was priced at one canonical block and \
                                  mined at a later one, and the contract's asks are exact, so a \
                                  moved market reverts rather than fills worse — a nonzero \
                                  difference here with status 1 is a read to explain, not a \
                                  number to edit (§32)",
        },
        "gas": {
            "simulation_gas_used": open_floor.outcome.gas_used,
            "simulation_proved_gas_limit": open_floor.outcome.gas_limit,
            "declared_margin": declared_gas_margin,
            "signed_transaction_gas_limit": signed_limit,
            "record_gas_limit": record.as_ref().map(|r| r.gas_limit),
            "receipt_gas_used": record.as_ref().and_then(|r| r.gas_used),
            "which_number_was_sent": "the limit the transaction was sent at is \
                                      `signed_transaction_gas_limit`, the field taken off the \
                                      bytes that were signed; `record_gas_limit` is §39's record \
                                      field, which carries the measurement the intent handed the \
                                      builder, and the two differ by exactly the declared margin",
        },
        "status": {
            "simulation": open_floor.outcome.describe(),
            "receipt": record.as_ref().map(|r| r.status.name()),
        },
        "profit": {
            "simulation_gross_delivered": open_floor.outcome.delivered.map(|d| d.to_string()),
            "actual_gross_out_in_the_execute_block": gross_out_in_block.map(|g| g.to_string()),
            "actual_weth_gain_net_of_principal": in_block_delta.clone(),
            "fee_bill_native_wei": record.as_ref().and_then(|r| r.total_fee).map(|f| f.to_string()),
            "verdict": "NOT_PROVEN",
            "why": "the route's gain and the bill are not one asset: the gain is WETH, the fee \
                    is the chain's native asset (§12/§14), and this run prices one in the other \
                    by no oracle — so the WETH gain is reported as measured and the profit \
                    verdict stays unproven rather than becoming a sum of two denominations. \
                    §59 keeps this verdict apart from the two above it: a real transaction, \
                    included, with a measured balance movement, is not by itself a proven \
                    profitable arbitrage."
        },
    });

    let execute_row = json!({
        "plan_hash": format!("{:#x}", executable.plan_hash()),
        "calldata_hash": format!("{:#x}", executable.calldata_hash()),
        "calldata_len": executable.calldata().len(),
        "route_id": executable.route_id(),
        "freshness": format!("{freshness:?}"),
        "gas_policy": format!(
            "§13: the limit this run's simulation completed at ({}) plus the declared margin \
             ({declared_gas_margin})",
            open_floor.outcome.gas_limit
        ),
        "signed_transaction_gas_limit": signed_limit,
        "report": report.to_json(),
    });

    write_evidence(
        "data/evidence/m10/real/giwa_ladder_steps.json",
        &json!({
            "endpoint_source": "GIWA_RPC_URL at run time",
            "chain_id": CHAIN,
            "operator": format!("{operator:#x}"),
            "executor": format!("{executor:#x}"),
            "principal_wei": INPUT_WEI.to_string(),
            "steps": steps,
        }),
    );
    write_evidence(
        "data/evidence/m10/contract/bytecode_hash.json",
        &json!({
            "creation_code_bytes": creation_code.len(),
            "creation_code_keccak256": format!("{:#x}", keccak256(&creation_code)),
            "deployment_address": format!("{executor:#x}"),
            "abi_version": abi_version(),
            "chain_id": CHAIN,
            "operator": format!("{operator:#x}"),
            "deployment_tx": format!("{:#x}", deployment.step.transaction_hash),
            "deployment_block": deployment.step.block().map(|(n, _)| n),
        }),
    );

    write_evidence(
        "data/evidence/m10/real/giwa_execution.json",
        &json!({
            "what_this_is": "§57's controlled ladder on the live GIWA testnet: a real execute \
                             transaction sent through the existing execution lifecycle",
            "market": "REAL_MARKET",
            "chain_id": CHAIN,
            "endpoint_source": "GIWA_RPC_URL at run time",
            "operator": format!("{operator:#x}"),
            "executor": format!("{executor:#x}"),
            "principal_wei": INPUT_WEI.to_string(),
            "route": {
                "buy_pool": format!("{buy:#x}"),
                "sell_pool": format!("{sell:#x}"),
                "ask_leg1": ask1.to_string(),
                "ask_leg2": ask2.to_string(),
            },
            "reserves_at_pinned_block": [reserve_a, reserve_b],
            "freshness_window": {
                "declared_max_block_age": MAX_BLOCK_AGE,
                "priced_at_block": head.number,
                "priced_at_hash": format!("{:#x}", head.hash),
                "send_head_block": send_head.number,
                "blocks_between_priced_and_send": send_head.number.saturating_sub(head.number),
                "sim_read_concurrency": SIM_READ_CONCURRENCY,
                "note": "§8 as the ladder declares and measures it: the bound is the plan's own \
                         field, the gap is the two block numbers this run read off the node, and \
                         the freshness verdict below is the lifecycle's, not this file's.",
            },
            "preflight_reads": preflight,
            "wallet_before": before.clone(),
            "wallet_before_note": wallet_note,
            "wallet_at_block_before_execute": pre_execute.unwrap_or(Value::Null),
            "wallet_after": after,
            "simulation": {
                "profit_floor": sim_row("profit_floor", &profit_floor),
                "open_floor": sim_row("open_floor", &open_floor),
                "floor_too_high": sim_row("floor_too_high", &too_high),
            },
            "execution": execute_row,
            "evidence_row_31": row_31,
            "reconciliation_32": row_32,
            "l1_fee_wei": record.as_ref().and_then(|r| r.l1_fee).map(|f| f.to_string()),
            "verdicts": {
                "executor_deployed_and_configured": true,
                "execute_transaction_sent": report.sent,
                "execute_receipt_bound_to_a_block": record.as_ref().and_then(|r| r.execution_block).is_some(),
                "real_profitable_arbitrage": "NOT_PROVEN",
                "note": "§52: receipt.status = 1 alone proves nothing about profit; §59 keeps \
                         the two verdicts apart. The measured deltas are above, and the \
                         unmeasured ones are null rather than zero."
            },
        }),
    );

    // §58: the same route with a floor the market cannot pay, sent for real.
    let failure = if too_high.outcome.succeeded() {
        return Err(format!(
            "the §58 control was expected to revert in REVM and did not: {}",
            too_high.outcome.describe()
        ));
    } else {
        // The snapshot §58's "no partial state" is judged against has to be the wallet as it
        // stands immediately before this transaction — the execute rung above already moved
        // balances, so the pre-execute row would make a clean revert look like residue.
        let failure_head = node.head().await?;
        let pre_failure = node.wallet(failure_head.number, operator, executor).await?;
        let step = deployer
            .call(
                failure_head,
                executor,
                too_high.call.encode(),
                "execute-floor-too-high".to_string(),
            )
            .await
            .map_err(|e| format!("§58's controlled failure: {e}"))?;
        // Read the residue at the block the receipt names, so "nothing changed" compares the
        // state directly after this transaction against the state directly before it.
        let residue_head = step.block().map(|(number, _)| number);
        let residue = match residue_head {
            Some(block) => node.wallet(block, operator, executor).await?,
            None => json!({
                "reason": "the failing transaction carries no block, so there is no state to \
                           read the residue at; §58's no-partial-state check is unread, not \
                           passed"
            }),
        };
        let unchanged = match residue_head {
            Some(_) => json!(
                residue["weth_operator_wei"] == pre_failure["weth_operator_wei"]
                    && residue["token0_operator_wei"] == pre_failure["token0_operator_wei"]
                    && residue["weth_executor_wei"] == pre_failure["weth_executor_wei"]
                    && residue["token0_executor_wei"] == pre_failure["token0_executor_wei"]
                    && residue["weth_allowance_to_executor_wei"]
                        == pre_failure["weth_allowance_to_executor_wei"]
            ),
            None => Value::Null,
        };
        write_evidence(
            "data/evidence/m10/real/giwa_failure.json",
            &json!({
                "what_this_is": "§58's real controlled failure: min_final_amount deliberately \
                                 above anything this market can pay",
                "chain_id": CHAIN,
                "executor": format!("{executor:#x}"),
                "operator": format!("{operator:#x}"),
                "min_final_amount_wei": too_high.outcome.min_final_amount.to_string(),
                "principal_wei": INPUT_WEI.to_string(),
                "expected": "tx included, receipt.status = 0, no partial state",
                "simulation_attribution": sim_row("floor_too_high", &too_high),
                "step": step.to_json(),
                "receipt_status": step.status.name(),
                "reverted": step.reverted(),
                "residue_read_at_block": residue_head,
                "no_partial_state": unchanged,
                "wallet_before_failure": pre_failure,
                "wallet_after_failure": residue,
                "unknown_fields": "If the node could not include this transaction the row is \
                                   an error, not a pass: §58 says UNKNOWN rather than a \
                                   fabricated revert, and this run stops with a reason. \
                                   no_partial_state is null when the failing receipt names no \
                                   block, because then nothing was read."
            }),
        );
        json!({ "reverted": step.reverted(), "no_partial_state": unchanged })
    };

    Ok(json!({
        "executor": format!("{executor:#x}"),
        "execute_status": report.reached.map(|status| status.name().to_string()),
        "execute_sent": report.sent,
        "failure": failure,
    }))
}
