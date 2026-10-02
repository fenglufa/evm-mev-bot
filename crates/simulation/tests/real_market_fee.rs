//! M7 §5 and §17, answered by execution instead of by assumption: the two
//! same-pair WETH round trips the P0 census reports are run against the node's own
//! state at one pinned live block, and what each pool charges is found by asking it
//! for a number and recording whether it says yes.
//!
//! ```text
//! GIWA_RPC_URL=https://sepolia-rpc.giwa.io \
//!   cargo test -p evm-simulation --test real_market_fee -- --ignored --nocapture
//! ```
//!
//! Why a search and not a reading. A V2 pair's `swap(amount0Out, amount1Out, …)`
//! pays out *exactly the amount the caller names* — it is a demand, not a quote, and
//! the pair only checks that the demand leaves its own invariant intact. So the fee
//! a pool charges is not a number a caller can observe in one trade: it is the
//! boundary between the demands the pool honours and the demands it refuses. This
//! file finds that boundary by bisection over an effective-fee parameter, to one
//! part per million of the input, and reports the switch as the answer.
//!
//! The parameterisation is deliberately form-free. [`payout`] below is M3's
//! `swap_exact_in` shape, but nothing here claims the pool computes it that way: the
//! probe asks for `payout(fee = p)` and records the reply, so the reported bracket
//! is a statement about the deployed bytecode's behaviour at this input — which is
//! also exactly the statement a submission needs (§5: the fee proven, §17: the
//! transfer behaviour measured rather than carried over from another pool).
//!
//! Three things this run does not do, and says so in the evidence file:
//!
//! - it does not broadcast. The only sender is §58's deterministic test account, and
//!   there is no signing key anywhere in this target;
//! - it does not price the L1 data fee. `l1Fee` is a sequencer charge, not an EVM
//!   one, so the profitability stated here is after L2 gas only;
//! - it does not decide whether a pool that has not traded in a year is a market.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use alloy_primitives::{address, Address, U256};

use evm_chain::{BlockContext, CallRequest, ChainAdapter, HttpChainAdapter};
use evm_core::{BlockNumber, ChainId, Fee, PoolId, TokenId};
use evm_protocol::{signatures::word, CallReturn, V2Call, V2Topics};
use evm_simulation::{
    engine::run, Binding, BlockPin, DumpStateProvider, EvmRules, ExecutionStatus, Funding,
    GasPricing, PricedRoute, RouteLeg, RpcStateProvider, Settle, SimulationRequest,
    SimulationResult, StateProvider, StateSpec, TransactionSpec,
};

const WETH: Address = address!("0x4200000000000000000000000000000000000006");

/// The bisection ceiling, in parts per million of the input treated as fee. Two
/// percent is far above any V2-shaped pool, so a pool that refuses even this demand
/// is refusing for a reason that is not its fee — and the refusal is reported.
const FEE_CEILING_PPM: u64 = 20_000;
const PPM: u64 = 1_000_000;

/// One candidate as the census left it: the mid token, the two pools that hold it
/// against WETH, and the input amount the census' grid reported its gross at.
///
/// The direction is not hardcoded. Which pool is bought through and which is sold
/// into is decided from `token0()` and `getReserves()` read at the pinned block, and
/// the opposite orientation is run as a control, so a wrong side reading shows up as
/// a loss instead of as a profit.
struct Candidate {
    mid: Address,
    pools: [Address; 2],
    input_wei: u128,
}

const CANDIDATES: &[Candidate] = &[
    Candidate {
        mid: address!("0x07d4af6e2bc8dd82beb06b4fd279df4c9028f26f"),
        pools: [
            address!("0x2a3ceafba30f6626170cbb0cd67392efb94bd9a4"),
            address!("0x5b3c1e3fb6a97c0130ae015ff10f53a1a30c353e"),
        ],
        input_wei: 1_500_000_000_000_000,
    },
    Candidate {
        mid: address!("0x7b9dc86c3495d377526247ade5ea7b864a5cec25"),
        pools: [
            address!("0xda1687064f6f0367678cb168ba10c980959d0272"),
            address!("0x7c580b975fb97c43018bc05a41f0c6a7d10350ad"),
        ],
        input_wei: 80_000_000_000_000,
    },
];

