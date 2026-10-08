//! §32: the last question before a transaction may be sent, asked of facts that were
//! read elsewhere.
//!
//! The gate is pure — no `async`, no endpoint, no state engine. That shape is the
//! point: every one of §32's seven conditions is a *read* the caller has to have
//! performed (simulation success, risk decision, staleness, chain id, block binding,
//! balance, nonce), and a gate that could fetch them itself would also be able to fetch
//! them at a moment that no longer matches the intent. Forcing the reads to arrive as
//! arguments means the values that decided the gate are the same values that can be
//! written into evidence and re-checked later.
//!
//! Three of the seven checks can come back "unread". An unread fact is a *failure*, not
//! a pass: §32 says "任何一个不满足 → No submission", and a check that silently treats
//! "we never looked" as satisfied would turn the gate into a formality. This is the same
//! rule the task book applies to receipts (§27) and to absent fields (§51).

use alloy_primitives::{B256, U256};
use serde::{Deserialize, Serialize};

use crate::error::ExecutionError;

/// §32's list, one variant per condition, in the order they are evaluated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateCheck {
    /// `simulation.success == true`.
    SimulationSucceeded,
    /// `risk.decision == Accept`.
    RiskAccepted,
    /// The opportunity has not gone stale (§31, carried in from M5's lifecycle).
    OpportunityFresh,
    /// `configured_chain_id == rpc_chain_id == intent.chain_id` (§6).
    ChainMatches,
    /// The block the intent pins is still the block the chain names at that height.
    BlockBindingValid,
    /// The real on-chain balance covers the transaction's maximum spend (§33).
    BalanceSufficient,
    /// The nonce the intent carries is still the nonce the chain would accept (§11).
    NonceValid,
    /// §35's one requirement for a validation transaction: it names itself. This is not
    /// an eighth arbitrage condition — it is the leg that stands in for the three an
    /// arbitrage answers, and it exists only for [`GateAttempt::Validation`].
    Labelled,
    /// §36/§7: M10's plan leg, standing in for `risk_accepted` on
    /// [`GateAttempt::Executor`]. The contract cannot see either binding (a chain id is not an
    /// opcode argument and a deployed address is not a guard), so this is the only place in the
    /// ladder where "this plan is about *this* deployment" is re-checked against facts the gate
    /// was handed rather than against the caller's memory.
    PlanValid,
}

impl GateCheck {
    pub fn name(self) -> &'static str {
        match self {
            Self::SimulationSucceeded => "simulation_succeeded",
            Self::RiskAccepted => "risk_accepted",
            Self::OpportunityFresh => "opportunity_fresh",
            Self::ChainMatches => "chain_matches",
            Self::BlockBindingValid => "block_binding_valid",
            Self::BalanceSufficient => "balance_sufficient",
            Self::NonceValid => "nonce_valid",
            Self::Labelled => "labelled",
            Self::PlanValid => "plan_valid",
        }
    }
}

/// Whether the opportunity is still an allowed execution state (§31).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Freshness {
    /// M5's lifecycle says the tracked opportunity is still active.
    Active,
    /// It went stale, with the reason M5 recorded. Anything here stops submission.
    Stale { reason: String },
    /// Nobody asked. The gate cannot pass on the absence of a check.
    Unknown,
}

/// Whether the pinned block is still canonical.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockBinding {
    /// The endpoint was asked for the intent's block *number* and returned the hash the
    /// intent pins.
    Confirmed { number: u64, hash: B256 },
    /// The same number now holds a different hash: a reorg moved under the intent.
    Reorged {
        number: u64,
        pinned: B256,
        found: B256,
    },
    /// Not read, or the read failed.
    Unverified(String),
}

/// Whether the sender's real balance covers the transaction (§33).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BalanceEvidence {
    /// `available >= maximum_spend`, both read from the chain, both kept for evidence.
    /// `native` says which account the read was made against, since an M4 run funded the
    /// sender by state override and that number is not this one (§34).
    Sufficient {
        available_wei: U256,
        maximum_spend_wei: U256,
        source: String,
    },
    Insufficient {
        available_wei: U256,
        maximum_spend_wei: U256,
        source: String,
    },
    Unverified(String),
}

