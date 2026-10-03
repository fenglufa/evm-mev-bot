//! §26's fourteen lines, answered by the node rather than by a caller.
//!
//! [`crate::preflight`] decides; this file *feeds* it. The distinction matters because the
//! gate is pure — no async, no I/O, so it can be run over scripted answers in a test and give
//! the same verdict it will give live — and because a gate that read its own facts could
//! quietly re-search, which §27 forbids. Everything here is a read, and every read is pinned
//! to a height that is named in the resulting row.
//!
//! Four things are read live, in this order, and the order is the argument:
//!
//! 1. **the head**, from one `eth_getBlockByNumber("latest")` so its number and hash come
//!    from the same header (two reads can straddle a block, and a binding check built from a
//!    straddled pair is a check against no block at all);
//! 2. **the fee twice** — at the block the route was priced on, which is the number the
//!    transaction will carry, and at the head, which is the number the chain is asking now.
//!    §26's fee line is the comparison between those two, so both halves have to be reads;
//! 3. **each pool's reserves at the head**, through `token0()`/`token1()` so the route's
//!    `(in, out)` is the pair's own ordering and not a guess about which side is which;
//! 4. **what the sequence would cost**, step by step: the same builder the lane will use, the
//!    same gas policy, and an L1 line from the fee predeploy over the pre-signing envelope.
//!
//! Nothing here fills in a number a read failed to produce. A failed reserve read is
//! [`ExecutionError::ChainRead`], not a `None` carried forward, because §27's verdict is a
//! comparison and a one-sided comparison would let this file *author* the very line the gate
//! exists to check.
//!
//! The intent the gate examines is a *priced copy* of the plan's first step — fee, nonce and
//! all, assigned the way [`crate::sequence`] will assign them at send time. §26's fee line
//! asks "does the number in the transaction equal the number at the head", and it can only
//! answer that about a transaction that has those numbers in it.

use alloy_primitives::{Address, B256, U256};
use serde_json::{json, Value};

use evm_chain::{rpc::chain_block_from_value, ChainAdapter, HttpChainAdapter};
use evm_core::BlockNumber;
use evm_metrics::{Clock, Metrics, Stage};
use evm_simulation::route::PricedRoute;

use crate::builder::TransactionBuilder;
use crate::chain_read::read_binding;
use crate::cost::EstimatedCost;
use crate::error::{ExecutionError, Result};
use crate::fee::FeeReading;
use crate::gate::{BalanceEvidence, GateAttempt, GateFacts, NonceEvidence};
use crate::intent::TransactionIntent;
use crate::lifecycle::metric_keys;
use crate::market::MarketKind;
use crate::nonce::NonceReading;
use crate::preflight::{
    ExecutionPreflight, HeadReading, InputAssetEvidence, PreflightFacts, PreflightReport,
    Repricing, ReserveReading, SequencePricing, StepPricing,
};
use crate::sequence::{PlannedStep, SequencePlan, SnapshotPin};
use crate::stage::{Abilities, ExecutionSetup};
use crate::tx::TransactionType;

use super::reads::{estimate_l1_fee, pool_state_row, pre_signing_envelope, read_pool, PoolState};
use super::GiwaAssetReader;

fn read_error(error: evm_chain::ChainError) -> ExecutionError {
    ExecutionError::ChainRead(error.to_string())
}

/// Everything a live preflight needs that is not itself a read: the plan under examination,
/// the route it was priced from, and the endpoint it is examined against.
///
/// `route` is not redundant with `plan`: the plan carries the executable shape (steps, pools,
/// amounts) and the route carries what the *finding* claimed — each leg's reserves and its
/// attested fee. §26's reserve line is the difference between the two, so both must be
/// present, and a gatherer handed only one could not fail to lie about it.
pub struct LivePreflightReads<'a> {
    /// The endpoint the market reads come from. Usually the same node the lane sends
    /// through, and named separately because §40 forbids a cached answer being the final
    /// evidence: these are reads, not a reuse of what the simulation already saw.
    pub market: &'a HttpChainAdapter,
    /// The lane's own four surfaces, so the gate legs are answered by the same endpoint that
    /// will sign and send rather than by a second opinion.
    pub abilities: &'a Abilities,
    pub setup: &'a ExecutionSetup,
    pub plan: &'a SequencePlan,
    pub route: &'a PricedRoute,
    /// What the simulation measured back at the pinned block — §18's expected side, and §27's
    /// reference point. Taken from the run rather than recomputed here.
    pub priced_output_wei: U256,
    /// §5's "fee proven", in words a reader can follow to the measurement.
    pub fee_evidence: String,
    /// The attempt as the caller states it: simulation outcome, risk decision, freshness. The
    /// gatherer reads none of those and re-decides none of them (§27).
    pub attempt: GateAttempt,
    pub clock: Clock,
}

