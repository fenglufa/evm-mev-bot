//! Transaction construction: `PricedRoute -> the exact sequence of calls a real
//! EVM would run`.
//!
//! §12 of the task asks M4 to be the first milestone that answers "how does an
//! opportunity become a transaction". The answer this crate gives is the boring
//! one, and it is the answer the chain itself gave: no router, no executor
//! contract, no flash swap. The deployed pair is called directly, from one
//! sender, in a sequence of ordinary transactions — which is exactly the shape of
//! the real transactions recorded in `data/simulation-m4/execution-evidence.json`
//! (an account calling `swap(...)` on the pool, 164 bytes of calldata,
//! `value = 0`). §13 permits a minimal `ArbitrageExecutor` only where the
//! environment offers nothing suitable; this environment offers the pair itself,
//! so M4 adds no Solidity at all.
//!
//! ## Why a sequence and not one atomic call
//!
//! A V2 pair's `swap` is **exact-out**: it pays `amountOut` first and then checks
//! the invariant against the balances it actually holds, so whatever the caller
//! sent beyond what the check needs stays in the pool. That makes the interesting
//! question "what is the largest output these pools will actually pay for this
//! input", and that question is answered by executing the sequence and reading the
//! balances back — not by a formula. So every amount in a plan is either a number
//! the plan was given or a number a previous execution produced
//! ([`AmountSource::Measured`]), and the second kind is what lets a token with
//! transfer tax be decided by the EVM instead of assumed (§23).
//!
//! ## What is setup and what is fact
//!
//! The sender is a deterministic test address with no private key (§58), and the
//! tokens it spends arrive through a state override that is labelled as setup
//! wherever it appears. Nothing here invents pool state: each pool's own
//! `token0()` / `token1()` answers, read at the pinned block, become
//! [`PairSides`] and decide which of `amount0Out` / `amount1Out` a leg fills.
//! There is no `approve` step, because the sender transfers its own tokens to the
//! pool it is trading with — §57's allowance override has no use in this plan, and
//! saying so is better than carrying an unused step type.
//!
//! REVM types do not appear in this file (§7). A [`ResolvedStep`] holds an
//! `evm_protocol::V2Call`, a `to`, a `value` and a nonce, and the engine is the
//! first place that turns those into something the EVM can run.

use std::collections::BTreeMap;

use alloy_primitives::{Address, Bytes, U256};
use serde::Serialize;

use evm_core::{ChainId, TokenId};
use evm_protocol::V2Call;

use crate::error::SimulationError;
use crate::route::{PricedRoute, SlippageRecord};

/// A balance a plan reads out of execution and spends or reports later.
///
/// A closed enum rather than a map of addresses, so a plan's data flow can be
/// read without running it: every [`AmountSource::Measured`] names the measurement
/// that has to have happened first, and a missing one is an error — never a zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Binding {
    /// The sender's balance of the route's input token, before anything is sent.
    /// Proves the funding override landed with exactly `input_amount`.
    SenderInputStart,
    /// What the first pool actually delivered, after the mid token's own transfer
    /// behaviour has been applied. A tax shows up here as a number below
    /// `analytical_mid_amount`.
    SenderMidReceived,
    /// The sender's balance of the input token after both legs.
    SenderInputEnd,
    /// The sender's balance of the input token after unwrapping.
    SenderInputAfterUnwrap,
    /// The sender's native balance before the sequence runs.
    SenderNativeStart,
    /// The sender's native balance after the sequence runs, gas paid.
    SenderNativeEnd,
}

impl Binding {
    pub const fn name(self) -> &'static str {
        match self {
            Self::SenderInputStart => "sender_input_start",
            Self::SenderMidReceived => "sender_mid_received",
            Self::SenderInputEnd => "sender_input_end",
            Self::SenderInputAfterUnwrap => "sender_input_after_unwrap",
            Self::SenderNativeStart => "sender_native_start",
            Self::SenderNativeEnd => "sender_native_end",
        }
    }
}

/// The measurements a run has produced so far.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Measurements {
    values: BTreeMap<Binding, U256>,
}

impl Measurements {
    pub fn set(&mut self, binding: Binding, value: U256) {
        self.values.insert(binding, value);
    }

    pub fn get(&self, binding: Binding) -> Option<U256> {
        self.values.get(&binding).copied()
    }

    /// Everything recorded, in binding order — the section of the evidence file
    /// that says what the EVM handed back.
    pub fn recorded(&self) -> Vec<(&'static str, U256)> {
        self.values
            .iter()
            .map(|(binding, value)| (binding.name(), *value))
            .collect()
    }
}

