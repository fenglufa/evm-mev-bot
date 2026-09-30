//! §46's three answers.

use alloy_primitives::U256;
use serde::Serialize;

/// Which of §45's three checks a decision is about.
///
/// The rule is carried as data rather than left in the prose so a report can group
/// decisions by the check that produced them — "9 rejected, 3 of them on gas" is a
/// question this layer will be asked, and string-matching a reason for the answer
/// would be a question about wording.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum RiskRule {
    /// `simulation_success`: did the sequence execute at all.
    SimulationSuccess,
    /// `maximum_gas`: did the run measure more gas than the ceiling allows.
    MaximumGas,
    /// `minimum_net_profit`: is the net figure large enough — and does it exist.
    MinimumNetProfit,
}

impl RiskRule {
    pub const fn label(self) -> &'static str {
        match self {
            Self::SimulationSuccess => "simulation_success",
            Self::MaximumGas => "maximum_gas",
            Self::MinimumNetProfit => "minimum_net_profit",
        }
    }
}

/// §47, in the words the task insists on, attached to every answer this crate gives.
///
/// It is a constant and not a test assertion because there is nothing here to
/// assert: no type in this crate can name a transaction, a key or a node, so the
/// statement is about the shape of the layer rather than about a path through it.
pub const NO_BROADCAST: &str = "no broadcast: M4 simulates. An Accept here means the \
                               simulation satisfied the stated thresholds, not that a \
                               transaction was or may be sent.";

/// §46's answer to one simulation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum RiskDecision {
    /// Every check passed, with the numbers that passed them and the thresholds
    /// they were compared against. The two are kept together on purpose: an Accept
    /// that does not say what it was measured against cannot be re-checked after
    /// the thresholds move.
    Accept {
        net_profit_wei: U256,
        gross_profit_wei: U256,
        gas_cost_wei: U256,
        gas_used: u64,
        minimum_net_profit_wei: U256,
        maximum_gas: u64,
        reason: String,
    },
    /// A check was answered and failed. §46's `simulation reverted → Reject`.
    Reject { rule: RiskRule, reason: String },
    /// A check could not be answered because a fact is missing — in practice §31's
    /// `NotComputable` net figure, which is neither a pass nor a fail. Turning it
    /// into a Reject would claim the run lost money; turning it into an Accept would
    /// claim it made some.
    Unknown { rule: RiskRule, reason: String },
}

impl RiskDecision {
    pub const fn accepted(&self) -> bool {
        matches!(self, Self::Accept { .. })
    }

    /// The check this decision is about, for the two answers that name one.
    pub fn rule(&self) -> Option<RiskRule> {
        match self {
            Self::Accept { .. } => None,
            Self::Reject { rule, .. } | Self::Unknown { rule, .. } => Some(*rule),
        }
    }

    pub fn reason(&self) -> &str {
        match self {
            Self::Accept { reason, .. }
            | Self::Reject { reason, .. }
            | Self::Unknown { reason, .. } => reason,
        }
    }

    pub fn net_profit_wei(&self) -> Option<U256> {
        match self {
            Self::Accept { net_profit_wei, .. } => Some(*net_profit_wei),
            _ => None,
        }
    }
}

impl std::fmt::Display for RiskDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (label, rule) = match self {
            Self::Accept { .. } => ("Accept", None),
            Self::Reject { rule, .. } => ("Reject", Some(*rule)),
            Self::Unknown { rule, .. } => ("Unknown", Some(*rule)),
        };
        match rule {
            Some(rule) => write!(f, "{label} on {}: {}", rule.label(), self.reason()),
            None => write!(f, "{label}: {}", self.reason()),
        }
    }
}
