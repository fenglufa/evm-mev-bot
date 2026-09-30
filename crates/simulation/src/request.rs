//! The request: what to simulate, on which block, paid at which price, and what
//! the engine is allowed to change about the state before it starts.
//!
//! §8 lists the fields a request has to express — chain, block, opportunity,
//! transaction, state source, and then `from`, `to`, `value`, `calldata`,
//! `gas_limit`, block context. This type carries all of them except three, and the
//! reason for those three is worth stating plainly because it is the difference
//! between a request and a result:
//!
//! ```text
//! to          which pool, in which orientation
//! value       msg.value of the step
//! calldata    the encoded call
//! ```
//!
//! They are properties of *each step* of the sequence, and a step's orientation
//! comes from the pair's own `token0()` / `token1()` answers at the pinned block.
//! A request that carried calldata would be asserting an encoding the engine had
//! not yet derived from state, and if the two disagreed the run would either revert
//! confusingly or execute the wrong trade. So the request carries the intent
//! ([`TransactionSpec`]) and the executed calldata appears in
//! [`SimulationResult::steps`][crate::result::SimulationResult], where it is a fact
//! about a run rather than a prediction.
//!
//! The block is a [`BlockContext`], not a height, and never `latest` (§49): the
//! header's hash is the pin the state has to match, its base fee is what the gas
//! model resolves against (§29), and its timestamp and gas limit are what the EVM
//! executes under. §20's check — did the state you loaded belong to the block you
//! asked for — is only possible when the request says which block it means.

use alloy_primitives::{keccak256, Address, B256, U256};
use serde::{Deserialize, Serialize};

use evm_chain::BlockContext;
use evm_core::{ChainId, TokenId};

use crate::error::SimulationError;
use crate::gas::GasPricing;
use crate::plan::{Funding, Settle};
use crate::route::PricedRoute;
use crate::state::{BlockPin, StateOverride};

/// The deterministic test sender (§58).
///
/// Derived from a fixed label rather than generated from a key, so two machines
/// pick the same address and the run stays reproducible (§37). There is no private
/// key for it because it was never a key: the bytes below are a hash of a sentence,
/// and nothing in this repository is capable of signing with it. Its only role is to
/// own enough balance and nonce to spend gas inside a sandbox that is discarded the
/// moment the run reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SimulationSender {
    pub address: Address,
    pub label: &'static str,
}

/// The label the test sender is derived from, kept so the address can be re-derived
/// by an auditor instead of trusted.
pub const DEFAULT_SENDER_LABEL: &str = "evm-mev-bot v0.1 M4 simulation sender";

impl SimulationSender {
    /// The address a label derives to: the last twenty bytes of `keccak256(label)`.
    pub fn address_of(label: &str) -> Address {
        let hash: B256 = keccak256(label.as_bytes());
        Address::from_slice(&hash.as_slice()[12..])
    }

    /// The sender M4 runs as, unless a request says otherwise.
    pub fn default_test_sender() -> Self {
        Self {
            address: Self::address_of(DEFAULT_SENDER_LABEL),
            label: DEFAULT_SENDER_LABEL,
        }
    }
}

/// The EVM ruleset a run executes under, stated rather than defaulted.
///
/// This belongs to the request because nothing in this repository can derive it.
/// §29's rule for gas pricing — declare it with its provenance, never guess — is
/// applied here for the same reason: the hardfork is a fact about the chain at the
/// pinned height, and the engine has no chain profile to ask. The choice is not
/// cosmetic either way. A ruleset that is too old charges different gas than the
/// chain did; one that is too new refuses a transaction the chain would have
/// accepted, and from outside the EVM both mistakes look like market results.
///
/// The variants cover the range this project can be asked to replay. Adding one is
/// a matter of naming it here — the mapping to the engine's own vocabulary lives
/// with the engine, since that is the only place allowed to name a REVM type (§7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvmRules {
    Shanghai,
    Cancun,
    /// The ruleset M4's real run uses: this chain is past Shanghai and before the
    /// per-transaction gas cap that the next variant introduces.
    #[default]
    Prague,
    /// From this ruleset on, EIP-7825 caps a transaction's gas limit, so a plan with
    /// a large per-step allowance is refused before execution rather than failing
    /// inside the EVM with a message about the cap.
    Osaka,
}

