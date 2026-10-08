//! The Arbitrage Executor's own ABI: declared in Solidity, derived here, and pinned
//! against the compiler's output rather than against this file's expectations.
//!
//! [`crate::signatures`] pins the V2 event half and [`crate::calls`] the V2 function
//! half, each by requiring a derived selector to equal a value the *chain* produced.
//! M10's contract has no chain history to copy from before it is deployed, so its
//! external witness is `solc` itself:
//! `contracts/artifacts/ArbitrageExecutor.signatures` is the compiler's own listing of
//! every selector and canonical signature it emitted for the bytecode M10 deploys and
//! simulates, and [`pinned_against_the_compiler`] compares this module against it —
//! selector *and* signature string, in both directions, counts included. M10 §47 names
//! the failure this guards: `Rust calldata ≠ 实际部署 contract ABI`. A guard built from
//! this file's own constants could not detect that, because both sides of the
//! comparison would be the same claim.
//!
//! Nothing here validates a route or executes anything. It encodes calls, decodes the
//! two read-only return shapes, and turns a revert payload into one of the contract's
//! 22 named errors — which is the only way §40's `IncludedReverted` can report *why* a
//! transaction was included and reverted instead of only that it was.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{sol, Panic, Revert, SolCall, SolError, SolEvent};

use crate::calls::CallReturn;
use crate::error::{ProtocolError, Result};
use crate::signatures::{address_word, selector_of, word};

sol! {
    /// The route leg as the contract's own struct: six fields, in this order, and the
    /// encoding is the encoding `solc` sees.
    struct Leg {
        address pool;
        address tokenIn;
        address tokenOut;
        uint256 amountIn;
        uint256 amountOut;
        uint256 minAmountOut;
    }

    function execute(
        Leg[] calldata legs,
        address inputToken,
        uint256 amountIn,
        uint256 minFinalAmount,
        address recipient
    ) returns (uint256 delivered);
    function withdraw(address token, address to, uint256 amount);
    function setOperator(address next);
    function setPairAllowed(address pair, bool allowed);
    function setTokenAllowed(address token, bool allowed);
    function operator() view returns (address);
    function pairAllowed(address pair) view returns (bool);
    function tokenAllowed(address token) view returns (bool);
    function MAX_LEGS() view returns (uint256);

    event OperatorSet(address indexed previous, address indexed next);
    event PairAllowedSet(address indexed pair, bool allowed);
    event TokenAllowedSet(address indexed token, bool allowed);
    event LegExecuted(
        uint256 indexed index,
        address indexed pool,
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 delivered
    );
    event Executed(
        address indexed operator,
        address indexed recipient,
        address indexed inputToken,
        uint256 legs,
        uint256 amountIn,
        uint256 delivered
    );
    event Withdrawn(address indexed token, address indexed to, uint256 amount);

    error NotOperator(address caller);
    error ReentrancyDetected();
    error ZeroAddress();
    error ZeroAmount();
    error NoLegs();
    error TooManyLegs(uint256 legs);
    error PairNotAllowed(address pool);
    error TokenNotAllowed(address token);
    error LegSelfLoop(uint256 index, address token);
    error BrokenContinuity(uint256 index, address expected, address found);
    error NotRoundTrip(address inputToken, address otherEnd);
    error AskBelowFloor(uint256 index, uint256 ask, uint256 floor);
    error AmountChainBroken(uint256 index, uint256 expected, uint256 claimed);
    error InsufficientBalance(uint256 available, uint256 needed);
    error InsufficientAllowance(uint256 available, uint256 needed);
    error InputDeliveryMismatch(uint256 claimed, uint256 received);
    error PoolSidesMismatch(address pool, address tokenIn, address token0, address token1);
    error HoldingMismatch(uint256 index, address token, uint256 held, uint256 claimed);
    error DeliveryMismatch(uint256 index, uint256 asked, uint256 received);
    error LegShortfall(uint256 index, uint256 received, uint256 floor);
    error PayoutMismatch(uint256 available, uint256 delivered);
    error FinalShortfall(uint256 delivered, uint256 floor);
}

/// The most legs one `execute` call may carry.
///
/// The contract owns this number (`uint256 public constant MAX_LEGS`); the Rust side
/// needs it to reject an over-long plan before paying for a transaction that would
/// revert, so it is restated here and pinned by
/// [`tests::max_legs_matches_the_contract_source`], which reads the declaration out of
/// the Solidity file instead of trusting this constant.
pub const MAX_LEGS: u64 = 4;

/// One leg of a route, in this crate's naming.
///
/// A mirror of the contract's `Leg`, kept separate on purpose: the generated struct's
/// fields are the Solidity spellings (`tokenIn`, `amountOut`), and the rest of the
/// workspace writes snake_case. [`ExecutorLeg::to_sol`] is the only place the two
/// spellings meet, so a field accidentally swapped between them shows up as a wrong
/// encoding in one place rather than as a naming convention spread over every caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutorLeg {
    pub pool: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub amount_out: U256,
    pub min_amount_out: U256,
}

impl ExecutorLeg {
    fn to_sol(self) -> Leg {
        Leg {
            pool: self.pool,
            tokenIn: self.token_in,
            tokenOut: self.token_out,
            amountIn: self.amount_in,
            amountOut: self.amount_out,
            minAmountOut: self.min_amount_out,
        }
    }

    fn from_sol(leg: Leg) -> Self {
        Self {
            pool: leg.pool,
            token_in: leg.tokenIn,
            token_out: leg.tokenOut,
            amount_in: leg.amountIn,
            amount_out: leg.amountOut,
            min_amount_out: leg.minAmountOut,
        }
    }
}

/// One call on the executor's surface.
///
/// `execute` is the one M10 exists for; the other eight are the configuration reads a
/// test or an auditor needs to prove the contract is in the state the plan assumed
/// (§46: the executor address, the allowlists and the operator all have to be
/// *observed*, not asserted from the deployment script's own variables).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutorCall {
    Execute {
        legs: Vec<ExecutorLeg>,
        input_token: Address,
        amount_in: U256,
        min_final_amount: U256,
        recipient: Address,
    },
    Withdraw {
        token: Address,
        to: Address,
        amount: U256,
    },
    SetOperator {
        next: Address,
    },
    SetPairAllowed {
        pair: Address,
        allowed: bool,
    },
    SetTokenAllowed {
        token: Address,
        allowed: bool,
    },
    Operator,
    PairAllowed {
        pair: Address,
    },
    TokenAllowed {
        token: Address,
    },
    MaxLegs,
}

