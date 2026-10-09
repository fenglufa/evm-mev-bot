//! M12-B §3's readiness gate: asking the node whether it is ready *before* the run
//! asks it for anything that could become an action.
//!
//! The gap this module closes is documented in M12-A §12.2 G1 and §16 D6. A node that
//! has not synced yet answers a block read with "there is no block at this height"
//! ([`crate::ChainError::MissingData`]) and a pending read with nothing at all, and
//! both of those are the same words the code uses when the chain really has no such
//! data. Without this gate an unsynced node is indistinguishable from an empty market,
//! which is exactly the "查不到当成没有" failure §7 of M12-B forbids.
//!
//! Two deliberate limits keep the module small:
//!
//! * It asks one method — `eth_syncing` — which M12-B §3 names as the only new RPC
//!   call this milestone is approved to add, and adds nothing else for logging or
//!   evidence. Head freshness, where it is judged at all, is judged from a block number
//!   the caller already read (§7 of M12-A's head rules), never from a second request.
//! * It is a decision function, not a policy engine. The whole of the fail-closed rule
//!   is visible in [`judge`]: anything that is not a clean `false` from the node holds
//!   the run, including an answer this module cannot decode.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ChainError, Result};
use crate::rpc::{parse_u64, HttpChainAdapter};

/// The three numbers a node publishes while it is working through a sync.
///
/// Quantities as the node's hex-quantity rule parses them, so the evidence line can
/// show what the node said rather than a paraphrase of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncProgress {
    pub starting_block: u64,
    pub current_block: u64,
    pub highest_block: u64,
}

/// What `eth_syncing` answered, in one of the two shapes the method is defined to
/// have. There is no third shape and no attempt is made to invent one: an answer that
/// is neither is a decode failure, which [`judge`] holds the run for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncStatus {
    /// The node reports it is not syncing. §3 is explicit that this is *not* a claim
    /// that the node is at the network head — only that it says it has no sync in
    /// progress. Head freshness is a separate question, and separate policy.
    NotSyncing,
    /// The node published a sync it is still working through.
    Syncing(SyncProgress),
}

/// Read one `eth_syncing` result, failing closed on every shape but the two legal ones.
///
/// The rejections are the point: a bare `true` is what a node that knows it is syncing
/// but publishes no progress answers with, and treating it as "syncing, details
/// unknown" would let the gate report a number the node never gave. It is an illegal
/// response instead, and §3 lists that among the answers that must never count as
/// ready.
pub fn decode_syncing(result: &Value) -> Result<SyncStatus> {
    match result {
        Value::Bool(false) => Ok(SyncStatus::NotSyncing),
        Value::Bool(true) => Err(ChainError::Decode(
            "eth_syncing answered `true` with no progress object: a sync this node \
             cannot describe is not an answer readiness can be judged from"
                .to_string(),
        )),
        Value::Object(fields) => {
            let progress = SyncProgress {
                starting_block: quantity(fields, "startingBlock")?,
                current_block: quantity(fields, "currentBlock")?,
                highest_block: quantity(fields, "highestBlock")?,
            };
            Ok(SyncStatus::Syncing(progress))
        }
        other => Err(ChainError::Decode(format!(
            "eth_syncing answered a {} rather than `false` or a progress object: {other}",
            shape_of(other)
        ))),
    }
}

/// One hex-quantity field of a progress object.
fn quantity(fields: &serde_json::Map<String, Value>, name: &str) -> Result<u64> {
    let value = fields.get(name).ok_or_else(|| {
        ChainError::Decode(format!("eth_syncing progress object carries no `{name}`"))
    })?;
    parse_u64(value, &format!("eth_syncing `{name}`"))
}

/// The name of a JSON value's type, for a rejection that has to say what it saw.
fn shape_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// What this run requires of the node's head height, beyond "the node says it is not
/// syncing".
///
/// The default is [`HeadFreshnessPolicy::NotJudged`], and it stays the default because
/// §3 forbids the alternative: a tolerance with no external reference is a number
/// invented to look like a measurement. Judging freshness needs a head from somewhere
/// other than the node being gated — and silently reading that somewhere from a public
/// endpoint is what §3's no-fallback rule rules out. So the reference is a fact the
/// operator supplies, and until they do the gate says plainly what it did not check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadFreshnessPolicy {
    /// Ask nothing of the head height. Readiness then means exactly what §3 says it
    /// means: the node reports no sync in progress. Not proof that it is caught up.
    #[default]
    NotJudged,
    /// Hold the node's own head against a reference height the operator supplied,
    /// allowing a stated number of blocks of lag. Both numbers come from configuration;
    /// neither has a default here, so a run cannot inherit a tolerance nobody chose.
    AgainstReference {
        reference_head: u64,
        tolerance_blocks: u64,
    },
}