/// Whether the nonce is still the one the chain would accept (§11).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NonceEvidence {
    /// The pending view equals the nonce the intent carries.
    Matches {
        nonce: u64,
        source: String,
    },
    /// It does not — something else has been sent from this account.
    Differs {
        intent_nonce: u64,
        pending_nonce: u64,
        source: String,
    },
    Unverified(String),
}

/// §7/§36: whether the plan this attempt carries is about *this* deployment.
///
/// The other legs ask about reads (a balance, a nonce, a block hash). This one asks about a
/// decided object: a plan names a chain id and an executor address, and neither of those is an
/// opcode argument on chain — the contract cannot refuse to run because the caller is on the
/// wrong network. So the check exists only here, and it has to be re-derived from the plan and
/// the binding the caller hands over rather than taken from whatever the caller remembered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanBinding {
    /// The plan validated against this runtime. Both identities are kept so the evidence row can
    /// tie the refusal-prone words (`route`, `plan`) to bytes (§39).
    Valid { plan_hash: B256, route_id: String },
    /// The plan disagrees with the runtime, or with the contract's own guards, with every
    /// rejection reported in the plan's words.
    Rejected { reason: String },
    /// No plan was validated. Unread means blocked, exactly as the other three read-shaped legs
    /// treat absence.
    Unverified(String),
}

impl PlanBinding {
    pub fn describe(&self) -> String {
        match self {
            Self::Valid {
                plan_hash,
                route_id,
            } => {
                format!("plan {plan_hash:#x} on {route_id} validated against this runtime")
            }
            Self::Rejected { reason } => format!("the plan was rejected: {reason}"),
            Self::Unverified(why) => format!("no plan was validated: {why}"),
        }
    }
}

/// Which of §35's submission paths an attempt belongs to, and the three facts §32 asks about
/// it.
///
/// This is a choice of *shape* rather than three more booleans, because §32's first
/// three conditions have no answer at all for a validation transaction: there is no
/// simulation, no risk decision and no opportunity behind it. Reading them as `true` to
/// let such a transaction through would have made the gate's most dangerous leg — "did a
/// risk layer approve this?" — a field a caller could set to whatever it needed. Stating
/// which path the attempt is on means the gate knows which of its seven legs are answers
/// rather than placeholders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateAttempt {
    /// A transaction that comes out of M4's run and M5's decision: all seven legs apply.
    Arbitrage {
        /// `simulation.success == true`.
        simulation_success: bool,
        /// `risk.decision == Accept`.
        risk_accepted: bool,
        /// The opportunity has not gone stale (§31, carried in from M5's lifecycle).
        freshness: Freshness,
    },
    /// M10's atomic executor transaction, which §36 gives a different answer to the same
    /// question: no risk layer judged this run, so `risk_accepted` is replaced by
    /// [`GateCheck::PlanValid`] and [`Freshness`] is read off the plan's own validity window
    /// rather than M5's lifecycle (§8 forbids inventing a second freshness rule; it does not
    /// forbid reusing this one). The four chain legs are unchanged.
    Executor {
        /// The plan validated against the runtime it is about to be handed to (§7, §36).
        plan: PlanBinding,
        /// REVM ran the whole `EOA → Executor → Pair → Pair → EOA` sequence and it completed
        /// (§25). A plan whose simulation reverted is refused *here*, not on chain.
        simulation_success: bool,
        /// The state the plan was simulated against is still recent enough to act on (§8).
        freshness: Freshness,
    },
    /// §35's controlled validation transaction, which is not an arbitrage and says so.
    /// The four chain legs are still checked in full; the three opportunity legs are
    /// replaced by this one requirement — the transaction carries the label §35 and §42
    /// ask for, and an empty label fails the gate rather than passing it silently.
    Validation { label: String },
}