/// Where a step's amount comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum AmountSource {
    /// A number the plan was handed when it was built.
    Absolute(U256),
    /// A number a previous step's measurement produced.
    Measured(Binding),
}

impl AmountSource {
    pub fn resolve(&self, measurements: &Measurements) -> Result<U256, SimulationError> {
        match self {
            Self::Absolute(amount) => Ok(*amount),
            Self::Measured(binding) => measurements.get(*binding).ok_or_else(|| {
                SimulationError::UnsupportedTransaction(format!(
                    "step needs the measurement `{}`, which this run has not produced",
                    binding.name()
                ))
            }),
        }
    }

    pub const fn binding(&self) -> Option<Binding> {
        match self {
            Self::Absolute(_) => None,
            Self::Measured(binding) => Some(*binding),
        }
    }

    const fn source_text(&self) -> Option<&'static str> {
        match self {
            Self::Absolute(_) => None,
            Self::Measured(Binding::SenderMidReceived) => Some("the measured mid delivery"),
            Self::Measured(Binding::SenderInputEnd) => Some("the measured final balance"),
            Self::Measured(binding) => Some(binding.name()),
        }
    }
}

/// A pool's own answer about which token is `token0`.
///
/// Built by the engine's preflight from `token0()` and `token1()` executed against
/// the pinned state, never from a sorting assumption in Rust: ordering the two
/// addresses here would make the plan depend on a claim about this protocol
/// instead of on the contract's reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PairSides {
    pub pool: Address,
    pub token0: Address,
    pub token1: Address,
}

impl PairSides {
    pub fn holds(&self, token: Address) -> bool {
        self.token0 == token || self.token1 == token
    }

    /// Which of `amount0Out` / `amount1Out` an output of `token_out` fills.
    pub fn out_words(
        &self,
        token_out: Address,
        amount_out: U256,
    ) -> Result<(U256, U256), SimulationError> {
        if self.token0 == token_out {
            Ok((amount_out, U256::ZERO))
        } else if self.token1 == token_out {
            Ok((U256::ZERO, amount_out))
        } else {
            Err(SimulationError::UnsupportedTransaction(format!(
                "pool {} reports token0 {} and token1 {}, so it cannot pay out {}",
                self.pool, self.token0, self.token1, token_out
            )))
        }
    }
}

/// How the sender comes to hold the input token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Funding {
    /// The override-funded account already holds the ERC20 input. Every step then
    /// sends `msg.value = 0`, and that is the correct value rather than a default
    /// (§59).
    Erc20Balance,
    /// The input is the chain's wrapped native token and the sender starts with
    /// native: the plan's first spend is `deposit()` carrying exactly
    /// `input_amount` as `msg.value`.
    WrapNative,
}

/// How the sender expresses what it ended up holding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Settle {
    /// Leave the result in the input token's own unit. This is all the profit
    /// numbers need, and it asserts nothing about the native asset.
    KeepInputToken,
    /// `withdraw()` the whole final input-token balance into native and measure
    /// the native delta. Executing this is what turns a `net_profit` denominated
    /// in the native unit into a proved statement instead of an assumed one
    /// (§31, §32). Declaring it for a token that is not the wrapped native asset
    /// is what the delta check catches: the balances simply will not agree.
    UnwrapInputToken,
}

/// What the engine has to read after a step, in the EVM's own state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measurement {
    /// Run the token contract's own `balanceOf`, so the number comes from the
    /// token's accounting rather than from a storage layout this crate would have
    /// to guess.
    Erc20Balance { token: Address, account: Address },
    /// The account's native balance. There is no call for this; the engine takes
    /// it from the state it is already maintaining.
    NativeBalance { account: Address },
}

/// One step of a plan: one transaction from one sender.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum PlanStep {
    MeasureErc20 {
        token: Address,
        account: Address,
        binding: Binding,
    },
    MeasureNative {
        account: Address,
        binding: Binding,
    },
    Transfer {
        token: Address,
        to: Address,
        amount: AmountSource,
    },
    Swap {
        sides: PairSides,
        token_out: Address,
        amount_out: AmountSource,
        to: Address,
    },
    Deposit {
        wrapped: Address,
        wad: U256,
    },
    Withdraw {
        wrapped: Address,
        wad: AmountSource,
    },
}

