//! Arbitrage opportunities: `GraphSnapshot -> Opportunity`.
//!
//! M1 proved the state, M2 proved the market graph. M3 asks one question of that
//! graph: does a route that spends token A, buys token B, and sells token B back
//! into token A — through two *different* pools, in the same block — come out
//! with more A than it went in with?
//!
//! The boundary is deliberate. This crate reads no node, no state store and no
//! decoder: a [`evm_graph::GraphSnapshot`] is the only market it knows, and the
//! block it carries is the only block it prices. It also writes nothing back —
//! quoting a pool does not move that pool's reserves, and the simulated amounts
//! stay inside this crate.
//!
//! What it does **not** model, and what no number here may be read as:
//! gas, execution, a bundle, a bribe, a slippage-tolerant fill, or profit
//! realised after any of those. `gross_profit` is the AMM's own integer
//! arithmetic on one block's reserves (§41 of the task). "Profitable" here means
//! only "the two pools disagree by more than their fees".
//!
//! ```text
//! detect_opportunities(GraphSnapshot)
//!   -> enumerate_candidates      A -> pool1 -> B -> pool2 -> A, deduplicated
//!   -> evaluate(path)            both hops' attested fees and reserves
//!   -> find_optimal_input        bounded search over the input amount
//!   -> swap_exact_in             U256 constant-product quote
//! ```

#[cfg(test)]
mod support;

pub mod detector;
pub mod error;
pub mod math;
pub mod optimizer;
pub mod path;

pub use detector::{
    enumerate_candidates, CandidateRejection, Detection, Opportunity, OpportunityDetector,
    RejectionReason, SkippedPair,
};
pub use error::{MathError, OpportunityError, PathError, Result};
pub use math::{swap_exact_in, swap_through_two_hops};
pub use optimizer::{
    find_optimal_input, OptimizedCycle, PricedCycle, PricedHop, SearchPolicy, SearchRecord,
    SearchStrategy,
};
pub use path::{ArbitragePath, Hop, PathSimulation};

/// The one-call form the rest of the system should reach for.
pub fn detect_opportunities(snapshot: &evm_graph::GraphSnapshot) -> Result<Detection> {
    OpportunityDetector::default().detect(snapshot)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, Address, U256};
    use evm_core::{BlockNumber, ChainId, Fee};

    use super::*;
    use crate::support::{graph, spec};

    const CHAIN: ChainId = ChainId(7);
    const BLOCK: u64 = 4_000_001;
    const FEE: Fee = Fee {
        numerator: 997,
        denominator: 1000,
    };
    const A: Address = address!("0x000000000000000000000000000000000000000a");
    const B: Address = address!("0x000000000000000000000000000000000000000b");
    const P1: Address = address!("0x00000000000000000000000000000000000000f1");
    const P2: Address = address!("0x00000000000000000000000000000000000000f2");

    /// The crate's public entry point is the one-call form, and it has to agree
    /// with the detector object it wraps.
    #[test]
    fn the_free_function_matches_the_detector() {
        let snapshot = graph(
            &[
                spec(CHAIN, P1, A, B, 100_000, 50_000, Some(FEE)),
                spec(CHAIN, P2, B, A, 50_000, 125_000, Some(FEE)),
            ],
            BLOCK,
        );
        let via_function = detect_opportunities(&snapshot).expect("detect");
        let via_detector = OpportunityDetector::default()
            .detect(&snapshot)
            .expect("detect");
        assert_eq!(via_function, via_detector);
        assert_eq!(via_function.chain_id, CHAIN);
        assert_eq!(via_function.block_number, BlockNumber(BLOCK));
        let best = via_function.best().expect("an opportunity");
        assert!(best.gross_profit > U256::ZERO);
    }
}
