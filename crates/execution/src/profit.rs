//! §11–§14/§38–§39/§56: what the wallet actually gained, in one stated denomination,
//! recomputable by a reader who has nothing but the evidence.
//!
//! The task book is unusually specific here, and each requirement removes a way of being
//! wrong that the previous milestone could have slipped into:
//!
//! * **§11** — `balance_after - balance_before` alone is not an answer. The input asset,
//!   the output asset and the two halves of the bill are separate lines, so a reader can
//!   see which one moved.
//! * **§12** — profit is `gross output − input − all execution costs`. "All" includes the
//!   L1 charge [`crate::cost`] now carries as a line of its own.
//! * **§13/§14** — one denomination, chosen up front. When the round trip settles back into
//!   native ETH the three quantities are already the same asset and the net is provable;
//!   when it settles in an ERC-20, gas and the L1 fee are ETH and §15 forbids inventing a
//!   price to join them, so the evidence reports both halves and states that the single
//!   net is *not* proven rather than producing one anyway.
//! * **§39** — the file must show `A + B − C − D = E`, and here the equation predicts the
//!   *after* balance from independent reads. A prediction that misses is not rounded,
//!   explained away, or published as a profit: it becomes `Inconclusive`, because the two
//!   halves of the evidence no longer describe the same event.
//! * **§56** — only `VerifiedPositive` counts as a successful real arbitrage.
//!
//! Nothing in this module reads a node or holds a cache (§40). Every number arrives as a
//! value that was read: a snapshot at a block named by hash, a bill from a bound receipt,
//! an input and an output measured from transaction logs. The snapshots carry their block
//! hash because §19 forbids accounting against "latest" — a before/after pair read at two
//! unfinalised heights is a difference between two guesses.

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256, I256, U256};
use serde::{Deserialize, Serialize};

use crate::cost::SequenceCost;
use crate::error::{ExecutionError, Result};

/// What one account held, at one block, as read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetSnapshot {
    pub block_number: u64,
    /// The hash of `block_number`, from the same read — the block the balances are
    /// stated against is pinned, not named by height alone.
    pub block_hash: B256,
    pub account: Address,
    pub native_wei: U256,
    /// The ERC-20 balances the run touches. Empty is a measurement: a round trip that
    /// leaves nothing behind is exactly what the route audit expects to see here.
    pub token_balances: BTreeMap<Address, U256>,
    /// Which methods produced this snapshot, so the evidence can be re-read by someone
    /// who is not running this binary.
    pub provenance: String,
}

impl AssetSnapshot {
    pub fn token_balance(&self, token: &Address) -> U256 {
        self.token_balances
            .get(token)
            .copied()
            .unwrap_or(U256::ZERO)
    }

    /// Whether this snapshot actually read the token, as opposed to answering zero because the
    /// read never happened. The two are different facts, and an audit that cannot tell them
    /// apart turns a missing measurement into a pass.
    pub fn balance_read(&self, token: &Address) -> bool {
        self.token_balances.contains_key(token)
    }
}

/// The two reads that bracket a sequence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceDelta {
    pub before: AssetSnapshot,
    pub after: AssetSnapshot,
}

impl BalanceDelta {
    /// A before/after pair must describe the same account. Two wallets' balances have a
    /// difference too, and calling it a delta is how one account's spending gets charged
    /// to another's profit.
    pub fn new(before: AssetSnapshot, after: AssetSnapshot) -> Result<Self> {
        if before.account != after.account {
            return Err(ExecutionError::Evidence(format!(
                "the before snapshot is for {} and the after snapshot for {}; a balance \
                 delta only means anything across one account",
                before.account, after.account
            )));
        }
        if before.block_number > after.block_number {
            return Err(ExecutionError::Evidence(format!(
                "the 'after' snapshot is at block {} and the 'before' at block {}; time \
                 cannot run backwards in the evidence, and a pair this assembled is a \
                 mislabelled read rather than a result",
                after.block_number, before.block_number
            )));
        }
        Ok(Self { before, after })
    }