impl PlanStep {
    /// The amount this step moves, before measurement (`None` for a native read).
    fn amount_source(&self) -> Option<AmountSource> {
        match self {
            Self::MeasureNative { .. } => None,
            Self::MeasureErc20 { .. } => Some(AmountSource::Absolute(U256::ZERO)),
            Self::Deposit { wad, .. } => Some(AmountSource::Absolute(*wad)),
            Self::Transfer { amount, .. }
            | Self::Swap {
                amount_out: amount, ..
            }
            | Self::Withdraw { wad: amount, .. } => Some(*amount),
        }
    }

    /// The contract the transaction is addressed to. A native balance read has no
    /// address, which is why it is refused rather than sent to `Address::ZERO`.
    fn target(&self) -> Option<Address> {
        match self {
            Self::MeasureNative { .. } => None,
            Self::MeasureErc20 { token, .. } | Self::Transfer { token, .. } => Some(*token),
            Self::Swap { sides, .. } => Some(sides.pool),
            Self::Deposit { wrapped, .. } | Self::Withdraw { wrapped, .. } => Some(*wrapped),
        }
    }

    /// The call this step makes, once its amount is known.
    pub fn call(&self, amount: U256) -> Result<V2Call, SimulationError> {
        match self {
            Self::MeasureNative { .. } => Err(SimulationError::UnsupportedTransaction(
                "a native balance read is not a contract call".to_string(),
            )),
            Self::MeasureErc20 { account, .. } => Ok(V2Call::BalanceOf { owner: *account }),
            Self::Transfer { to, .. } => Ok(V2Call::Transfer {
                to: *to,
                value: amount,
            }),
            Self::Swap {
                sides,
                token_out,
                to,
                ..
            } => {
                let (amount0_out, amount1_out) = sides.out_words(*token_out, amount)?;
                Ok(V2Call::Swap {
                    amount0_out,
                    amount1_out,
                    to: *to,
                })
            }
            Self::Deposit { .. } => Ok(V2Call::Deposit),
            Self::Withdraw { .. } => Ok(V2Call::Withdraw { wad: amount }),
        }
    }

    /// `msg.value` for this step. Only `deposit` carries native, and it carries
    /// exactly the amount it wraps — the one place §59 applies.
    pub const fn value(&self, amount: U256) -> U256 {
        match self {
            Self::Deposit { .. } => amount,
            _ => U256::ZERO,
        }
    }

    /// The binding this step fills, if any.
    pub const fn binding(&self) -> Option<Binding> {
        match self {
            Self::MeasureErc20 { binding, .. } | Self::MeasureNative { binding, .. } => {
                Some(*binding)
            }
            _ => None,
        }
    }

    /// What to read after this step: the call itself, for an ERC20 read, or the
    /// account state, for a native one.
    pub const fn measurement(&self) -> Option<Measurement> {
        match self {
            Self::MeasureErc20 { token, account, .. } => Some(Measurement::Erc20Balance {
                token: *token,
                account: *account,
            }),
            Self::MeasureNative { account, .. } => {
                Some(Measurement::NativeBalance { account: *account })
            }
            _ => None,
        }
    }

    /// In words, for the evidence file and for §56's requirement that a plan be
    /// explainable without executing it. An amount that has not been measured yet
    /// is named by its binding.
    pub fn describe(&self) -> String {
        let measured = self
            .amount_source()
            .and_then(|source| source.source_text())
            .map(str::to_string);
        let amount = match &measured {
            Some(text) => text.clone(),
            None => match self.amount_source() {
                Some(AmountSource::Absolute(amount)) => amount.to_string(),
                _ => "0".to_string(),
            },
        };
        match self {
            Self::MeasureErc20 {
                token,
                account,
                binding,
            } => format!(
                "balanceOf({account}) on {token}, recorded as {}",
                binding.name()
            ),
            Self::MeasureNative { account, binding } => {
                format!(
                    "native balance of {account}, recorded as {}",
                    binding.name()
                )
            }
            Self::Transfer { token, to, .. } => format!("transfer {amount} of {token} to {to}"),
            Self::Swap {
                sides, token_out, ..
            } => format!(
                "ask {} to pay out {amount} of {token_out} to {}",
                sides.pool, sides.pool
            ),
            Self::Deposit { wrapped, .. } => {
                format!("deposit {amount} native into {wrapped}, msg.value = {amount}")
            }
            Self::Withdraw { wrapped, .. } => {
                format!("withdraw {amount} from {wrapped} into native")
            }
        }
    }
}