/// The gate's answer, and — for every answer that is not `Ready` — the fact it rests on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    /// Proceed. The node says it is not syncing, and any head freshness this run asked
    /// for was met.
    Ready,
    /// Hold. The node is still syncing, so no stage that can produce a real action runs.
    Syncing(SyncProgress),
    /// Hold. The node says it is not syncing but its head is further behind the
    /// configured reference than the configured tolerance allows.
    HeadBehindReference {
        reference_head: u64,
        tolerance_blocks: u64,
        observed_head: u64,
        lag_blocks: u64,
    },
    /// Hold. There is no answer to judge — the node rejected the call, never answered
    /// it, timed out, dropped the connection, or answered in a form that cannot be
    /// decoded. §3 requires this to be held rather than assumed, so an unknown is never
    /// folded into `Ready` and never folded into "the node is syncing" either.
    Unverified(String),
}

impl Readiness {
    /// Whether a run may proceed into a stage that produces real actions.
    ///
    /// Written as an accessor rather than left to `matches!` at each call site because
    /// §3's whole risk is a future caller treating one of the three holds as the others:
    /// this is the only reading of this type the gate permits.
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Why the run is held, or `None` when it is not.
    ///
    /// §3's "readiness failure must not be reported as no arbitrage opportunity" is a
    /// statement about this string reaching the operator: a held run has a reason, and
    /// the reason is not an empty opportunity table.
    pub fn withheld_because(&self) -> Option<String> {
        match self {
            Self::Ready => None,
            Self::Syncing(progress) => Some(format!(
                "the node reports a sync still in progress: block {} of {} (started at {})",
                progress.current_block, progress.highest_block, progress.starting_block
            )),
            Self::HeadBehindReference {
                reference_head,
                tolerance_blocks,
                observed_head,
                lag_blocks,
            } => Some(format!(
                "the node's head is {observed_head}, {lag_blocks} blocks behind the \
                 configured reference {reference_head}, which is past the configured \
                 tolerance of {tolerance_blocks}"
            )),
            Self::Unverified(detail) => {
                Some(format!("the node's sync status is unverified: {detail}"))
            }
        }
    }
}

/// The decision, as a pure function of what the node said and what the run requires.
///
/// `Err` here is never a failure of the gate: it is the gate's third answer. A node that
/// rejects the call, answers nothing, or answers something undecodable leaves this run
/// unable to tell whether the node is ready, and §3 settles that case once — held.
///
/// `observed_head` is the block number the caller already read for its own start
/// position, or `None` when the run never read one. It is only consulted by
/// [`HeadFreshnessPolicy::AgainstReference`]; `NotJudged` asks the node for nothing it
/// did not already ask.
pub fn judge(
    answer: &Result<SyncStatus>,
    policy: &HeadFreshnessPolicy,
    observed_head: Option<u64>,
) -> Readiness {
    let status = match answer {
        Ok(status) => *status,
        Err(error) => {
            return Readiness::Unverified(format!("{}: {error}", answer_class(error)));
        }
    };

    // The node's own "not yet" outranks any freshness question: while it is syncing,
    // its head number measures progress against its own target, not against the chain.
    if let SyncStatus::Syncing(progress) = status {
        return Readiness::Syncing(progress);
    }

    match policy {
        HeadFreshnessPolicy::NotJudged => Readiness::Ready,
        HeadFreshnessPolicy::AgainstReference {
            reference_head,
            tolerance_blocks,
        } => {
            let Some(head) = observed_head else {
                return Readiness::Unverified(
                    "head freshness was configured against reference \
                     {reference_head} with a tolerance of {tolerance_blocks} blocks, but \
                     this run read no block number to compare"
                        .to_string(),
                );
            };
            let lag_blocks = reference_head.saturating_sub(head);
            if lag_blocks > *tolerance_blocks {
                Readiness::HeadBehindReference {
                    reference_head: *reference_head,
                    tolerance_blocks: *tolerance_blocks,
                    observed_head: head,
                    lag_blocks,
                }
            } else {
                Readiness::Ready
            }
        }
    }
}