fn rpc_url() -> String {
    std::env::var("GIWA_RPC_URL").expect(
        "GIWA_RPC_URL is required: this target reads a live node, and no endpoint is \
         hardcoded (§44 of the M5 task, still in force)",
    )
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// One pool, exactly as it answered at the pinned block.
#[derive(Clone, Copy, Debug)]
struct Pool {
    address: Address,
    token0: Address,
    token1: Address,
    reserve0: U256,
    reserve1: U256,
    /// `blockTimestampLast` — the block time the pool itself says these reserves are
    /// from. Carried into the evidence because §26's staleness question is answerable
    /// from it and from nothing else in a pool that has never emitted a Swap.
    last_sync: U256,
}

impl Pool {
    fn holds(&self, token: Address) -> bool {
        self.token0 == token || self.token1 == token
    }

    fn reserve_of(&self, token: Address) -> Option<U256> {
        if self.token0 == token {
            Some(self.reserve0)
        } else if self.token1 == token {
            Some(self.reserve1)
        } else {
            None
        }
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "address": self.address.to_string(),
            "token0": self.token0.to_string(),
            "token1": self.token1.to_string(),
            "reserve0": self.reserve0.to_string(),
            "reserve1": self.reserve1.to_string(),
            "getReserves_blockTimestampLast": self.last_sync.to_string(),
        })
    }
}

async fn read_pool(adapter: &HttpChainAdapter, at: BlockNumber, address: Address) -> Pool {
    let (token0, token1) = match (
        call(adapter, at, address, &V2Call::Token0).await,
        call(adapter, at, address, &V2Call::Token1).await,
    ) {
        (CallReturn::Address(a), CallReturn::Address(b)) => (a, b),
        other => panic!("token0()/token1() at {address} answered {other:?}, not addresses"),
    };
    let reserves = match call(adapter, at, address, &V2Call::GetReserves).await {
        CallReturn::Reserves(r) => r,
        other => panic!("getReserves() at {address} answered {other:?}"),
    };
    Pool {
        address,
        token0,
        token1,
        reserve0: reserves.reserve0,
        reserve1: reserves.reserve1,
        last_sync: reserves.block_timestamp_last,
    }
}

async fn call(
    adapter: &HttpChainAdapter,
    at: BlockNumber,
    to: Address,
    request: &V2Call,
) -> CallReturn {
    let raw = adapter
        .call(
            at,
            &CallRequest {
                to,
                data: request.encode(),
            },
        )
        .await
        .unwrap_or_else(|error| panic!("{} at {to} block {}: {error}", request.signature(), at.0));
    request
        .decode_return(&raw)
        .unwrap_or_else(|error| panic!("decoding {} at {to}: {error}", request.signature()))
}

/// What a pool holding these reserves is asked to pay for `amount_in` when only
/// `amount_in * retained / scale` of the input counts as traded.
///
/// Every intermediate is `U256` and every operation is checked: a silent saturation
/// here would move the boundary the probe is hunting for, which is the one thing this
/// file is measuring.
fn payout(reserve_in: U256, reserve_out: U256, amount_in: U256, retained: u64, scale: u64) -> U256 {
    let trimmed = amount_in
        .checked_mul(U256::from(retained))
        .expect("input x retained fits 256 bits")
        / U256::from(scale);
    if trimmed.is_zero() {
        return U256::ZERO;
    }
    reserve_out
        .checked_mul(trimmed)
        .expect("reserve x trimmed input fits 256 bits")
        / reserve_in
            .checked_add(trimmed)
            .expect("reserve plus trimmed input fits 256 bits")
}

/// The two legs in the order the plan executes them, with the reserves each is
/// traded against and the size of the trade.
#[derive(Clone, Copy, Debug)]
struct Orientation {
    chain: ChainId,
    /// The pool bought through: it takes WETH and pays the mid token.
    buy: Pool,
    /// The pool sold into: it takes the mid token and pays WETH.
    sell: Pool,
    mid: Address,
    input: U256,
}