/// A step with its amount resolved: what the engine actually executes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedStep {
    pub index: usize,
    pub nonce: u64,
    pub from: Address,
    pub to: Address,
    pub value: U256,
    pub call: V2Call,
    pub amount: U256,
    pub measurement: Option<Measurement>,
    pub binding: Option<Binding>,
}

impl ResolvedStep {
    pub fn selector(&self) -> [u8; 4] {
        self.call.selector()
    }

    pub fn selector_hex(&self) -> String {
        let mut out = String::from("0x");
        for byte in self.call.selector() {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    pub fn calldata(&self) -> Bytes {
        self.call.encode()
    }

    pub fn describe(&self) -> String {
        format!(
            "step {} (nonce {}) from {} to {}: {}, value {}, selector {}",
            self.index,
            self.nonce,
            self.from,
            self.to,
            self.call.signature(),
            self.value,
            self.selector_hex(),
        )
    }
}

/// The sequence a simulation runs.
///
/// The plan is data: it serializes next to its evidence, prints without executing,
/// and carries the route it came from — because §69 asks an audit to compare the
/// analytical and simulated numbers, and a plan that lost the numbers it was built
/// from cannot be checked against them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExecutionPlan {
    pub chain_id: ChainId,
    pub sender: Address,
    pub route: PricedRoute,
    pub slippage: SlippageRecord,
    pub funding: Funding,
    pub settle: Settle,
    pub steps: Vec<PlanStep>,
}

impl ExecutionPlan {
    /// The canonical two-pool round trip.
    ///
    /// `asked_output` is what the second pool is asked to pay — the simulation's
    /// `minimum_amount_out` (§21), which the caller varies to find the highest
    /// executable ask. The first pool is always asked for exactly what M3 priced:
    /// that leg's feasibility is the claim M4 is testing.
    pub fn two_pool_cycle(
        route: &PricedRoute,
        sender: Address,
        sides: [PairSides; 2],
        funding: Funding,
        settle: Settle,
        asked_output: U256,
    ) -> Result<Self, SimulationError> {
        let unsupported = |reason: String| SimulationError::UnsupportedTransaction(reason);
        for (side, leg) in sides.iter().zip(route.legs.iter()) {
            if side.pool != leg.pool.address {
                return Err(unsupported(format!(
                    "preflight sides are for pool {}, but the route's leg is {}",
                    side.pool, leg.pool.address
                )));
            }
            for token in [leg.token_in, leg.token_out] {
                if !side.holds(token.address) {
                    return Err(unsupported(format!(
                        "pool {} reports token0 {} and token1 {}, and does not hold {}",
                        side.pool, side.token0, side.token1, token.address
                    )));
                }
            }
        }
        let input = route.input_token;
        let mid = route.mid_token();
        let pool_a = sides[0].pool;
        let pool_b = sides[1].pool;

        let mut steps = Vec::new();
        if matches!(settle, Settle::UnwrapInputToken) {
            steps.push(PlanStep::MeasureNative {
                account: sender,
                binding: Binding::SenderNativeStart,
            });
        }
        steps.push(PlanStep::MeasureErc20 {
            token: input.address,
            account: sender,
            binding: Binding::SenderInputStart,
        });
        if matches!(funding, Funding::WrapNative) {
            steps.push(PlanStep::Deposit {
                wrapped: input.address,
                wad: route.input_amount,
            });
        }
        steps.push(PlanStep::Transfer {
            token: input.address,
            to: pool_a,
            amount: AmountSource::Absolute(route.input_amount),
        });
        steps.push(PlanStep::Swap {
            sides: sides[0],
            token_out: mid.address,
            amount_out: AmountSource::Absolute(route.analytical_mid_amount),
            to: sender,
        });
        steps.push(PlanStep::MeasureErc20 {
            token: mid.address,
            account: sender,
            binding: Binding::SenderMidReceived,
        });
        steps.push(PlanStep::Transfer {
            token: mid.address,
            to: pool_b,
            amount: AmountSource::Measured(Binding::SenderMidReceived),
        });
        steps.push(PlanStep::Swap {
            sides: sides[1],
            token_out: input.address,
            amount_out: AmountSource::Absolute(asked_output),
            to: sender,
        });
        steps.push(PlanStep::MeasureErc20 {
            token: input.address,
            account: sender,
            binding: Binding::SenderInputEnd,
        });
        if matches!(settle, Settle::UnwrapInputToken) {
            steps.push(PlanStep::Withdraw {
                wrapped: input.address,
                wad: AmountSource::Measured(Binding::SenderInputEnd),
            });
            steps.push(PlanStep::MeasureErc20 {
                token: input.address,
                account: sender,
                binding: Binding::SenderInputAfterUnwrap,
            });
            steps.push(PlanStep::MeasureNative {
                account: sender,
                binding: Binding::SenderNativeEnd,
            });
        }

        Ok(Self {
            chain_id: route.chain_id,
            sender,
            route: route.clone(),
            slippage: route.slippage_record(asked_output),
            funding,
            settle,
            steps,
        })
    }

