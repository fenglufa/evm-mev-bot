// SPDX-License-Identifier: MIT
pragma solidity 0.8.37;

/// What the executor is allowed to call: a Uniswap-V2-shaped pair and an ERC20.
///
/// Both interfaces exist here so that every external call this contract makes is
/// written against a declaration the compiler checks, rather than against a
/// `bytes` blob someone assembled elsewhere. There is no `address target` argument
/// anywhere in this file that is not first checked against an allowlist, and no
/// `call`/`delegatecall`/`callcode` at all — see the scan gate in the M10 task
/// book (§42/§62).
interface IERC20 {
    function transfer(address to, uint256 value) external returns (bool);
    function transferFrom(address from, address to, uint256 value) external returns (bool);
    function balanceOf(address account) external view returns (uint256);
    function allowance(address owner, address spender) external view returns (uint256);
}

/// The V2 pair's own declaration. `swap` is exact-out: it pays `amountOut` first
/// and then validates the invariant against the balance it actually received, so
/// the input has to arrive before the call and any shortfall is the pair's own
/// revert, not this contract's arithmetic.
interface IV2Pair {
    function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data) external;
    function token0() external view returns (address);
    function token1() external view returns (address);
}

/// @title ArbitrageExecutor
///
/// One atomic, operator-initiated, multi-leg round trip over allowlisted V2 pools.
///
/// This contract executes a route that was decided somewhere else. It does not
/// search, it does not price, it does not read a market, and it does not decide
/// whether a trade is worth making: it is an execution primitive (M10 §2), and the
/// only judgement inside it is "does what actually arrived match what the plan
/// said would".
///
/// ## Why one transaction is the point
///
/// Every leg runs in this contract's own context, so a failure at any leg reverts
/// the whole transaction, including the legs that had already succeeded. That is
/// the property a formula cannot give you: leg 1's profit is not real until leg 2
/// has paid, and here it is not even *booked* until then.
///
/// ## The two rules the rest of the file is built around
///
/// 1. **Never trust a return value.** Every transfer is measured as a balance
///    delta, and every delta has to equal the number the calldata claimed. A token
///    that charges a transfer fee therefore cannot be traded through here: it
///    reverts with `DeliveryMismatch` / `InputDeliveryMismatch` instead of silently
///    computing a smaller output (M10 §21/§22).
/// 2. **The amounts are claims, and claims are checked.** Leg `i`'s `amountIn`
///    must equal exactly what the executor is holding of that token at that moment,
///    which after leg 0 is only what the previous leg delivered. So the calldata
///    either describes the sequence that actually happened or the transaction
///    reverts — there is no adaptive path where the contract quietly carries a
///    different number forward.
contract ArbitrageExecutor {
    // --------------------------------------------------------------------
    //  Types
    // --------------------------------------------------------------------

    /// One leg of the route, with every number the plan claimed about it.
    ///
    /// `amountOut` is what this contract asks the pool to pay — a V2 pair cannot be
    /// told "give me what your invariant allows", only "pay this much if you can" —
    /// and `minAmountOut` is the floor below which the plan refuses to continue even
    /// if the pool is willing to pay. Both are in the calldata, so both are part of
    /// the byte-for-byte reproducible encoding.
    struct Leg {
        address pool;
        address tokenIn;
        address tokenOut;
        uint256 amountIn;
        uint256 amountOut;
        uint256 minAmountOut;
    }

    // --------------------------------------------------------------------
    //  Storage
    // --------------------------------------------------------------------

    /// The only account that may execute or rescue. Deliberately one address, not a
    /// role system: M10 asks for an execution primitive with an access control that
    /// can be read in one line (§44/§45).
    address public operator;

    /// Pools this contract is willing to call `swap` on. An unlisted pool reverts
    /// before any token moves (§43).
    mapping(address => bool) public pairAllowed;

    /// Tokens this contract is willing to move. Both sides of every leg have to be
    /// listed, including the input token.
    mapping(address => bool) public tokenAllowed;

    /// 1 means unlocked. A manual lock rather than a library modifier, because the
    /// alternative is taking on an entire dependency tree for six lines of assembly
    /// (M10 §20: "不要为了一个 modifier 引入整个大型依赖树").
    uint256 private _lock = 1;

    /// The most legs one call may contain. A bounded loop is what makes the gas cost
    /// of a revert path predictable; the route this contract exists for has two.
    uint256 public constant MAX_LEGS = 4;

    // --------------------------------------------------------------------
    //  Events
    // --------------------------------------------------------------------

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

    // --------------------------------------------------------------------
    //  Errors — every rejection names itself, so a revert is a classification
    // --------------------------------------------------------------------

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
    /// The claimed inputs do not chain: leg `i`'s `amountIn` is not leg `i-1`'s
    /// `amountOut`. A planning error, caught before anything moves — which is why it
    /// is a different error from `HoldingMismatch`, the same disagreement found
    /// against the balance the contract actually holds while running (§40).
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

    // --------------------------------------------------------------------
    //  Modifiers
    // --------------------------------------------------------------------

    modifier onlyOperator() {
        if (msg.sender != operator) revert NotOperator(msg.sender);
        _;
    }

    modifier nonReentrant() {
        if (_lock != 1) revert ReentrancyDetected();
        _lock = 2;
        _;
        _lock = 1;
    }

    // --------------------------------------------------------------------
    //  Configuration
    // --------------------------------------------------------------------

    /// @param operator_ the single account allowed to execute and to rescue.
    constructor(address operator_) {
        if (operator_ == address(0)) revert ZeroAddress();
        operator = operator_;
        emit OperatorSet(address(0), operator_);
    }

    function setOperator(address next) external onlyOperator {
        if (next == address(0)) revert ZeroAddress();
        emit OperatorSet(operator, next);
        operator = next;
    }

    function setPairAllowed(address pair, bool allowed) external onlyOperator {
        if (pair == address(0)) revert ZeroAddress();
        pairAllowed[pair] = allowed;
        emit PairAllowedSet(pair, allowed);
    }

    function setTokenAllowed(address token, bool allowed) external onlyOperator {
        if (token == address(0)) revert ZeroAddress();
        tokenAllowed[token] = allowed;
        emit TokenAllowedSet(token, allowed);
    }

    // --------------------------------------------------------------------
    //  Execution
    // --------------------------------------------------------------------

    /// Run `legs` as one transaction and hand the closing token to `recipient`.
    ///
    /// The input comes from the operator's own wallet through an `allowance` the
    /// operator granted, and the output goes to `recipient` — so the contract holds
    /// nothing after a successful run, and a failed run leaves nothing here either
    /// because the whole call reverted (M10 §18: no permanent sink for funds).
    ///
    /// The body is five calls on purpose. Each of them is a stage that has to pass
    /// before the next one can move anything, and keeping each stage's own locals in
    /// its own frame is what lets this compile without the legacy pipeline's 16-slot
    /// stack ceiling. Validation order matters for the negative controls: shape and
    /// allowlists are checked for every leg *before* any token moves, so a route that
    /// is illegal costs a call's worth of gas and no balance.
    function execute(
        Leg[] calldata legs,
        address inputToken,
        uint256 amountIn,
        uint256 minFinalAmount,
        address recipient
    ) external onlyOperator nonReentrant returns (uint256 delivered) {
        if (recipient == address(0)) revert ZeroAddress();
        if (amountIn == 0 || minFinalAmount == 0) revert ZeroAmount();
        if (!tokenAllowed[inputToken]) revert TokenNotAllowed(inputToken);

        _checkRoute(legs, inputToken, amountIn);
        uint256 carried = _pullInput(inputToken, amountIn);
        for (uint256 i = 0; i < legs.length; i++) {
            carried = _deliverLeg(i, legs[i], carried);
        }
        delivered = _settle(inputToken, carried, minFinalAmount, recipient, legs.length);

        emit Executed(msg.sender, recipient, inputToken, legs.length, amountIn, delivered);
    }

    /// The route as a whole: every leg legal on its own, the legs joined end to end,
    /// the sequence closing back on `inputToken`, and each leg's claimed `amountIn`
    /// equal to the previous leg's claimed `amountOut`.
    ///
    /// Moves nothing. A route that fails here never reaches a transfer, which is what
    /// makes the planted negative controls cheap enough to run on a testnet.
    function _checkRoute(Leg[] calldata legs, address inputToken, uint256 amountIn) private view {
        uint256 count = legs.length;
        if (count == 0) revert NoLegs();
        if (count > MAX_LEGS) revert TooManyLegs(count);
        for (uint256 i = 0; i < count; i++) {
            _checkLeg(i, legs[i]);
        }
        if (legs[0].tokenIn != inputToken) revert BrokenContinuity(0, inputToken, legs[0].tokenIn);
        uint256 expected = amountIn;
        for (uint256 i = 0; i < count; i++) {
            Leg calldata leg = legs[i];
            // `amountIn` is what this leg puts into the pool: the call's `amountIn`
            // for leg 0, and the previous leg's ask afterwards.
            if (leg.amountIn != expected) revert AmountChainBroken(i, expected, leg.amountIn);
            if (i + 1 < count) {
                if (legs[i + 1].tokenIn != leg.tokenOut) {
                    revert BrokenContinuity(i + 1, leg.tokenOut, legs[i + 1].tokenIn);
                }
            } else if (leg.tokenOut != inputToken) {
                // The route has to come back to where it started. A→B→C is not a
                // completed arbitrage, it is an unhedged position (M10 §17).
                revert NotRoundTrip(inputToken, leg.tokenOut);
            }
            expected = leg.amountOut;
        }
    }

    /// Take `amountIn` of `inputToken` from the operator, and return what actually
    /// arrived — which has to be the whole amount or the call reverts.
    ///
    /// The balance and allowance reads before the transfer are what make NC6 and NC7
    /// report themselves as this contract's own failures instead of arriving as the
    /// token contract's error string, which no caller could classify uniformly.
    function _pullInput(address inputToken, uint256 amountIn) private returns (uint256 received) {
        IERC20 start = IERC20(inputToken);
        uint256 operatorHolding = start.balanceOf(msg.sender);
        if (operatorHolding < amountIn) revert InsufficientBalance(operatorHolding, amountIn);
        uint256 granted = start.allowance(msg.sender, address(this));
        if (granted < amountIn) revert InsufficientAllowance(granted, amountIn);

        uint256 beforeExecutor = start.balanceOf(address(this));
        start.transferFrom(msg.sender, address(this), amountIn);
        received = start.balanceOf(address(this)) - beforeExecutor;
        // A fee-on-transfer input token lands short of what was claimed. That is a
        // rejection here, not a smaller reserve-side number in the next line.
        if (received != amountIn) revert InputDeliveryMismatch(amountIn, received);
    }

    /// Hand the closing token to `recipient` and prove the final invariant there.
    ///
    /// `carried` is what the last leg delivered of `inputToken`. The guard is measured
    /// at the recipient rather than asserted from the arithmetic, because the transfer
    /// out is itself a transfer this contract does not trust (§11/§13). A non-zero
    /// `minFinalAmount` was required by the caller, so a run that delivers less than
    /// the plan's floor cannot silently be a run that asked for nothing.
    function _settle(
        address inputToken,
        uint256 carried,
        uint256 minFinalAmount,
        address recipient,
        uint256 count
    ) private returns (uint256 delivered) {
        IERC20 start = IERC20(inputToken);
        uint256 available = start.balanceOf(address(this));
        if (available != carried) revert HoldingMismatch(count, inputToken, available, carried);

        uint256 beforeRecipient = start.balanceOf(recipient);
        start.transfer(recipient, available);
        delivered = start.balanceOf(recipient) - beforeRecipient;
        if (delivered != available) revert PayoutMismatch(available, delivered);
        if (delivered < minFinalAmount) revert FinalShortfall(delivered, minFinalAmount);
    }

    /// One leg, in three steps that each keep their own locals.
    function _deliverLeg(uint256 index, Leg calldata leg, uint256 carried) private returns (uint256 received) {
        bool forward = _checkSides(leg);
        _pushInput(index, leg, carried);
        received = _pullOutput(index, leg, forward);
        emit LegExecuted(index, leg.pool, leg.tokenIn, leg.tokenOut, leg.amountIn, received);
    }

    /// Which output word this pool pays is the pair's answer, not this contract's
    /// sorting assumption — the same reason the Rust side asks `token0()` instead of
    /// ordering two addresses (M10 §16). A pool that is not this token pair fails
    /// here, before the input moves.
    function _checkSides(Leg calldata leg) private view returns (bool forward) {
        IV2Pair pool = IV2Pair(leg.pool);
        address t0 = pool.token0();
        address t1 = pool.token1();
        forward = (t0 == leg.tokenIn && t1 == leg.tokenOut);
        if (!forward && !(t0 == leg.tokenOut && t1 == leg.tokenIn)) {
            revert PoolSidesMismatch(leg.pool, leg.tokenIn, t0, t1);
        }
    }

    /// Send this leg's input to the pool, after proving the executor holds exactly the
    /// amount the plan claimed and that the pool received all of it.
    ///
    /// `carried` is what the previous step delivered; `held` is what the contract
    /// actually owns. Both have to equal `leg.amountIn`, and the delta at the pool has
    /// to equal it too — three numbers that agree only if the route is the one encoded.
    function _pushInput(uint256 index, Leg calldata leg, uint256 carried) private {
        IERC20 in_ = IERC20(leg.tokenIn);
        uint256 held = in_.balanceOf(address(this));
        // Exact equality, not "at least": leftover dust of a route token would mean
        // this contract's balance is no longer a fact about this transaction alone.
        if (held != leg.amountIn || carried != held) revert HoldingMismatch(index, leg.tokenIn, held, leg.amountIn);

        uint256 beforePool = in_.balanceOf(leg.pool);
        in_.transfer(leg.pool, leg.amountIn);
        uint256 arrived = in_.balanceOf(leg.pool) - beforePool;
        if (arrived != leg.amountIn) revert InputDeliveryMismatch(leg.amountIn, arrived);
    }

    /// Ask for the output and take delivery of it, then apply the two guards.
    ///
    /// `DeliveryMismatch` fires when the pool paid something other than what it was
    /// asked (a taxed or non-standard token); `LegShortfall` fires when a whole
    /// delivery is still below the plan's floor. They are different failures and the
    /// evidence has to tell them apart (§12/§21/§22).
    function _pullOutput(uint256 index, Leg calldata leg, bool forward) private returns (uint256 received) {
        IERC20 out = IERC20(leg.tokenOut);
        uint256 beforeOut = out.balanceOf(address(this));
        if (forward) {
            IV2Pair(leg.pool).swap(0, leg.amountOut, address(this), "");
        } else {
            IV2Pair(leg.pool).swap(leg.amountOut, 0, address(this), "");
        }
        received = out.balanceOf(address(this)) - beforeOut;
        if (received != leg.amountOut) revert DeliveryMismatch(index, leg.amountOut, received);
        if (received < leg.minAmountOut) revert LegShortfall(index, received, leg.minAmountOut);
    }

    /// Everything a leg has to satisfy on its own, before any neighbour or balance
    /// is consulted. Reads no state and moves nothing.
    function _checkLeg(uint256 index, Leg calldata leg) private view {
        if (leg.pool == address(0) || leg.tokenIn == address(0) || leg.tokenOut == address(0)) {
            revert ZeroAddress();
        }
        if (leg.amountIn == 0 || leg.amountOut == 0) revert ZeroAmount();
        if (leg.tokenIn == leg.tokenOut) revert LegSelfLoop(index, leg.tokenIn);
        if (!pairAllowed[leg.pool]) revert PairNotAllowed(leg.pool);
        if (!tokenAllowed[leg.tokenIn]) revert TokenNotAllowed(leg.tokenIn);
        if (!tokenAllowed[leg.tokenOut]) revert TokenNotAllowed(leg.tokenOut);
        // The plan's own ask has to sit on or above the plan's own floor; a plan that
        // asks for less than it demands is malformed rather than optimistic.
        if (leg.amountOut < leg.minAmountOut) revert AskBelowFloor(index, leg.amountOut, leg.minAmountOut);
    }

    // --------------------------------------------------------------------
    //  Rescue
    // --------------------------------------------------------------------

    /// Move a stuck balance out again.
    ///
    /// A successful `execute` leaves this contract holding zero of the route's
    /// tokens, and a reverted one leaves it holding nothing either — so anything
    /// reachable here got in by mistake (a token airdropped to the contract address,
    /// or dust left by a route this file's checks did not cover). It is operator-only,
    /// separately tested, and it cannot mint or create: it transfers what the
    /// contract genuinely holds, measured the same delta way as everywhere else
    /// (M10 §19).
    function withdraw(address token, address to, uint256 amount) external onlyOperator nonReentrant {
        if (token == address(0) || to == address(0)) revert ZeroAddress();
        if (amount == 0) revert ZeroAmount();
        IERC20 erc = IERC20(token);
        uint256 held = erc.balanceOf(address(this));
        if (held < amount) revert InsufficientBalance(held, amount);
        uint256 before = erc.balanceOf(to);
        erc.transfer(to, amount);
        uint256 moved = erc.balanceOf(to) - before;
        if (moved != amount) revert PayoutMismatch(amount, moved);
        emit Withdrawn(token, to, moved);
    }
}