impl EvmRules {
    /// `2^24` = 16 777 216: the per-transaction gas limit cap EIP-7825 introduces.
    ///
    /// The number is REVM's own (`revm_primitives::eip7825::TX_GAS_LIMIT_CAP`,
    /// 43.0.3), and the check around it is `gas_limit > cap`, so a transaction at
    /// exactly the cap is valid. Getting this boundary wrong by one would refuse
    /// plans the chain accepts.
    pub const EIP7825_GAS_LIMIT_CAP: u64 = 16_777_216;

    pub const fn model(self) -> &'static str {
        match self {
            Self::Shanghai => "shanghai",
            Self::Cancun => "cancun",
            Self::Prague => "prague",
            Self::Osaka => "osaka",
        }
    }

    /// The cap this ruleset puts on one transaction's gas limit, when it puts one on.
    pub const fn gas_limit_cap(self) -> Option<u64> {
        match self {
            Self::Osaka => Some(Self::EIP7825_GAS_LIMIT_CAP),
            Self::Shanghai | Self::Cancun | Self::Prague => None,
        }
    }
}

/// What the run is asked to do with tokens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TransactionSpec {
    pub sender: SimulationSender,
    /// Each step is a transaction, so the limit is per step. §28: this is the
    /// allowance, never the measurement.
    pub gas_limit_per_step: u64,
    /// How many steps the plan is expected to have — used for the funding budget
    /// before the plan exists, and checked against it afterwards.
    pub steps_planned: usize,
    /// The exact-out ask of the second pool: this simulation's
    /// `minimum_amount_out` (§21). Varying it is how the highest executable ask is
    /// found; the plan records the relation to M3's output as a
    /// [`SlippageRecord`][crate::route::SlippageRecord].
    pub asked_output: U256,
    pub funding: Funding,
    pub settle: Settle,
    /// Which EVM rules the sequence executes under. Declared, not derived — see
    /// [`EvmRules`] for why nothing here can work it out.
    pub rules: EvmRules,
}

impl TransactionSpec {
    /// The two-pool round trip's plan length: a measurement before each of the two
    /// legs, a transfer per leg, a measurement after the input balance, plus the
    /// optional wrap and the optional unwrap sequence.
    pub const fn canonical_step_count(funding: Funding, settle: Settle) -> usize {
        let core = 7;
        core + matches!(funding, Funding::WrapNative) as usize
            + match settle {
                Settle::KeepInputToken => 0,
                // native before, withdraw, balance after, native after
                Settle::UnwrapInputToken => 4,
            }
    }

    pub fn canonical(funding: Funding, settle: Settle, asked_output: U256) -> Self {
        Self {
            sender: SimulationSender::default_test_sender(),
            gas_limit_per_step: Self::DEFAULT_GAS_LIMIT_PER_STEP,
            steps_planned: Self::canonical_step_count(funding, settle),
            asked_output,
            funding,
            settle,
            rules: EvmRules::default(),
        }
    }

    /// The per-step allowance a plan is given before anyone has measured anything.
    ///
    /// A block gas limit, not a guess at this route's cost: the EVM's own refusal is
    /// what tells the run whether a step needed more, and §28 forbids reporting an
    /// allowance as a measurement. Kept as a constant with a name so a fixture that
    /// needs less says so explicitly.
    pub const DEFAULT_GAS_LIMIT_PER_STEP: u64 = 30_000_000;

    /// The gas the whole plan is allowed to consume, in **gas units**: the per-step
    /// allowance times the number of steps. Not a cost in wei — §30 makes that
    /// `gas_used × effective_gas_price`, and no step has been executed yet.
    pub fn gas_units(&self) -> U256 {
        U256::from(self.gas_limit_per_step) * U256::from(self.steps_planned as u64)
    }

    /// The native balance the test sender has to hold for this plan to be possible
    /// at all (§58), at the price the request declares.
    ///
    /// Two terms, both ceilings rather than measurements. The first is the whole
    /// plan's gas allowance valued at `max_fee_per_gas`, because a transaction is
    /// checked against the sender's balance *before* it runs and at its limit, not
    /// at what it ends up spending. The second is the native a `WrapNative` funding
    /// step sends as `msg.value`, since that deposit is a value transfer the fee
    /// check adds to the gas. A plan funded from an existing token balance pays only
    /// gas.
    ///
    /// Saturating on purpose: a figure this absurd would be refused by the EVM's own
    /// balance check with both numbers in the message, which is a better report than
    /// one this function made up.
    pub fn endowment_wei(&self, max_fee_per_gas: u128, wrapped_native: U256) -> U256 {
        self.gas_units()
            .saturating_mul(U256::from(max_fee_per_gas))
            .saturating_add(wrapped_native)
    }
}