impl ExecutorCall {
    /// The canonical signature string, taken from the generated type so it is derived
    /// by the same macro that derives the selector. Hand-writing it here — as
    /// [`crate::calls::V2Call`] does — would put a second, independent claim about the
    /// struct's field order into the file whose job is to check that claim.
    pub fn signature(&self) -> &'static str {
        match self {
            Self::Execute { .. } => executeCall::SIGNATURE,
            Self::Withdraw { .. } => withdrawCall::SIGNATURE,
            Self::SetOperator { .. } => setOperatorCall::SIGNATURE,
            Self::SetPairAllowed { .. } => setPairAllowedCall::SIGNATURE,
            Self::SetTokenAllowed { .. } => setTokenAllowedCall::SIGNATURE,
            Self::Operator => operatorCall::SIGNATURE,
            Self::PairAllowed { .. } => pairAllowedCall::SIGNATURE,
            Self::TokenAllowed { .. } => tokenAllowedCall::SIGNATURE,
            Self::MaxLegs => MAX_LEGSCall::SIGNATURE,
        }
    }

    pub fn selector(&self) -> [u8; 4] {
        match self {
            Self::Execute { .. } => executeCall::SELECTOR,
            Self::Withdraw { .. } => withdrawCall::SELECTOR,
            Self::SetOperator { .. } => setOperatorCall::SELECTOR,
            Self::SetPairAllowed { .. } => setPairAllowedCall::SELECTOR,
            Self::SetTokenAllowed { .. } => setTokenAllowedCall::SELECTOR,
            Self::Operator => operatorCall::SELECTOR,
            Self::PairAllowed { .. } => pairAllowedCall::SELECTOR,
            Self::TokenAllowed { .. } => tokenAllowedCall::SELECTOR,
            Self::MaxLegs => MAX_LEGSCall::SELECTOR,
        }
    }

    /// Selector plus ABI-encoded arguments. Deterministic by construction: the input is
    /// this value and nothing else — no map iteration, no clock, no state read — which
    /// is what M10 §23/§37 asks the byte-for-byte test to confirm.
    pub fn encode(&self) -> Bytes {
        match self {
            Self::Execute {
                legs,
                input_token,
                amount_in,
                min_final_amount,
                recipient,
            } => executeCall {
                legs: legs.iter().copied().map(ExecutorLeg::to_sol).collect(),
                inputToken: *input_token,
                amountIn: *amount_in,
                minFinalAmount: *min_final_amount,
                recipient: *recipient,
            }
            .abi_encode(),
            Self::Withdraw { token, to, amount } => withdrawCall {
                token: *token,
                to: *to,
                amount: *amount,
            }
            .abi_encode(),
            Self::SetOperator { next } => setOperatorCall { next: *next }.abi_encode(),
            Self::SetPairAllowed { pair, allowed } => setPairAllowedCall {
                pair: *pair,
                allowed: *allowed,
            }
            .abi_encode(),
            Self::SetTokenAllowed { token, allowed } => setTokenAllowedCall {
                token: *token,
                allowed: *allowed,
            }
            .abi_encode(),
            Self::Operator => operatorCall {}.abi_encode(),
            Self::PairAllowed { pair } => pairAllowedCall { pair: *pair }.abi_encode(),
            Self::TokenAllowed { token } => tokenAllowedCall { token: *token }.abi_encode(),
            Self::MaxLegs => MAX_LEGSCall {}.abi_encode(),
        }
        .into()
    }

    /// Decode a `return value`: the bytes this call produced, selector already off.
    ///
    /// The write calls answer nothing, and `execute`'s number is not the invariant —
    /// the contract's own guard is measured at the recipient's balance, so a caller that
    /// wanted the delivered amount from the return data would be trusting exactly the
    /// thing §11 says not to trust.
    pub fn decode_return(&self, raw: &[u8]) -> Result<CallReturn> {
        match self {
            Self::Execute { .. } => Ok(CallReturn::Amount(word(raw, 0)?)),
            Self::MaxLegs => Ok(CallReturn::Amount(word(raw, 0)?)),
            Self::Operator => Ok(CallReturn::Address(address_word(
                &raw[..32.min(raw.len())],
            )?)),
            Self::PairAllowed { .. } | Self::TokenAllowed { .. } => {
                Ok(CallReturn::Boolean(!word(raw, 0)?.is_zero()))
            }
            _ => Err(ProtocolError::MalformedLog(format!(
                "{} returns nothing to decode",
                self.signature()
            ))),
        }
    }

    /// The leg list this call carries, when it carries one.
    pub fn legs(&self) -> Option<&[ExecutorLeg]> {
        match self {
            Self::Execute { legs, .. } => Some(legs),
            _ => None,
        }
    }
}

/// Read calldata back into the call it encodes.
///
/// [`ExecutorCall::encode`] alone would make stored calldata a claim nothing can check:
/// §48 asks a plan's calldata hash to be *rebuildable* from evidence, and the only way
/// to prove the bytes in an artifact are the bytes a plan produces is to decode them and
/// re-derive them from the plan. The decode is the strict variant, so a payload with a
/// non-canonical address or bool word fails here instead of silently becoming a value —
/// the same reason [`crate::signatures::address_word`] refuses loose words.
pub fn decode_calldata(data: &[u8]) -> Result<ExecutorCall> {
    if data.len() < 4 {
        return Err(ProtocolError::MalformedLog(format!(
            "executor calldata is {} bytes, shorter than a selector",
            data.len()
        )));
    }
    let mut selector = [0u8; 4];
    selector.copy_from_slice(&data[..4]);

    if selector == executeCall::SELECTOR {
        let decoded = strict("execute", executeCall::abi_decode_validate(data))?;
        return Ok(ExecutorCall::Execute {
            legs: decoded
                .legs
                .into_iter()
                .map(ExecutorLeg::from_sol)
                .collect(),
            input_token: decoded.inputToken,
            amount_in: decoded.amountIn,
            min_final_amount: decoded.minFinalAmount,
            recipient: decoded.recipient,
        });
    }
    if selector == withdrawCall::SELECTOR {
        let decoded = strict("withdraw", withdrawCall::abi_decode_validate(data))?;
        return Ok(ExecutorCall::Withdraw {
            token: decoded.token,
            to: decoded.to,
            amount: decoded.amount,
        });
    }
    if selector == setOperatorCall::SELECTOR {
        let decoded = strict("setOperator", setOperatorCall::abi_decode_validate(data))?;
        return Ok(ExecutorCall::SetOperator { next: decoded.next });
    }
    if selector == setPairAllowedCall::SELECTOR {
        let decoded = strict(
            "setPairAllowed",
            setPairAllowedCall::abi_decode_validate(data),
        )?;
        return Ok(ExecutorCall::SetPairAllowed {
            pair: decoded.pair,
            allowed: decoded.allowed,
        });
    }
    if selector == setTokenAllowedCall::SELECTOR {
        let decoded = strict(
            "setTokenAllowed",
            setTokenAllowedCall::abi_decode_validate(data),
        )?;
        return Ok(ExecutorCall::SetTokenAllowed {
            token: decoded.token,
            allowed: decoded.allowed,
        });
    }
    if selector == operatorCall::SELECTOR {
        strict("operator", operatorCall::abi_decode_validate(data))?;
        return Ok(ExecutorCall::Operator);
    }
    if selector == pairAllowedCall::SELECTOR {
        let decoded = strict("pairAllowed", pairAllowedCall::abi_decode_validate(data))?;
        return Ok(ExecutorCall::PairAllowed { pair: decoded.pair });
    }
    if selector == tokenAllowedCall::SELECTOR {
        let decoded = strict("tokenAllowed", tokenAllowedCall::abi_decode_validate(data))?;
        return Ok(ExecutorCall::TokenAllowed {
            token: decoded.token,
        });
    }
    if selector == MAX_LEGSCall::SELECTOR {
        strict("MAX_LEGS", MAX_LEGSCall::abi_decode_validate(data))?;
        return Ok(ExecutorCall::MaxLegs);
    }

    Err(ProtocolError::MalformedLog(format!(
        "0x{} is not one of the executor's nine declared selectors",
        hex_str(&selector)
    )))
}

