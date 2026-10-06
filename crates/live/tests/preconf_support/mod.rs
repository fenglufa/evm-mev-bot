//! Shared builders for the M9.4 test files.
//!
//! Everything here is a *controlled fixture*: block numbers and addresses are derived
//! from single bytes so a fixture is legible and a wrong value is visible, and no pool,
//! reserve, fee or opportunity is invented (§43's ban on fabricated market data applies
//! to test scaffolding as much as to evidence). The real-data half of the same code path
//! is `data/evidence/m9/m9.4/`, built from actual captures.
//!
//! Two shapes of source are provided because the two claims need different pacing:
//!
//! * [`ScriptedSource`] is finite and synchronous — it proves ordering, the §48 count of
//!   frames, and the §41 determinism double-run.
//! * [`PacedSource`] awaits each item from a channel — it is what lets a test interleave
//!   a canonical digest *between* frames, which the synchronous shape cannot express
//!   because a finite source runs to exhaustion before the test gets a turn.
//!
//! Both count reads. That counter is §34's witness: the link must perform exactly one
//! read per payload it applies, and a run whose reads exceed the served items would be
//! inventing RPC.

#![allow(dead_code)]

use std::collections::VecDeque;

use alloy_primitives::{Address, B256};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use evm_core::ChainId;
use evm_live::{
    frame_from_pending_value, receipts_from_value, AffectedPool, CanonicalDigest, EarlyRadar,
    FrameSource, PoolSet, PreconfError, RadarConfig, RadarEvent, RadarInput,
};

/// The chain M9.4 was measured on. Named once so a wrong-identity fixture has something
/// to be wrong relative to.
pub const CHAIN: ChainId = ChainId(91342);
/// The committed endpoint digest (§33: an evidence or test file carries a digest, never
/// a URL).
pub const ENDPOINT: &str = "rpc-faa716cada04a9ef";
/// A different chain id, for NC6.
pub const OTHER_CHAIN: ChainId = ChainId(1);
/// The reth client version string the capture reported, for §47's protocol version field.
pub const PROTOCOL_VERSION: &str = "reth/v2.3.0";

pub const fn b256(byte: u8) -> B256 {
    B256::new([byte; 32])
}

/// An address from one byte, so `addr(0xa1)` is obviously a fixture and not a real pool.
pub const fn addr(byte: u8) -> Address {
    Address::new([byte; 20])
}

/// Lowercase `0x` hex of an arbitrary byte string, without a hex crate dependency.
/// An address as the wire spells it.
pub fn hex_addr(value: Address) -> String {
    format!("{:#x}", value)
}

pub fn hex_bytes(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("0x");
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    out
}

pub fn hex(byte: u8) -> String {
    hex_bytes(&[byte])
}

/// Hex for a 32-byte all-same-value blob, as the wire spells it.
pub fn hex32(byte: u8) -> String {
    hex_bytes(&[byte; 32])
}

/// The Swap event topic0 the measured emitters actually used, reduced to a fixture byte.
pub fn swap_topic() -> String {
    hex32(0xd0)
}

/// One full transaction object, in the shape the endpoint answers
/// (`eth_getBlockByNumber(["pending", true])`): an object, not a hash.
pub fn transaction(byte: u8, to: Option<Address>, index: u64) -> Value {
    json!({
        "hash": hex32(byte),
        "from": format!("{:#x}", addr(0xf1)),
        "to": to.map(|target| format!("{:#x}", target)),
        "input": if byte.is_multiple_of(2) { "0x095ea7b3" } else { "0x38ed1739000000000000000000000000" },
        "transactionIndex": format!("0x{index:x}"),
        "nonce": "0x7",
        "value": "0x0",
    })
}