/// Which state the run reads, and what the engine is allowed to change first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StateSpec {
    /// The source this request was built against, e.g. `ethRpc` or
    /// `dump:fixtures/m4-tax.json`. It has to match what the provider reports (§66
    /// records the state source; §20 makes a disagreement a refusal).
    pub source: String,
    /// Setup state, each one labelled as setup. Never pool state (§18).
    pub overrides: Vec<StateOverride>,
}

impl StateSpec {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            overrides: Vec::new(),
        }
    }

    pub fn with_override(mut self, override_state: StateOverride) -> Self {
        self.overrides.push(override_state);
        self
    }
}

/// One simulation's worth of intent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SimulationRequest {
    pub chain_id: ChainId,
    /// The pinned block, with its header hash.
    pub block: BlockContext,
    /// M3's finding, in this crate's form (§8's "opportunity").
    pub route: PricedRoute,
    pub transaction: TransactionSpec,
    pub state: StateSpec,
    /// How gas is priced, declared with provenance (§29).
    pub pricing: GasPricing,
}

impl SimulationRequest {
    pub fn new(
        block: BlockContext,
        route: PricedRoute,
        transaction: TransactionSpec,
        state: StateSpec,
        pricing: GasPricing,
    ) -> Result<Self, SimulationError> {
        if block.chain_id != route.chain_id {
            return Err(SimulationError::ChainMismatch {
                expected: route.chain_id,
                found: block.chain_id,
            });
        }
        if block.number != route.block_number {
            return Err(SimulationError::StateMismatch {
                priced: route.block_number,
                pinned: BlockPin::new(block.number, block.hash),
                reason: "the request pins a different block than the one the opportunity was \
                         priced on"
                    .to_string(),
            });
        }
        if transaction.asked_output.is_zero() {
            return Err(SimulationError::UnsupportedTransaction(
                "an ask of zero output is not a trade to simulate; it is a way of donating the \
                 input"
                    .to_string(),
            ));
        }
        if transaction.steps_planned == 0 {
            return Err(SimulationError::UnsupportedTransaction(
                "a request with no planned steps has nothing to execute".to_string(),
            ));
        }
        if let Some(cap) = transaction.rules.gas_limit_cap() {
            if transaction.gas_limit_per_step > cap {
                return Err(SimulationError::InvalidTransaction(format!(
                    "{} caps one transaction's gas limit at {cap}, and this plan allows {} per \
                     step; either name an older ruleset or lower the allowance — refusing here \
                     is what keeps an EVM rule from arriving later dressed up as a market result",
                    transaction.rules.model(),
                    transaction.gas_limit_per_step
                )));
            }
        }
        Ok(Self {
            chain_id: block.chain_id,
            block,
            route,
            transaction,
            state,
            pricing,
        })
    }

    /// The pin this request is asking for, derived from the header it carries.
    pub fn pin(&self) -> BlockPin {
        BlockPin::new(self.block.number, self.block.hash)
    }

    /// The address the sequence runs as, and the only account setup may rewrite.
    pub fn sender_address(&self) -> Address {
        self.transaction.sender.address
    }

    /// The most a unit of gas could cost this run, in wei, as the request declares
    /// it (§29) against the header it pins.
    ///
    /// `Ok(0)` is `GasPricing::Unresolved` speaking honestly: the chain's price model
    /// is not established, so the run executes with no fee declared and
    /// [`GasCharge::Unpriced`][crate::gas::GasCharge::Unpriced] is what comes back —
    /// §31's `NotComputable`, not a zero cost.
    pub fn max_fee_per_gas(&self) -> Result<u128, SimulationError> {
        self.pricing
            .max_fee_per_gas(&self.block)
            .map(|price| price.unwrap_or_default())
    }

    /// The native this plan spends as *value* rather than as gas: what a
    /// `WrapNative` funding step sends to the wrapped contract as `msg.value`, and
    /// zero when the plan starts from a token balance the sender already holds.
    pub fn native_to_wrap(&self) -> U256 {
        match self.transaction.funding {
            Funding::WrapNative => self.route.input_amount,
            Funding::Erc20Balance => U256::ZERO,
        }
    }