    pub fn native_delta(&self) -> I256 {
        signed_difference(self.after.native_wei, self.before.native_wei)
    }

    pub fn token_delta(&self, token: &Address) -> I256 {
        signed_difference(
            self.after.token_balance(token),
            self.before.token_balance(token),
        )
    }
}

/// §14's rule made a type: the unit is chosen before the arithmetic, and it is stated on
/// every number that follows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum ProfitDenomination {
    /// The sequence begins in native ETH and ends in native ETH — M7's shape, reached by
    /// `deposit()` at the front and `withdraw()` at the back, both executed on the WETH
    /// contract itself. Gas, the L1 fee and the round-trip asset are then the same unit
    /// with no conversion step, so §15's ban on external prices costs nothing: there is no
    /// price to look up.
    NativeWei,
    /// The sequence settles in an ERC-20 while the bill is paid in ETH. §13's
    /// `token_profit`, `native_gas_cost` and `native_l1_fee` are all recorded; their
    /// difference is not, because producing it would require a price this repository is
    /// not allowed to invent (§15).
    TokenSettled { token: Address, reason: String },
}

impl ProfitDenomination {
    pub fn describes_one_unit(&self) -> bool {
        matches!(self, Self::NativeWei)
    }

    pub fn describe(&self) -> String {
        match self {
            Self::NativeWei => "wei — the round trip starts and ends in native ETH, so \
                                profit and cost are already the same unit"
                .to_string(),
            Self::TokenSettled { token, reason } => format!(
                "split: profit in {} and cost in native ETH; no single-denomination net is \
                 claimed ({reason})",
                token
            ),
        }
    }
}

/// §11's separate lines and §38's evidence fields, in one record so a reader cannot get a
/// gross without the cost that produced it.
///
/// Two names in the task book refer to one number here: §12's `realized_profit` is §38's
/// `net_profit`, and the field is called `net_profit`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealizedProfit {
    pub denomination: ProfitDenomination,

    // §38's balance pair, and §11's native line.
    pub initial_balance: U256,
    pub final_balance: U256,
    pub native_balance_delta: I256,

    // §38's token pair: the asset the round trip passed through, as it sits in the wallet
    // before and after. Both zero is the expected shape of a closed cycle.
    pub initial_token_balance: U256,
    pub final_token_balance: U256,

    // §11's asset-side lines, signed in the direction the wallet experienced them.
    pub input_asset_delta: I256,
    pub output_asset_delta: I256,

    // §11's two cost lines, and their sum.
    pub gas_cost: U256,
    pub l1_fee: U256,
    pub total_execution_cost: U256,

    /// `gross output − input amount`, before any cost (§12's first subtraction).
    pub gross_profit: I256,
    /// `gross output − input amount − all execution costs` (§12), or `None` when the two
    /// halves are in different units (§14's unprovable case). A missing number here is the
    /// honest rendering; a fabricated one is what §14 and §15 forbid.
    pub net_profit: Option<I256>,
}

/// One side of §39's equation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfitTerm {
    pub label: String,
    pub sign: ProfitSign,
    pub amount: U256,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfitSign {
    Add,
    Subtract,
}

impl ProfitSign {
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Subtract => "-",
        }
    }
}

/// §39: the arithmetic a third party can repeat with the numbers in the file.
///
/// The equation predicts the *after* balance from the before balance and the independently
/// measured quantities (what left the wallet, what came back, what was charged). §57's P
/// then needs only the evidence, and `checks_out` is where the prediction meets the read.
/// A mismatch is reported, not repaired: it is §18's `ExecutionMismatch` finding arriving
/// in the accounting layer rather than in a log line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfitEquation {
    pub terms: Vec<ProfitTerm>,
    pub predicted_final_balance: I256,
    pub observed_final_balance: I256,
    pub checks_out: bool,
}