fn strict<T>(name: &str, decoded: core::result::Result<T, alloy_sol_types::Error>) -> Result<T> {
    decoded.map_err(|error| {
        ProtocolError::MalformedLog(format!("{name} calldata is not strictly encoded: {error}"))
    })
}

fn hex_str(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One of the contract's 22 named rejections, with the numbers it reported.
///
/// The arguments are decoded rather than dropped because §50's planted negative controls
/// have to be *read* from evidence: `FinalShortfall { delivered, floor }` is the
/// difference between "the profit guard fired" and "something else failed and the
/// report guessed".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutorRevert {
    NotOperator {
        caller: Address,
    },
    ReentrancyDetected,
    ZeroAddress,
    ZeroAmount,
    NoLegs,
    TooManyLegs {
        legs: U256,
    },
    PairNotAllowed {
        pool: Address,
    },
    TokenNotAllowed {
        token: Address,
    },
    LegSelfLoop {
        index: U256,
        token: Address,
    },
    BrokenContinuity {
        index: U256,
        expected: Address,
        found: Address,
    },
    NotRoundTrip {
        input_token: Address,
        other_end: Address,
    },
    AskBelowFloor {
        index: U256,
        ask: U256,
        floor: U256,
    },
    AmountChainBroken {
        index: U256,
        expected: U256,
        claimed: U256,
    },
    InsufficientBalance {
        available: U256,
        needed: U256,
    },
    InsufficientAllowance {
        available: U256,
        needed: U256,
    },
    InputDeliveryMismatch {
        claimed: U256,
        received: U256,
    },
    PoolSidesMismatch {
        pool: Address,
        token_in: Address,
        token0: Address,
        token1: Address,
    },
    HoldingMismatch {
        index: U256,
        token: Address,
        held: U256,
        claimed: U256,
    },
    DeliveryMismatch {
        index: U256,
        asked: U256,
        received: U256,
    },
    LegShortfall {
        index: U256,
        received: U256,
        floor: U256,
    },
    PayoutMismatch {
        available: U256,
        delivered: U256,
    },
    FinalShortfall {
        delivered: U256,
        floor: U256,
    },
}

impl ExecutorRevert {
    /// The Solidity identifier, for the evidence column that names a rejection.
    pub fn name(&self) -> &'static str {
        match self {
            Self::NotOperator { .. } => "NotOperator",
            Self::ReentrancyDetected => "ReentrancyDetected",
            Self::ZeroAddress => "ZeroAddress",
            Self::ZeroAmount => "ZeroAmount",
            Self::NoLegs => "NoLegs",
            Self::TooManyLegs { .. } => "TooManyLegs",
            Self::PairNotAllowed { .. } => "PairNotAllowed",
            Self::TokenNotAllowed { .. } => "TokenNotAllowed",
            Self::LegSelfLoop { .. } => "LegSelfLoop",
            Self::BrokenContinuity { .. } => "BrokenContinuity",
            Self::NotRoundTrip { .. } => "NotRoundTrip",
            Self::AskBelowFloor { .. } => "AskBelowFloor",
            Self::AmountChainBroken { .. } => "AmountChainBroken",
            Self::InsufficientBalance { .. } => "InsufficientBalance",
            Self::InsufficientAllowance { .. } => "InsufficientAllowance",
            Self::InputDeliveryMismatch { .. } => "InputDeliveryMismatch",
            Self::PoolSidesMismatch { .. } => "PoolSidesMismatch",
            Self::HoldingMismatch { .. } => "HoldingMismatch",
            Self::DeliveryMismatch { .. } => "DeliveryMismatch",
            Self::LegShortfall { .. } => "LegShortfall",
            Self::PayoutMismatch { .. } => "PayoutMismatch",
            Self::FinalShortfall { .. } => "FinalShortfall",
        }
    }

    pub fn signature(&self) -> &'static str {
        match self {
            Self::NotOperator { .. } => NotOperator::SIGNATURE,
            Self::ReentrancyDetected => ReentrancyDetected::SIGNATURE,
            Self::ZeroAddress => ZeroAddress::SIGNATURE,
            Self::ZeroAmount => ZeroAmount::SIGNATURE,
            Self::NoLegs => NoLegs::SIGNATURE,
            Self::TooManyLegs { .. } => TooManyLegs::SIGNATURE,
            Self::PairNotAllowed { .. } => PairNotAllowed::SIGNATURE,
            Self::TokenNotAllowed { .. } => TokenNotAllowed::SIGNATURE,
            Self::LegSelfLoop { .. } => LegSelfLoop::SIGNATURE,
            Self::BrokenContinuity { .. } => BrokenContinuity::SIGNATURE,
            Self::NotRoundTrip { .. } => NotRoundTrip::SIGNATURE,
            Self::AskBelowFloor { .. } => AskBelowFloor::SIGNATURE,
            Self::AmountChainBroken { .. } => AmountChainBroken::SIGNATURE,
            Self::InsufficientBalance { .. } => InsufficientBalance::SIGNATURE,
            Self::InsufficientAllowance { .. } => InsufficientAllowance::SIGNATURE,
            Self::InputDeliveryMismatch { .. } => InputDeliveryMismatch::SIGNATURE,
            Self::PoolSidesMismatch { .. } => PoolSidesMismatch::SIGNATURE,
            Self::HoldingMismatch { .. } => HoldingMismatch::SIGNATURE,
            Self::DeliveryMismatch { .. } => DeliveryMismatch::SIGNATURE,
            Self::LegShortfall { .. } => LegShortfall::SIGNATURE,
            Self::PayoutMismatch { .. } => PayoutMismatch::SIGNATURE,
            Self::FinalShortfall { .. } => FinalShortfall::SIGNATURE,
        }
    }

    pub fn selector(&self) -> [u8; 4] {
        match self {
            Self::NotOperator { .. } => NotOperator::SELECTOR,
            Self::ReentrancyDetected => ReentrancyDetected::SELECTOR,
            Self::ZeroAddress => ZeroAddress::SELECTOR,
            Self::ZeroAmount => ZeroAmount::SELECTOR,
            Self::NoLegs => NoLegs::SELECTOR,
            Self::TooManyLegs { .. } => TooManyLegs::SELECTOR,
            Self::PairNotAllowed { .. } => PairNotAllowed::SELECTOR,
            Self::TokenNotAllowed { .. } => TokenNotAllowed::SELECTOR,
            Self::LegSelfLoop { .. } => LegSelfLoop::SELECTOR,
            Self::BrokenContinuity { .. } => BrokenContinuity::SELECTOR,
            Self::NotRoundTrip { .. } => NotRoundTrip::SELECTOR,
            Self::AskBelowFloor { .. } => AskBelowFloor::SELECTOR,
            Self::AmountChainBroken { .. } => AmountChainBroken::SELECTOR,
            Self::InsufficientBalance { .. } => InsufficientBalance::SELECTOR,
            Self::InsufficientAllowance { .. } => InsufficientAllowance::SELECTOR,
            Self::InputDeliveryMismatch { .. } => InputDeliveryMismatch::SELECTOR,
            Self::PoolSidesMismatch { .. } => PoolSidesMismatch::SELECTOR,
            Self::HoldingMismatch { .. } => HoldingMismatch::SELECTOR,
            Self::DeliveryMismatch { .. } => DeliveryMismatch::SELECTOR,
            Self::LegShortfall { .. } => LegShortfall::SELECTOR,
            Self::PayoutMismatch { .. } => PayoutMismatch::SELECTOR,
            Self::FinalShortfall { .. } => FinalShortfall::SELECTOR,
        }
    }

    /// The rejection's own words, with the numbers it carried. Written for evidence
    /// tables and for §40's error semantics, where a revert has to say which invariant
    /// closed the transaction.
    pub fn describe(&self) -> String {
        match self {
            Self::NotOperator { caller } => format!("NotOperator: {caller} is not the operator"),
            Self::ReentrancyDetected => "ReentrancyDetected".to_string(),
            Self::ZeroAddress => "ZeroAddress".to_string(),
            Self::ZeroAmount => "ZeroAmount".to_string(),
            Self::NoLegs => "NoLegs: the route is empty".to_string(),
            Self::TooManyLegs { legs } => {
                format!("TooManyLegs: {legs} legs, at most {MAX_LEGS}")
            }
            Self::PairNotAllowed { pool } => {
                format!("PairNotAllowed: {pool} is not on the pair allowlist")
            }
            Self::TokenNotAllowed { token } => {
                format!("TokenNotAllowed: {token} is not on the token allowlist")
            }
            Self::LegSelfLoop { index, token } => {
                format!("LegSelfLoop: leg {index} trades {token} for itself")
            }
            Self::BrokenContinuity {
                index,
                expected,
                found,
            } => {
                format!("BrokenContinuity: leg {index} starts at {found}, the previous leg ends at {expected}")
            }
            Self::NotRoundTrip {
                input_token,
                other_end,
            } => {
                format!("NotRoundTrip: the route opened on {input_token} and closed on {other_end}")
            }
            Self::AskBelowFloor { index, ask, floor } => {
                format!("AskBelowFloor: leg {index} asks {ask} but demands {floor}")
            }
            Self::AmountChainBroken {
                index,
                expected,
                claimed,
            } => {
                format!("AmountChainBroken: leg {index} claims {claimed} as its input, the plan carries {expected}")
            }
            Self::InsufficientBalance { available, needed } => {
                format!("InsufficientBalance: {available} held, {needed} needed")
            }
            Self::InsufficientAllowance { available, needed } => {
                format!("InsufficientAllowance: {available} granted, {needed} needed")
            }
            Self::InputDeliveryMismatch { claimed, received } => {
                format!("InputDeliveryMismatch: {claimed} sent, {received} arrived")
            }
            Self::PoolSidesMismatch {
                pool,
                token_in,
                token0,
                token1,
            } => {
                format!("PoolSidesMismatch: {pool} holds {token0}/{token1}, the leg asks to trade in {token_in}")
            }
            Self::HoldingMismatch {
                index,
                token,
                held,
                claimed,
            } => {
                format!("HoldingMismatch: leg {index} holds {held} of {token}, the plan claims {claimed}")
            }
            Self::DeliveryMismatch {
                index,
                asked,
                received,
            } => {
                format!("DeliveryMismatch: leg {index} was paid {received}, the plan asked {asked}")
            }
            Self::LegShortfall {
                index,
                received,
                floor,
            } => format!("LegShortfall: leg {index} delivered {received}, below the floor {floor}"),
            Self::PayoutMismatch {
                available,
                delivered,
            } => format!("PayoutMismatch: {available} sent, {delivered} received"),
            Self::FinalShortfall { delivered, floor } => {
                format!("FinalShortfall: {delivered} delivered, below the required {floor}")
            }
        }
    }
}