    /// Everything the engine has to have decided before it executes: pins, chains,
    /// overrides, and the sender's §58 scaffolding.
    ///
    /// Returns the setup it is going to apply rather than applying it in secret, so
    /// a reader of the request can see the one balance the run manufactured
    /// (§57: approval and balance setup is stated, not implied).
    pub fn preflight(&self) -> Result<Vec<StateOverride>, SimulationError> {
        self.check_overrides()?;
        let mut setup = self.state.overrides.clone();
        setup.push(self.sender_setup_override()?);
        Ok(setup)
    }

    /// The sender's own setup entry: [`TransactionSpec::endowment_wei`] at the price
    /// this request declares, with the numbers in the reason so the override is
    /// quotable without re-deriving them.
    pub fn sender_setup_override(&self) -> Result<StateOverride, SimulationError> {
        let price = self.max_fee_per_gas()?;
        let wrapped = self.native_to_wrap();
        let spec = &self.transaction;
        Ok(StateOverride::balance(
            self.sender_address(),
            spec.endowment_wei(price, wrapped),
            format!(
                "simulation setup: §58's test sender funded for {} steps at {} gas each, at a \
                 declared fee ceiling of {price} wei per gas, plus {} native wrapped as value — \
                 which is this run's scaffolding and not a fact about this chain",
                spec.steps_planned, spec.gas_limit_per_step, wrapped,
            ),
        ))
    }

    pub fn input_token(&self) -> TokenId {
        self.route.input_token
    }

    /// §20's first check, before any state is loaded: does the source claim to serve
    /// the block this request means? A provider that reports a different height or a
    /// different hash has not been asked for this state, and running on it anyway
    /// would produce a number that describes some other market.
    pub fn check_pin(&self, provider_pin: BlockPin) -> Result<(), SimulationError> {
        let expected = self.pin();
        if provider_pin == expected {
            return Ok(());
        }
        Err(SimulationError::StateMismatch {
            priced: self.route.block_number,
            pinned: provider_pin,
            reason: if provider_pin.number != expected.number {
                format!(
                    "the state source is pinned to height {}, the request asks for {}",
                    provider_pin.number.0, expected.number.0
                )
            } else {
                format!(
                    "both pins are height {}, but the source reports {} and the request carries {}",
                    expected.number.0, provider_pin.hash, expected.hash
                )
            },
        })
    }

    /// §20's second check, and §57 read literally: an override may touch the test
    /// sender's own account and nothing else.
    ///
    /// Two facts fix the boundary. §18 forbids replacing the market being measured,
    /// and §57 allows setup overrides on "测试执行者的 balance/allowance/nonce".
    /// §18 cannot be enforced by address alone, because the ERC20 balance a run
    /// needs and the reserves it must not touch live in the *same* contract's
    /// storage — so the rule is written on ownership instead of location: anything
    /// at an address this simulation does not run as is refused. The consequence is
    /// the one that matters for the real run: token balances are never manufactured,
    /// so a sender that wants WETH has to be an account the pinned state already
    /// says holds WETH. Native gas money, by contrast, is a field on the sender's
    /// own account and is overridden freely.
    pub fn check_overrides(&self) -> Result<(), SimulationError> {
        let sender = self.transaction.sender.address;
        let pools = self.route.pools();
        for entry in &self.state.overrides {
            let touched = entry.address;
            if touched == sender {
                continue;
            }
            let what = if pools.iter().any(|pool| pool.address == touched) {
                "a pool this route trades through"
            } else if touched == self.route.input_token.address
                || touched == self.route.mid_token().address
            {
                "a token this route trades"
            } else {
                "an account this simulation does not run as"
            };
            return Err(SimulationError::UnsupportedTransaction(format!(
                "override at {touched} is {what}; §57 confines setup overrides to the test \
                 sender ({sender}), and §18 forbids replacing the state being measured",
            )));
        }
        Ok(())
    }

    pub fn describe(&self) -> String {
        format!(
            "simulate {} on chain {} at block {} ({}) against {}; ask {}, {} steps at {} gas \
             under {}, fee ceiling {}",
            self.route.input_token.address,
            self.chain_id.0,
            self.block.number.0,
            self.block.hash,
            self.state.source,
            self.transaction.asked_output,
            self.transaction.steps_planned,
            self.transaction.gas_limit_per_step,
            self.transaction.rules.model(),
            self.pricing.model(),
        )
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, b256};