impl Orientation {
    fn reversed(&self) -> Self {
        Self {
            chain: self.chain,
            buy: self.sell,
            sell: self.buy,
            mid: self.mid,
            input: self.input,
        }
    }

    /// The demand put on the buying pool at an assumed effective fee.
    fn mid_demand_at(&self, fee_ppm: u64) -> U256 {
        payout(
            self.buy.reserve_of(WETH).expect("checked"),
            self.buy.reserve_of(self.mid).expect("checked"),
            self.input,
            PPM - fee_ppm,
            PPM,
        )
    }

    /// The demand put on the selling pool for exactly `mid_in` of the mid token. The
    /// token's own transfer behaviour is not inside this number: it is measured, and
    /// reported as the gap between what the sender was handed and what the pool took.
    fn out_demand_at(&self, mid_in: U256, fee_ppm: u64) -> U256 {
        payout(
            self.sell.reserve_of(self.mid).expect("checked"),
            self.sell.reserve_of(WETH).expect("checked"),
            mid_in,
            PPM - fee_ppm,
            PPM,
        )
    }

    fn route(&self, block: BlockNumber, mid_demand: U256, out_demand: U256) -> PricedRoute {
        // The M3 carry-over, stated in the route and contradicted or confirmed by the
        // run. It is a field of the request, not a fact about these pools: no pool
        // here has a Swap in its history to read a fee off.
        let fee = Fee {
            numerator: 997,
            denominator: 1000,
        };
        let leg = |pool: &Pool, token_in: Address, token_out: Address| RouteLeg {
            pool: PoolId::new(self.chain, pool.address),
            token_in: TokenId::new(self.chain, token_in),
            token_out: TokenId::new(self.chain, token_out),
            reserve_in: pool.reserve_of(token_in).expect("checked"),
            reserve_out: pool.reserve_of(token_out).expect("checked"),
            fee,
        };
        let gross = out_demand
            .checked_sub(self.input)
            .unwrap_or_else(|| self.input.checked_sub(out_demand).expect("one is larger"));
        let height = block.0;
        PricedRoute::new(
            self.chain,
            block,
            leg(&self.buy, WETH, self.mid),
            leg(&self.sell, self.mid, WETH),
            self.input,
            mid_demand,
            out_demand,
            gross,
            format!(
                "reserve math at pinned block {height}, from getReserves()/token0() read live \
                 off the node; {mid_demand} and {out_demand} are the demands being put to the \
                 pools, not predictions of what they will pay, and the 997/1000 fee on these \
                 legs is the unproven carry-over this run exists to measure"
            ),
        )
        .expect("a route built from a pool's own reserves and side ordering")
    }
}

/// One pool's own account of a trade, from the logs it emitted while the run ran.
#[derive(Clone, Copy, Debug, Default)]
struct Observed {
    received: U256,
    paid: U256,
    sync_log: bool,
    swap_log: bool,
    /// Whether the Swap event's amounts are the same numbers the reserve diff gives.
    logs_agree: bool,
    /// How many logs this pool emitted during the run; more than one Sync means the
    /// reading below took the last of them, which is worth saying out loud.
    sync_logs: u32,
}

