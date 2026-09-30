//! §48's execution interface and the only two implementations M4 is allowed.
//!
//! The real executor does not belong to this milestone. What belongs here is the
//! shape it will one day sit in, built so that the shape itself cannot broadcast:
//! [`Executor::execute`] takes [`ExecutionRequest`] — addresses, amounts, one block
//! pin — and returns [`ExecutionResult`], whose three variants are all *statements
//! about not sending*. There is no transaction type in this module, no signer, no
//! network client, and no way to reach one through a trait object of
//! [`Executor`].

use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use evm_core::ChainId;
use evm_simulation::{BlockPin, SimulationResult};
use serde::Serialize;

use crate::decision::{RiskDecision, NO_BROADCAST};

/// What would be sent, if M4 could send.
///
/// Built from a result by [`ExecutionRequest::from_run`] and from nothing else, so
/// the request cannot describe a run that was not simulated: the block, the sender,
/// the two pools and the size all come out of the execution record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExecutionRequest {
    pub chain_id: ChainId,
    /// The state this was decided against. A real executor has to be told to build
    /// against *this* block or refuse; carrying the pin is what makes §20's guarantee
    /// survive the handover instead of ending in the report.
    pub block: BlockPin,
    pub sender: Address,
    /// The two pools the route trades through, in leg order (§55's targets).
    pub targets: Vec<Address>,
    pub input_amount: U256,
    /// §28's measured figure, which is an estimate for a hypothetical transaction and
    /// nothing more.
    pub gas_estimate: u64,
    pub decision: RiskDecision,
}

impl ExecutionRequest {
    /// The request that corresponds to one simulation of one route.
    pub fn from_run(run: &SimulationResult, decision: RiskDecision) -> Self {
        Self {
            chain_id: run.chain_id,
            block: run.block,
            sender: run.sender,
            targets: run.plan_summary.pools.clone(),
            input_amount: run.plan_summary.input_amount,
            gas_estimate: run.gas_used(),
            decision,
        }
    }
}

/// What an executor did — which in this milestone is always nothing to the chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum ExecutionResult {
    /// The request was not acted on, and the reason names the rule that stopped it.
    /// A refusal is a value and not an error: the policy working as designed is not
    /// a failure of the executor.
    Refused { reason: String },
    /// [`NullExecutor`]'s answer to any request: the decision travelled with it and
    /// was then dropped, in the right order, on purpose.
    Discarded { reason: String },
    /// [`DryRunExecutor`]'s answer: the request as one line of JSON, and the name of
    /// the place a real executor would have had to log it before doing anything else.
    /// §47 — the record is the whole effect of the call.
    Recorded { destination: String, record: String },
}

impl ExecutionResult {
    /// The one line a report prints for this outcome.
    pub fn note(&self) -> String {
        match self {
            Self::Refused { reason } | Self::Discarded { reason } => reason.clone(),
            Self::Recorded {
                destination,
                record,
            } => format!("recorded for {destination}: {record}"),
        }
    }
}

/// §48's trait.
#[async_trait]
pub trait Executor {
    async fn execute(&self, request: &ExecutionRequest) -> ExecutionResult;
}

/// The executor that does nothing at all, including checking: it is the answer to
/// "what if the decision were wrong", and it is the default because a milestone that
/// cannot broadcast should not be one call away from deciding to.
pub struct NullExecutor;

#[async_trait]
impl Executor for NullExecutor {
    async fn execute(&self, request: &ExecutionRequest) -> ExecutionResult {
        ExecutionResult::Discarded {
            reason: format!(
                "the NullExecutor was handed a {} and did nothing with it: {}",
                match &request.decision {
                    RiskDecision::Accept { .. } => "policy Accept",
                    RiskDecision::Reject { .. } => "policy Reject",
                    RiskDecision::Unknown { .. } => "policy Unknown",
                },
                NO_BROADCAST
            ),
        }
    }
}

/// The executor that looks at the decision and writes the request down.
///
/// `destination` is a name, not an open file: nothing here touches the filesystem, so
/// a dry run can be called twice in a test and produce the same string, which is the
/// cheapest available proof that its only effect was the return value.
pub struct DryRunExecutor {
    pub destination: String,
}