    use super::*;
    use crate::route::RouteLeg;
    use evm_core::{BlockNumber, Fee, PoolId};

    const CHAIN: ChainId = ChainId(91342);
    const WETH: Address = address!("0x4200000000000000000000000000000000000006");
    const TTAX: Address = address!("0xcffe7472a7a1a6947f56233854ae91a54c862f62");
    const POOL_A: Address = address!("0xf487d533cae6cddd0c7e7bbbac084dd04d876578");
    const POOL_B: Address = address!("0x5bef6275607901dcd58160356660151be0637440");
    const STRANGER: Address = address!("0x9999999999999999999999999999999999999999");

    const HEADER_HASH: B256 =
        b256!("0x3719116937191169371911693719116937191169371911693719116937191169");

    fn block(chain_id: ChainId, number: u64) -> BlockContext {
        BlockContext {
            chain_id,
            number: BlockNumber(number),
            hash: HEADER_HASH,
            timestamp: 1_758_000_000,
            gas_limit: 60_000_000,
            base_fee_per_gas: Some(1_000_000),
            excess_blob_gas: None,
            beneficiary: address!("0x0000000000000000000000000000000000000000"),
            prevrandao: None,
        }
    }

    fn leg(pool: Address, from: Address, to: Address) -> RouteLeg {
        RouteLeg {
            pool: PoolId::new(CHAIN, pool),
            token_in: TokenId::new(CHAIN, from),
            token_out: TokenId::new(CHAIN, to),
            reserve_in: U256::from(35_099_900_253_008u128),
            reserve_out: U256::from(45_655_538_604_883_371_699_u128),
            fee: Fee {
                numerator: 997,
                denominator: 1000,
            },
        }
    }

    fn route() -> PricedRoute {
        PricedRoute::new(
            CHAIN,
            BlockNumber(37_191_169),
            leg(POOL_A, WETH, TTAX),
            leg(POOL_B, TTAX, WETH),
            U256::from(714_844_720_992u128),
            U256::from(910_000_000_000u128),
            U256::from(744_486_240_802u128),
            U256::from(29_641_519_810u128),
            "opportunity on chain 91342 at block 37191169".to_string(),
        )
        .expect("a valid route")
    }

    fn pricing() -> GasPricing {
        GasPricing::Eip1559 {
            priority_fee_per_gas: 1_000,
            provenance: "declared by a test".to_string(),
        }
    }

    fn request() -> SimulationRequest {
        SimulationRequest::new(
            block(CHAIN, 37_191_169),
            route(),
            TransactionSpec::canonical(Funding::Erc20Balance, Settle::KeepInputToken, U256::ONE),
            StateSpec::new("dump:fixtures/m4-test.json"),
            pricing(),
        )
        .expect("a valid request")
    }

    /// §58: the sender is a label, not a key. Both the label and the address it
    /// derives to are asserted, because a reader who doubts the address can re-run
    /// the derivation and a reader who doubts the sentence can read it.
    #[test]
    fn the_test_sender_is_derived_from_a_label_not_a_key() {
        let sender = SimulationSender::default_test_sender();
        assert_eq!(
            sender.label, "evm-mev-bot v0.1 M4 simulation sender",
            "the label is part of the record, so it can be cited"
        );
        assert_eq!(
            sender.address,
            SimulationSender::address_of(DEFAULT_SENDER_LABEL)
        );
        assert_ne!(sender.address, Address::ZERO);
        // A derivation, not a claim about a real account: a different label has to
        // give a different address.
        assert_ne!(
            sender.address,
            SimulationSender::address_of("evm-mev-bot v0.1 M5 simulation sender")
        );
    }