/// Read the pool's numbers out of its own logs.
///
/// A Sync log carries the reserves *after* the trade, so its difference from the
/// reserves read before the run is what the pool received and paid — a reading that
/// needs nothing beyond two words and no claim about an event's field order. The
/// Swap event is decoded alongside it and the two are compared, so an implementation
/// whose event disagrees with its balances is caught here rather than believed.
fn observe(
    result: &SimulationResult,
    pool: &Pool,
    token_in: Address,
    token_out: Address,
) -> Observed {
    let topics = V2Topics::default();
    let mut observed = Observed::default();
    for (_, log) in result.logs() {
        if log.address != pool.address {
            continue;
        }
        if log.topics.first() == Some(&topics.sync) {
            let (Ok(reserve0), Ok(reserve1)) = (word(&log.data, 0), word(&log.data, 1)) else {
                continue;
            };
            let after = Pool {
                reserve0,
                reserve1,
                ..*pool
            };
            let (Some(before_in), Some(after_in)) =
                (pool.reserve_of(token_in), after.reserve_of(token_in))
            else {
                continue;
            };
            let (Some(before_out), Some(after_out)) =
                (pool.reserve_of(token_out), after.reserve_of(token_out))
            else {
                continue;
            };
            observed.received = after_in.saturating_sub(before_in);
            observed.paid = before_out.saturating_sub(after_out);
            observed.sync_log = true;
            observed.sync_logs += 1;
        }
        if log.topics.first() == Some(&topics.swap) && log.data.len() >= 128 {
            let amounts = [0usize, 1, 2, 3].map(|i| word(&log.data, i).ok());
            let [Some(in0), Some(in1), Some(out0), Some(out1)] = amounts else {
                continue;
            };
            let (swapped_in, swapped_out) = if pool.token0 == token_in {
                (in0, out1)
            } else {
                (in1, out0)
            };
            observed.swap_log = true;
            observed.logs_agree = swapped_in == observed.received && swapped_out == observed.paid;
        }
    }
    observed
}

/// How a run ended, in the two words the search needs: did the whole sequence
/// complete, and what did the sender end up holding.
fn verdict(result: &SimulationResult) -> bool {
    result.status.completed()
}

fn stopped_at(result: &SimulationResult) -> String {
    match &result.status {
        ExecutionStatus::Completed => "completed".to_string(),
        ExecutionStatus::Reverted { step, call, revert } => {
            format!("reverted at step {step} ({call}): {}", revert.reason())
        }
        ExecutionStatus::OutOfGas { step, call } => format!("out of gas at step {step} ({call})"),
        ExecutionStatus::Halted { step, call, reason } => {
            format!("halted at step {step} ({call}): {reason}")
        }
    }
}

/// One candidate, one pinned block, one provider shared by every probe — so the node
/// is asked for each piece of state once, and the dump that comes out the other end
/// is the union of everything the whole search needed.
struct Probe {
    provider: Arc<RpcStateProvider>,
    header: BlockContext,
    source: String,
    orientation: Orientation,
    probes: u32,
}

impl Probe {
    fn new(
        adapter: Arc<HttpChainAdapter>,
        pin: BlockPin,
        header: BlockContext,
        orientation: Orientation,
    ) -> Self {
        let chain: Arc<dyn ChainAdapter> = adapter;
        let provider = Arc::new(RpcStateProvider::new(chain, pin));
        let source = provider.source();
        Self {
            provider,
            header,
            source,
            orientation,
            probes: 0,
        }
    }

    fn shared(&self) -> Arc<dyn StateProvider> {
        let provider: Arc<RpcStateProvider> = Arc::clone(&self.provider);
        provider
    }

    /// Run the canonical shape — `deposit()`, WETH to the buying pool, its swap, the
    /// mid token to the selling pool, its swap, `withdraw()` — against the node's
    /// state at the pin, demanding exactly these two amounts.
    async fn run(&mut self, mid_demand: U256, out_demand: U256) -> SimulationResult {
        self.probes += 1;
        let route = self
            .orientation
            .route(self.header.number, mid_demand, out_demand);
        run(
            self.shared(),
            &request_for(&self.header, route, out_demand, self.source.clone()),
        )
        .await
        .expect("the run produced a result or a refusal, never a crash")
    }