/// Which of §3's failure kinds an unanswered call is, in the words the evidence uses.
///
/// Kept separate from `ChainError`'s own Display so a report can say what class of
/// failure it was rather than make the operator read a transport string to learn
/// whether the node replied. `ChainError::Rpc` is the wire layer's class for every
/// failure that is not a rejection and not a decode — a timeout, a dropped connection,
/// a 5xx, a body that was not JSON — so its word covers all four without claiming the
/// node stayed silent when it in fact answered in a form nothing can read.
fn answer_class(error: &ChainError) -> &'static str {
    match error {
        // The call went out and nothing usable came back.
        ChainError::Rpc(_) => "the call produced no usable answer",
        // The node answered, and its answer was a JSON-RPC error.
        ChainError::RpcRejected(_) => "the node rejected the call",
        // An answer arrived and it was not a shape readiness can be judged from.
        ChainError::Decode(_) => "the node's answer could not be decoded",
        ChainError::MissingData(_) => "the node gave no data",
        ChainError::Io(_) => "the local request could not be made",
        ChainError::Inconsistent(_) => "the node's answer is inconsistent",
    }
}

/// How many times one run's gate may ask the node, total.
///
/// The number is the WebSocket client's own `max_reconnect_attempts`
/// ([`crate::WsOptions`] default, 8) rather than a fresh constant: the recovery points
/// this budget pays for are reconnects, so a run cannot spend asks faster than it can
/// reconnect. §3's ban on an unbounded retry loop is satisfied by the gate refusing to
/// call the node once the budget is spent — it holds the run instead.
pub const DEFAULT_CHECK_BUDGET: usize = 8;

/// One run's readiness gate: the policy, the budget, and the record of what has been spent.
///
/// Not `Sync`-shared and not global on purpose — §3's recheck points are the pipeline's,
/// which is the code that owns the adapter and knows whether it just started or just
/// reconnected. The caller hands it both.
#[derive(Clone, Debug)]
pub struct ReadinessGate {
    policy: HeadFreshnessPolicy,
    budget: usize,
    checks: usize,
}

impl ReadinessGate {
    pub fn new(policy: HeadFreshnessPolicy) -> Self {
        Self::with_budget(policy, DEFAULT_CHECK_BUDGET)
    }

    pub const fn with_budget(policy: HeadFreshnessPolicy, budget: usize) -> Self {
        Self {
            policy,
            budget,
            checks: 0,
        }
    }

    pub const fn policy(&self) -> &HeadFreshnessPolicy {
        &self.policy
    }

    /// How many asks this gate has made the node, including the one that got here.
    ///
    /// This is the number §3 asks for as evidence — "记录实际调用次数" — which is why it
    /// counts calls rather than answers: a held run that retried three times is a
    /// different cost from one that was refused once.
    pub const fn checks(&self) -> usize {
        self.checks
    }

    pub const fn budget(&self) -> usize {
        self.budget
    }

    pub const fn remaining(&self) -> usize {
        self.budget.saturating_sub(self.checks)
    }

    /// Ask the node once and judge the answer.
    ///
    /// One logical call per invocation, no retry at this layer (the adapter's own
    /// single transport retry is unchanged and applies to every method equally), and no
    /// call at all past the budget. A caller that wants to retry on a schedule has to
    /// come back through here, so the count stays the truth.
    pub async fn check(
        &mut self,
        adapter: &HttpChainAdapter,
        observed_head: Option<u64>,
    ) -> Readiness {
        if self.checks >= self.budget {
            return Readiness::Unverified(format!(
                "the readiness recheck budget of {} asks is spent, so the gate will not \
                 ask the node again",
                self.budget
            ));
        }
        self.checks += 1;
        let answer = adapter.syncing_status().await;
        judge(&answer, &self.policy, observed_head)
    }
}