    /// The funding override is generated for the sender only, and says out loud that
    /// it is setup (§18, §58).
    #[test]
    fn the_sender_endowment_is_gas_at_the_declared_price_plus_any_value() {
        let request = request();
        // The test pricing is a 1_000 tip over this block's 1_000_000 base fee.
        assert_eq!(
            request.max_fee_per_gas().expect("a declared price"),
            1_001_000
        );
        let setup = request
            .sender_setup_override()
            .expect("setup for the sender");
        assert_eq!(setup.address, request.sender_address());
        assert_eq!(
            setup.balance,
            Some(U256::from(30_000_000u64 * 7) * U256::from(1_001_000u128)),
            "the plan's whole allowance valued at the declared ceiling"
        );
        assert_eq!(
            request.native_to_wrap(),
            U256::ZERO,
            "funded from a balance"
        );
        assert!(setup.reason.contains("not a fact"), "{:?}", setup.reason);
        assert!(setup.reason.contains("§58"), "{}", setup.reason);

        // A wrap-funded plan also sends the input as msg.value, so the fee check —
        // which prices value and gas together — needs room for both.
        let wrapped = SimulationRequest {
            transaction: TransactionSpec::canonical(
                Funding::WrapNative,
                Settle::KeepInputToken,
                U256::ONE,
            ),
            ..request.clone()
        };
        assert_eq!(
            wrapped.native_to_wrap(),
            wrapped.route.input_amount,
            "the deposit is the input amount"
        );
        assert_eq!(
            wrapped.sender_setup_override().expect("setup").balance,
            Some(
                U256::from(30_000_000u64 * 8) * U256::from(1_001_000u128)
                    + wrapped.route.input_amount
            ),
            "one more step of gas, plus the native it wraps"
        );
    }

    /// An unresolved price funds nothing, because there is no price to fund against:
    /// §29 forbids inventing one, so the run has no fee to pay and §31's
    /// `NotComputable` is what the report says about its cost.
    #[test]
    fn an_unresolved_price_endows_nothing() {
        let unpriced = SimulationRequest {
            pricing: GasPricing::Unresolved {
                reason: "no gas model has been established for this chain".to_string(),
            },
            ..request()
        };
        assert_eq!(unpriced.max_fee_per_gas().expect("an honest zero"), 0);
        assert_eq!(
            unpriced
                .sender_setup_override()
                .expect("setup")
                .balance
                .expect("a balance"),
            U256::ZERO
        );
    }

    /// A ruleset that caps one transaction's gas is refused when the plan's per-step
    /// allowance would break it — before execution, not as an EVM error that a
    /// reader would mistake for the market's answer.
    #[test]
    fn a_gas_cap_the_ruleset_would_refuse_is_refused_first() {
        assert_eq!(EvmRules::Osaka.gas_limit_cap(), Some(16_777_216));
        assert_eq!(EvmRules::Prague.gas_limit_cap(), None);
        assert_eq!(
            EvmRules::Osaka.model(),
            "osaka",
            "the name a report cites, not a REVM type"
        );
        let capped = SimulationRequest::new(
            block(CHAIN, 37_191_169),
            route(),
            TransactionSpec {
                rules: EvmRules::Osaka,
                ..TransactionSpec::canonical(
                    Funding::Erc20Balance,
                    Settle::KeepInputToken,
                    U256::ONE,
                )
            },
            StateSpec::new("dump"),
            pricing(),
        )
        .expect_err("30_000_000 is over EIP-7825's cap");
        assert!(
            matches!(capped, SimulationError::InvalidTransaction(_)),
            "{capped}"
        );
        assert!(capped.to_string().contains("16777216"), "{capped}");
        assert!(capped.to_string().contains("osaka"), "{capped}");

        // At the cap is not over it: REVM's own check is `gas_limit > cap`, so a plan
        // that sits exactly on the boundary has to be allowed through.
        let exactly_at_cap = SimulationRequest::new(
            block(CHAIN, 37_191_169),
            route(),
            TransactionSpec {
                gas_limit_per_step: EvmRules::EIP7825_GAS_LIMIT_CAP,
                rules: EvmRules::Osaka,
                ..TransactionSpec::canonical(
                    Funding::Erc20Balance,
                    Settle::KeepInputToken,
                    U256::ONE,
                )
            },
            StateSpec::new("dump"),
            pricing(),
        )
        .expect("exactly at the cap is under it");
        assert_eq!(
            exactly_at_cap.transaction.gas_limit_per_step,
            EvmRules::EIP7825_GAS_LIMIT_CAP
        );

        // Fitting under the cap is allowed, and says which ruleset it will run under.
        let fitted = SimulationRequest::new(
            block(CHAIN, 37_191_169),
            route(),
            TransactionSpec {
                gas_limit_per_step: 15_000_000,
                rules: EvmRules::Osaka,
                ..TransactionSpec::canonical(
                    Funding::Erc20Balance,
                    Settle::KeepInputToken,
                    U256::ONE,
                )
            },
            StateSpec::new("dump"),
            pricing(),
        )
        .expect("under the cap");
        assert!(
            fitted.describe().contains("under osaka"),
            "{}",
            fitted.describe()
        );
    }