    /// The smallest assumed fee whose demand the pools honour.
    ///
    /// Honours is monotone in the assumed fee — a bigger assumption means a smaller
    /// demand, and no pool refuses a smaller demand for a reason a bigger one gave —
    /// so a bisection finds the switch. `Err` carries the run that refused even the
    /// ceiling demand, which is not a fee measurement but a failure to trade at all,
    /// and its reason is what the report needs.
    async fn search(
        &mut self,
        demanding: &dyn Fn(u64) -> (U256, U256),
    ) -> Result<(u64, SimulationResult), SimulationResult> {
        let free = self.run(demanding(0).0, demanding(0).1).await;
        if verdict(&free) {
            return Ok((0, free));
        }
        let ceiling = self
            .run(demanding(FEE_CEILING_PPM).0, demanding(FEE_CEILING_PPM).1)
            .await;
        if !verdict(&ceiling) {
            return Err(ceiling);
        }
        let (mut lo, mut hi) = (0u64, FEE_CEILING_PPM);
        while hi - lo > 1 {
            let middle = lo + (hi - lo) / 2;
            let result = self.run(demanding(middle).0, demanding(middle).1).await;
            if verdict(&result) {
                hi = middle;
            } else {
                lo = middle;
            }
            println!(
                "    probe {middle:>6} ppm -> {} ({} probes so far)",
                verdict(&result),
                self.probes
            );
        }
        let settled = self.run(demanding(hi).0, demanding(hi).1).await;
        Ok((hi, settled))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "reads a live archive node; writes the evidence file and the dumps it replays from"]
async fn measure_the_fees_the_pools_actually_charge() {
    let adapter = Arc::new(
        HttpChainAdapter::connect(&rpc_url())
            .await
            .expect("the rpc connects"),
    );
    let head = adapter
        .latest_block()
        .await
        .expect("eth_blockNumber answers");
    let header = adapter
        .get_block_context(head)
        .await
        .expect("the node answers for the head it just reported");
    // One read of "now", turned into a pin. Everything below names a block by number
    // and hash; nothing asks the node again what the latest block is.
    let pin = BlockPin::new(head, header.hash);
    let chain = adapter.chain_id();
    println!(
        "pinned {} {:?} base fee {:?} timestamp {}",
        head.0, header.hash, header.base_fee_per_gas, header.timestamp
    );

    let mut evidence = Vec::new();
    for candidate in CANDIDATES {
        let mut pools = Vec::new();
        for pool in candidate.pools {
            pools.push(read_pool(&adapter, head, pool).await);
        }
        for pool in &pools {
            assert!(
                pool.holds(candidate.mid) && pool.holds(WETH),
                "pool {} holds {} and {}, so the census' pairing of it is wrong",
                pool.address,
                pool.token0,
                pool.token1
            );
        }

        let orientation = decide_orientation(
            chain,
            &pools,
            candidate.mid,
            U256::from(candidate.input_wei),
        );
        let control = orientation.reversed();
        println!(
            "\n=== candidate {} ===\n    buy  {} (WETH {} / mid {})\n    sell {} (WETH {} / mid {})\n    input {}",
            candidate.mid,
            orientation.buy.address,
            orientation.buy.reserve_of(WETH).expect("checked"),
            orientation.buy.reserve_of(orientation.mid).expect("checked"),
            orientation.sell.address,
            orientation.sell.reserve_of(WETH).expect("checked"),
            orientation.sell.reserve_of(orientation.mid).expect("checked"),
            orientation.input,
        );

        let mut probe = Probe::new(Arc::clone(&adapter), pin, header.clone(), orientation);

        // Leg one: what does the buying pool pay for this WETH, exactly?
        let buy_result = {
            let orientation = probe.orientation;
            probe
                .search(&move |fee| (orientation.mid_demand_at(fee), U256::ONE))
                .await
        };
        let (buy_ppm, buy_run) = match buy_result {
            Ok(found) => found,
            Err(refusal) => {
                let reason = stopped_at(&refusal);
                println!(
                    "    buying pool refused every demand up to {FEE_CEILING_PPM} ppm: {reason}"
                );
                evidence.push(serde_json::json!({
                    "mid": candidate.mid.to_string(),
                    "input_wei": orientation.input.to_string(),
                    "buying_pool": orientation.buy.json(),
                    "selling_pool": orientation.sell.json(),
                    "executes": false,
                    "first_leg_refusal": reason,
                    "probes": probe.probes,
                }));
                write_evidence(head, &header, &evidence);
                continue;
            }
        };
        let mid_demanded = orientation.mid_demand_at(buy_ppm);
        let mid_handed = buy_run
            .measurement(Binding::SenderMidReceived)
            .expect("a completed run measures what the first pool handed over");
        println!(
            "    buy leg: honours a demand of {mid_demanded} of the mid token at {buy_ppm} ppm \
             and refuses one wei more; the sender closed the leg holding {mid_handed}, so the \
             token's transfer took {}",
            mid_demanded.saturating_sub(mid_handed),
        );

        // Leg two: what does the selling pool pay back for exactly that much mid?
        let sell_result = {
            let orientation = probe.orientation;
            probe
                .search(&move |fee| (mid_demanded, orientation.out_demand_at(mid_handed, fee)))
                .await
        };
        let (sell_ppm, priced_run) = match sell_result {
            Ok(found) => found,
            Err(refusal) => {
                let reason = stopped_at(&refusal);
                // One more run, asking the selling pool for a single wei: the sequence
                // completes, and its logs say how much of the mid token the pool was
                // actually handed. That is the difference between "this pool will not
                // pay" and "this pool was paid less than the sender was charged".
                let diagnostic = probe.run(mid_demanded, U256::ONE).await;
                let taken = observe(&diagnostic, &orientation.sell, candidate.mid, WETH);
                println!(
                    "    selling pool refused every demand up to {FEE_CEILING_PPM} ppm: {reason}; \
                     with a one-wei ask it took {} of the {mid_handed} the sender held and paid \
                     {}",
                    taken.received, taken.paid,
                );
                evidence.push(serde_json::json!({
                    "mid": candidate.mid.to_string(),
                    "input_wei": orientation.input.to_string(),
                    "buying_pool": orientation.buy.json(),
                    "selling_pool": orientation.sell.json(),
                    "executes": true,
                    "buy_leg_fee_ppm_upper_inclusive": buy_ppm,
                    "mid_demanded_from_buyer": mid_demanded.to_string(),
                    "mid_handed_to_sender": mid_handed.to_string(),
                    "selling_pool_refusal": reason,
                    "selling_pool_took_with_one_wei_ask": taken.received.to_string(),
                    "selling_pool_paid_with_one_wei_ask": taken.paid.to_string(),
                    "diagnostic_status": stopped_at(&diagnostic),
                    "probes": probe.probes,
                }));
                write_evidence(head, &header, &evidence);
                continue;
            }
        };
        let out_demanded = orientation.out_demand_at(mid_handed, sell_ppm);
        let weth_handed = priced_run
            .measurement(Binding::SenderInputEnd)
            .expect("a completed run measures the sender's input-token balance");
        println!(
            "    sell leg: honours a demand of {out_demanded} WETH at {sell_ppm} ppm and refuses \
             one wei more; the sender closed the round trip holding {weth_handed} WETH against \
             {} spent",
            orientation.input,
        );

        let observed_buy = observe(&buy_run, &orientation.buy, WETH, candidate.mid);
        let observed_sell = observe(&priced_run, &orientation.sell, candidate.mid, WETH);
        let tax_taken = mid_handed.saturating_sub(observed_sell.received);
        println!(
            "    buy pool received {} paid {} (sync {}, swap {}, agree {}, {} sync logs)",
            observed_buy.received,
            observed_buy.paid,
            observed_buy.sync_log,
            observed_buy.swap_log,
            observed_buy.logs_agree,
            observed_buy.sync_logs,
        );
        println!(
            "    sell pool received {} paid {} (sync {}, swap {}, agree {}) — the sender held \
             {mid_handed} and the pool took {}, so the mid token's own transfer behaviour is \
             {}",
            observed_sell.received,
            observed_sell.paid,
            observed_sell.sync_log,
            observed_sell.swap_log,
            observed_sell.logs_agree,
            tax_taken,
            if tax_taken.is_zero() {
                "plain"
            } else {
                "taxed or non-standard"
            },
        );
        println!("    priced run: {}", priced_run.summary());
        println!("      net profit: {:?}", priced_run.net_profit);
        println!("      compared:   {:?}", priced_run.compared);

        // The control: the same two pools, the same input, the other direction. If the
        // divergence is a fact about the market it survives being traded the wrong way,
        // and the wrong way loses.
        let mut control_probe = Probe::new(Arc::clone(&adapter), pin, header.clone(), control);
        let control_mid = control.mid_demand_at(FEE_CEILING_PPM);
        let control_run = control_probe.run(control_mid, U256::ONE).await;
        println!(
            "    control (reversed legs, 2% ask): {}",
            stopped_at(&control_run)
        );

        let dump = probe.provider.dump();
        let path = dump_path(head, candidate.mid);
        std::fs::create_dir_all(path.parent().expect("fixtures/simulation-m7")).expect("dir");
        dump.write_file(&path).expect("the dump writes");
        println!(
            "    wrote {} ({} accounts, {} reads recorded)",
            path.display(),
            dump.accounts.len(),
            dump.reads.len()
        );

        evidence.push(serde_json::json!({
            "mid": candidate.mid.to_string(),
            "input_wei": orientation.input.to_string(),
            "buying_pool": orientation.buy.json(),
            "selling_pool": orientation.sell.json(),
            "executes": true,
            "buy_leg": {
                "fee_ppm_upper_inclusive": buy_ppm,
                "demanded_mid": mid_demanded.to_string(),
                "mid_handed_to_sender": mid_handed.to_string(),
                "pool_received_per_logs": observed_buy.received.to_string(),
                "pool_paid_per_logs": observed_buy.paid.to_string(),
                "swap_log_present": observed_buy.swap_log,
                "logs_agree": observed_buy.logs_agree,
                "sync_logs_in_run": observed_buy.sync_logs,
            },
            "sell_leg": {
                "fee_ppm_upper_inclusive": sell_ppm,
                "demanded_weth": out_demanded.to_string(),
                "pool_received_per_logs": observed_sell.received.to_string(),
                "pool_paid_per_logs": observed_sell.paid.to_string(),
                "swap_log_present": observed_sell.swap_log,
                "logs_agree": observed_sell.logs_agree,
                "mid_transfer_took": tax_taken.to_string(),
            },
            "priced_run_at_measured_boundaries": {
                "status": stopped_at(&priced_run),
                "steps": priced_run.steps.len(),
                "gas_used": priced_run.gas_used(),
                "gas_charge": format!("{:?}", priced_run.gas_charge),
                "net_profit": format!("{:?}", priced_run.net_profit),
                "outcome": format!("{:?}", priced_run.outcome),
                "compared": format!("{:?}", priced_run.compared),
                "weth_handed_back": weth_handed.to_string(),
                "fingerprint": priced_run.fingerprint().to_string(),
            },
            "control_reversed_orientation": {
                "status": stopped_at(&control_run),
                "demanded_mid": control_mid.to_string(),
            },
            "dump": path.display().to_string(),
            "probes": probe.probes + control_probe.probes,
        }));
        // On disk before the replay is put to it: the replay is a check on the
        // fixture, and a check that fails must not be able to take the measurement
        // with it.
        write_evidence(head, &header, &evidence);

        // The fixture and the live run are the same reads: the priced run is replayed
        // against the dump this provider recorded and compared in full, but for the one
        // field that names which object answered.
        let replay_source = format!("dump-of-this-run:{}", path.display());
        let replay: Arc<dyn StateProvider> =
            Arc::new(DumpStateProvider::new(dump.clone(), replay_source.clone()));
        let replay_request = request_for(
            &header,
            orientation.route(head, mid_demanded, out_demanded),
            out_demanded,
            replay_source,
        );
        let mut replayed = run(replay, &replay_request)
            .await
            .expect("the dump replays the run it was recorded from");
        assert_ne!(
            replayed.state_source, priced_run.state_source,
            "the replay reads the dump and the live run read the node — that difference is the \
             one thing about the replay that is not a copy of the run"
        );
        replayed.state_source = priced_run.state_source.clone();
        assert_eq!(
            replayed, priced_run,
            "the fixture does not replay the live run exactly"
        );
        println!("    the dump replays this candidate's priced run exactly");
    }

    write_evidence(head, &header, &evidence);
}

/// The evidence file, written from whatever the candidates have produced so far.
fn write_evidence(head: BlockNumber, header: &BlockContext, evidence: &[serde_json::Value]) {
    let path = workspace_root().join("data/evidence/m7/candidate-fee-measurement.json");
    let file = serde_json::json!({
        "_provenance": {
            "milestone": "M7-P1",
            "read_at_head": head.0,
            "block_hash": format!("{:?}", header.hash),
            "block_timestamp": header.timestamp,
            "base_fee_per_gas": header.base_fee_per_gas.map(|fee| fee.to_string()),
            "rpc_endpoint": rpc_url(),
            "methods": [
                "eth_blockNumber",
                "eth_getBlockByNumber",
                "eth_call",
                "eth_getCode",
                "eth_getStorageAt",
                "eth_getBalance",
                "eth_getTransactionCount"
            ],
            "how": "each candidate's two pools are read at one pinned live block; the canonical \
                    six-step sequence is then executed in REVM against the node's state at that \
                    block, demanding a payout computed from an assumed effective fee; the \
                    assumption is bisected until the pool switches from honouring the demand to \
                    refusing it, and that switch is the fee",
            "proves": "what each of these pools actually hands over for this input, against its \
                       own deployed bytecode and the node's own state — the fee a submission \
                       would get, whether each leg executes at all, what the mid token's transfer \
                       does between the pools, and the native-denominated result of the whole \
                       sequence after L2 gas",
            "does_not_prove": "that any of this is worth broadcasting: the L1 data fee is not an \
                               EVM charge and is not in these numbers, the pools are priced as \
                               they were at one head block, and a simulation never competes for \
                               inclusion. It also does not prove the pools are a market — see \
                               getReserves_blockTimestampLast on each pool",
        },
        "bracket_reading": "a leg's fee_ppm is the smallest assumed fee, in parts per million of \
                            the input, whose demand the pool honours; it refuses the demand at \
                            one ppm less, so the effective fee is in (fee_ppm - 1, fee_ppm]. The \
                            figure is form-free: it claims only what the pool hands over, not \
                            that the pool computes 997/1000",
        "fee_ceiling_ppm_tested": FEE_CEILING_PPM,
        "candidates": evidence,
    });
    std::fs::write(&path, serde_json::to_string_pretty(&file).expect("json"))
        .expect("the evidence writes");
    println!("\nwrote {}", path.display());
}

/// Which pool is bought through: the one that pays more mid for this WETH. Stated as
/// a comparison of the two pools' own reserve ratios at the pinned block, so the
/// orientation is a reading of the chain and not of the order the addresses were
/// typed in.
fn decide_orientation(chain: ChainId, pools: &[Pool], mid: Address, input: U256) -> Orientation {
    let pays = |pool: &Pool| {
        payout(
            pool.reserve_of(WETH).expect("holds WETH"),
            pool.reserve_of(mid).expect("holds mid"),
            input,
            PPM,
            PPM,
        )
    };
    let (buy, sell) = if pays(&pools[0]) >= pays(&pools[1]) {
        (pools[0], pools[1])
    } else {
        (pools[1], pools[0])
    };
    Orientation {
        chain,
        buy,
        sell,
        mid,
        input,
    }
}

fn dump_path(head: BlockNumber, mid: Address) -> PathBuf {
    workspace_root().join(format!(
        "fixtures/simulation-m7/dump-{}-{}.json",
        head.0,
        &mid.to_string()[2..10]
    ))
}

/// The request shape every run in this file is put through, live and replayed alike.
///
/// There is deliberately one builder: the replay at the end of a candidate is the
/// same question asked of recorded state, and the first version of this file had two
/// builders whose price-provenance strings differed by a clause — which the engine
/// carried into the result, so the replay "failed" on wording rather than on state.
fn request_for(
    header: &BlockContext,
    route: PricedRoute,
    asked_output: U256,
    state_source: String,
) -> SimulationRequest {
    let transaction =
        TransactionSpec::canonical(Funding::WrapNative, Settle::UnwrapInputToken, asked_output);
    SimulationRequest::new(
        header.clone(),
        route,
        TransactionSpec {
            rules: EvmRules::Prague,
            ..transaction
        },
        StateSpec::new(state_source),
        GasPricing::Eip1559 {
            priority_fee_per_gas: 0,
            provenance: format!(
                "block {}'s own base fee as its header reports it, with no tip",
                header.number.0
            ),
        },
    )
    .expect("a request built from the node's own header and reserves")
}
