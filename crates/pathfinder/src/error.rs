/// Why a search produced nothing usable, in the search's own terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PathFinderError {
    /// The caller asked for a depth this crate will not walk. A cycle needs two
    /// hops to be a cycle, and v0.1 stops at three because nothing downstream is
    /// priced to go further.
    #[error("`max_hops` must be between 2 and 3, got {actual}")]
    MaxHopsOutOfRange { actual: usize },
}

pub type Result<T> = std::result::Result<T, PathFinderError>;