    /// The canonical step count has to agree with the plan the builder produces,
    /// otherwise the funding budget and the gas ceiling are computed for a sequence
    /// that is not the one that runs.
    #[test]
    fn the_planned_step_count_matches_the_plan_that_gets_built() {
        use crate::plan::{ExecutionPlan, PairSides};

        let sides = [
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
        ];
        for (funding, settle) in [
            (Funding::Erc20Balance, Settle::KeepInputToken),
            (Funding::Erc20Balance, Settle::UnwrapInputToken),
            (Funding::WrapNative, Settle::KeepInputToken),
            (Funding::WrapNative, Settle::UnwrapInputToken),
        ] {
            let expected = TransactionSpec::canonical_step_count(funding, settle);
            let plan = ExecutionPlan::two_pool_cycle(
                &route(),
                SimulationSender::default_test_sender().address,
                sides,
                funding,
                settle,
                U256::from(1u8),
            )
            .expect("a plan");
            assert_eq!(plan.len(), expected, "{funding:?} / {settle:?}");
            assert_eq!(
                TransactionSpec::canonical(funding, settle, U256::ONE).steps_planned,
                expected
            );
        }
    }

    /// A request whose header names another chain, or another height than the one the
    /// opportunity was priced on, is refused before anything is loaded (§15, §20).
    #[test]
    fn a_request_has_to_agree_with_its_own_block() {
        let wrong_chain = SimulationRequest::new(
            block(ChainId(1), 37_191_169),
            route(),
            TransactionSpec::canonical(Funding::Erc20Balance, Settle::KeepInputToken, U256::ONE),
            StateSpec::new("dump"),
            pricing(),
        )
        .expect_err("chain mismatch");
        assert!(matches!(wrong_chain, SimulationError::ChainMismatch { .. }));

        let wrong_height = SimulationRequest::new(
            block(CHAIN, 37_191_170),
            route(),
            TransactionSpec::canonical(Funding::Erc20Balance, Settle::KeepInputToken, U256::ONE),
            StateSpec::new("dump"),
            pricing(),
        )
        .expect_err("height mismatch");
        assert!(
            matches!(wrong_height, SimulationError::StateMismatch { .. }),
            "{wrong_height}"
        );
        assert!(
            wrong_height
                .to_string()
                .contains("priced on block 37191169"),
            "{wrong_height}"
        );

        let zero_ask = SimulationRequest::new(
            block(CHAIN, 37_191_169),
            route(),
            TransactionSpec::canonical(Funding::Erc20Balance, Settle::KeepInputToken, U256::ZERO),
            StateSpec::new("dump"),
            pricing(),
        )
        .expect_err("a zero ask donates instead of trading");
        assert!(matches!(
            zero_ask,
            SimulationError::UnsupportedTransaction(_)
        ));
    }

    /// The provider's own pin is checked against the request's header, and the two
    /// failure modes are told apart: a different height, and the same height with a
    /// different hash (which is a reorg or a different chain).
    #[test]
    fn a_source_pinned_elsewhere_is_a_state_mismatch() {
        let request = request();
        assert_eq!(
            request.pin(),
            BlockPin::new(BlockNumber(37_191_169), HEADER_HASH)
        );
        request
            .check_pin(BlockPin::new(BlockNumber(37_191_169), HEADER_HASH))
            .expect("the same pin is not a mismatch");

        let height = request
            .check_pin(BlockPin::new(BlockNumber(37_191_168), HEADER_HASH))
            .expect_err("wrong height");
        assert!(
            height.to_string().contains("pinned to height 37191168"),
            "{height}"
        );

        let other_hash =
            b256!("0x0000000000000000000000000000000000000000000000000000000000000001");
        let reorg = request
            .check_pin(BlockPin::new(BlockNumber(37_191_169), other_hash))
            .expect_err("same height, different hash");
        assert!(reorg.to_string().contains("reports"), "{reorg}");
        assert!(matches!(reorg, SimulationError::StateMismatch { .. }));
    }

