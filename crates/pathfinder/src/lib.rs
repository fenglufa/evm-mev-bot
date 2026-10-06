//! Cycle search: `GraphSnapshot -> CycleCandidate[]`.
//!
//! M2 proved the graph, M3 proved what a graph is worth pricing, M9.1 proved which
//! pools exist, and M9.2 proved which block each pool's reserves are true at. This
//! crate asks the one question left before money enters the picture: which closed
//! routes does this block's market actually contain?
//!
//! The boundary is the deliverable. This is a topology layer, and it is sealed at
//! the dependency level: `evm-pathfinder`'s production dependencies are `evm-core`,
//! `evm-graph`, `serde` and `thiserror` — no node client, no protocol decoder, no
//! AMM arithmetic, no simulator, no signer. (`evm-state` appears only as a
//! dev-dependency, because a test cannot invent a `GraphSnapshot` without a state
//! layer to put into it; it reaches no network either.) A reader who wants to know
//! whether the search reached the chain can answer it by reading `Cargo.toml`.
//!
//! ```text
//! find_cycles(GraphSnapshot, PathFinderConfig)
//!   -> walk each start token, depth-bounded by `max_hops`
//!   -> refuse a repeated pool, a repeated token, an open route
//!   -> CanonicalKey = minimum(rotation(edges))   three seats, one cycle
//!   -> CycleCandidate { chain, block, start_token, edges, hops, key, fee_status }
//! ```
//!
//! What a candidate deliberately does **not** carry, because each of these is a
//! later level and this milestone's whole claim is that the levels stay distinct:
//!
//! ```text
//! Path != CycleCandidate != Executable Opportunity
//!       != Simulated Profit != Executed Profit != Realized Profit
//! ```
//!
//! no `amount_in`, no `amount_out`, no `gross_profit`, no `net_profit`, no gas, no
//! L1 fee, no price impact, no simulation status, no execution status. A
//! `FeeStatus::Incomplete` candidate is a shape in a graph whose fees nobody has
//! proved; it is enumerated, reported, and unusable by anything that prices. Nothing
//! here resolves a fee, defaults one, or guesses 997/1000.

pub mod candidate;
pub mod config;
pub mod error;
pub mod search;

pub use candidate::{CanonicalKey, CycleCandidate, FeeStatus};
pub use config::PathFinderConfig;
pub use error::{PathFinderError, Result};
pub use search::{find_cycles, find_cycles_traced, PathFinderRun};

#[cfg(test)]
mod tests {
    use super::*;

    /// The bound the task sets: two hops is the shortest thing that closes, and
    /// three is as far as v0.1 walks.
    #[test]
    fn the_bound_is_two_to_three_and_default_is_three() {
        assert_eq!(PathFinderConfig::MIN_MAX_HOPS, 2);
        assert_eq!(PathFinderConfig::MAX_MAX_HOPS, 3);
        assert_eq!(PathFinderConfig::default().max_hops, 3);
    }

    #[test]
    fn a_config_outside_the_bound_is_refused_before_any_walking() {
        for max_hops in [0usize, 1, 4, 5, 100] {
            let config = PathFinderConfig::new(max_hops);
            assert_eq!(
                config.validate(),
                Err(PathFinderError::MaxHopsOutOfRange { actual: max_hops }),
                "{max_hops} hops is not a search this crate runs"
            );
        }
        for max_hops in [2usize, 3] {
            assert_eq!(PathFinderConfig::new(max_hops).validate(), Ok(()));
        }
    }
}
