//! §51's separation between a route the market produced and a route a test produced.
//!
//! M7's success condition is one *real* arbitrage, and the only defence against counting a
//! route the bot itself arranged is a label that travels with the plan from the moment the
//! plan exists. A string in a report can be forgotten; a field with no `Default` cannot be,
//! which is why [`MarketKind`] is a constructor argument of
//! [`crate::sequence::SequencePlan::from_run`] rather than something a caller may set later.
//!
//! The two names are §51's, in its own words:
//!
//! ```text
//! REAL_MARKET        the pools, reserves and fees are the chain's, and this run put nothing there
//! CONTROLLED_FIXTURE the run set something up so that a route would exist
//! ```
//!
//! A controlled fixture still proves the executor works end to end — §50 says that is worth
//! running — but it proves `execution system works`, not `real market arbitrage works`, so
//! [`MarketKind::counts_as_real_arbitrage`] is the one function here that decides anything,
//! and it decides *no*: a profit made on a fixture is a profit made on a fixture.
//!
//! What makes the distinction checkable is that each variant must name its evidence. A
//! `REAL_MARKET` tag with no evidence string behind it is the fabrication §1 forbids, wearing
//! a label; the field is not a comment, it is the thing a reader goes to look at.

use serde_json::{json, Value};

/// §51's two kinds of route, each carrying the evidence that puts it in that kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarketKind {
    /// The venues, their reserves and their fees are the chain's own state, which this run
    /// read and did not write. `attested_by` names where a reader can check that — an
    /// attestation file, an evidence file, a block.
    RealMarket { attested_by: String },
    /// The run arranged something so a route would exist: it funded a sender it would not
    /// otherwise fund, deployed a pool, or priced against a value no node holds. `proves`
    /// states what the run is therefore entitled to claim, in the §50 words.
    ControlledFixture { proves: String },
}

impl MarketKind {
    /// §51's two spellings, exactly as the report must show them.
    pub fn name(&self) -> &'static str {
        match self {
            Self::RealMarket { .. } => "REAL_MARKET",
            Self::ControlledFixture { .. } => "CONTROLLED_FIXTURE",
        }
    }

    /// The evidence string, whichever kind this is.
    pub fn evidence(&self) -> &str {
        match self {
            Self::RealMarket { attested_by } => attested_by,
            Self::ControlledFixture { proves } => proves,
        }
    }

    /// §50/§51's rule, stated once: a fixture's profit is not a real arbitrage, whatever the
    /// receipts say.
    pub fn counts_as_real_arbitrage(&self) -> bool {
        matches!(self, Self::RealMarket { .. })
    }

    /// One line for a report or a log: the label and the evidence beside it, so a reader who
    /// sees only this line can still go and check.
    pub fn describe(&self) -> String {
        format!("{} — {}", self.name(), self.evidence())
    }

    pub fn to_json(&self) -> Value {
        json!({
            "market_kind": self.name(),
            "evidence": self.evidence(),
            "counts_as_real_arbitrage": self.counts_as_real_arbitrage(),
        })
    }

    /// Parse §51's spelling. The evidence string is required by the type, so this refuses a
    /// bare label rather than filling one in: `real-market --market-evidence ""` is exactly
    /// the unfounded claim §1 and §49 exist to stop, and no default can make it safe.
    pub fn parse(spec: &str, evidence: &str) -> std::result::Result<Self, String> {
        let evidence = evidence.trim();
        if evidence.is_empty() {
            return Err(format!(
                "{spec} needs --market-evidence to name what puts this run in that category; \
                 §51's separation is only a separation if a reader can check which side a run \
                 is on"
            ));
        }
        match spec {
            "REAL_MARKET" | "real_market" | "real-market" => Ok(Self::RealMarket {
                attested_by: evidence.to_string(),
            }),
            "CONTROLLED_FIXTURE" | "controlled_fixture" | "controlled-fixture" => {
                Ok(Self::ControlledFixture {
                    proves: evidence.to_string(),
                })
            }
            other => Err(format!(
                "{other} is neither of §51's two kinds (REAL_MARKET, CONTROLLED_FIXTURE)"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_controlled_fixture_is_never_counted_as_a_real_arbitrage() {
        let fixture = MarketKind::ControlledFixture {
            proves: "execution system works".to_string(),
        };
        assert_eq!(fixture.name(), "CONTROLLED_FIXTURE");
        assert!(!fixture.counts_as_real_arbitrage());
        assert_eq!(
            fixture.to_json()["counts_as_real_arbitrage"],
            json!(false),
            "the JSON row a report reads has to carry the same answer as the type"
        );

        let real = MarketKind::RealMarket {
            attested_by: "data/protocols-m7/v2-sepolia-42000006-*.json".to_string(),
        };
        assert_eq!(real.name(), "REAL_MARKET");
        assert!(real.counts_as_real_arbitrage());
    }

    #[test]
    fn a_label_without_evidence_is_refused_rather_than_assumed() {
        for spec in ["REAL_MARKET", "CONTROLLED_FIXTURE"] {
            let error = MarketKind::parse(spec, "   ")
                .err()
                .unwrap_or_else(|| panic!("{spec} with no evidence must not parse"));
            assert!(
                error.contains("--market-evidence"),
                "the refusal has to name the flag that would fix it: {error}"
            );
        }
        assert!(MarketKind::parse("mostly_real", "somewhere").is_err());
    }
}