impl GateAttempt {
    /// §31's staleness leg, asked of whichever subject carries it: M5's tracked opportunity, or
    /// M10's plan validity window. Shared because the two subjects must not get two rules — §8
    /// says the existing gate decides fresh / stale / reject.
    fn freshness_failure(freshness: &Freshness, subject: &str) -> Option<GateFailure> {
        match freshness {
            Freshness::Active => None,
            Freshness::Stale { reason } => Some(GateFailure {
                check: GateCheck::OpportunityFresh,
                reason: format!("the {subject} is stale: {reason}"),
            }),
            Freshness::Unknown => Some(GateFailure {
                check: GateCheck::OpportunityFresh,
                reason: format!(
                    "the lifecycle state of the {subject} was never read; §31 forbids forcing a \
                     send on an unchecked {subject}"
                ),
            }),
        }
    }

    /// The three opportunity legs, in §32's order, as failures where they do not hold.
    fn opportunity_failures(&self) -> Vec<GateFailure> {
        let mut failures = Vec::new();
        match self {
            Self::Arbitrage {
                simulation_success,
                risk_accepted,
                freshness,
            } => {
                if !simulation_success {
                    failures.push(GateFailure {
                        check: GateCheck::SimulationSucceeded,
                        reason: "the simulation this intent came from did not complete \
                                successfully"
                            .to_string(),
                    });
                }
                if !risk_accepted {
                    failures.push(GateFailure {
                        check: GateCheck::RiskAccepted,
                        reason: "the risk decision about this run is not Accept".to_string(),
                    });
                }
                failures.extend(Self::freshness_failure(freshness, "opportunity"));
            }
            Self::Executor {
                plan,
                simulation_success,
                freshness,
            } => {
                if !simulation_success {
                    failures.push(GateFailure {
                        check: GateCheck::SimulationSucceeded,
                        reason: "REVM never saw this route complete; §25's replay of \
                                 EOA → Executor → Pair → Pair → EOA is what stands between a \
                                 plan and a broadcast"
                            .to_string(),
                    });
                }
                failures.extend(Self::freshness_failure(freshness, "plan"));
                match plan {
                    PlanBinding::Valid { .. } => {}
                    PlanBinding::Rejected { reason } => failures.push(GateFailure {
                        check: GateCheck::PlanValid,
                        reason: reason.clone(),
                    }),
                    PlanBinding::Unverified(why) => failures.push(GateFailure {
                        check: GateCheck::PlanValid,
                        reason: format!("the plan was never validated against this runtime: {why}"),
                    }),
                }
            }
            Self::Validation { label } => {
                if label.trim().is_empty() {
                    failures.push(GateFailure {
                        check: GateCheck::Labelled,
                        reason: "§35's validation transaction has to be labelled as one; an \
                                 unlabelled transaction would be read by everything after \
                                 this gate as an arbitrage"
                            .to_string(),
                    });
                }
            }
        }
        failures
    }

    /// Whether this attempt is an arbitrage — the question §53's evidence line answers
    /// with the word `arbitrage` or the words `validation`.
    ///
    /// M10's executor transaction is deliberately *not* an arbitrage here: it is an atomic
    /// execution primitive, and §69 forbids the two words meaning the same thing. The one place
    /// the distinction is load-bearing is the lifecycle's `RiskApproved` rung, which only an
    /// arbitrage that a risk layer actually judged may claim.
    pub fn is_arbitrage(&self) -> bool {
        matches!(self, Self::Arbitrage { .. })
    }

    /// Whether this attempt came out of a simulation at all — the question the gas policy and
    /// the `Simulated` rung ask. REVM runs the executor route before it is ever signed (§25), so
    /// an executor attempt answers yes without having been judged by risk.
    pub fn has_simulation(&self) -> bool {
        matches!(self, Self::Arbitrage { .. } | Self::Executor { .. })
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Arbitrage { .. } => "arbitrage: §32's seven legs".to_string(),
            Self::Executor { plan, .. } => format!(
                "executor: §32's four chain legs plus the plan leg ({})",
                plan.describe()
            ),
            Self::Validation { label } => {
                format!("validation transaction ({label}): §32's four chain legs")
            }
        }
    }
}