impl HttpChainAdapter {
    /// The node's own answer to `eth_syncing`, decoded fail-closed.
    ///
    /// An inherent method rather than a [`crate::ChainAdapter`] trait method, and the
    /// reason is M12-A §12.2's own constraint about blast radius: the trait has seven
    /// implementors, three of which are a recorded directory, a fixture and a simulated
    /// state provider — none of which has a node to ask. A trait method would have to
    /// answer for them anyway, and "recorded directories are always ready" is precisely
    /// the unfalsifiable claim §3 forbids. The readiness gate is for the one source that
    /// can be syncing, so this is the one source that can say so.
    ///
    /// The call goes through [`HttpChainAdapter::request_raw`] — the same client, the
    /// same connection pool, the same single transport retry, and the same recording
    /// sink as every other read of this run. §3's "不新建第二套 HTTP client" is satisfied
    /// by there being no second client to build.
    pub async fn syncing_status(&self) -> Result<SyncStatus> {
        let result = self
            .request_raw("eth_syncing", serde_json::json!([]))
            .await?;
        decode_syncing(&result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(starting: &str, current: &str, highest: &str) -> Value {
        serde_json::json!({
            "startingBlock": starting,
            "currentBlock": current,
            "highestBlock": highest,
        })
    }

    #[test]
    fn a_false_answer_is_not_syncing() {
        // §3's pass case, checked as the decoded value rather than as "it did not panic".
        assert!(
            matches!(
                decode_syncing(&Value::Bool(false)),
                Ok(SyncStatus::NotSyncing)
            ),
            "`false` is the node saying it is not syncing"
        );
    }

    #[test]
    fn a_progress_object_is_syncing_with_its_numbers() {
        let answer = decode_syncing(&progress("0x0", "0x24c", "0x3e8")).expect("legal object");
        assert_eq!(
            answer,
            SyncStatus::Syncing(SyncProgress {
                starting_block: 0,
                current_block: 588,
                highest_block: 1000,
            })
        );
    }

    #[test]
    fn a_bare_true_answer_is_rejected_not_guessed_at() {
        // A node that says "syncing" without saying where is an illegal response (§3),
        // not a sync at an assumed height.
        let error = decode_syncing(&Value::Bool(true)).unwrap_err();
        assert!(
            matches!(error, ChainError::Decode(_)),
            "a bare `true` must be a decode failure, got {error:?}"
        );
        // …and the failure still holds the run rather than being read as ready.
        assert!(!judge(&Err(error), &HeadFreshnessPolicy::default(), Some(1000)).is_ready());
    }

    #[test]
    fn a_progress_object_missing_a_field_is_rejected() {
        let truncated = serde_json::json!({ "startingBlock": "0x0", "currentBlock": "0x1" });
        let error = decode_syncing(&truncated).unwrap_err();
        match error {
            ChainError::Decode(detail) => assert!(
                detail.contains("highestBlock"),
                "the rejection has to name the field that was absent: {detail}"
            ),
            other => panic!("expected a decode failure, got {other:?}"),
        }
    }

    #[test]
    fn a_progress_field_of_the_wrong_type_is_rejected() {
        // Some test doubles answer quantities as JSON numbers. §3's "类型错误 → 拒绝"
        // means this is held, not coerced.
        let numeric = serde_json::json!({
            "startingBlock": 0,
            "currentBlock": 588,
            "highestBlock": 1000,
        });
        assert!(matches!(
            decode_syncing(&numeric),
            Err(ChainError::Decode(_))
        ));
    }

    #[test]
    fn a_progress_field_that_is_not_hex_is_rejected() {
        let garbled = progress("0x0", "not-a-number", "0x3e8");
        assert!(matches!(
            decode_syncing(&garbled),
            Err(ChainError::Decode(_))
        ));
    }

    #[test]
    fn null_and_other_shapes_are_rejected() {
        for answer in [
            Value::Null,
            Value::String("ok".to_string()),
            serde_json::json!([]),
            serde_json::json!(7),
        ] {
            assert!(
                matches!(decode_syncing(&answer), Err(ChainError::Decode(_))),
                "{answer} is not a legal eth_syncing result and must not decode"
            );
        }
    }

    #[test]
    fn the_default_policy_judges_no_freshness_at_all() {
        // §3's ban on a fabricated tolerance: the default adds no number nobody chose.
        assert_eq!(
            HeadFreshnessPolicy::default(),
            HeadFreshnessPolicy::NotJudged
        );
        assert_eq!(
            judge(
                &Ok(SyncStatus::NotSyncing),
                &HeadFreshnessPolicy::default(),
                Some(1)
            ),
            Readiness::Ready
        );
    }

    #[test]
    fn syncing_holds_even_where_freshness_would_pass() {
        let answer = SyncStatus::Syncing(SyncProgress {
            starting_block: 0,
            current_block: 999,
            highest_block: 1000,
        });
        let policy = HeadFreshnessPolicy::AgainstReference {
            reference_head: 999,
            tolerance_blocks: 0,
        };
        assert_eq!(
            judge(&Ok(answer), &policy, Some(999)),
            Readiness::Syncing(SyncProgress {
                starting_block: 0,
                current_block: 999,
                highest_block: 1000,
            })
        );
    }

    #[test]
    fn a_rejected_call_is_unverified_not_syncing_and_never_ready() {
        for error in [
            ChainError::RpcRejected(
                r#"{"code":-32601,"message":"the method is not available"}"#.to_string(),
            ),
            ChainError::Rpc("operation timed out".to_string()),
            ChainError::Rpc("connection closed before message completed".to_string()),
            ChainError::Decode("an undecodable answer".to_string()),
        ] {
            let verdict = judge(&Err(error), &HeadFreshnessPolicy::default(), Some(10));
            match &verdict {
                Readiness::Unverified(detail) => assert!(!detail.is_empty()),
                other => panic!("a failed readiness call must be Unverified, got {other:?}"),
            }
            assert!(!verdict.is_ready());
            assert!(verdict.withheld_because().is_some());
        }
    }

    #[test]
    fn the_answer_class_names_which_kind_of_failure_it_was() {
        assert_eq!(
            answer_class(&ChainError::Rpc("timed out".to_string())),
            "the call produced no usable answer"
        );
        assert_eq!(
            answer_class(&ChainError::RpcRejected("{}".to_string())),
            "the node rejected the call"
        );
        assert_eq!(
            answer_class(&ChainError::Decode("{}".to_string())),
            "the node's answer could not be decoded"
        );
    }

    #[test]
    fn freshness_within_tolerance_passes_and_beyond_it_holds() {
        let policy = HeadFreshnessPolicy::AgainstReference {
            reference_head: 1000,
            tolerance_blocks: 5,
        };
        assert_eq!(
            judge(&Ok(SyncStatus::NotSyncing), &policy, Some(995)),
            Readiness::Ready,
            "exactly at the tolerance is within it"
        );
        assert_eq!(
            judge(&Ok(SyncStatus::NotSyncing), &policy, Some(994)),
            Readiness::HeadBehindReference {
                reference_head: 1000,
                tolerance_blocks: 5,
                observed_head: 994,
                lag_blocks: 6,
            }
        );
        // A node ahead of a stale reference is not behind it, so the lag floors at zero
        // rather than wrapping into a hold.
        assert_eq!(
            judge(&Ok(SyncStatus::NotSyncing), &policy, Some(1200)),
            Readiness::Ready
        );
    }

    #[test]
    fn a_configured_freshness_check_with_no_head_read_holds() {
        // Fail closed rather than judging freshness against a number the run never read.
        let policy = HeadFreshnessPolicy::AgainstReference {
            reference_head: 1000,
            tolerance_blocks: 5,
        };
        let verdict = judge(&Ok(SyncStatus::NotSyncing), &policy, None);
        assert!(matches!(verdict, Readiness::Unverified(_)), "{verdict:?}");
        assert!(!verdict.is_ready());
    }

    #[test]
    fn a_gate_starts_with_its_whole_budget_unspent() {
        let gate = ReadinessGate::new(HeadFreshnessPolicy::NotJudged);
        assert_eq!(gate.checks(), 0);
        assert_eq!(gate.budget(), DEFAULT_CHECK_BUDGET);
        assert_eq!(gate.remaining(), DEFAULT_CHECK_BUDGET);
        // §3's ban on an unbounded retry loop is the budget's job, and the wire-facing
        // half of it — that a spent budget asks the node nothing — is measured against a
        // served endpoint in `crates/chain/tests/readiness_gate.rs`, where the call count
        // is the node's own record rather than this gate's word for it.
    }
}