impl ProfitEquation {
    /// The sum of the terms, recomputed here rather than trusted from the caller: a report
    /// that prints `A + B − C − D = E` has to be able to show that it equals E.
    pub fn evaluate(terms: &[ProfitTerm]) -> Result<I256> {
        let mut running = I256::ZERO;
        for term in terms {
            let amount = I256::from_raw(term.amount);
            running = match term.sign {
                ProfitSign::Add => running.checked_add(amount),
                ProfitSign::Subtract => running.checked_sub(amount),
            }
            .ok_or_else(|| {
                ExecutionError::Evidence(format!(
                    "the term `{}` ({}) overflows the running equation total",
                    term.label, term.amount
                ))
            })?;
        }
        Ok(running)
    }

    /// §39's rendering: one line, every term named, no derived number missing.
    pub fn render(&self) -> String {
        let mut line = String::new();
        for (index, term) in self.terms.iter().enumerate() {
            if index == 0 {
                line.push_str(&format!("{} {}", term.sign.symbol(), term.amount));
            } else {
                line.push_str(&format!(" {} {}", term.sign.symbol(), term.amount));
            }
        }
        format!(
            "{line} = {} (predicted); the wallet's balance read afterwards was {}",
            self.predicted_final_balance, self.observed_final_balance
        )
    }

    /// The same equation as a labelled block for the report's evidence section.
    pub fn render_labelled(&self) -> Vec<String> {
        self.terms
            .iter()
            .map(|term| format!("{} {} — {}", term.sign.symbol(), term.amount, term.label))
            .collect()
    }
}

/// §56's four answers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfitVerificationStatus {
    /// The sequence has not been accounted for yet — either it is still in flight or the
    /// after-read has not been taken. §54's ladder stops short of `ProfitVerified` here.
    #[default]
    Pending,
    /// Net positive, costs fully measured, and the equation agrees with the balance read.
    /// The only status that may be counted (§56).
    VerifiedPositive,
    /// Net zero or negative, with the same completeness behind it. A proven loss is not
    /// `Inconclusive`: §34 requires failed arbitrages to be recorded in full, and this is
    /// what a full record of a loss looks like.
    VerifiedNegative,
    /// Something needed for a claim is missing: an unmeasured L1 line, an equation that
    /// does not match the balances, or a settlement asset that cannot be netted against
    /// its own costs (§14).
    Inconclusive,
}

impl ProfitVerificationStatus {
    /// §56, verbatim in code: only a verified positive may be counted as a successful real
    /// arbitrage.
    pub fn counts_as_successful_real_arbitrage(self) -> bool {
        self == Self::VerifiedPositive
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::VerifiedPositive => "verified_positive",
            Self::VerifiedNegative => "verified_negative",
            Self::Inconclusive => "inconclusive",
        }
    }

    /// §34's rule that a failure gets a complete record: `Pending` and `Inconclusive` are
    /// the two statuses that are not yet a finding, and only these may be superseded.
    pub fn is_final(self) -> bool {
        matches!(self, Self::VerifiedPositive | Self::VerifiedNegative)
    }

    /// The decision, from the three things that can be checked: was every wei of the bill
    /// read, does the equation agree with the balance pair, and which way does the net
    /// point. A missing net (§14) is `Inconclusive` regardless of the other two.
    pub fn decide(
        costs_fully_measured: bool,
        equation_checks_out: bool,
        net_profit: Option<I256>,
    ) -> Self {
        match net_profit {
            _ if !costs_fully_measured || !equation_checks_out => Self::Inconclusive,
            Some(net) if net.is_positive() && !net.is_zero() => Self::VerifiedPositive,
            Some(_) => Self::VerifiedNegative,
            None => Self::Inconclusive,
        }
    }
}