/// The seven facts, as read. `chain_ids` are separate because the intent's own chain id
/// and the endpoint's answer are two claims, and §6 compares them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateFacts {
    /// Which path the attempt is on, and the three opportunity facts (§32, §35).
    pub attempt: GateAttempt,
    pub intent_chain_id: u64,
    pub configured_chain_id: u64,
    pub endpoint_chain_id: u64,
    pub binding: BlockBinding,
    pub balance: BalanceEvidence,
    pub nonce: NonceEvidence,
}

/// One failed check, with the value that failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateFailure {
    pub check: GateCheck,
    pub reason: String,
}

/// The gate's answer: pass, or every failing check at once.
///
/// Reporting *all* failures rather than the first one is deliberate. A run that stopped
/// because of three conditions has to say so in one line — an operator reading the
/// evidence needs to know whether fixing the balance would be enough, and it would not
/// be if the block binding had also reorged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateOutcome {
    Passed,
    Blocked { failures: Vec<GateFailure> },
}

impl GateOutcome {
    pub fn passed(&self) -> bool {
        matches!(self, Self::Passed)
    }

    /// §39: the closest error variant for the first failure, so a caller can record the
    /// refusal in the taxonomy rather than invent a string. Checks are evaluated in
    /// [`GateCheck`]'s declared order, so the first failure is stable.
    pub fn error(&self) -> Option<ExecutionError> {
        let failure = match self {
            Self::Passed => return None,
            Self::Blocked { failures } => failures.first()?,
        };
        Some(match failure.check {
            GateCheck::SimulationSucceeded | GateCheck::RiskAccepted => {
                ExecutionError::InvalidIntent(failure.reason.clone())
            }
            GateCheck::OpportunityFresh => ExecutionError::StaleOpportunity(failure.reason.clone()),
            GateCheck::ChainMatches => ExecutionError::ChainMismatch(failure.reason.clone()),
            GateCheck::BlockBindingValid => {
                ExecutionError::StaleOpportunity(failure.reason.clone())
            }
            GateCheck::BalanceSufficient => {
                ExecutionError::InsufficientBalance(failure.reason.clone())
            }
            GateCheck::NonceValid => ExecutionError::NonceUnavailable(failure.reason.clone()),
            GateCheck::Labelled => ExecutionError::InvalidIntent(failure.reason.clone()),
            GateCheck::PlanValid => ExecutionError::PlanRejected(failure.reason.clone()),
        })
    }

    /// The failures, rendered for a log line (§37 names the ids; this names the reason).
    pub fn describe(&self) -> String {
        match self {
            Self::Passed => "every §32 leg this attempt has an answer for holds".to_string(),
            Self::Blocked { failures } => failures
                .iter()
                .map(|f| format!("{}: {}", f.check.name(), f.reason))
                .collect::<Vec<_>>()
                .join("; "),
        }
    }
}

/// §32's gate.
pub struct PreSubmitGate;