/// What a transaction's revert output actually said, as far as this module can tell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevertPayload {
    /// One of the executor contract's own rejections, arguments decoded.
    Executor(ExecutorRevert),
    /// A `revert("...")` / `require(..., "...")` string — what a V2 pair answers with.
    StandardString(String),
    /// A Solidity panic code (arithmetic overflow, under-encoded array, etc.).
    Panic(U256),
    /// Bytes this module cannot attribute: an unknown selector, or a known one whose
    /// payload did not decode. Kept whole rather than dropped, because §52 forbids
    /// turning an unread result into a plausible one.
    Unrecognized(Bytes),
    /// No revert data at all.
    Empty,
}

impl RevertPayload {
    /// The name a report prints for this payload. `Unrecognized` and `Empty` keep the
    /// raw bytes in the evidence row; this function never invents a reason.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Executor(revert) => revert.name(),
            Self::StandardString(_) => "Error(string)",
            Self::Panic(_) => "Panic(uint256)",
            Self::Unrecognized(_) => "Unrecognized",
            Self::Empty => "Empty",
        }
    }

    /// The typed rejection, when the payload was one of the contract's own.
    ///
    /// A caller that has to distinguish "our guard fired" from "the pair reverted"
    /// (§40's `IncludedReverted`, §58's controlled failure) reads the reason through
    /// here, so the distinction stays the classifier's answer rather than a string
    /// comparison in the execution layer.
    pub fn executor(&self) -> Option<ExecutorRevert> {
        match self {
            Self::Executor(revert) => Some(*revert),
            _ => None,
        }
    }
}