/// One pending block object.
///
/// `state_root` is deliberately the all-zero placeholder: that is what the measured
/// endpoint answers (52/52 reads), and a fixture that invented a real root would hide the
/// §52 limitation the decoder is written to record.
pub fn pending(number: u64, parent: B256, view: Option<B256>, transactions: Vec<Value>) -> Value {
    let mut object = json!({
        "number": format!("0x{number:x}"),
        "parentHash": hex32_disp(parent),
        "timestamp": "0x67089d20",
        "gasUsed": "0x5208",
        "gasLimit": "0x1c9c380",
        "miner": format!("{:#x}", addr(0xfe)),
        "stateRoot": hex32(0),
        "transactions": transactions,
    });
    if let Some(hash) = view {
        object["hash"] = json!(hex32_disp(hash));
    }
    object
}

pub fn hex32_disp(value: B256) -> String {
    format!("{:#x}", value)
}

/// One log entry inside a receipt.
pub fn log(
    emitter: Address,
    transaction_hash: B256,
    log_index: u64,
    tx_index: u64,
    removed: bool,
) -> Value {
    json!({
        "address": format!("{:#x}", emitter),
        "topics": [swap_topic()],
        "data": "0x",
        "blockNumber": "0x0",
        "blockHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
        "transactionHash": hex32_disp(transaction_hash),
        "logIndex": format!("0x{log_index:x}"),
        "transactionIndex": format!("0x{tx_index:x}"),
        "removed": removed,
    })
}

/// One receipt. `status` is `None` to omit the key entirely (§25's fail-closed case), and
/// a `Some(0)` is a revert that must not reach the affected set (NC11).
pub fn receipt(
    transaction_hash: B256,
    block_number: u64,
    block_hash: B256,
    status: Option<u64>,
    logs: Vec<Value>,
) -> Value {
    let mut object = json!({
        "transactionHash": hex32_disp(transaction_hash),
        "transactionIndex": "0x0",
        "blockNumber": format!("0x{block_number:x}"),
        "blockHash": hex32_disp(block_hash),
        "logs": logs,
        "gasUsed": "0x5208",
    });
    if let Some(status) = status {
        object["status"] = json!(format!("0x{status:x}"));
    }
    object
}

/// §15's canonical side: a sealed block, as identity and content only.
pub fn digest(
    number: u64,
    hash: B256,
    parent: B256,
    transaction_hashes: Vec<B256>,
    observed_at_unix_ms: u64,
) -> CanonicalDigest {
    CanonicalDigest {
        chain_id: CHAIN,
        number: evm_core::BlockNumber(number),
        hash,
        parent_hash: parent,
        chain_timestamp_secs: 0x6708_9d20,
        transaction_hashes,
        affected_pools: None,
        observed_at_unix_ms,
    }
}

/// §47's mandatory fixture labels. Every fixture carries all five; a missing label is a
/// fixture that cannot be re-derived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixtureMeta {
    pub name: &'static str,
    pub source: &'static str,
    pub captured_at_unix_ms: u64,
    pub chain_id: u64,
    pub block_number: u64,
    pub protocol_version: &'static str,
}

impl FixtureMeta {
    /// A controlled fixture: synthesised in this file, labelled as such (§57's
    /// REAL_MARKET/CONTROLLED_FIXTURE discipline from M7).
    pub const fn controlled(name: &'static str, block_number: u64) -> Self {
        Self {
            name,
            source: "controlled-fixture (crates/live/tests/preconf_support)",
            captured_at_unix_ms: 1_728_000_000_000,
            chain_id: 91_342,
            block_number,
            protocol_version: PROTOCOL_VERSION,
        }
    }

    pub fn as_json(&self) -> Value {
        json!({
            "fixture": self.name,
            "source": self.source,
            "capture_timestamp_unix_ms": self.captured_at_unix_ms,
            "chain_id": self.chain_id,
            "block": self.block_number,
            "protocol_version": self.protocol_version,
        })
    }
}

/// A clock the caller owns: each read of it advances by `step`. No test in M9.4 reads a
/// system clock, which is what makes §41's determinism claim checkable and §26's stamps
/// reproducible.
pub struct StepClock {
    pub now: u64,
    pub step: u64,
}

impl StepClock {
    pub const fn new(start: u64, step: u64) -> Self {
        Self { now: start, step }
    }