/// §38's evidence block, assembled from two reads and the bills between them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfitEvidence {
    pub opportunity_id: String,
    pub account: Address,
    pub block_before: u64,
    pub block_after: u64,
    pub before: AssetSnapshot,
    pub after: AssetSnapshot,
    /// Every transaction this accounting covers, with the per-transaction bill (§37) that
    /// produced the cost totals below.
    pub cost: SequenceCost,
    #[serde(flatten)]
    pub realized: RealizedProfit,
    /// §39's recomputation. `None` when the evidence makes no single-denomination claim,
    /// since there is then no asserted number for a reader to recompute (§14).
    pub equation: Option<ProfitEquation>,
    pub status: ProfitVerificationStatus,
    pub source: String,
}

impl ProfitEvidence {
    /// The native-funded, native-settled round trip: §12's formula evaluated on the one
    /// account that paid for everything and received everything back.
    ///
    /// `input_native_wei` and `gross_output_native_wei` must be measured from what the
    /// transactions actually moved — for a sequence whose first transaction reverted, the
    /// value that left the wallet is zero, and passing the *planned* amount instead would
    /// make the equation predict a balance that could not have occurred.
    pub fn from_native_round_trip(
        opportunity_id: &str,
        delta: &BalanceDelta,
        cost: &SequenceCost,
        settled_token: Address,
        input_native_wei: U256,
        gross_output_native_wei: U256,
    ) -> Result<Self> {
        Self::assemble(
            opportunity_id,
            delta,
            cost,
            settled_token,
            input_native_wei,
            gross_output_native_wei,
            ProfitDenomination::NativeWei,
        )
    }

    /// The ERC-20-settled round trip (§13/§14's case that cannot be netted).
    ///
    /// `input_token_amount` and `gross_output_token_amount` are token units; the cost lines
    /// are wei. Both halves are recorded and the net is left absent, because the only way
    /// to add them is a price, and §15 forbids this repository from producing one.
    pub fn from_token_round_trip(
        opportunity_id: &str,
        delta: &BalanceDelta,
        cost: &SequenceCost,
        token: Address,
        reason: &str,
        input_token_amount: U256,
        gross_output_token_amount: U256,
    ) -> Result<Self> {
        Self::assemble(
            opportunity_id,
            delta,
            cost,
            token,
            input_token_amount,
            gross_output_token_amount,
            ProfitDenomination::TokenSettled {
                token,
                reason: reason.to_string(),
            },
        )
    }

