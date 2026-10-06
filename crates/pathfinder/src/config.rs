use serde::{Deserialize, Serialize};

/// How far a search may reach before it stops calling a route worth walking.
///
/// `max_hops` is the only knob, and it is bounded by the topology layer rather
/// than by a policy file: v0.1 prices cycles of two and three pools, so a search
/// longer than three hops cannot reach anything a later stage would act on, and
/// an unbounded search on a real graph is not a search but a wait. Two is the
/// floor because a one-hop "cycle" is a token traded against itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathFinderConfig {
    pub max_hops: usize,
}

impl PathFinderConfig {
    /// The smallest search that can close: `A -> pool1 -> B -> pool2 -> A`.
    pub const MIN_MAX_HOPS: usize = 2;
    /// The largest search v0.1 runs. Anything beyond this is a different
    /// milestone's problem, not a wider setting of this one.
    pub const MAX_MAX_HOPS: usize = 3;

    pub const fn new(max_hops: usize) -> Self {
        Self { max_hops }
    }

    /// Refuse a bound this crate cannot honour, instead of quietly searching at a
    /// different depth than the caller asked for.
    pub fn validate(&self) -> Result<(), crate::error::PathFinderError> {
        if self.max_hops < Self::MIN_MAX_HOPS || self.max_hops > Self::MAX_MAX_HOPS {
            return Err(crate::error::PathFinderError::MaxHopsOutOfRange {
                actual: self.max_hops,
            });
        }
        Ok(())
    }
}

impl Default for PathFinderConfig {
    fn default() -> Self {
        Self::new(Self::MAX_MAX_HOPS)
    }
}