    pub fn tick(&mut self) -> u64 {
        self.now += self.step;
        self.now
    }
}

/// What the source was asked to do at one step.
#[derive(Clone, Debug)]
pub enum Step {
    /// A payload (or `None`, meaning the provider answered null).
    Answer(Option<Value>, Option<Value>),
    /// The transport itself failed.
    Failed(String),
}

impl Step {
    pub fn null() -> Self {
        Self::Answer(None, None)
    }

    pub fn frame(view: Value) -> Self {
        Self::Answer(Some(view), None)
    }

    pub fn malformed() -> Self {
        Self::Answer(Some(json!({"gasUsed": "not-a-quantity"})), None)
    }

    pub fn failed(reason: &str) -> Self {
        Self::Failed(reason.to_string())
    }
}

/// The finite, synchronous source. Reads are counted, and a read never happens after the
/// script is empty — the link's own exhaustion check is what stops it.
pub struct ScriptedSource {
    script: VecDeque<Step>,
    receipts_supported: bool,
    pending_receipts: Option<Value>,
    pub reads: usize,
}

impl ScriptedSource {
    pub fn new(script: Vec<Step>, receipts_supported: bool) -> Self {
        Self {
            script: script.into_iter().collect(),
            receipts_supported,
            pending_receipts: None,
            reads: 0,
        }
    }
}

#[async_trait]
impl FrameSource for ScriptedSource {
    fn endpoint_id(&self) -> &str {
        ENDPOINT
    }

    fn transport(&self) -> &'static str {
        "scripted-finite"
    }

    fn is_finite(&self) -> bool {
        true
    }

    fn finished(&self) -> bool {
        self.script.is_empty()
    }

    fn supports_pending_receipts(&self) -> bool {
        self.receipts_supported
    }

    async fn next_view(&mut self) -> Result<Option<Value>, PreconfError> {
        self.reads += 1;
        match self.script.pop_front() {
            Some(Step::Answer(view, receipts)) => {
                self.pending_receipts = receipts;
                Ok(view)
            }
            Some(Step::Failed(reason)) => Err(PreconfError::Transport(reason)),
            None => Ok(None),
        }
    }

    async fn next_receipts(&mut self) -> Result<Option<Value>, PreconfError> {
        Ok(self.pending_receipts.take())
    }
}

/// The paced source: each step is pulled from a channel, so a test task can interleave
/// canonical digests between frames while the link runs. When the test drops its sender
/// the supply ends, which is how the run finishes without a timeout.
pub struct PacedSource {
    steps: mpsc::Receiver<Step>,
    /// Receipts paired with the view returned by the current read, consumed by
    /// `next_receipts` before the next read overwrites it.
    pending_receipts: Option<Value>,
    /// Set once a read has found the channel closed. `finished` cannot await, so the
    /// ending is remembered from the read that discovered it rather than re-probed.
    ended: bool,
    pub reads: usize,
}

impl PacedSource {
    pub fn channel() -> (Self, mpsc::Sender<Step>) {
        let (tx, rx) = mpsc::channel(16);
        (
            Self {
                steps: rx,
                pending_receipts: None,
                ended: false,
                reads: 0,
            },
            tx,
        )
    }
}

#[async_trait]
impl FrameSource for PacedSource {
    fn endpoint_id(&self) -> &str {
        ENDPOINT
    }

    fn transport(&self) -> &'static str {
        "scripted-paced"
    }

    fn is_finite(&self) -> bool {
        true
    }

    fn finished(&self) -> bool {
        self.ended
    }

    fn supports_pending_receipts(&self) -> bool {
        true
    }

    async fn next_view(&mut self) -> Result<Option<Value>, PreconfError> {
        self.reads += 1;
        match self.steps.recv().await {
            Some(Step::Answer(view, receipts)) => {
                self.pending_receipts = receipts;
                Ok(view)
            }
            Some(Step::Failed(reason)) => {
                self.pending_receipts = None;
                Err(PreconfError::Transport(reason))
            }
            None => {
                self.ended = true;
                Ok(None)
            }
        }
    }

    async fn next_receipts(&mut self) -> Result<Option<Value>, PreconfError> {
        Ok(self.pending_receipts.take())
    }
}