#[async_trait]
impl Executor for DryRunExecutor {
    async fn execute(&self, request: &ExecutionRequest) -> ExecutionResult {
        if !request.decision.accepted() {
            return ExecutionResult::Refused {
                reason: format!(
                    "the dry run refused a request the policy did not accept: {}",
                    request.decision
                ),
            };
        }
        let record = serde_json::to_string(request)
            .expect("an execution request is plain data and always serialises");
        ExecutionResult::Recorded {
            destination: self.destination.clone(),
            record,
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, b256, Address, U256};
    use evm_core::BlockNumber;
    use serde_json::Value;

    use super::*;
    use crate::decision::RiskRule;

    const CHAIN: ChainId = ChainId(1);
    const PIN: BlockPin = BlockPin::new(
        BlockNumber(1),
        b256!("0x0000000000000000000000000000000000000000000000000000000000000001"),
    );
    const POOL_ONE: Address = address!("0x00000000000000000000000000000000000000a1");
    const POOL_TWO: Address = address!("0x00000000000000000000000000000000000000b2");

    /// A decision with nothing behind it but the numbers an executor reads: the rules
    /// themselves are tested in [`crate::policy`], against runs built by the code
    /// that produces them.
    fn decision(net: u64) -> RiskDecision {
        if net == 0 {
            return RiskDecision::Reject {
                rule: RiskRule::MinimumNetProfit,
                reason: "the run netted 0".to_string(),
            };
        }
        RiskDecision::Accept {
            net_profit_wei: U256::from(net),
            gross_profit_wei: U256::from(net) + U256::from(300u64),
            gas_cost_wei: U256::from(300u64),
            gas_used: 1,
            minimum_net_profit_wei: U256::from(1u64),
            maximum_gas: 1_000,
            reason: format!("netted {net}, above the minimum: {NO_BROADCAST}"),
        }
    }

    fn request(net: u64) -> ExecutionRequest {
        ExecutionRequest {
            chain_id: CHAIN,
            block: PIN,
            sender: POOL_ONE,
            targets: vec![POOL_ONE, POOL_TWO],
            input_amount: U256::from(1_000_000u64),
            gas_estimate: 1,
            decision: decision(net),
        }
    }

    fn dry_run() -> DryRunExecutor {
        DryRunExecutor {
            destination: "data/simulation-m4/dry-run.ndjson".to_string(),
        }
    }

    #[tokio::test]
    async fn the_null_executor_does_nothing_even_to_an_accept() {
        let outcome = NullExecutor.execute(&request(500)).await;
        match &outcome {
            ExecutionResult::Discarded { reason } => {
                assert!(reason.contains("policy Accept"), "{reason}");
                assert!(reason.contains("no broadcast"), "{reason}");
            }
            other => panic!("the NullExecutor must not decide: {other:?}"),
        }
        // And the same non-action for a Reject: it does not inspect, so it cannot be
        // the place where a mistake is made.
        let outcome = NullExecutor.execute(&request(0)).await;
        assert!(
            matches!(&outcome, ExecutionResult::Discarded { reason } if reason.contains("policy Reject")),
            "{outcome:?}",
        );
    }

    #[tokio::test]
    async fn the_dry_run_refuses_what_the_policy_did_not_accept() {
        let refused = dry_run().execute(&request(0)).await;
        match &refused {
            ExecutionResult::Refused { reason } => {
                assert!(reason.contains("did not accept"), "{reason}");
                assert!(
                    reason.contains("minimum_net_profit"),
                    "the rule that fired travels into the refusal: {reason}"
                );
            }
            other => panic!("an unaccepted request must be refused: {other:?}"),
        }

        // Unknown is refused too, and not treated as a Reject: the request is handed
        // back unchanged, which is how a caller can still go and get the missing fact.
        let unresolved = ExecutionRequest {
            decision: RiskDecision::Unknown {
                rule: RiskRule::MinimumNetProfit,
                reason: "no price was declared".to_string(),
            },
            ..request(500)
        };
        let refused = dry_run().execute(&unresolved).await;
        assert!(
            matches!(&refused, ExecutionResult::Refused { reason } if reason.contains("Unknown on minimum_net_profit")),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn the_dry_run_records_an_accept_as_data_and_nothing_else() {
        let executor = dry_run();
        let first = executor.execute(&request(500)).await;
        let second = executor.execute(&request(500)).await;
        assert_eq!(first, second, "the record is the whole effect of the call");
        let ExecutionResult::Recorded {
            destination,
            record,
        } = &first
        else {
            panic!("an accepted request is recorded: {first:?}");
        };
        assert_eq!(destination, "data/simulation-m4/dry-run.ndjson");
        assert_eq!(record.lines().count(), 1, "one request, one line");
        let parsed: Value = serde_json::from_str(record).expect("JSON");
        assert_eq!(parsed["chain_id"], 1);
        assert_eq!(parsed["block"]["number"], 1);
        // A U256 in these records is a hex string, not a decimal one, so the number
        // has to be read back as the type it was written from rather than compared to
        // the digits it happened to be built from.
        for (field, value, expected) in [
            (
                "input_amount",
                parsed["input_amount"].clone(),
                U256::from(1_000_000u64),
            ),
            (
                "decision.Accept.net_profit_wei",
                parsed["decision"]["Accept"]["net_profit_wei"].clone(),
                U256::from(500u64),
            ),
        ] {
            let written = value.as_str().unwrap_or_else(|| {
                panic!("{field} is a uint256, which serialises as hex: {record}")
            });
            let read_back: U256 = written
                .parse()
                .unwrap_or_else(|_| panic!("{field}: {written} is not a uint256"));
            assert_eq!(read_back, expected, "{field} came back as {written}");
        }
        assert_eq!(parsed["targets"].as_array().expect("two").len(), 2);
        assert_eq!(parsed["decision"]["Accept"]["maximum_gas"], 1_000);
        assert!(
            record.contains("no broadcast"),
            "the record carries §47 with it: {record}"
        );
    }

    #[tokio::test]
    async fn the_note_is_the_line_a_report_prints() {
        let recorded = dry_run().execute(&request(500)).await;
        assert!(recorded
            .note()
            .starts_with("recorded for data/simulation-m4/dry-run.ndjson: {"));
        let discarded = NullExecutor.execute(&request(500)).await;
        assert!(discarded.note().starts_with("the NullExecutor was handed"));
    }
}