    pub fn len(&self) -> usize {
        self.steps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Resolve one step against the measurements taken so far.
    ///
    /// Called once per step as the run proceeds, which is what makes the
    /// measured amounts honest: a step cannot read a balance that has not been
    /// produced yet, and the error says so instead of substituting zero.
    pub fn resolve_step(
        &self,
        index: usize,
        measurements: &Measurements,
        nonce: u64,
    ) -> Result<ResolvedStep, SimulationError> {
        let step = self.steps.get(index).ok_or_else(|| {
            SimulationError::UnsupportedTransaction(format!(
                "plan has {} steps, index {index} is not one of them",
                self.steps.len()
            ))
        })?;
        let amount = step
            .amount_source()
            .map(|source| source.resolve(measurements))
            .transpose()?;
        let Some(amount) = amount else {
            return Err(SimulationError::UnsupportedTransaction(format!(
                "step {index} reads a native balance; the engine takes it from the \
                 state it holds, it is not a transaction"
            )));
        };
        let Some(to) = step.target() else {
            return Err(SimulationError::UnsupportedTransaction(format!(
                "step {index} has no target contract"
            )));
        };
        Ok(ResolvedStep {
            index,
            nonce,
            from: self.sender,
            to,
            value: step.value(amount),
            call: step.call(amount)?,
            amount,
            measurement: step.measurement(),
            binding: step.binding(),
        })
    }

    /// The token a plan is expected to end holding: the input token, because the
    /// route closes on it.
    pub fn output_token(&self) -> TokenId {
        self.route.input_token
    }

    /// The listing of the whole sequence, with the amounts that are knowable
    /// before it runs.
    pub fn describe(&self) -> Vec<String> {
        self.steps.iter().map(PlanStep::describe).collect()
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::address;

    use super::*;
    use crate::route::{RouteLeg, SlippagePolicy};
    use evm_core::{BlockNumber, ChainId, Fee, PoolId};

    const CHAIN: ChainId = ChainId(91342);
    const WETH: Address = address!("0x4200000000000000000000000000000000000006");
    const TTAX: Address = address!("0xcffe7472a7a1a6947f56233854ae91a54c862f62");
    const POOL_A: Address = address!("0xf487d533cae6cddd0c7e7bbbac084dd04d876578");
    const POOL_B: Address = address!("0x5bef6275607901dcd58160356660151be0637440");
    const SENDER: Address = address!("0x00000000000000000000000000000000cafe0001");
    const FOREIGN: Address = address!("0x1111111111111111111111111111111111111111");

    const FEE: Fee = Fee {
        numerator: 997,
        denominator: 1000,
    };

    const INPUT: u128 = 714_844_720_992;
    const MID: u128 = 910_000_000_000;
    const OUTPUT: u128 = 744_486_240_802;
    const PROFIT: u128 = 29_641_519_810;

    fn leg(pool: Address, from: Address, to: Address, r_in: u128, r_out: u128) -> RouteLeg {
        RouteLeg {
            pool: PoolId::new(CHAIN, pool),
            token_in: TokenId::new(CHAIN, from),
            token_out: TokenId::new(CHAIN, to),
            reserve_in: U256::from(r_in),
            reserve_out: U256::from(r_out),
            fee: FEE,
        }
    }

    fn route() -> PricedRoute {
        PricedRoute::new(
            CHAIN,
            BlockNumber(37_191_169),
            leg(
                POOL_A,
                WETH,
                TTAX,
                35_099_900_253_008,
                45_655_538_604_883_371_699,
            ),
            leg(
                POOL_B,
                TTAX,
                WETH,
                43_677_608_078_641_141_054,
                36_641_298_079_327,
            ),
            U256::from(INPUT),
            U256::from(MID),
            U256::from(OUTPUT),
            U256::from(PROFIT),
            "opportunity on chain 91342 at block 37191169".to_string(),
        )
        .expect("a valid route")
    }

    /// Pool A and pool B both hold WETH/TTAX, and 0x42 sorts below 0xcf, so the
    /// mid token is `token1` in each. These are the values the pair's own
    /// `token0()` / `token1()` return, which the preflight reads.
    fn sides() -> [PairSides; 2] {
        [
            PairSides {
                pool: POOL_A,
                token0: WETH,
                token1: TTAX,
            },
            PairSides {
                pool: POOL_B,
                token0: WETH,
                token1: TTAX,
            },
        ]
    }

    fn plan() -> ExecutionPlan {
        ExecutionPlan::two_pool_cycle(
            &route(),
            SENDER,
            sides(),
            Funding::Erc20Balance,
            Settle::KeepInputToken,
            U256::from(OUTPUT),
        )
        .expect("a plan")
    }

    #[test]
    fn the_canonical_sequence_is_measure_transfer_swap_measure_transfer_swap_measure() {
        let plan = plan();
        assert_eq!(
            plan.steps,
            vec![
                PlanStep::MeasureErc20 {
                    token: WETH,
                    account: SENDER,
                    binding: Binding::SenderInputStart,
                },
                PlanStep::Transfer {
                    token: WETH,
                    to: POOL_A,
                    amount: AmountSource::Absolute(U256::from(INPUT)),
                },
                PlanStep::Swap {
                    sides: sides()[0],
                    token_out: TTAX,
                    amount_out: AmountSource::Absolute(U256::from(MID)),
                    to: SENDER,
                },
                PlanStep::MeasureErc20 {
                    token: TTAX,
                    account: SENDER,
                    binding: Binding::SenderMidReceived,
                },
                PlanStep::Transfer {
                    token: TTAX,
                    to: POOL_B,
                    amount: AmountSource::Measured(Binding::SenderMidReceived),
                },
                PlanStep::Swap {
                    sides: sides()[1],
                    token_out: WETH,
                    amount_out: AmountSource::Absolute(U256::from(OUTPUT)),
                    to: SENDER,
                },
                PlanStep::MeasureErc20 {
                    token: WETH,
                    account: SENDER,
                    binding: Binding::SenderInputEnd,
                },
            ]
        );
        assert_eq!(plan.len(), 7);
        assert!(!plan.is_empty());
        assert_eq!(plan.output_token(), TokenId::new(CHAIN, WETH));
        assert_eq!(plan.slippage.expected_output, U256::from(OUTPUT));
        assert_eq!(plan.slippage.minimum_output, U256::from(OUTPUT));
        assert_eq!(
            plan.slippage.policy,
            SlippagePolicy::Exact,
            "asking for exactly the analytical output is the zero-slippage form"
        );
    }

    /// The second leg's transfer moves what the first leg *delivered*, which is
    /// the point of measuring instead of assuming: a taxed mid token makes
    /// `SenderMidReceived` smaller than `analytical_mid_amount`, and the plan
    /// spends the smaller number without knowing why.
    #[test]
    fn a_measured_amount_resolves_from_the_measurement_it_names() {
        let plan = plan();
        let err = plan
            .resolve_step(4, &Measurements::default(), 4)
            .expect_err("nothing measured yet");
        assert!(matches!(err, SimulationError::UnsupportedTransaction(_)));
        assert!(err.to_string().contains("sender_mid_received"), "{err}");

        let mut measurements = Measurements::default();
        let delivered = U256::from(MID - 1_000_000_000u128);
        measurements.set(Binding::SenderMidReceived, delivered);
        let step = plan.resolve_step(4, &measurements, 4).expect("resolves");
        assert_eq!(step.to, TTAX);
        assert_eq!(step.from, SENDER);
        assert_eq!(step.nonce, 4);
        assert_eq!(step.value, U256::ZERO);
        assert_eq!(
            step.call,
            V2Call::Transfer {
                to: POOL_B,
                value: delivered
            }
        );
        assert_eq!(step.amount, delivered);
        assert_eq!(step.binding, None);
    }

    /// `msg.value` is never a default here (§59): only a `deposit` carries native,
    /// and it carries exactly the amount it wraps.
    #[test]
    fn only_deposit_carries_value_and_it_carries_exactly_its_wad() {
        let native = ExecutionPlan::two_pool_cycle(
            &route(),
            SENDER,
            sides(),
            Funding::WrapNative,
            Settle::UnwrapInputToken,
            U256::from(OUTPUT),
        )
        .expect("a native plan");
        assert_eq!(
            native.steps[1],
            PlanStep::MeasureErc20 {
                token: WETH,
                account: SENDER,
                binding: Binding::SenderInputStart,
            },
            "the balance the override funded is read before anything is wrapped"
        );
        assert_eq!(
            native.steps[2],
            PlanStep::Deposit {
                wrapped: WETH,
                wad: U256::from(INPUT),
            }
        );
        let step = native
            .resolve_step(2, &Measurements::default(), 2)
            .expect("deposit resolves without measurements");
        assert_eq!(step.to, WETH);
        assert_eq!(step.value, U256::from(INPUT));
        assert_eq!(step.selector_hex(), "0xd0e30db0");

        let with_value: Vec<usize> = native
            .steps
            .iter()
            .enumerate()
            .filter(|(_, step)| step.value(U256::from(1u8)) != U256::ZERO)
            .map(|(index, _)| index)
            .collect();
        assert_eq!(with_value, vec![2], "exactly one step may carry native");
    }

    /// Unwrapping brackets the sequence with the two native reads a denomination
    /// proof needs, plus the withdraw and the balance read that must come out zero.
    #[test]
    fn unwrapping_appends_the_withdraw_and_its_measurements() {
        let unwrapping = ExecutionPlan::two_pool_cycle(
            &route(),
            SENDER,
            sides(),
            Funding::Erc20Balance,
            Settle::UnwrapInputToken,
            U256::from(OUTPUT),
        )
        .expect("a plan");
        assert_eq!(unwrapping.len(), 11);
        assert_eq!(
            unwrapping.steps[0],
            PlanStep::MeasureNative {
                account: SENDER,
                binding: Binding::SenderNativeStart,
            }
        );
        assert_eq!(
            unwrapping.steps[8],
            PlanStep::Withdraw {
                wrapped: WETH,
                wad: AmountSource::Measured(Binding::SenderInputEnd),
            }
        );
        assert_eq!(
            unwrapping.steps[9],
            PlanStep::MeasureErc20 {
                token: WETH,
                account: SENDER,
                binding: Binding::SenderInputAfterUnwrap,
            }
        );
        assert_eq!(
            unwrapping.steps[10],
            PlanStep::MeasureNative {
                account: SENDER,
                binding: Binding::SenderNativeEnd,
            }
        );
        assert_eq!(
            unwrapping.steps[1].measurement(),
            Some(Measurement::Erc20Balance {
                token: WETH,
                account: SENDER
            })
        );
    }

    /// The sides come from the pair's own answers, so a preflight that read a
    /// different pool or a pool that does not hold the leg's token is a hard
    /// refusal — not calldata for the wrong asset.
    #[test]
    fn sides_that_do_not_match_the_route_are_refused() {
        let mut wrong_pool = sides();
        wrong_pool[0].pool = POOL_B;
        let err = ExecutionPlan::two_pool_cycle(
            &route(),
            SENDER,
            wrong_pool,
            Funding::Erc20Balance,
            Settle::KeepInputToken,
            U256::from(1u8),
        )
        .expect_err("sides for the wrong pool");
        assert!(err.to_string().contains("but the route's leg is"), "{err}");

        let foreign_token = [
            PairSides {
                pool: POOL_A,
                token0: WETH,
                token1: FOREIGN,
            },
            sides()[1],
        ];
        let err = ExecutionPlan::two_pool_cycle(
            &route(),
            SENDER,
            foreign_token,
            Funding::Erc20Balance,
            Settle::KeepInputToken,
            U256::from(1u8),
        )
        .expect_err("pool does not hold the leg's token");
        assert!(err.to_string().contains("does not hold"), "{err}");
    }

    /// Which output word gets filled is the pair's decision, and getting it
    /// backwards would ask the pool for the wrong asset entirely.
    #[test]
    fn the_pair_decides_which_output_word_an_ask_fills() {
        let pair = sides()[0];
        assert!(pair.holds(TTAX) && pair.holds(WETH) && !pair.holds(FOREIGN));
        assert_eq!(
            pair.out_words(TTAX, U256::from(5u8))
                .expect("ttax is token1"),
            (U256::ZERO, U256::from(5u8))
        );
        assert_eq!(
            pair.out_words(WETH, U256::from(5u8))
                .expect("weth is token0"),
            (U256::from(5u8), U256::ZERO)
        );
        assert!(pair.out_words(FOREIGN, U256::from(5u8)).is_err());
    }

    /// The calldata the engine will execute, with the selectors this chain's own
    /// transactions showed: `transfer` 0xa9059cbb and `swap` 0x022c0d9f, the swap
    /// call 164 bytes long exactly like the recorded real ones.
    #[test]
    fn resolved_steps_carry_the_callable_surface() {
        let plan = plan();
        let mut measurements = Measurements::default();
        measurements.set(Binding::SenderMidReceived, U256::from(MID));

        let transfer = plan.resolve_step(1, &measurements, 1).expect("transfer");
        assert_eq!(transfer.selector_hex(), "0xa9059cbb");
        assert_eq!(transfer.calldata().len(), 4 + 2 * 32);
        assert_eq!(transfer.to, WETH);

        let swap = plan.resolve_step(2, &measurements, 2).expect("swap");
        assert_eq!(swap.selector_hex(), "0x022c0d9f");
        assert_eq!(swap.selector(), [0x02, 0x2c, 0x0d, 0x9f]);
        assert_eq!(swap.calldata().len(), 4 + 5 * 32);
        assert_eq!(swap.to, POOL_A);
        assert_eq!(swap.value, U256::ZERO);
        assert_eq!(
            swap.call,
            V2Call::Swap {
                amount0_out: U256::ZERO,
                amount1_out: U256::from(MID),
                to: SENDER,
            }
        );
        assert!(swap
            .describe()
            .contains("swap(uint256,uint256,address,bytes)"));

        let probe = plan.resolve_step(0, &measurements, 0).expect("balanceOf");
        assert_eq!(probe.selector_hex(), "0x70a08231");
        assert_eq!(probe.binding, Some(Binding::SenderInputStart));
        assert_eq!(probe.calldata().len(), 4 + 32);
    }

    /// A native balance read is not a transaction, so it must not silently become
    /// one aimed at the zero address.
    #[test]
    fn a_native_measurement_refuses_to_be_executed_as_a_call() {
        let unwrapping = ExecutionPlan::two_pool_cycle(
            &route(),
            SENDER,
            sides(),
            Funding::Erc20Balance,
            Settle::UnwrapInputToken,
            U256::from(OUTPUT),
        )
        .expect("a plan");
        let err = unwrapping
            .resolve_step(0, &Measurements::default(), 0)
            .expect_err("not a call");
        assert!(matches!(err, SimulationError::UnsupportedTransaction(_)));
        assert!(err.to_string().contains("native balance"), "{err}");
        assert_eq!(
            unwrapping.steps[0].measurement(),
            Some(Measurement::NativeBalance { account: SENDER })
        );
    }

    /// An index past the end of the sequence is a pipeline bug, and naming the
    /// plan's length is what makes it findable.
    #[test]
    fn resolving_past_the_end_names_the_plan() {
        let plan = plan();
        let err = plan
            .resolve_step(plan.len(), &Measurements::default(), 0)
            .expect_err("no such step");
        assert!(err.to_string().contains("plan has 7 steps"), "{err}");
    }

    /// The plan prints and serializes without executing: a measured amount is
    /// described by what it will be read from, not by a number that does not
    /// exist yet.
    #[test]
    fn a_plan_prints_and_serializes_without_executing() {
        let plan = plan();
        let listing = plan.describe();
        assert_eq!(listing.len(), 7);
        assert!(listing[1].contains(&INPUT.to_string()), "{:?}", listing[1]);
        assert!(
            listing[4].contains("the measured mid delivery"),
            "{:?}",
            listing[4]
        );
        assert!(
            listing[0].contains("sender_input_start"),
            "{:?}",
            listing[0]
        );

        let text = serde_json::to_string(&plan).expect("serializes");
        let value: serde_json::Value = serde_json::from_str(&text).expect("is json");
        assert_eq!(value["chain_id"], serde_json::Value::from(91342u64));
        assert_eq!(value["steps"].as_array().map(Vec::len), Some(7usize));
        assert!(text.contains("SenderMidReceived"), "{text}");
    }

    /// The bindings a run produced are part of the evidence, in a stable order.
    #[test]
    fn measurements_report_themselves_in_binding_order() {
        let mut measurements = Measurements::default();
        measurements.set(Binding::SenderInputEnd, U256::from(OUTPUT));
        measurements.set(Binding::SenderInputStart, U256::from(INPUT));
        measurements.set(Binding::SenderMidReceived, U256::from(MID));
        let recorded = measurements.recorded();
        let names: Vec<&str> = recorded.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            vec![
                "sender_input_start",
                "sender_mid_received",
                "sender_input_end"
            ]
        );
        assert_eq!(recorded[0].1, U256::from(INPUT));
    }
}