/// Collect every event a run produced. The sink is dropped by `run` when it returns, so
/// this terminates on the channel closing rather than on a count.
pub async fn drain(mut rx: mpsc::Receiver<RadarEvent>) -> Vec<RadarEvent> {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    events
}

/// The event classes, in order — the §43 assertion target. Comparing labels rather than
/// events keeps an ordering failure readable and a detail-wording change harmless.
pub fn labels(events: &[RadarEvent]) -> Vec<&'static str> {
    events.iter().map(RadarEvent::kind_label).collect()
}

/// The heights a run accepted, in the order it accepted them.
pub fn accepted_heights(events: &[RadarEvent]) -> Vec<u64> {
    events
        .iter()
        .filter_map(|event| match event {
            RadarEvent::FrameAccepted { number, .. } => Some(*number),
            _ => None,
        })
        .collect()
}

pub fn count(events: &[RadarEvent], kind: &str) -> usize {
    labels(events)
        .iter()
        .filter(|label| *label == &kind)
        .count()
}

/// The pool set a fixture radar is pointed at (§12).
pub fn pool_set(pools: impl IntoIterator<Item = Address>) -> PoolSet {
    PoolSet::new(CHAIN, pools)
}

/// A radar over the measured chain and endpoint digest, with the config the test names.
pub fn radar_with(pools: impl IntoIterator<Item = Address>, config: RadarConfig) -> EarlyRadar {
    EarlyRadar::new(CHAIN, ENDPOINT, pool_set(pools), config)
}

/// The same, with the defaults — most matrix rows only care about the frames.
pub fn radar(pools: impl IntoIterator<Item = Address>) -> EarlyRadar {
    radar_with(pools, RadarConfig::default())
}

/// Hand one payload to the radar the way the link does: take the next session-local
/// sequence, decode with the entry point the link uses, then ingest. It is not a
/// shortcut past §9's checks — it is the same call a run makes, minus the transport.
pub fn offer(radar: &mut EarlyRadar, raw: Value, clock: &mut StepClock) -> Vec<RadarEvent> {
    offer_with(radar, raw, clock, None)
}

/// [`offer`] plus the receipt list read against the same height, as the replay
/// transport can supply it (§34: never on the live path).
pub fn offer_with(
    radar: &mut EarlyRadar,
    raw: Value,
    clock: &mut StepClock,
    receipts: Option<Vec<Value>>,
) -> Vec<RadarEvent> {
    let chain = radar.chain_id();
    let endpoint = radar.endpoint_id().to_string();
    let observed_at = clock.tick();
    let decoded_at = clock.tick();
    let local_frame_sequence = radar.next_local_sequence();
    let frame = frame_from_pending_value(chain, &raw, local_frame_sequence, observed_at, &endpoint)
        .expect("a fixture the decoder accepts");
    let receipts = receipts
        .map(|list| receipts_from_value(&Value::Array(list)).expect("fixture receipts decode"));
    let input = RadarInput {
        frame,
        receipts,
        decoded_at_unix_ms: decoded_at,
    };
    let mut stamp = move || clock.tick();
    radar.ingest(input, &mut stamp)
}

/// The canonical side of one height, offered to the same radar.
pub fn seal(
    radar: &mut EarlyRadar,
    sealed: CanonicalDigest,
    clock: &mut StepClock,
) -> Vec<RadarEvent> {
    let mut stamp = move || clock.tick();
    radar.note_canonical(&sealed, &mut stamp)
}

/// The pool findings a batch of events carries, in emission order.
pub fn affected(events: &[RadarEvent]) -> Vec<AffectedPool> {
    events
        .iter()
        .filter_map(|event| match event {
            RadarEvent::PoolAffected(pool) => Some(pool.clone()),
            _ => None,
        })
        .collect()
}

