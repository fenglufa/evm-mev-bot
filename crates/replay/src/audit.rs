//! The audit trail of a state change (§11): who said it, where in the chain they
//! said it, what the store held before, and what it holds after.
//!
//! This is an *observation* of the one path updates already take — it is recorded
//! where the engine already stood, at the moment the store was asked to apply. It
//! is not a second write path, and it is not a second state engine (§9, §10): a
//! live run and a replay run produce these records from the same lines of
//! [`crate::engine::ReplayEngine::replay_block`], which is the only reason a
//! parity claim between them means anything.

use serde::Serialize;

use evm_core::{BlockNumber, LogIndex, PoolId, PoolState, TxHash, TxIndex};
use evm_protocol::ProtocolEvent;

/// Which protocol event produced the write.
///
/// Kept separate from what was written because the two are not the same fact: a
/// `Sync` writes reserves, a `Swap` writes nothing, and a pool being registered is
/// a consequence of the first claim about it rather than a distinct event. §12's
/// rule — `Sync` is the reserve authority, `Swap` is information — is only
/// checkable in a record that keeps both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ChangeSource {
    Sync,
    Swap,
    PoolCreated,
}

impl ChangeSource {
    pub fn of(event: &ProtocolEvent) -> Self {
        match event {
            ProtocolEvent::Sync(_) => Self::Sync,
            ProtocolEvent::Swap(_) => Self::Swap,
            ProtocolEvent::PoolCreated(_) => Self::PoolCreated,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sync => "Sync",
            Self::Swap => "Swap",
            Self::PoolCreated => "PoolCreated",
        }
    }
}

/// What the store accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Written {
    /// The pool's identity and pair, from attested metadata.
    Registration,
    /// A reserve statement: the only thing that may move a pool's price.
    ReserveStatement,
}

impl Written {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Registration => "registered",
            Self::ReserveStatement => "synced",
        }
    }
}

/// One accepted write, with its chain position and both sides of the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateChange {
    pub block_number: BlockNumber,
    pub tx_hash: TxHash,
    pub tx_index: TxIndex,
    pub log_index: LogIndex,
    pub pool: PoolId,
    pub source: ChangeSource,
    pub written: Written,
    /// What the store held for this pool immediately before. `None` on a
    /// registration and on the first reserve statement for a newly registered pool.
    pub before: Option<PoolState>,
    /// What it holds after. `None` can only mean a registration, which sets
    /// metadata and no reserves.
    pub after: Option<PoolState>,
}

impl StateChange {
    /// The reserve delta this change states, in the pool's own `token0`/`token1`
    /// order. `None` when either side is absent, because "changed by" a missing
    /// value is not a number.
    pub fn reserve_delta(&self) -> Option<(alloy_primitives::U256, alloy_primitives::U256)> {
        let (before, after) = match (self.before, self.after) {
            (Some(before), Some(after)) => (before, after),
            _ => return None,
        };
        Some((
            after.reserve0.saturating_sub(before.reserve0),
            after.reserve1.saturating_sub(before.reserve1),
        ))
    }
}

/// The audit form (§11): the chain position, the pool, the event, both sides.
/// One line per change, so a session file can be read without re-deriving
/// anything from the store.
impl std::fmt::Display for StateChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let position = format!(
            "block {} tx {} log {}",
            self.block_number.0, self.tx_index.0, self.log_index.0
        );
        let (before, after) = match (self.before, self.after) {
            (Some(before), Some(after)) => (
                format!("{}/{}", before.reserve0, before.reserve1),
                format!("{}/{}", after.reserve0, after.reserve1),
            ),
            (None, Some(after)) => (
                "-".to_string(),
                format!("{}/{}", after.reserve0, after.reserve1),
            ),
            (before, None) => (
                before
                    .map(|state| format!("{}/{}", state.reserve0, state.reserve1))
                    .unwrap_or_else(|| "-".to_string()),
                "-".to_string(),
            ),
        };
        write!(
            f,
            "{position} pool {} event {} wrote {} reserves {before} -> {after} tx {}",
            self.pool.address,
            self.source.as_str(),
            self.written.as_str(),
            self.tx_hash.0,
        )
    }
}

impl Serialize for StateChange {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct as _;
        let mut out = serializer.serialize_struct("StateChange", 10)?;
        out.serialize_field("block_number", &self.block_number.0)?;
        out.serialize_field("tx_hash", &format!("{:#x}", self.tx_hash.0))?;
        out.serialize_field("tx_index", &self.tx_index.0)?;
        out.serialize_field("log_index", &self.log_index.0)?;
        out.serialize_field("chain_id", &self.pool.chain_id.0)?;
        out.serialize_field("pool", &self.pool.address.to_string())?;
        out.serialize_field("event", self.source.as_str())?;
        out.serialize_field("written", self.written.as_str())?;
        out.serialize_field("before", &self.before)?;
        out.serialize_field("after", &self.after)?;
        out.end()
    }
}