impl PreSubmitGate {
    /// Evaluate the conditions this attempt has. Ordering is by [`GateCheck`]'s
    /// declaration so two runs over the same facts produce the same report (§16's
    /// determinism, applied to refusals).
    pub fn evaluate(facts: &GateFacts) -> GateOutcome {
        let mut failures = facts.attempt.opportunity_failures();
        if !(facts.intent_chain_id == facts.configured_chain_id
            && facts.configured_chain_id == facts.endpoint_chain_id)
        {
            failures.push(GateFailure {
                check: GateCheck::ChainMatches,
                reason: format!(
                    "intent {} / configured {} / endpoint {}",
                    facts.intent_chain_id, facts.configured_chain_id, facts.endpoint_chain_id
                ),
            });
        }
        match &facts.binding {
            BlockBinding::Confirmed { .. } => {}
            BlockBinding::Reorged {
                number,
                pinned,
                found,
            } => failures.push(GateFailure {
                check: GateCheck::BlockBindingValid,
                reason: format!(
                    "block {number} is now {found:#x}; the intent pins {pinned:#x}, so the \
                     state this transaction was decided against no longer exists at that \
                     height"
                ),
            }),
            BlockBinding::Unverified(why) => failures.push(GateFailure {
                check: GateCheck::BlockBindingValid,
                reason: format!("the pinned block was not verified: {why}"),
            }),
        }
        match &facts.balance {
            BalanceEvidence::Sufficient { .. } => {}
            BalanceEvidence::Insufficient {
                available_wei,
                maximum_spend_wei,
                source,
            } => failures.push(GateFailure {
                check: GateCheck::BalanceSufficient,
                reason: format!(
                    "{source}: {available_wei} wei available, {maximum_spend_wei} wei is the \
                     transaction's maximum spend (gas_limit * max_fee + value, the L2 half of \
                     an OP-stack bill)"
                ),
            }),
            BalanceEvidence::Unverified(why) => failures.push(GateFailure {
                check: GateCheck::BalanceSufficient,
                reason: format!("the real balance was not read: {why}"),
            }),
        }
        match &facts.nonce {
            NonceEvidence::Matches { .. } => {}
            NonceEvidence::Differs {
                intent_nonce,
                pending_nonce,
                source,
            } => failures.push(GateFailure {
                check: GateCheck::NonceValid,
                reason: format!(
                    "{source}: the intent carries nonce {intent_nonce} but the account's \
                     pending view is {pending_nonce}"
                ),
            }),
            NonceEvidence::Unverified(why) => failures.push(GateFailure {
                check: GateCheck::NonceValid,
                reason: format!("the nonce was not read: {why}"),
            }),
        }
        if failures.is_empty() {
            GateOutcome::Passed
        } else {
            failures.sort_by_key(|f| f.check);
            GateOutcome::Blocked { failures }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> GateFacts {
        GateFacts {
            attempt: GateAttempt::Arbitrage {
                simulation_success: true,
                risk_accepted: true,
                freshness: Freshness::Active,
            },
            intent_chain_id: 91_342,
            configured_chain_id: 91_342,
            endpoint_chain_id: 91_342,
            binding: BlockBinding::Confirmed {
                number: 100,
                hash: B256::left_padding_from(&[1]),
            },
            balance: BalanceEvidence::Sufficient {
                available_wei: U256::from(1_000u64),
                maximum_spend_wei: U256::from(900u64),
                source: "eth_getBalance@99".to_string(),
            },
            nonce: NonceEvidence::Matches {
                nonce: 3,
                source: "pending view".to_string(),
            },
        }
    }

    /// The same four chain legs, on §35's other path.
    fn validation_facts(label: &str) -> GateFacts {
        let mut facts = facts();
        facts.attempt = GateAttempt::Validation {
            label: label.to_string(),
        };
        facts
    }

    #[test]
    fn a_fully_evidenced_intent_passes_and_one_missing_field_blocks() {
        assert_eq!(PreSubmitGate::evaluate(&facts()), GateOutcome::Passed);

        let mut one = facts();
        one.nonce = NonceEvidence::Differs {
            intent_nonce: 3,
            pending_nonce: 4,
            source: "pending view".to_string(),
        };
        let outcome = PreSubmitGate::evaluate(&one);
        assert!(matches!(
            outcome.error(),
            Some(ExecutionError::NonceUnavailable(_))
        ));
        match outcome {
            GateOutcome::Blocked { failures } => {
                assert_eq!(failures.len(), 1);
                assert_eq!(failures[0].check, GateCheck::NonceValid);
            }
            GateOutcome::Passed => panic!("a nonce that differs from the chain must block"),
        }
    }

    #[test]
    fn an_unread_fact_blocks_because_absence_of_proof_is_not_a_pass() {
        let mut one = facts();
        one.binding = BlockBinding::Unverified("the node did not answer".to_string());
        one.balance = BalanceEvidence::Unverified("not read".to_string());
        one.nonce = NonceEvidence::Unverified("not read".to_string());
        one.attempt = GateAttempt::Arbitrage {
            simulation_success: true,
            risk_accepted: true,
            freshness: Freshness::Unknown,
        };
        let outcome = PreSubmitGate::evaluate(&one);
        let GateOutcome::Blocked { failures } = &outcome else {
            panic!("four unread facts cannot pass a seven-item gate");
        };
        let checks = failures.iter().map(|f| f.check.name()).collect::<Vec<_>>();
        assert_eq!(
            checks,
            [
                "opportunity_fresh",
                "block_binding_valid",
                "balance_sufficient",
                "nonce_valid"
            ]
        );
    }

    #[test]
    fn every_failure_is_reported_at_once_and_in_a_stable_order() {
        let mut one = facts();
        one.attempt = GateAttempt::Arbitrage {
            simulation_success: false,
            risk_accepted: false,
            freshness: Freshness::Active,
        };
        one.intent_chain_id = 1;
        one.balance = BalanceEvidence::Insufficient {
            available_wei: U256::from(1u64),
            maximum_spend_wei: U256::from(2u64),
            source: "eth_getBalance@99".to_string(),
        };
        let first = PreSubmitGate::evaluate(&one);
        let second = PreSubmitGate::evaluate(&one);
        assert_eq!(first, second, "a refusal must be deterministic");
        let GateOutcome::Blocked { failures } = &first else {
            panic!()
        };
        assert_eq!(failures.len(), 4);
        // The order is the declaration order, not the order the code happened to check.
        assert_eq!(
            failures.iter().map(|f| f.check.name()).collect::<Vec<_>>(),
            [
                "simulation_succeeded",
                "risk_accepted",
                "chain_matches",
                "balance_sufficient"
            ]
        );
        // §39: the first failure's taxonomy entry is the one a caller records.
        assert!(matches!(
            first.error(),
            Some(ExecutionError::InvalidIntent(_))
        ));
    }

    #[test]
    fn a_risk_rejection_and_a_stale_opportunity_are_both_refusals_not_errors() {
        let mut rejected = facts();
        rejected.attempt = GateAttempt::Arbitrage {
            simulation_success: true,
            risk_accepted: false,
            freshness: Freshness::Active,
        };
        assert!(!PreSubmitGate::evaluate(&rejected).passed());

        let mut stale = facts();
        stale.attempt = GateAttempt::Arbitrage {
            simulation_success: true,
            risk_accepted: true,
            freshness: Freshness::Stale {
                reason: "block 101 arrived".to_string(),
            },
        };
        let outcome = PreSubmitGate::evaluate(&stale);
        assert!(matches!(
            outcome.error(),
            Some(ExecutionError::StaleOpportunity(_))
        ));
        assert!(outcome.describe().contains("block 101 arrived"));
    }

    /// §35 and acceptance T's counterpart: the validation transaction is the one path M6
    /// can really send, and the gate has to be able to say *why* it lets that through
    /// without ever having seen a simulation or a risk decision.
    #[test]
    fn a_labelled_validation_transaction_answers_only_the_chain_legs() {
        assert_eq!(
            PreSubmitGate::evaluate(&validation_facts("M6 execution validation transaction")),
            GateOutcome::Passed,
            "the four chain legs are the whole question for a transaction that is not an \
             arbitrage — and they are still read, not assumed"
        );

        // The same facts with no label: nothing about the chain changed, and the attempt
        // is refused. An unlabelled validation transaction would otherwise be indistinguishable
        // in the evidence from an arbitrage that skipped the risk layer.
        let unlabelled = PreSubmitGate::evaluate(&validation_facts("   "));
        let GateOutcome::Blocked { failures } = &unlabelled else {
            panic!("an unlabelled transaction cannot pass §35's gate")
        };
        assert_eq!(failures.len(), 1, "{unlabelled:?}");
        assert_eq!(failures[0].check, GateCheck::Labelled);
        assert!(matches!(
            unlabelled.error(),
            Some(ExecutionError::InvalidIntent(_))
        ));

        // A validation transaction is not exempt from §33: the balance leg still blocks it.
        let mut broke = validation_facts("M6 execution validation transaction");
        broke.balance = BalanceEvidence::Insufficient {
            available_wei: U256::from(1u64),
            maximum_spend_wei: U256::from(2u64),
            source: "eth_getBalance@100".to_string(),
        };
        assert!(matches!(
            PreSubmitGate::evaluate(&broke).error(),
            Some(ExecutionError::InsufficientBalance(_))
        ));

        // And the two paths are not the same question: an arbitrage that has never been
        // decided by risk is refused *for being an arbitrage*, which a validation attempt
        // cannot be asked at all.
        assert_eq!(
            facts().attempt.describe(),
            "arbitrage: §32's seven legs",
            "{}",
            facts().attempt.describe()
        );
        assert!(validation_facts("x")
            .attempt
            .describe()
            .starts_with("validation"));
    }

    /// M10's third path. §36 says the plan leg replaces the risk leg for an executor
    /// transaction, and §8 says the freshness leg is answered by *this* gate rather than a new
    /// rule — so the two facts under test are that the plan leg can block on its own, and that
    /// `risk_accepted` is never silently assumed for a path that has no risk layer.
    #[test]
    fn an_executor_attempt_is_answered_by_the_plan_leg_and_never_by_the_risk_leg() {
        let attempt = GateAttempt::Executor {
            plan: PlanBinding::Valid {
                plan_hash: B256::left_padding_from(&[7]),
                route_id: "m10-91342-pool>pool-tok>tok>tok".to_string(),
            },
            simulation_success: true,
            freshness: Freshness::Active,
        };
        assert!(attempt.has_simulation(), "REVM ran the route (§25)");
        assert!(
            !attempt.is_arbitrage(),
            "§69: an atomic execution primitive is not an arbitrage, and the lifecycle rung \
             this predicate gates is the risk one"
        );
        let mut one = facts();
        one.attempt = attempt.clone();
        assert_eq!(PreSubmitGate::evaluate(&one), GateOutcome::Passed);

        // A plan that disagrees with the runtime blocks on its own leg, in §40's taxonomy.
        one.attempt = GateAttempt::Executor {
            plan: PlanBinding::Rejected {
                reason: "§7: the plan is for chain 1, this runtime is 91342".to_string(),
            },
            simulation_success: true,
            freshness: Freshness::Active,
        };
        let outcome = PreSubmitGate::evaluate(&one);
        let GateOutcome::Blocked { failures } = &outcome else {
            panic!("a plan bound to another chain cannot pass the gate");
        };
        assert_eq!(failures.len(), 1, "{outcome:?}");
        assert_eq!(failures[0].check, GateCheck::PlanValid);
        assert!(matches!(
            outcome.error(),
            Some(ExecutionError::PlanRejected(_))
        ));

        // Never validated is never a pass, and a stale plan is stale in the gate's own words.
        one.attempt = GateAttempt::Executor {
            plan: PlanBinding::Unverified("the caller kept a plan hash, not a plan".to_string()),
            simulation_success: false,
            freshness: Freshness::Stale {
                reason: "4 blocks have passed since 37984319".to_string(),
            },
        };
        let checks = match PreSubmitGate::evaluate(&one) {
            GateOutcome::Blocked { failures } => {
                failures.iter().map(|f| f.check.name()).collect::<Vec<_>>()
            }
            GateOutcome::Passed => panic!("three unanswered legs cannot pass"),
        };
        assert_eq!(
            checks,
            ["simulation_succeeded", "opportunity_fresh", "plan_valid"]
        );
        assert!(
            one.attempt.describe().contains("executor:"),
            "{}",
            one.attempt.describe()
        );

        // The risk leg stays an arbitrage-only question, and the two other paths differ on
        // whether a simulation exists behind them at all.
        assert!(facts().attempt.has_simulation());
        assert!(!validation_facts("M6 execution validation transaction")
            .attempt
            .has_simulation());
    }
}