/// The refusal classes a batch of events carries.
pub fn refusals(events: &[RadarEvent]) -> Vec<(u64, &'static str)> {
    events
        .iter()
        .filter_map(|event| match event {
            RadarEvent::FrameRejected { number, reason } => Some((*number, *reason)),
            _ => None,
        })
        .collect()
}

/// The `Sequence` details a batch of events carries — the channel §9's unmeasurable
/// and unconfirmed findings travel on, so a negative control asserts on its text.
pub fn sequences(events: &[RadarEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            RadarEvent::Sequence { detail } => Some(detail.as_str()),
            _ => None,
        })
        .collect()
}

// — fixture conventions shared by the matrix, the negative controls, and the §47 set.
//
// A pending frame and the sealed block that closes it must be able to name *exactly*
// the same transaction list, and a parent must be able to name the hash the fixture
// actually sealed. Both come from these two rules, so no test has to invent a hash to
// make a check meaningful — and no check passes because a hash was random.

/// The transaction hash byte a fixture gives to (height, index).
pub fn tx_byte(number: u64, index: usize) -> u8 {
    (number as u8)
        .saturating_mul(4)
        .saturating_add(index as u8)
        .saturating_add(1)
}

/// The transaction list the block at `number` carries, first `count` entries.
pub fn tx_hashes(number: u64, count: usize) -> Vec<B256> {
    (0..count)
        .map(|index| b256(tx_byte(number, index)))
        .collect()
}

/// The hash a sealed block has on this convention: its own height, spelled as a hash.
/// Distinct from every view hash a fixture uses, so `hash_matched` is false by
/// construction and the claim stays measurable rather than accidental.
pub fn sealed_hash(number: u64) -> B256 {
    b256(number as u8)
}

/// A pending frame at `number`, read under `view_byte`, whose transactions target the
/// given pools — a `0` entry targets an address that is not in the pool set. The
/// parent names [`sealed_hash`] of the height below, so a radar that has sealed that
/// height can check continuity, and one that has not records it as uncheckable.
pub fn frame_of(number: u64, view_byte: u8, targets: &[u8]) -> Value {
    let transactions = targets
        .iter()
        .enumerate()
        .map(|(index, &target)| {
            let to = match target {
                0 => None,
                byte => Some(addr(byte)),
            };
            transaction(tx_byte(number, index), to, index as u64)
        })
        .collect();
    let parent = if number == 0 {
        b256(0)
    } else {
        sealed_hash(number - 1)
    };
    pending(number, parent, Some(b256(view_byte)), transactions)
}

/// Seal `number` with the fixture's own identity, at this clock's next tick, so every
/// stamp in a test comes from the same clock the lead is computed from.
pub fn seal_next(
    radar: &mut EarlyRadar,
    number: u64,
    transactions: Vec<B256>,
    clock: &mut StepClock,
) -> Vec<RadarEvent> {
    let observed_at_unix_ms = clock.tick();
    seal(
        radar,
        digest(
            number,
            sealed_hash(number),
            if number == 0 {
                b256(0)
            } else {
                sealed_hash(number - 1)
            },
            transactions,
            observed_at_unix_ms,
        ),
        clock,
    )
}

/// The part of a height record that does not depend on when the height was read:
/// `(number, stage, frames, views, transactions, affected pools, state root known,
/// wire index known, endpoint)`. §43's determinism row compares this projection
/// rather than a full record because the stamps in a full record are this process's
/// wall clock, which is read-order dependent by design — a table claiming otherwise
/// would be claiming the process clock is the chain.
pub type RecordShape = (u64, &'static str, u64, u64, u64, u64, bool, bool, String);

pub fn record_shape(radar: &EarlyRadar) -> Vec<RecordShape> {
    radar
        .records()
        .iter()
        .map(|record| {
            (
                record.number,
                record.stage.as_str(),
                record.frames_observed,
                record.distinct_view_hashes,
                record.transactions_in_latest,
                record.affected_pools,
                record.state_root_known,
                record.wire_index_known,
                record.endpoint_id.clone(),
            )
        })
        .collect()
}