/// Classify the `output` of a reverted EVM call, or the `revertReason` of a receipt.
pub fn decode_revert(raw: &[u8]) -> RevertPayload {
    if raw.is_empty() {
        return RevertPayload::Empty;
    }
    if raw.len() < 4 {
        return RevertPayload::Unrecognized(Bytes::copy_from_slice(raw));
    }
    let mut selector = [0u8; 4];
    selector.copy_from_slice(&raw[..4]);
    match selector {
        s if s == Revert::SELECTOR => match Revert::abi_decode(raw) {
            Ok(reason) => RevertPayload::StandardString(reason.reason),
            Err(_) => RevertPayload::Unrecognized(Bytes::copy_from_slice(raw)),
        },
        s if s == Panic::SELECTOR => match Panic::abi_decode(raw) {
            Ok(panic) => RevertPayload::Panic(panic.code),
            Err(_) => RevertPayload::Unrecognized(Bytes::copy_from_slice(raw)),
        },
        s if s == NotOperator::SELECTOR => custom(raw, NotOperator::abi_decode(raw), |e| {
            ExecutorRevert::NotOperator { caller: e.caller }
        }),
        s if s == ReentrancyDetected::SELECTOR => {
            custom(raw, ReentrancyDetected::abi_decode(raw), |_| {
                ExecutorRevert::ReentrancyDetected
            })
        }
        s if s == ZeroAddress::SELECTOR => custom(raw, ZeroAddress::abi_decode(raw), |_| {
            ExecutorRevert::ZeroAddress
        }),
        s if s == ZeroAmount::SELECTOR => custom(raw, ZeroAmount::abi_decode(raw), |_| {
            ExecutorRevert::ZeroAmount
        }),
        s if s == NoLegs::SELECTOR => {
            custom(raw, NoLegs::abi_decode(raw), |_| ExecutorRevert::NoLegs)
        }
        s if s == TooManyLegs::SELECTOR => custom(raw, TooManyLegs::abi_decode(raw), |e| {
            ExecutorRevert::TooManyLegs { legs: e.legs }
        }),
        s if s == PairNotAllowed::SELECTOR => custom(raw, PairNotAllowed::abi_decode(raw), |e| {
            ExecutorRevert::PairNotAllowed { pool: e.pool }
        }),
        s if s == TokenNotAllowed::SELECTOR => custom(raw, TokenNotAllowed::abi_decode(raw), |e| {
            ExecutorRevert::TokenNotAllowed { token: e.token }
        }),
        s if s == LegSelfLoop::SELECTOR => custom(raw, LegSelfLoop::abi_decode(raw), |e| {
            ExecutorRevert::LegSelfLoop {
                index: e.index,
                token: e.token,
            }
        }),
        s if s == BrokenContinuity::SELECTOR => {
            custom(raw, BrokenContinuity::abi_decode(raw), |e| {
                ExecutorRevert::BrokenContinuity {
                    index: e.index,
                    expected: e.expected,
                    found: e.found,
                }
            })
        }
        s if s == NotRoundTrip::SELECTOR => custom(raw, NotRoundTrip::abi_decode(raw), |e| {
            ExecutorRevert::NotRoundTrip {
                input_token: e.inputToken,
                other_end: e.otherEnd,
            }
        }),
        s if s == AskBelowFloor::SELECTOR => custom(raw, AskBelowFloor::abi_decode(raw), |e| {
            ExecutorRevert::AskBelowFloor {
                index: e.index,
                ask: e.ask,
                floor: e.floor,
            }
        }),
        s if s == AmountChainBroken::SELECTOR => {
            custom(raw, AmountChainBroken::abi_decode(raw), |e| {
                ExecutorRevert::AmountChainBroken {
                    index: e.index,
                    expected: e.expected,
                    claimed: e.claimed,
                }
            })
        }
        s if s == InsufficientBalance::SELECTOR => {
            custom(raw, InsufficientBalance::abi_decode(raw), |e| {
                ExecutorRevert::InsufficientBalance {
                    available: e.available,
                    needed: e.needed,
                }
            })
        }
        s if s == InsufficientAllowance::SELECTOR => {
            custom(raw, InsufficientAllowance::abi_decode(raw), |e| {
                ExecutorRevert::InsufficientAllowance {
                    available: e.available,
                    needed: e.needed,
                }
            })
        }
        s if s == InputDeliveryMismatch::SELECTOR => {
            custom(raw, InputDeliveryMismatch::abi_decode(raw), |e| {
                ExecutorRevert::InputDeliveryMismatch {
                    claimed: e.claimed,
                    received: e.received,
                }
            })
        }
        s if s == PoolSidesMismatch::SELECTOR => {
            custom(raw, PoolSidesMismatch::abi_decode(raw), |e| {
                ExecutorRevert::PoolSidesMismatch {
                    pool: e.pool,
                    token_in: e.tokenIn,
                    token0: e.token0,
                    token1: e.token1,
                }
            })
        }
        s if s == HoldingMismatch::SELECTOR => custom(raw, HoldingMismatch::abi_decode(raw), |e| {
            ExecutorRevert::HoldingMismatch {
                index: e.index,
                token: e.token,
                held: e.held,
                claimed: e.claimed,
            }
        }),
        s if s == DeliveryMismatch::SELECTOR => {
            custom(raw, DeliveryMismatch::abi_decode(raw), |e| {
                ExecutorRevert::DeliveryMismatch {
                    index: e.index,
                    asked: e.asked,
                    received: e.received,
                }
            })
        }
        s if s == LegShortfall::SELECTOR => custom(raw, LegShortfall::abi_decode(raw), |e| {
            ExecutorRevert::LegShortfall {
                index: e.index,
                received: e.received,
                floor: e.floor,
            }
        }),
        s if s == PayoutMismatch::SELECTOR => custom(raw, PayoutMismatch::abi_decode(raw), |e| {
            ExecutorRevert::PayoutMismatch {
                available: e.available,
                delivered: e.delivered,
            }
        }),
        s if s == FinalShortfall::SELECTOR => custom(raw, FinalShortfall::abi_decode(raw), |e| {
            ExecutorRevert::FinalShortfall {
                delivered: e.delivered,
                floor: e.floor,
            }
        }),
        _ => RevertPayload::Unrecognized(Bytes::copy_from_slice(raw)),
    }
}

/// A known executor error selector whose payload must still decode to be named.
///
/// The unmatched branch keeps the raw bytes: §52 forbids turning a result this module
/// cannot read into a plausible one, and a truncated `FinalShortfall` is evidence of a
/// different shape than a full one.
fn custom<T>(
    raw: &[u8],
    decoded: core::result::Result<T, alloy_sol_types::Error>,
    build: impl FnOnce(T) -> ExecutorRevert,
) -> RevertPayload {
    match decoded {
        Ok(value) => RevertPayload::Executor(build(value)),
        Err(_) => RevertPayload::Unrecognized(Bytes::copy_from_slice(raw)),
    }
}

/// The executor's event topics, each derived from the declaration above.
///
/// A simulation reads logs and a receipt carries them; §55 asks both to be checked, and
/// the check starts at topic0. Same argument as [`crate::signatures::V2Topics`]: a topic
/// typed from memory is a claim about a contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutorTopics {
    pub operator_set: B256,
    pub pair_allowed_set: B256,
    pub token_allowed_set: B256,
    pub leg_executed: B256,
    pub executed: B256,
    pub withdrawn: B256,
}

impl Default for ExecutorTopics {
    fn default() -> Self {
        Self {
            operator_set: OperatorSet::SIGNATURE_HASH,
            pair_allowed_set: PairAllowedSet::SIGNATURE_HASH,
            token_allowed_set: TokenAllowedSet::SIGNATURE_HASH,
            leg_executed: LegExecuted::SIGNATURE_HASH,
            executed: Executed::SIGNATURE_HASH,
            withdrawn: Withdrawn::SIGNATURE_HASH,
        }
    }
}

/// The selector `execute` carries, as the four hex bytes evidence prints.
pub fn execute_selector_hex() -> String {
    format!("0x{}", hex_str(&executeCall::SELECTOR))
}