/// What one live gather read, and what the gate made of it.
pub struct GatheredPreflight {
    /// §51's label, copied out of the plan so the evidence row is readable without the plan
    /// beside it: a preflight that passed over a `CONTROLLED_FIXTURE` route is not a real
    /// arbitrage gate, and the row has to say which of the two it cleared.
    pub market: MarketKind,
    pub head: HeadReading,
    /// [`crate::sequence::SequenceStage`]'s `before_head` argument: the same block as
    /// [`Self::head`], kept as a pin because a gather that produced no head returned an error
    /// instead of this value.
    pub before: SnapshotPin,
    /// The fee the transaction will carry: read at the block the route was pinned to, which
    /// is what [`crate::sequence`] does per step.
    pub fee_at_pin: FeeReading,
    /// The fee the chain is asking now: read at [`Self::head`]. §26's fee line is the
    /// agreement of these two, so both are kept in the row.
    pub fee_at_head: FeeReading,
    /// The two pools, in route order — leg 0 first, leg 1 second, never sorted.
    pub reserves: [PoolState; 2],
    pub pricing: SequencePricing,
    pub input_asset: InputAssetEvidence,
    /// The first step as the gate saw it: the plan's intent with the fee and nonce this
    /// gather read into it.
    pub priced_intent: TransactionIntent,
    pub report: PreflightReport,
    /// §53's `preflight_latency`, measured over the reads and the verdict together, and also
    /// recorded into the caller's [`Metrics`] by [`LivePreflightReads::gather`].
    pub latency_ms: u64,
}

impl GatheredPreflight {
    /// The reads, as one JSON row for the evidence file: every number the gate used, with the
    /// height and the method that produced it.
    pub fn to_json(&self) -> Value {
        json!({
            "attempt_id": self.report.attempt_id,
            "market": self.market.to_json(),
            "head": match &self.head {
                HeadReading::Read { number, hash, source } => json!({
                    "block_number": number,
                    "block_hash": format!("{hash:#x}"),
                    "source": source,
                }),
                HeadReading::Unread(why) => json!({ "unread": why }),
            },
            "pinned_block": {
                "block_number": self.priced_intent.block_number.0,
                "block_hash": format!("{:#x}", self.priced_intent.block_hash),
            },
            "fee_at_pinned_block": fee_row(&self.fee_at_pin),
            "fee_at_head": fee_row(&self.fee_at_head),
            "pools": self.reserves.iter().map(pool_state_row).collect::<Vec<_>>(),
            "steps": self.pricing.steps.iter().map(|step| json!({
                "index": step.index,
                "to": format!("{:#x}", step.to),
                "gas_limit": step.cost.gas_limit,
                "simulated_gas_used": step.simulated_gas_used,
                "max_fee_per_gas": step.cost.max_fee_per_gas.to_string(),
                "value_wei": step.cost.value_wei.to_string(),
                "l2_ceiling_wei": step.cost.l2_ceiling.to_string(),
                "l1_fee_estimate_wei": step.cost.l1_fee_estimate.to_string(),
                "l1_fee_source": step.cost.l1_fee_source.describe(),
                "total_ceiling_wei": step.cost.total_ceiling_wei.to_string(),
            })).collect::<Vec<_>>(),
            "sequence_ceiling_wei": self.pricing.total_ceiling_wei.to_string(),
            "sequence_l1_estimate_wei": self.pricing.total_l1_estimate_wei.to_string(),
            "input_asset": match &self.input_asset {
                InputAssetEvidence::Native { required_wei, available_wei, source } => json!({
                    "kind": "native",
                    "required_wei": required_wei.to_string(),
                    "available_wei": available_wei.to_string(),
                    "source": source,
                }),
                InputAssetEvidence::Token { token, required_amount, available_amount, source } => json!({
                    "kind": "token",
                    "token": format!("{token:#x}"),
                    "required_amount": required_amount.to_string(),
                    "available_amount": available_amount.to_string(),
                    "source": source,
                }),
                InputAssetEvidence::Unread(why) => json!({ "unread": why }),
            },
            "priced_intent": serde_json::to_value(&self.priced_intent).unwrap_or(Value::Null),
            "preflight_latency_ms": self.latency_ms,
            "preflight": serde_json::to_value(&self.report).unwrap_or(Value::Null),
        })
    }
}