    fn assemble(
        opportunity_id: &str,
        delta: &BalanceDelta,
        cost: &SequenceCost,
        settled_token: Address,
        input_amount: U256,
        gross_output_amount: U256,
        denomination: ProfitDenomination,
    ) -> Result<Self> {
        let gross_profit = signed_difference(gross_output_amount, input_amount);
        let total_execution_cost = cost.total_execution_cost;
        let native_balance_delta = delta.native_delta();

        // In the native case §12's result is the wallet's own difference, and in the token
        // case there is no single-unit result to state — the two branches are the whole of
        // §13's warning about treating a token profit and an ETH cost as one number. The
        // equation below is what checks §12's formula against that difference: an
        // disagreement is recorded rather than smoothed over.
        let net_profit = match &denomination {
            ProfitDenomination::NativeWei => Some(native_balance_delta),
            ProfitDenomination::TokenSettled { .. } => None,
        };

        let terms = vec![
            ProfitTerm {
                label: format!(
                    "A — native balance read before the sequence, at block {}",
                    delta.before.block_number
                ),
                sign: ProfitSign::Add,
                amount: delta.before.native_wei,
            },
            ProfitTerm {
                label: "B — native handed back by the executed settle (gross output, \
                        measured from the transaction logs)"
                    .to_string(),
                sign: ProfitSign::Add,
                amount: gross_output_amount,
            },
            ProfitTerm {
                label: "C — native put into the round trip (input, measured from the \
                        executed step that spent it)"
                    .to_string(),
                sign: ProfitSign::Subtract,
                amount: input_amount,
            },
            ProfitTerm {
                label: format!(
                    "D — L2 gas charged across {} transaction(s)",
                    cost.transaction_count
                ),
                sign: ProfitSign::Subtract,
                amount: cost.l2_fee_total,
            },
            ProfitTerm {
                label: format!(
                    "E — L1 data fee charged across the same {} transaction(s)",
                    cost.transaction_count
                ),
                sign: ProfitSign::Subtract,
                amount: cost.l1_fee_total,
            },
        ];
        let predicted_final_balance = if denomination.describes_one_unit() {
            Some(ProfitEquation::evaluate(&terms)?)
        } else {
            // The terms are in two units; predicting a native balance from them would be
            // the unit error, so no equation is offered and none is needed.
            None
        };
        let equation = predicted_final_balance.map(|predicted| ProfitEquation {
            observed_final_balance: I256::from_raw(delta.after.native_wei),
            checks_out: predicted == I256::from_raw(delta.after.native_wei),
            predicted_final_balance: predicted,
            terms,
        });

        let realized = RealizedProfit {
            denomination,
            initial_balance: delta.before.native_wei,
            final_balance: delta.after.native_wei,
            native_balance_delta,
            initial_token_balance: delta.before.token_balance(&settled_token),
            final_token_balance: delta.after.token_balance(&settled_token),
            input_asset_delta: -I256::from_raw(input_amount),
            output_asset_delta: I256::from_raw(gross_output_amount),
            gas_cost: cost.l2_fee_total,
            l1_fee: cost.l1_fee_total,
            total_execution_cost,
            gross_profit,
            net_profit,
        };
        let status = ProfitVerificationStatus::decide(
            cost.is_fully_measured(),
            equation.as_ref().is_some_and(|e| e.checks_out),
            realized.net_profit,
        );
        Ok(Self {
            opportunity_id: opportunity_id.to_string(),
            account: delta.before.account,
            block_before: delta.before.block_number,
            block_after: delta.after.block_number,
            before: delta.before.clone(),
            after: delta.after.clone(),
            cost: cost.clone(),
            realized,
            equation,
            status,
            source: format!(
                "balances from {} and {}; costs from {} bound receipt(s): {}",
                delta.before.provenance,
                delta.after.provenance,
                cost.transaction_count,
                cost.describe_l1_sources().join(" | ")
            ),
        })
    }

    /// Whether this evidence may appear as a successful real arbitrage in the metrics and
    /// the report's headline (§56/§57 N).
    pub fn counts_as_successful_real_arbitrage(&self) -> bool {
        self.status.counts_as_successful_real_arbitrage()
    }

    /// One line for the log and the metrics dump: the status, the net, and why it is not
    /// more than that when it is `Inconclusive`.
    pub fn describe(&self) -> String {
        let net = match self.realized.net_profit {
            Some(net) => net.to_string(),
            None => format!(
                "not provable in one denomination ({})",
                self.realized.denomination.describe()
            ),
        };
        match self
            .equation
            .as_ref()
            .filter(|equation| !equation.checks_out)
        {
            Some(equation) => format!(
                "{}: net {net} — but the equation does not close: {}",
                self.status.name(),
                equation.render()
            ),
            None => format!("{}: net {net}", self.status.name()),
        }
    }
}