/// Re-derive a selector from a signature string, for the test that compares the three
/// independent ways this module can know a selector: alloy's generated constant,
/// [`selector_of`] (keccak of the string), and `solc`'s emitted table.
pub fn selector_from_signature(signature: &str) -> [u8; 4] {
    selector_of(signature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, keccak256};

    /// solc's own listing of every selector and canonical signature it emitted for the
    /// bytecode M10 deploys and simulates. This is the witness the module is checked
    /// against; `contracts/BUILD.md` §2 records the one command that regenerates it.
    const SOLC_LISTING: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/artifacts/ArbitrageExecutor.signatures"
    ));
    /// The contract source, included so a number the Rust side restates (`MAX_LEGS`) is
    /// read from the file that owns it instead of from this crate's own constant.
    const CONTRACT_SOURCE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/ArbitrageExecutor.sol"
    ));

    const PAIR_A: Address = address!("0x1111111111111111111111111111111111111111");
    const PAIR_B: Address = address!("0x2222222222222222222222222222222222222222");
    const TOKEN_A: Address = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const TOKEN_B: Address = address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    const SENDER: Address = address!("0xcccccccccccccccccccccccccccccccccccccccc");

    fn leg(index: u8) -> ExecutorLeg {
        let (pool, token_in, token_out) = if index == 0 {
            (PAIR_A, TOKEN_A, TOKEN_B)
        } else {
            (PAIR_B, TOKEN_B, TOKEN_A)
        };
        ExecutorLeg {
            pool,
            token_in,
            token_out,
            amount_in: U256::from(1_000_000_000_000_000_000u128 + u128::from(index)),
            amount_out: U256::from(1_000_000_000_000_000_010u128 + u128::from(index)),
            min_amount_out: U256::from(999_000_000_000_000_000u128 + u128::from(index)),
        }
    }

    fn two_leg_route() -> ExecutorCall {
        ExecutorCall::Execute {
            legs: vec![leg(0), leg(1)],
            input_token: TOKEN_A,
            amount_in: U256::from(1_000_000_000_000_000_000u128),
            min_final_amount: U256::from(1_001_000_000_000_000_000u128),
            recipient: SENDER,
        }
    }

    /// Every call this module can encode, so a test can assert the list is the same size
    /// as the compiler's list rather than the same size as this function's author
    /// remembered.
    fn all_calls() -> Vec<ExecutorCall> {
        vec![
            two_leg_route(),
            ExecutorCall::Withdraw {
                token: TOKEN_A,
                to: SENDER,
                amount: U256::from(5u8),
            },
            ExecutorCall::SetOperator { next: SENDER },
            ExecutorCall::SetPairAllowed {
                pair: PAIR_A,
                allowed: true,
            },
            ExecutorCall::SetTokenAllowed {
                token: TOKEN_A,
                allowed: false,
            },
            ExecutorCall::Operator,
            ExecutorCall::PairAllowed { pair: PAIR_A },
            ExecutorCall::TokenAllowed { token: TOKEN_A },
            ExecutorCall::MaxLegs,
        ]
    }

    fn all_reverts() -> Vec<ExecutorRevert> {
        vec![
            ExecutorRevert::NotOperator { caller: SENDER },
            ExecutorRevert::ReentrancyDetected,
            ExecutorRevert::ZeroAddress,
            ExecutorRevert::ZeroAmount,
            ExecutorRevert::NoLegs,
            ExecutorRevert::TooManyLegs {
                legs: U256::from(5u8),
            },
            ExecutorRevert::PairNotAllowed { pool: PAIR_A },
            ExecutorRevert::TokenNotAllowed { token: TOKEN_A },
            ExecutorRevert::LegSelfLoop {
                index: U256::from(1u8),
                token: TOKEN_A,
            },
            ExecutorRevert::BrokenContinuity {
                index: U256::from(1u8),
                expected: TOKEN_B,
                found: TOKEN_A,
            },
            ExecutorRevert::NotRoundTrip {
                input_token: TOKEN_A,
                other_end: TOKEN_B,
            },
            ExecutorRevert::AskBelowFloor {
                index: U256::from(0u8),
                ask: U256::from(9u8),
                floor: U256::from(10u8),
            },
            ExecutorRevert::AmountChainBroken {
                index: U256::from(1u8),
                expected: U256::from(100u8),
                claimed: U256::from(99u8),
            },
            ExecutorRevert::InsufficientBalance {
                available: U256::from(1u8),
                needed: U256::from(2u8),
            },
            ExecutorRevert::InsufficientAllowance {
                available: U256::from(1u8),
                needed: U256::from(2u8),
            },
            ExecutorRevert::InputDeliveryMismatch {
                claimed: U256::from(2u8),
                received: U256::from(1u8),
            },
            ExecutorRevert::PoolSidesMismatch {
                pool: PAIR_A,
                token_in: TOKEN_A,
                token0: TOKEN_B,
                token1: TOKEN_A,
            },
            ExecutorRevert::HoldingMismatch {
                index: U256::from(0u8),
                token: TOKEN_A,
                held: U256::from(3u8),
                claimed: U256::from(4u8),
            },
            ExecutorRevert::DeliveryMismatch {
                index: U256::from(1u8),
                asked: U256::from(5u8),
                received: U256::from(4u8),
            },
            ExecutorRevert::LegShortfall {
                index: U256::from(1u8),
                received: U256::from(4u8),
                floor: U256::from(5u8),
            },
            ExecutorRevert::PayoutMismatch {
                available: U256::from(6u8),
                delivered: U256::from(5u8),
            },
            ExecutorRevert::FinalShortfall {
                delivered: U256::from(5u8),
                floor: U256::from(6u8),
            },
        ]
    }

    fn module_events() -> Vec<(String, &'static str)> {
        vec![
            OperatorSet::SIGNATURE_HASH,
            PairAllowedSet::SIGNATURE_HASH,
            TokenAllowedSet::SIGNATURE_HASH,
            LegExecuted::SIGNATURE_HASH,
            Executed::SIGNATURE_HASH,
            Withdrawn::SIGNATURE_HASH,
        ]
        .into_iter()
        .zip([
            OperatorSet::SIGNATURE,
            PairAllowedSet::SIGNATURE,
            TokenAllowedSet::SIGNATURE,
            LegExecuted::SIGNATURE,
            Executed::SIGNATURE,
            Withdrawn::SIGNATURE,
        ])
        .map(|(hash, signature)| (hex_str(hash.as_slice()), signature))
        .collect()
    }

    /// `[(id, signature)]` for one section of solc's listing.
    fn section(header: &str) -> Vec<(String, String)> {
        let marker = format!("{header}:");
        let start = SOLC_LISTING
            .find(&marker)
            .unwrap_or_else(|| panic!("{marker} is not in the compiler's listing"));
        let body = &SOLC_LISTING[start + marker.len()..];
        let end = body.find("\n\n").unwrap_or(body.len());
        body[..end]
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let (id, signature) = line
                    .trim()
                    .split_once(": ")
                    .unwrap_or_else(|| panic!("listing row `{line}` has no `: ` separator"));
                (id.to_string(), signature.to_string())
            })
            .collect()
    }

    /// The guard M10 §47 names: Rust's idea of this ABI vs the compiler's.
    ///
    /// Three independent derivations have to agree for every entry — alloy's generated
    /// constant, keccak of the signature string ([`selector_of`], the same hash the EVM
    /// uses), and solc's emitted table — and the comparison runs in both directions with
    /// counts, so a variant this module forgot and a signature this module invented both
    /// fail.
    #[test]
    fn pinned_against_the_compiler() {
        let functions = section("Function signatures");
        let errors = section("Error signatures");
        let events = section("Event signatures");

        let calls = all_calls();
        assert_eq!(
            calls.len(),
            functions.len(),
            "this module encodes {} calls, solc emitted {}",
            calls.len(),
            functions.len()
        );
        for call in &calls {
            let id = hex_str(&call.selector());
            let (_, signature) = functions
                .iter()
                .find(|(listed_id, _)| *listed_id == id)
                .unwrap_or_else(|| {
                    panic!(
                        "selector {id} ({}) is not in the compiler's listing",
                        call.signature()
                    )
                });
            assert_eq!(
                signature,
                call.signature(),
                "solc and alloy disagree about the canonical signature behind {id}"
            );
            assert_eq!(
                selector_from_signature(signature),
                call.selector(),
                "keccak of the compiler's own signature string does not reproduce its selector"
            );
        }
        for (id, signature) in &functions {
            assert!(
                calls.iter().any(|call| hex_str(&call.selector()) == *id),
                "the compiler exposes {signature} ({id}) and this module cannot encode it"
            );
        }

        let reverts = all_reverts();
        assert_eq!(
            reverts.len(),
            errors.len(),
            "this module classifies {} errors, solc emitted {}",
            reverts.len(),
            errors.len()
        );
        for revert in &reverts {
            let id = hex_str(&revert.selector());
            let (_, signature) = errors
                .iter()
                .find(|(listed_id, _)| *listed_id == id)
                .unwrap_or_else(|| {
                    panic!(
                        "error selector {id} ({}) is not in the listing",
                        revert.name()
                    )
                });
            assert_eq!(signature, revert.signature(), "{id}: signature mismatch");
            assert_eq!(
                selector_from_signature(signature),
                revert.selector(),
                "{id}: keccak of the compiler's error signature is not its selector"
            );
        }
        for (id, signature) in &errors {
            assert!(
                reverts
                    .iter()
                    .any(|revert| hex_str(&revert.selector()) == *id),
                "the contract can revert with {signature} ({id}) and this module would not name it"
            );
        }

        let topics = module_events();
        assert_eq!(
            topics.len(),
            events.len(),
            "this module knows {} event topics, solc emitted {}",
            topics.len(),
            events.len()
        );
        for (hash, signature) in &topics {
            let (listed_hash, listed_signature) = events
                .iter()
                .find(|(listed_hash, _)| listed_hash == hash)
                .unwrap_or_else(|| panic!("topic {hash} ({signature}) is not in the listing"));
            assert_eq!(listed_signature, signature, "{hash}: signature mismatch");
            assert_eq!(
                hex_str(keccak256(listed_signature.as_bytes()).as_slice()),
                *listed_hash,
                "keccak of the compiler's event signature is not its topic0"
            );
        }
        for (listed_hash, listed_signature) in &events {
            assert!(
                topics.iter().any(|(hash, _)| hash == listed_hash),
                "the contract emits {listed_signature} and this module does not know its topic0"
            );
        }
    }

    #[test]
    fn topic_table_is_the_generated_hashes() {
        let topics = ExecutorTopics::default();
        assert_eq!(topics.operator_set, OperatorSet::SIGNATURE_HASH);
        assert_eq!(topics.pair_allowed_set, PairAllowedSet::SIGNATURE_HASH);
        assert_eq!(topics.token_allowed_set, TokenAllowedSet::SIGNATURE_HASH);
        assert_eq!(topics.leg_executed, LegExecuted::SIGNATURE_HASH);
        assert_eq!(topics.executed, Executed::SIGNATURE_HASH);
        assert_eq!(topics.withdrawn, Withdrawn::SIGNATURE_HASH);
        // The three indexed positions of `Executed` are what a log filter keys on.
        assert_eq!(
            execute_selector_hex().as_str(),
            format!("0x{}", hex_str(&executeCall::SELECTOR))
        );
    }

    /// `MAX_LEGS` is the one number this crate restates from Solidity. Reading it out of
    /// the contract source is the only way the Rust-side const can be wrong.
    #[test]
    fn max_legs_matches_the_contract_source() {
        let marker = "uint256 public constant MAX_LEGS = ";
        let start = CONTRACT_SOURCE
            .find(marker)
            .expect("the contract declares MAX_LEGS");
        let digits: String = CONTRACT_SOURCE[start + marker.len()..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let declared: u64 = digits.parse().expect("MAX_LEGS is a number");
        assert_eq!(
            declared, MAX_LEGS,
            "the contract declares {declared} and evm-protocol restates {MAX_LEGS}"
        );
        // The guard in `execute` compares against the same declaration, and the getter
        // returns it, so a reader can confirm all three agree without running anything.
        assert!(
            CONTRACT_SOURCE.contains("revert TooManyLegs(count)"),
            "the contract no longer rejects an over-long route the way this module assumes"
        );
    }

    /// §23/§37: `plan → calldata` is deterministic, byte for byte.
    ///
    /// The assertion that carries weight is the one against bytes computed from the ABI
    /// specification, because re-encoding with this module would only prove the encoder
    /// is a function. A 2-leg `execute` head is five words (array offset + four scalars),
    /// then the array's length word, then six words per leg: 4 + 160 + 32 + 384 = 580.
    #[test]
    fn encoding_is_deterministic_and_structurally_correct() {
        let call = two_leg_route();
        let first = call.encode();
        for _ in 0..3 {
            assert_eq!(first, call.encode(), "encode() is not byte-for-byte stable");
        }
        assert_eq!(first.len(), 580, "2-leg execute calldata");
        assert_eq!(&first[..4], &executeCall::SELECTOR);
        assert_eq!(
            word(&first[4..], 0).expect("head offset"),
            U256::from(160u32),
            "the dynamic legs array must be head-referenced from byte 160"
        );
        assert_eq!(
            address_word(&first[4 + 32..4 + 64]).expect("inputToken"),
            TOKEN_A
        );
        assert_eq!(
            word(&first[4..], 2).expect("amountIn"),
            U256::from(1_000_000_000_000_000_000u128)
        );
        assert_eq!(
            word(&first[4..], 3).expect("minFinalAmount"),
            U256::from(1_001_000_000_000_000_000u128)
        );
        assert_eq!(
            address_word(&first[4 + 4 * 32..4 + 5 * 32]).expect("recipient"),
            SENDER
        );
        // The array's tail begins at byte 4 + 160: the selector, then the five head
        // words. Its first word is the length, then six words per leg.
        let tail = &first[4 + 160..];
        assert_eq!(word(tail, 0).expect("legs length"), U256::from(2u8));
        assert_eq!(address_word(&tail[32..64]).expect("leg0 pool"), PAIR_A);
        assert_eq!(address_word(&tail[64..96]).expect("leg0 tokenIn"), TOKEN_A);
        assert_eq!(
            address_word(&tail[96..128]).expect("leg0 tokenOut"),
            TOKEN_B
        );
        assert_eq!(
            word(tail, 4).expect("leg0 amountIn"),
            U256::from(1_000_000_000_000_000_000u128)
        );
        assert_eq!(
            word(tail, 5).expect("leg0 amountOut"),
            U256::from(1_000_000_000_000_000_010u128)
        );
        assert_eq!(
            word(tail, 6).expect("leg0 minAmountOut"),
            U256::from(999_000_000_000_000_000u128)
        );
        // leg[1] starts at tail word 7, so its last field is word 12.
        assert_eq!(
            address_word(&tail[7 * 32..8 * 32]).expect("leg1 pool"),
            PAIR_B
        );
        assert_eq!(
            word(tail, 12).expect("leg1 minAmountOut"),
            U256::from(999_000_000_000_000_001u128)
        );
        assert_eq!(
            tail.len(),
            32 + 384,
            "the tail is the length plus both legs"
        );

        // 1 and 3 legs scale the way the same arithmetic predicts.
        for (count, expected_len) in [(1usize, 4usize + 160 + 32 + 192), (3, 4 + 160 + 32 + 576)] {
            let legs: Vec<ExecutorLeg> = (0..count as u8).map(leg).collect();
            let encoded = ExecutorCall::Execute {
                legs: legs.clone(),
                input_token: TOKEN_A,
                amount_in: U256::from(1u8),
                min_final_amount: U256::from(1u8),
                recipient: SENDER,
            }
            .encode();
            assert_eq!(encoded.len(), expected_len, "{count}-leg execute calldata");
            assert_eq!(
                word(&encoded[4 + 160..], 0).expect("length"),
                U256::from(count)
            );
        }
    }

    /// §48 replayability: the bytes an artifact stores decode back to the call that made
    /// them, for all nine entries — and a payload that is not strictly encoded is
    /// rejected rather than read into a plausible value.
    #[test]
    fn calldata_round_trips_strictly() {
        for call in all_calls() {
            let encoded = call.encode();
            let decoded = decode_calldata(&encoded).unwrap_or_else(|error| {
                panic!("{} cannot be read back: {error}", call.signature())
            });
            assert_eq!(decoded, call, "round trip changed the call");
        }

        let mut call = two_leg_route();
        if let ExecutorCall::Execute { legs, .. } = &mut call {
            legs.truncate(1);
        }
        let encoded = call.encode();
        assert_eq!(decode_calldata(&encoded).expect("1-leg reads back"), call);
        assert_eq!(
            decode_calldata(&encoded)
                .expect("same bytes")
                .legs()
                .map(|legs| legs.to_vec()),
            call.legs().map(|legs| legs.to_vec())
        );

        // A non-canonical address word (any of its 12 leading padding bytes set) is not
        // a valid encoding. Byte 4 + 160 + 32 is the first padding byte of leg[0]'s pool.
        let mut loose = encoded.to_vec();
        loose[4 + 160 + 32] = 0x01;
        assert!(
            matches!(decode_calldata(&loose), Err(ProtocolError::MalformedLog(_))),
            "strict decode accepted a padded address"
        );

        // Truncated, selector-only, and unknown selectors are all refusals, not defaults.
        assert!(decode_calldata(&[]).is_err());
        assert!(decode_calldata(&[0xfe, 0xb0]).is_err());
        assert!(decode_calldata(&[0u8; 4]).is_err());
        let mut unknown = encoded.to_vec();
        unknown[..4].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        assert!(decode_calldata(&unknown).is_err());
    }

    /// Every error the contract can revert with is classified by name, not just the ones
    /// a test happened to think of. The payloads are built from the compiler's listing
    /// (selector + one zero word per declared argument), so the check does not reuse this
    /// module's own encoder on either side.
    #[test]
    fn all_twenty_two_error_selectors_classify() {
        for (id, signature) in section("Error signatures") {
            let mut selector = [0u8; 4];
            for (n, byte) in hex_bytes(&id).into_iter().enumerate() {
                selector[n] = byte;
            }
            let arity = argument_count(&signature);
            let mut raw = selector.to_vec();
            raw.extend(vec![0u8; arity * 32]);
            let payload = decode_revert(&raw);
            assert_eq!(
                payload.kind(),
                name_of(&signature),
                "{signature}: the classifier answered {}",
                payload.kind()
            );
            assert!(
                matches!(payload, RevertPayload::Executor(_)),
                "{signature}: classified as {}, not an executor error",
                payload.kind()
            );
        }
    }

    /// The arguments survive decoding: §50's planted controls are only evidence if the
    /// numbers the contract reported are readable from it.
    #[test]
    fn guard_errors_keep_their_numbers() {
        let shortfall = FinalShortfall {
            delivered: U256::from(900u32),
            floor: U256::from(1000u32),
        }
        .abi_encode();
        assert_eq!(
            decode_revert(&shortfall),
            RevertPayload::Executor(ExecutorRevert::FinalShortfall {
                delivered: U256::from(900u32),
                floor: U256::from(1000u32),
            })
        );

        let not_operator = NotOperator { caller: SENDER }.abi_encode();
        assert_eq!(
            decode_revert(&not_operator),
            RevertPayload::Executor(ExecutorRevert::NotOperator { caller: SENDER })
        );

        let holding = HoldingMismatch {
            index: U256::from(1u8),
            token: TOKEN_B,
            held: U256::from(7u8),
            claimed: U256::from(8u8),
        }
        .abi_encode();
        assert_eq!(
            decode_revert(&holding),
            RevertPayload::Executor(ExecutorRevert::HoldingMismatch {
                index: U256::from(1u8),
                token: TOKEN_B,
                held: U256::from(7u8),
                claimed: U256::from(8u8),
            })
        );
        assert_eq!(
            decode_revert(&holding).kind(),
            "HoldingMismatch",
            "describe() and kind() must name the same rejection"
        );
        assert!(
            decode_revert(&holding)
                .executor()
                .unwrap()
                .describe()
                .contains("7"),
            "the describe() line has to carry the reported number"
        );
    }

    /// A V2 pair answers with `Error(string)`; an overflow answers with `Panic`; anything
    /// else stays `Unrecognized` with its bytes intact. None of these become an executor
    /// error, which is §52's rule that an unread result is never given a plausible reason.
    #[test]
    fn foreign_and_malformed_payloads_are_never_relabelled() {
        let pair_revert = Revert {
            reason: "UniswapV2: K".to_string(),
        }
        .abi_encode();
        assert_eq!(
            decode_revert(&pair_revert),
            RevertPayload::StandardString("UniswapV2: K".to_string())
        );
        assert_eq!(decode_revert(&pair_revert).kind(), "Error(string)");

        let overflow = Panic {
            code: U256::from(17u8),
        }
        .abi_encode();
        assert_eq!(
            decode_revert(&overflow),
            RevertPayload::Panic(U256::from(17u8))
        );

        assert_eq!(decode_revert(&[]), RevertPayload::Empty);
        assert_eq!(
            decode_revert(&[0x00, 0x01]),
            RevertPayload::Unrecognized(Bytes::copy_from_slice(&[0x00, 0x01]))
        );
        let unknown_selector = {
            let mut raw = FinalShortfall {
                delivered: U256::ZERO,
                floor: U256::ZERO,
            }
            .abi_encode();
            raw[..4].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
            raw
        };
        assert_eq!(
            decode_revert(&unknown_selector),
            RevertPayload::Unrecognized(Bytes::copy_from_slice(&unknown_selector))
        );

        // A known selector with a short payload keeps its bytes rather than becoming a
        // zero-argument answer of the right name.
        let mut truncated = FinalShortfall {
            delivered: U256::from(1u8),
            floor: U256::from(2u8),
        }
        .abi_encode();
        truncated.truncate(4 + 32);
        let payload = decode_revert(&truncated);
        assert_eq!(payload.kind(), "Unrecognized");
        assert_eq!(
            payload,
            RevertPayload::Unrecognized(Bytes::copy_from_slice(&truncated))
        );
    }

    /// The hash this crate uses is the hash the shell gate uses: alloy's keccak256 and
    /// `openssl dgst -keccak-256` agree on the two published controls.
    #[test]
    fn keccak_witness_matches_the_shell_tool() {
        assert_eq!(
            hex_str(keccak256(&[] as &[u8]).as_slice()),
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
        assert_eq!(
            hex_str(keccak256(b"abc").as_slice()),
            "4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45"
        );
        assert_eq!(
            hex_str(&selector_of("transfer(address,uint256)")),
            "a9059cbb",
            "keccak-derived selector disagrees with the V2 surface this crate already pins"
        );
    }

    fn name_of(signature: &str) -> &str {
        signature.split('(').next().unwrap_or(signature)
    }

    /// The arity of a flat error signature. Only used for the contract's own errors,
    /// whose arguments are all `uint256` or `address` and therefore never nest.
    fn argument_count(signature: &str) -> usize {
        let open = signature
            .find('(')
            .unwrap_or_else(|| panic!("`{signature}` has no argument list"));
        let close = signature
            .rfind(')')
            .unwrap_or_else(|| panic!("`{signature}` has no closing paren"));
        let args = &signature[open + 1..close];
        if args.trim().is_empty() {
            0
        } else {
            args.split(',').count()
        }
    }

    fn hex_bytes(value: &str) -> Vec<u8> {
        (0..value.len() / 2)
            .map(|n| {
                u8::from_str_radix(&value[n * 2..n * 2 + 2], 16)
                    .unwrap_or_else(|_| panic!("`{value}` is not hex"))
            })
            .collect()
    }
}
