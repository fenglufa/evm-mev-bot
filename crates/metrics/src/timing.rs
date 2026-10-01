//! §39's `PipelineTiming`: one record per market event, stamped as it travels.
//!
//! The stage stamps are all on the run's [`Clock`] (monotonic, milliseconds
//! since this process started) except `chain_timestamp_secs`, which is the
//! chain's own seconds-from-epoch. The two are never combined into one number;
//! the cross-clock gap gets its own named delta so a reader knows which clock it
//! was measured against (§70's `block_received_latency` says so in its name).

use serde::Serialize;

/// One block's journey through this process, in the stage order §39 lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PipelineTiming {
    /// The chain's clock for the block this event belongs to.
    pub chain_timestamp_secs: u64,
    /// Wall-clock Unix milliseconds at the moment this process first heard of it.
    pub received_unix_ms: u64,
    /// Monotonic stamps, milliseconds since the run began. `None` means the
    /// event never reached that stage — which is a result, not a gap in the
    /// record.
    pub received_at: u64,
    pub decoded_at: Option<u64>,
    pub state_updated_at: Option<u64>,
    pub graph_updated_at: Option<u64>,
    pub opportunity_detected_at: Option<u64>,
    pub simulation_started_at: Option<u64>,
    pub simulation_finished_at: Option<u64>,
    pub risk_decided_at: Option<u64>,
}

impl PipelineTiming {
    pub fn new(chain_timestamp_secs: u64, received_unix_ms: u64, received_at: u64) -> Self {
        Self {
            chain_timestamp_secs,
            received_unix_ms,
            received_at,
            decoded_at: None,
            state_updated_at: None,
            graph_updated_at: None,
            opportunity_detected_at: None,
            simulation_started_at: None,
            simulation_finished_at: None,
            risk_decided_at: None,
        }
    }

    pub fn set_decoded(&mut self, at: u64) {
        self.decoded_at = Some(at);
    }

    pub fn set_state_updated(&mut self, at: u64) {
        self.state_updated_at = Some(at);
    }

    pub fn set_graph_updated(&mut self, at: u64) {
        self.graph_updated_at = Some(at);
    }

    pub fn set_opportunity_detected(&mut self, at: u64) {
        self.opportunity_detected_at = Some(at);
    }

    pub fn set_simulation_started(&mut self, at: u64) {
        self.simulation_started_at = Some(at);
    }

    pub fn set_simulation_finished(&mut self, at: u64) {
        self.simulation_finished_at = Some(at);
    }

    pub fn set_risk_decided(&mut self, at: u64) {
        self.risk_decided_at = Some(at);
    }

    /// Every stage-to-stage duration the record can support, named as §38 names
    /// them. A pair whose later stage never happened produces no entry — the
    /// latency table reports "measured: false" for a series with no samples,
    /// which is the honest form of "this run got no further".
    pub fn deltas(&self) -> Vec<(&'static str, u64)> {
        let mut out = Vec::new();
        let mut push = |name: &'static str, from: Option<u64>, to: Option<u64>| {
            if let (Some(from), Some(to)) = (from, to) {
                out.push((name, to.saturating_sub(from)));
            }
        };
        push(
            "received_to_decoded",
            Some(self.received_at),
            self.decoded_at,
        );
        push(
            "decoded_to_state_updated",
            self.decoded_at,
            self.state_updated_at,
        );
        push(
            "state_to_graph_updated",
            self.state_updated_at,
            self.graph_updated_at,
        );
        push(
            "graph_to_opportunity_detected",
            self.graph_updated_at,
            self.opportunity_detected_at,
        );
        push(
            "opportunity_to_simulation_started",
            self.opportunity_detected_at,
            self.simulation_started_at,
        );
        push(
            "simulation_duration",
            self.simulation_started_at,
            self.simulation_finished_at,
        );
        push(
            "simulation_to_risk_decided",
            self.simulation_finished_at,
            self.risk_decided_at,
        );
        // §38's aggregate questions, asked from the record's own stamps.
        push(
            "block_to_state",
            Some(self.received_at),
            self.state_updated_at,
        );
        push(
            "block_to_opportunity",
            Some(self.received_at),
            self.opportunity_detected_at,
        );
        push(
            "block_to_simulation_end",
            Some(self.received_at),
            self.simulation_finished_at,
        );
        push(
            "block_to_risk",
            Some(self.received_at),
            self.risk_decided_at,
        );
        out
    }

    /// `chain timestamp → received`, across the two clocks. Not a pipeline
    /// latency: it contains the provider's propagation delay, and it is named
    /// for that.
    pub fn chain_to_received_ms(&self) -> u64 {
        crate::clock::chain_to_wall_lag_ms(self.chain_timestamp_secs, self.received_unix_ms).max(0)
            as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_record_reports_only_the_hops_it_actually_measured() {
        let mut t = PipelineTiming::new(1_700_000_000, 1_700_000_001_500, 10);
        t.set_decoded(14);
        t.set_state_updated(20);
        let names: Vec<&str> = t.deltas().iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            vec![
                "received_to_decoded",
                "decoded_to_state_updated",
                "block_to_state",
            ],
            "{names:?}"
        );
        assert_eq!(t.deltas()[2], ("block_to_state", 10));
        assert_eq!(t.chain_to_received_ms(), 1500);
    }

    #[test]
    fn end_to_end_is_the_whole_record_not_a_sum_of_the_middle() {
        let mut t = PipelineTiming::new(1, 2, 0);
        for (index, stamp) in [1u64, 2, 3, 4, 5, 6, 7].iter().enumerate() {
            match index {
                0 => t.set_decoded(*stamp),
                1 => t.set_state_updated(*stamp),
                2 => t.set_graph_updated(*stamp),
                3 => t.set_opportunity_detected(*stamp),
                4 => t.set_simulation_started(*stamp),
                5 => t.set_simulation_finished(*stamp),
                _ => t.set_risk_decided(*stamp),
            }
        }
        let deltas = t.deltas();
        let end_to_end = deltas
            .iter()
            .find(|(name, _)| *name == "block_to_risk")
            .map(|(_, ms)| *ms);
        assert_eq!(end_to_end, Some(7));
    }
}