/// `after − before` without a wrapping subtraction: a balance can fall as well as rise,
/// and §34 requires the fall to be recorded with the same precision as the gain.
fn signed_difference(after: U256, before: U256) -> I256 {
    if after >= before {
        I256::from_raw(after - before)
    } else {
        -I256::from_raw(before - after)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::ExecutionCostEvidence;
    use crate::receipt::Receipt;
    use alloy_primitives::b256;

    const BEFORE: u128 = 20_000_000_000_000_000;
    const INPUT: u128 = 100_000_000_000_000;
    /// Six transactions at the bill M6's receipt actually carried: L2 45 320 774 280 and
    /// L1 7 378 574 336 each, so the sequence total is 316 196 091 696 wei — which is why
    /// the fixtures below are chosen around that number rather than at a round one.
    const L2_TOTAL: u128 = 6 * 45_320_774_280;
    const L1_TOTAL: u128 = 6 * 7_378_574_336;
    const COST_TOTAL: u128 = L2_TOTAL + L1_TOTAL;

    fn snapshot(
        account: u8,
        block_number: u64,
        native: u128,
        tokens: &[(u8, u128)],
    ) -> AssetSnapshot {
        AssetSnapshot {
            block_number,
            block_hash: b256!("000000000000000000000000000000000000000000000000000000000000002a"),
            account: Address::with_last_byte(account),
            native_wei: U256::from(native),
            token_balances: tokens
                .iter()
                .map(|(byte, amount)| (Address::with_last_byte(*byte), U256::from(*amount)))
                .collect(),
            provenance: format!("eth_getBalance + eth_getBalance at block {block_number}"),
        }
    }

    fn cost(l1_charged: bool) -> SequenceCost {
        let lines = (0..6u8)
            .map(|index| {
                let mut receipt = Receipt {
                    transaction_hash: B256::left_padding_from(&[index + 1]),
                    block_number: 37_500_000 + index as u64,
                    block_hash: b256!(
                        "000000000000000000000000000000000000000000000000000000000000002b"
                    ),
                    transaction_index: 0,
                    success: true,
                    gas_used: 47_270,
                    effective_gas_price: U256::from(958_764u64),
                    cumulative_gas_used: None,
                    from: Address::with_last_byte(1),
                    to: Some(Address::with_last_byte(2)),
                    contract_address: None,
                    tx_type: Some(2),
                    logs: Vec::new(),
                    l1_fee: l1_charged.then_some(U256::from(7_378_574_336u64)),
                    l1_gas_price: None,
                    l1_gas_used: None,
                    l1_base_fee_scalar: None,
                    l1_blob_base_fee: None,
                    l1_blob_base_fee_scalar: None,
                    provenance: "eth_getTransactionReceipt (test)".to_string(),
                };
                receipt.transaction_index = index as u64;
                ExecutionCostEvidence::from_receipt(&receipt).unwrap()
            })
            .collect::<Vec<_>>();
        SequenceCost::new(lines).unwrap()
    }

    /// `gross` is the native amount the sequence handed back before costs. The after
    /// balance is derived from the cost the run was actually *billed for in the evidence*,
    /// so a receipt with no `l1Fee` field isolates the missing measurement rather than
    /// also disagreeing with the balances.
    fn evidence(gross: u128, l1_charged: bool) -> ProfitEvidence {
        let billed = if l1_charged { COST_TOTAL } else { L2_TOTAL };
        let weth = Address::with_last_byte(0x06);
        let delta = BalanceDelta::new(
            snapshot(1, 37_530_593, BEFORE, &[(0x06, 0)]),
            snapshot(1, 37_530_599, BEFORE - INPUT + gross - billed, &[(0x06, 0)]),
        )
        .unwrap();
        ProfitEvidence::from_native_round_trip(
            "opportunity-under-test",
            &delta,
            &cost(l1_charged),
            weth,
            U256::from(INPUT),
            U256::from(gross),
        )
        .unwrap()
    }

    #[test]
    fn a_profitable_native_round_trip_is_the_balance_difference_after_both_bills() {
        // 400 000 000 000 of gross token gain against 316 196 091 696 of measured cost.
        let run = evidence(INPUT + 400_000_000_000, true);
        assert_eq!(
            run.realized.gross_profit,
            I256::from_raw(U256::from(400_000_000_000u64))
        );
        assert_eq!(
            run.realized.net_profit,
            Some(I256::from_raw(U256::from(83_803_908_304u64)))
        );
        assert_eq!(run.realized.gas_cost, U256::from(L2_TOTAL));
        assert_eq!(run.realized.l1_fee, U256::from(L1_TOTAL));
        assert_eq!(run.realized.total_execution_cost, U256::from(COST_TOTAL));
        assert_eq!(run.status, ProfitVerificationStatus::VerifiedPositive);
        assert!(run.counts_as_successful_real_arbitrage());
        assert_eq!(run.block_before, 37_530_593);
        assert_eq!(run.block_after, 37_530_599);
        assert!(run.equation.as_ref().unwrap().checks_out);
    }

    #[test]
    fn an_unmeasured_l1_line_cannot_certify_a_profit_however_good_the_numbers_look() {
        // The same balances and the same gross, with the receipts silent about `l1Fee`. §35
        // says this is exactly where a zero must not be assumed, so §56's positive answer
        // is unavailable even though the run gained wei.
        let run = evidence(INPUT + 400_000_000_000, false);
        assert_eq!(run.realized.l1_fee, U256::ZERO);
        assert!(!run.cost.is_fully_measured());
        assert_eq!(run.status, ProfitVerificationStatus::Inconclusive);
        assert!(!run.counts_as_successful_real_arbitrage());
        // The balances and the bill agree here — the only thing missing is the L1
        // measurement itself, and that alone is enough to withhold the claim.
        assert!(run.equation.as_ref().unwrap().checks_out);
        assert!(
            run.describe().contains("inconclusive"),
            "{}",
            run.describe()
        );
    }

    #[test]
    fn a_measured_loss_is_verified_negative_and_not_a_missing_answer() {
        // 200 000 000 000 gained on the legs, 316 196 091 696 charged: a real loss of
        // 116 196 091 696. §34 demands the full record and §56 demands it not be counted —
        // `Inconclusive` would be the softer, wrong answer here.
        let run = evidence(INPUT + 200_000_000_000, true);
        assert_eq!(
            run.realized.net_profit,
            Some(-I256::from_raw(U256::from(116_196_091_696u64)))
        );
        assert_eq!(run.status, ProfitVerificationStatus::VerifiedNegative);
        assert!(run.status.is_final());
        assert!(!run.counts_as_successful_real_arbitrage());
    }

    #[test]
    fn a_breakeven_run_is_not_a_success() {
        // Only a strictly positive net counts (§57 N). At exactly the cost total the
        // balance delta is zero and §12's formula agrees, so this is a verified negative —
        // the strongest honest statement available.
        let run = evidence(INPUT + COST_TOTAL, true);
        assert_eq!(run.realized.net_profit, Some(I256::ZERO));
        assert_eq!(run.status, ProfitVerificationStatus::VerifiedNegative);
    }

    #[test]
    fn output_and_balances_that_disagree_produce_no_claim_at_all() {
        // The logs say 400 000 000 000 of gross gain; the balance pair says the wallet lost
        // 1 wei instead. Neither read is discarded and neither is averaged: the equation
        // reports the disagreement and the status stops being a verified one.
        let delta = BalanceDelta::new(
            snapshot(1, 100, BEFORE, &[]),
            snapshot(1, 101, BEFORE - 1, &[]),
        )
        .unwrap();
        let run = ProfitEvidence::from_native_round_trip(
            "mismatched",
            &delta,
            &cost(true),
            Address::with_last_byte(0x06),
            U256::from(INPUT),
            U256::from(INPUT + 400_000_000_000),
        )
        .unwrap();
        let equation = run.equation.as_ref().unwrap();
        assert!(!equation.checks_out);
        assert_eq!(run.status, ProfitVerificationStatus::Inconclusive);
        assert!(!run.counts_as_successful_real_arbitrage());
        assert!(
            run.describe().contains("does not close"),
            "{}",
            run.describe()
        );
        // The prediction and the observation are both kept visible, so the report can say
        // which one it is refusing to publish.
        assert_ne!(
            equation.predicted_final_balance,
            equation.observed_final_balance
        );
    }

    #[test]
    fn the_equation_states_its_own_terms_in_the_order_a_reader_can_add_them() {
        let run = evidence(INPUT + 400_000_000_000, true);
        let equation = run.equation.as_ref().unwrap();
        assert_eq!(equation.terms.len(), 5);
        let rendered = equation.render();
        for amount in [
            BEFORE.to_string(),
            (INPUT + 400_000_000_000).to_string(),
            INPUT.to_string(),
            L2_TOTAL.to_string(),
            L1_TOTAL.to_string(),
        ] {
            assert!(rendered.contains(&amount), "{rendered} lacks {amount}");
        }
        // Recomputing the terms must give the prediction the file prints — a reader with
        // only the evidence text can do exactly this (§57 P).
        assert_eq!(
            ProfitEquation::evaluate(&equation.terms).unwrap(),
            equation.predicted_final_balance
        );
        assert_eq!(equation.render_labelled().len(), 5);
    }

    #[test]
    fn a_token_settled_run_reports_both_halves_and_nets_neither() {
        // §13/§14's fallback: 1 000 of the token gained while the wallet spent ETH. The
        // token number and the wei number are both recorded; no price joins them, so the
        // single-denomination net is absent and §56's positive is unreachable.
        let token = Address::with_last_byte(0xee);
        let delta = BalanceDelta::new(
            snapshot(1, 100, BEFORE, &[(0xee, 5_000)]),
            snapshot(1, 101, BEFORE - COST_TOTAL, &[(0xee, 6_000)]),
        )
        .unwrap();
        let run = ProfitEvidence::from_token_round_trip(
            "token-settled",
            &delta,
            &cost(true),
            token,
            "the route never unwraps, so the gain is in the token and the bill is in ETH",
            U256::from(5_000u64),
            U256::from(6_000u64),
        )
        .unwrap();
        assert_eq!(run.realized.net_profit, None);
        assert_eq!(run.equation, None);
        assert_eq!(
            run.realized.gross_profit,
            I256::from_raw(U256::from(1_000u64))
        );
        assert_eq!(run.realized.gas_cost, U256::from(L2_TOTAL));
        assert_eq!(run.status, ProfitVerificationStatus::Inconclusive);
        assert!(!run.counts_as_successful_real_arbitrage());
        assert!(run
            .realized
            .denomination
            .describe()
            .contains("no single-denomination net"));
    }

    #[test]
    fn the_closed_round_trip_leaves_no_token_behind_and_the_evidence_shows_it() {
        let run = evidence(INPUT + 400_000_000_000, true);
        assert_eq!(run.realized.initial_token_balance, U256::ZERO);
        assert_eq!(run.realized.final_token_balance, U256::ZERO);
        // The input left and the output arrived, as separate signed lines (§11) rather
        // than as one net the reader has to take on trust.
        assert_eq!(
            run.realized.input_asset_delta,
            -I256::from_raw(U256::from(INPUT))
        );
        assert_eq!(
            run.realized.output_asset_delta,
            I256::from_raw(U256::from(INPUT + 400_000_000_000))
        );
    }

    #[test]
    fn a_delta_between_two_accounts_or_backwards_in_time_is_refused() {
        let error = BalanceDelta::new(snapshot(1, 100, BEFORE, &[]), snapshot(2, 101, BEFORE, &[]))
            .unwrap_err();
        assert!(matches!(error, ExecutionError::Evidence(_)), "{error}");

        let backwards =
            BalanceDelta::new(snapshot(1, 101, BEFORE, &[]), snapshot(1, 100, BEFORE, &[]))
                .unwrap_err();
        assert!(
            backwards.to_string().contains("time cannot run backwards"),
            "{backwards}"
        );
    }

    #[test]
    fn only_verified_positive_is_counted_and_pending_is_not_final() {
        for status in [
            ProfitVerificationStatus::Pending,
            ProfitVerificationStatus::Inconclusive,
            ProfitVerificationStatus::VerifiedNegative,
        ] {
            assert!(!status.counts_as_successful_real_arbitrage());
        }
        assert!(ProfitVerificationStatus::VerifiedPositive.counts_as_successful_real_arbitrage());
        assert!(!ProfitVerificationStatus::Pending.is_final());
        assert!(!ProfitVerificationStatus::Inconclusive.is_final());
        assert_eq!(
            ProfitVerificationStatus::default(),
            ProfitVerificationStatus::Pending
        );
    }
}