    /// §18's boundary, enforced on the request: the only account setup may rewrite is
    /// the test sender itself.
    #[test]
    fn an_override_outside_the_test_sender_is_refused() {
        let clean = request();
        let sender = clean.transaction.sender.address;
        let mut funded = clean.clone();
        funded.state = funded.state.with_override(StateOverride::balance(
            sender,
            U256::from(1u8),
            "test sender needs gas money".to_string(),
        ));
        assert_eq!(funded.state.overrides.len(), 1);
        funded
            .check_overrides()
            .expect("an override on the account the run executes as is setup");

        // A balance written anywhere else is a fabricated holding, and a slot
        // written on a traded token is a fabricated ERC20 balance: §57 confining
        // setup to the sender is what stops either one entering quietly.
        for forbidden in [POOL_A, POOL_B, WETH, TTAX, STRANGER] {
            let expected = match forbidden {
                POOL_A | POOL_B => "trades through",
                WETH | TTAX => "a token this route trades",
                _ => "does not run as",
            };
            for tainted in [
                SimulationRequest {
                    state: StateSpec::new("dump").with_override(StateOverride::balance(
                        forbidden,
                        U256::from(1u8),
                        "tampering with the market".to_string(),
                    )),
                    ..clean.clone()
                },
                SimulationRequest {
                    state: StateSpec::new("dump").with_override(StateOverride::slot(
                        forbidden,
                        U256::ZERO,
                        U256::from(7u8),
                        "tampering with a holding".to_string(),
                    )),
                    ..clean.clone()
                },
            ] {
                let err = tainted.check_overrides().expect_err("not the sender");
                let text = err.to_string();
                assert!(text.contains(&format!("{forbidden}")), "{text}");
                assert!(text.contains(expected), "{text}");
                assert!(text.contains("§57"), "{text}");
            }
        }

        // The guard reads the route, not the plan, so it runs on a request the
        // constructor would never have produced.
        let empty = SimulationRequest {
            transaction: TransactionSpec {
                steps_planned: 0,
                ..clean.transaction.clone()
            },
            ..clean.clone()
        };
        assert!(empty.check_overrides().is_ok());
        assert!(SimulationRequest::new(
            block(CHAIN, 37_191_169),
            route(),
            TransactionSpec {
                steps_planned: 0,
                ..clean.transaction
            },
            StateSpec::new("dump"),
            pricing(),
        )
        .is_err());
    }

    /// `preflight` hands the caller the setup it is going to apply — the caller's own
    /// entries and the sender's funding — so nothing is added to the state on the
    /// quiet (§57: approval and balance setup is stated, not implied).
    #[test]
    fn preflight_returns_every_setup_it_will_apply() {
        let request = request();
        let setup = request.preflight().expect("a clean request");
        assert_eq!(setup.len(), 1, "the funding entry, and nothing else");
        assert_eq!(setup[0].address, request.sender_address());
        assert_eq!(
            setup[0].slots,
            Vec::new(),
            "setup funds an account, not a slot"
        );

        // A caller's own setup survives, and the generated one is still last so it
        // wins the fold when both touch the sender.
        let mut declared = request.clone();
        declared.state = declared.state.with_override(StateOverride::nonce(
            request.sender_address(),
            4,
            "the sender's real nonce at this height".to_string(),
        ));
        let setup = declared.preflight().expect("two setup entries");
        assert_eq!(setup.len(), 2);
        assert_eq!(setup[0].nonce, Some(4));
        assert_eq!(setup[1].address, request.sender_address());

        assert!(
            request.describe().contains("block 37191169"),
            "{}",
            request.describe()
        );
        assert!(request.describe().contains("dump:fixtures/m4-test.json"));
        assert_eq!(request.input_token(), TokenId::new(CHAIN, WETH));
        assert_eq!(request.chain_id, CHAIN);
    }

    #[test]
    fn a_request_serializes_for_the_evidence_file() {
        let text = serde_json::to_string(&request()).expect("serializes");
        let value: serde_json::Value = serde_json::from_str(&text).expect("is json");
        assert_eq!(value["chain_id"], serde_json::Value::from(91342u64));
        assert_eq!(
            value["block"]["number"],
            serde_json::Value::from(37191169u64)
        );
        assert_eq!(
            value["transaction"]["sender"]["address"]
                .as_str()
                .map(|s| s.starts_with("0x")),
            Some(true)
        );
        assert!(
            text.contains("eip1559") || text.contains("Eip1559"),
            "{text}"
        );
    }
}