fn fee_row(fee: &FeeReading) -> Value {
    json!({
        "block_number": fee.block_number,
        "block_hash": format!("{:#x}", fee.block_hash),
        "base_fee_per_gas": fee.base_fee_per_gas.map(|v| v.to_string()),
        "suggested_tip_wei": fee.suggested_tip_wei.map(|v| v.to_string()),
        "max_fee_per_gas": fee.max_fee_per_gas.to_string(),
        "max_priority_fee_per_gas": fee.max_priority_fee_per_gas.map(|v| v.to_string()),
        "provenance": fee.provenance,
    })
}

impl LivePreflightReads<'_> {
    /// Read everything §26 asks and hand it to the gate. The cost is bounded and named: one
    /// head read, two fee reads (a block plus a tip each), three calls per pool, one
    /// `getL1Fee` per step, one nonce pair, one balance, one `eth_getBalance`, two `balanceOf`
    /// at most, and one `eth_chainId`.
    ///
    /// §53's `preflight_latency` is recorded here rather than at the call site because the
    /// span belongs to these reads: the gate itself is a pure decision and the stage that
    /// consumes the verdict must not re-time it.
    pub async fn gather(&self, metrics: &mut Metrics) -> Result<GatheredPreflight> {
        let gathered = self.gather_legs(metrics).await;
        // One exit, so a leg that failed partway cannot leave its name on the sink: the label
        // outlives this function otherwise, and the reads that follow are the lane's, made for
        // its own reasons. §12's requirement is that the instrument say nothing it did not do.
        self.clear_stamp();
        gathered
    }

    /// The gather itself: §26's fifteen-odd reads, then the pure verdict.
    async fn gather_legs(&self, metrics: &mut Metrics) -> Result<GatheredPreflight> {
        let started = self.clock.now_ms();
        let first = self.plan.steps.first().ok_or_else(|| {
            ExecutionError::Evidence(
                "the plan this preflight would examine has no step to examine".to_string(),
            )
        })?;
        self.route_matches_plan()?;

        self.stamp("head at latest");
        let (head_number, head_hash, head_source) = self.read_head().await?;
        let head = HeadReading::Read {
            number: head_number,
            hash: head_hash,
            source: head_source,
        };
        let before = SnapshotPin {
            block_number: head_number,
            block_hash: head_hash,
        };

        self.stamp("fee at pinned block");
        let fee_at_pin = self
            .fee_at(
                self.plan.block_number,
                self.plan.block_hash,
                first.intent.tx_type,
            )
            .await?;
        self.stamp("fee at head");
        let fee_at_head = self
            .fee_at(head_number, head_hash, first.intent.tx_type)
            .await?;

        self.stamp("pending and latest nonces");
        let nonce_reading = self.abilities.nonces.nonce(self.plan.sender).await?;
        let priced_intent = self.priced(first, &fee_at_pin, nonce_reading.pending)?;
        self.stamp("cost ceiling per step");
        let pricing = self
            .price_steps(&fee_at_pin, nonce_reading.pending, head_number)
            .await?;

        // One stamp per leg, so the two pools' reads are toldable apart in the trace; the
        // order is the route's, and stamping in it is what keeps that order a fact rather than
        // an assumption a reader has to trust.
        self.stamp("reserves of leg 0");
        let leg_zero = self.read_leg(0, head_number).await?;
        self.stamp("reserves of leg 1");
        let leg_one = self.read_leg(1, head_number).await?;
        let reserves: [PoolState; 2] = [leg_zero, leg_one];
        let reads: [ReserveReading; 2] = [
            self.reserve_reading(0, &reserves[0])?,
            self.reserve_reading(1, &reserves[1])?,
        ];

        self.stamp("native balance of the sender");
        let available_wei = self
            .abilities
            .fees
            .balance(self.plan.sender, head_number)
            .await?;
        self.stamp("input asset of the route");
        let input_asset = self.input_asset(first, available_wei, head_number).await?;
        self.stamp("endpoint chain id");
        let endpoint_chain_id = self.abilities.chain.endpoint_chain_id().await?;
        self.stamp("block binding at pin");
        let binding = read_binding(
            &*self.abilities.chain,
            BlockNumber(self.plan.block_number),
            self.plan.block_hash,
        )
        .await;
        // The verdict below is pure, so nothing reads after this point; `gather` still takes the
        // label off on its way out, because a leg that errored never reaches here.
        self.clear_stamp();

        let gate = GateFacts {
            attempt: self.attempt.clone(),
            intent_chain_id: priced_intent.chain_id,
            configured_chain_id: self.setup.build.expected_chain_id,
            endpoint_chain_id,
            binding,
            balance: balance_evidence(available_wei, &pricing, self.plan.sender, head_number),
            nonce: nonce_evidence(priced_intent.nonce, &nonce_reading),
        };
        let repricing = Repricing {
            input_amount: self.plan.input_amount,
            first_fee: self.route.legs[0].fee,
            second_fee: self.route.legs[1].fee,
            fee_evidence: self.fee_evidence.clone(),
            priced_output_wei: self.priced_output_wei,
            minimum_required_profit_wei: priced_intent.minimum_required_profit_wei,
        };
        let facts = PreflightFacts {
            attempt_id: self.plan.opportunity_id.clone(),
            intent: &priced_intent,
            gate,
            head: head.clone(),
            reserves: reads,
            repricing,
            pricing: pricing.clone(),
            fee: &fee_at_head,
            input_asset: input_asset.clone(),
            signed_max_fee_per_gas: priced_intent.max_fee_per_gas,
        };
        let report = ExecutionPreflight::run(&facts);
        let latency_ms = self.clock.since_ms(started);
        metrics.record_latency(metric_keys::PREFLIGHT_LATENCY, latency_ms);
        Ok(GatheredPreflight {
            market: self.plan.market.clone(),
            head,
            before,
            fee_at_pin,
            fee_at_head,
            reserves,
            pricing,
            input_asset,
            priced_intent,
            report,
            latency_ms,
        })
    }

    /// M8.4.1 §9's label for the gate's next reads: this stage, and the §26 leg about to be
    /// read.
    ///
    /// The handle is the market adapter's own because a live gather reads through two sockets —
    /// the market's for the head, the reserves and the L1 fee, the lane's for the fees, nonces,
    /// balance and binding — and §11 wires both to the *same* sink, so one stamp covers a leg
    /// whichever of the two answers it. That sharing is also what makes the last stamp of a
    /// gather able to mislead: the label outlives this function unless it is taken off, and the
    /// lane reads for its own reasons afterwards.
    ///
    /// With no sink watching this is a `None` test and nothing else — no lock on the request
    /// path, no clock read, no request (§12). The gather reads what it always read.
    fn stamp(&self, caller: &str) {
        if let Some(sink) = self.market.rpc_trace() {
            sink.set_context(Stage::Preflight.as_str(), caller);
        }
    }

    /// Take the label off, once the last read this gather makes is in hand.
    fn clear_stamp(&self) {
        if let Some(sink) = self.market.rpc_trace() {
            sink.clear_context();
        }
    }

    /// The chain's present head, as one header. `ChainAdapter::latest_block` would need a
    /// second read for the hash, and the two halves of a head have to be the same block.
    async fn read_head(&self) -> Result<(u64, B256, String)> {
        let raw = self
            .market
            .request_raw("eth_getBlockByNumber", json!(["latest", false]))
            .await
            .map_err(read_error)?;
        let block = chain_block_from_value(self.market.chain_id(), &raw).map_err(read_error)?;
        Ok((
            block.number.0,
            block.hash,
            format!(
                "eth_getBlockByNumber(\"latest\") over {}: number and hash out of the same \
                 header",
                self.market.url()
            ),
        ))
    }

    async fn fee_at(
        &self,
        number: u64,
        hash: B256,
        tx_type: TransactionType,
    ) -> Result<FeeReading> {
        self.abilities
            .fees
            .fee_reading(number, hash, tx_type, &self.setup.fee)
            .await
    }

    /// The two things this gather compares side by side have to be about the same route: the
    /// plan being sent and the finding it was priced from. A plan for one pair of pools and a
    /// route for another would make §26's reserve line a comparison between two different
    /// trades, so the mismatch is refused before any read is paid for.
    fn route_matches_plan(&self) -> Result<()> {
        if self.plan.pools.len() != 2 {
            return Err(ExecutionError::Evidence(format!(
                "the plan names {} venues and §16's shape is two, so there is no pair of legs \
                 to re-price",
                self.plan.pools.len()
            )));
        }
        for (index, leg) in self.route.legs.iter().enumerate() {
            if self.plan.pools[index] != leg.pool.address {
                return Err(ExecutionError::Evidence(format!(
                    "leg {} of this route is pool {} and leg {} of the plan is {}; §27's gate \
                     cannot re-price a route the plan does not send",
                    index, leg.pool.address, index, self.plan.pools[index]
                )));
            }
        }
        if self.plan.input_token != self.route.input_token.address {
            return Err(ExecutionError::Evidence(format!(
                "the plan spends {} and the route was priced in {}; the input amount this gate \
                 compares would be in two units at once",
                self.plan.input_token, self.route.input_token.address
            )));
        }
        Ok(())
    }

    /// One pool, read at the head, in the order the route names its legs. Not sorted: leg 0 is
    /// the buy and leg 1 is the sell, and §23's route audit asks about them in that order.
    async fn read_leg(&self, leg_index: usize, at_head: u64) -> Result<PoolState> {
        read_pool(
            self.market,
            self.route.legs[leg_index].pool.address,
            BlockNumber(at_head),
        )
        .await
    }

    /// One entry of §26's reserve line: the reserves the finding claimed, beside the reserves
    /// the head holds, in the direction this leg moves.
    fn reserve_reading(&self, leg_index: usize, pool: &PoolState) -> Result<ReserveReading> {
        let leg = &self.route.legs[leg_index];
        let (current_in, current_out) = pool.in_out(leg.token_in.address, leg.token_out.address)?;
        Ok(ReserveReading {
            pool: leg.pool,
            leg_index,
            priced_reserve_in: leg.reserve_in,
            priced_reserve_out: leg.reserve_out,
            current_reserve_in: Some(current_in),
            current_reserve_out: Some(current_out),
            source: pool.source.clone(),
        })
    }

    /// The first step as it will be signed: the plan's intent with the fee read at the pinned
    /// block and the pending nonce, which is what §26's fee and nonce lines are about.
    fn priced(
        &self,
        step: &PlannedStep,
        fee: &FeeReading,
        pending_nonce: u64,
    ) -> Result<TransactionIntent> {
        let mut intent = step.intent.clone();
        if step.position == 0 {
            intent.nonce = pending_nonce;
        }
        let (max_fee_per_gas, max_priority_fee_per_gas) = fee.fields_for(intent.tx_type)?;
        intent.max_fee_per_gas = max_fee_per_gas;
        intent.max_priority_fee_per_gas = max_priority_fee_per_gas;
        Ok(intent)
    }

    /// Each step's ceiling, built the way the lane will build it: the same
    /// [`TransactionBuilder`], the same gas policy, and the envelope the oracle will be shown
    /// after signing. The nonce the envelope is built with is the pending nonce plus the step
    /// position, because a serial sequence from one EOA spends exactly that range.
    async fn price_steps(
        &self,
        fee: &FeeReading,
        pending_nonce: u64,
        at_head: u64,
    ) -> Result<SequencePricing> {
        let mut steps = Vec::with_capacity(self.plan.steps.len());
        for step in &self.plan.steps {
            let mut intent = self.priced(step, fee, pending_nonce)?;
            intent.nonce = pending_nonce
                .checked_add(step.position as u64)
                .ok_or_else(|| {
                    ExecutionError::Evidence(format!(
                        "step {} of this sequence does not fit in the nonce range above {}",
                        step.position, pending_nonce
                    ))
                })?;
            let mut policy = self.setup.build.clone();
            policy.simulated_gas_used = Some(step.simulated_gas_used);
            let build = TransactionBuilder::build(&intent, &policy)?;
            // The fee the ceiling uses is the one in the envelope, not the one in the
            // reading: for a legacy step those are the same field under a different name, and
            // for an eip1559 step the envelope is what a node will price against.
            let (_, max_fee_per_gas) = build.unsigned.fee_field()?;
            let envelope = pre_signing_envelope(&build.unsigned);
            let l1 = estimate_l1_fee(self.market, &envelope, BlockNumber(at_head)).await;
            let cost = EstimatedCost::new(build.gas_limit, max_fee_per_gas, intent.value, l1)?;
            steps.push(StepPricing {
                index: step.position,
                to: intent.target,
                cost,
                simulated_gas_used: Some(step.simulated_gas_used),
            });
        }
        SequencePricing::new(steps)
    }

    /// §26's `input_token_balance` line, answered from the first step's own shape: a route
    /// that opens by wrapping native needs native, and a route that opens by spending a token
    /// needs that token. A step that does neither is reported as unread rather than assumed.
    async fn input_asset(
        &self,
        first: &PlannedStep,
        available_native: U256,
        at_head: u64,
    ) -> Result<InputAssetEvidence> {
        if first.intent.value > U256::ZERO && first.signature.contains("deposit()") {
            return Ok(InputAssetEvidence::Native {
                required_wei: first.intent.value,
                available_wei: available_native,
                source: format!(
                    "eth_getBalance({}) at block {at_head}, against the {} wei this route's \
                     first step wraps",
                    self.plan.sender, first.intent.value
                ),
            });
        }
        let spends_input_token =
            first.intent.target == self.plan.input_token && !first.intent.calldata.is_empty();
        if spends_input_token {
            let reader = GiwaAssetReader::new(self.market);
            let reading = reader
                .token_balance(
                    self.plan.input_token,
                    self.plan.sender,
                    BlockNumber(at_head),
                )
                .await?;
            return Ok(InputAssetEvidence::Token {
                token: self.plan.input_token,
                required_amount: self.plan.input_amount,
                available_amount: reading.amount,
                source: reading.source,
            });
        }
        Ok(InputAssetEvidence::Unread(format!(
            "step 1 of this route is {}, which neither wraps native nor sends to {}, so this \
             gather cannot say which asset funds the route from the step's own shape",
            first.signature, self.plan.input_token
        )))
    }
}

/// §11's nonce leg, formed from the two numbers rather than asserted: the gatherer knows what
/// it put into the intent and what the node says is pending, and if those differ the gate has
/// to see the difference rather than a `Matches` built from one of them.
fn nonce_evidence(intent_nonce: u64, reading: &NonceReading) -> NonceEvidence {
    let source = format!(
        "{} — step 1 of this sequence is priced to carry the pending nonce, and the lane \
         allocates from that same reading when it signs",
        reading.source
    );
    if intent_nonce == reading.pending {
        NonceEvidence::Matches {
            nonce: reading.pending,
            source,
        }
    } else {
        NonceEvidence::Differs {
            intent_nonce,
            pending_nonce: reading.pending,
            source,
        }
    }
}

/// §26's native-balance line for a whole sequence.
///
/// The `maximum_spend_wei` the gate's own leg gets is **step 1's** ceiling, because that is
/// the transaction [`crate::gate`] is examining; §26's line then compares the same balance
/// against the sequence total (`SequencePricing::total_ceiling_wei`) and reports the gap
/// between the two in its detail. Six steps of ceiling and one step of ceiling are different
/// questions, and a caller that conflated them would pass a wallet that cannot fund the route.
fn balance_evidence(
    available_wei: U256,
    pricing: &SequencePricing,
    sender: Address,
    at_head: u64,
) -> BalanceEvidence {
    let first_step_wei = pricing.steps[0].cost.total_ceiling_wei;
    let source = format!(
        "eth_getBalance({sender}) at block {at_head}: step 1's ceiling is {} wei and the whole \
         {}-step sequence's is {}",
        first_step_wei,
        pricing.steps.len(),
        pricing.total_ceiling_wei
    );
    if available_wei >= first_step_wei {
        BalanceEvidence::Sufficient {
            available_wei,
            maximum_spend_wei: first_step_wei,
            source,
        }
    } else {
        BalanceEvidence::Insufficient {
            available_wei,
            maximum_spend_wei: first_step_wei,
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::L1FeeSource;
    use crate::preflight::StepPricing;

    fn wallet() -> Address {
        Address::from_slice(&[1u8; 20])
    }

    /// One step whose ceiling is exactly `l2 + l1`: `gas_limit` 1 over a fee of `l2`, plus an
    /// oracle line of `l1`.
    fn step(index: usize, l2: u64, l1: u64) -> StepPricing {
        StepPricing {
            index,
            to: wallet(),
            cost: EstimatedCost::new(
                1,
                U256::from(l2),
                U256::ZERO,
                L1FeeSource::OracleEstimate {
                    amount: U256::from(l1),
                    block_number: 7,
                    read_by: "test oracle".to_string(),
                },
            )
            .unwrap(),
            simulated_gas_used: Some(900),
        }
    }

    fn pricing(steps: &[(u64, u64)]) -> SequencePricing {
        SequencePricing::new(
            steps
                .iter()
                .enumerate()
                .map(|(i, (l2, l1))| step(i, *l2, *l1))
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn a_wallet_that_covers_step_one_but_not_the_sequence_keeps_both_readings_in_the_evidence() {
        // Two steps, each 2 000 wei of ceiling: [`crate::gate`]'s leg is about the one
        // transaction it was handed, §26's line is about the route. A balance of 3 000 passes
        // the first and fails the second, so the detail string has to name both numbers — a
        // reader told only "3 000 ≥ 2 000" would conclude the run is funded.
        let pricing = pricing(&[(1_500, 500), (1_500, 500)]);
        let balance = balance_evidence(U256::from(3_000u64), &pricing, wallet(), 7);
        match balance {
            BalanceEvidence::Sufficient {
                available_wei,
                maximum_spend_wei,
                source,
            } => {
                assert_eq!(available_wei, U256::from(3_000u64));
                assert_eq!(maximum_spend_wei, U256::from(2_000u64));
                assert!(source.contains("2000"), "{source}");
                assert!(source.contains("4000"), "{source}");
                assert!(source.contains("2-step"), "{source}");
            }
            other => panic!("step 1's ceiling was not covered: {other:?}"),
        }
        assert_eq!(pricing.total_ceiling_wei, U256::from(4_000u64));
    }

    #[test]
    fn a_balance_below_even_the_first_step_is_the_gate_refusal_not_just_the_sequence_one() {
        let pricing = pricing(&[(1_500, 500), (1_500, 500)]);
        let balance = balance_evidence(U256::from(1_500u64), &pricing, wallet(), 7);
        assert!(matches!(balance, BalanceEvidence::Insufficient { .. }));
    }

    #[test]
    fn the_nonce_leg_reports_the_difference_it_sees_instead_of_the_value_it_wanted() {
        let reading = NonceReading {
            address: wallet(),
            confirmed: 3,
            pending: 5,
            at_block: 7,
            source: "eth_getTransactionCount".to_string(),
        };
        assert!(matches!(
            nonce_evidence(4, &reading),
            NonceEvidence::Differs {
                intent_nonce: 4,
                pending_nonce: 5,
                ..
            }
        ));
        assert!(matches!(
            nonce_evidence(5, &reading),
            NonceEvidence::Matches { nonce: 5, .. }
        ));
    }
}
