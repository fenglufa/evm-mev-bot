//! Where a simulation's state comes from, and what it is allowed to invent.
//!
//! §5 of the task is blunt about this: real simulation means real bytecode read
//! at real state. So the whole crate sits behind one trait, [`StateProvider`],
//! whose every read is already pinned to a block — a provider cannot be asked
//! for "latest", because it does not carry the height as a parameter. That is
//! §49 turned into a type: there is no method to call the wrong way.
//!
//! Two providers exist (§62, and only two): [`RpcStateProvider`], which reads a
//! node through the chain adapter, and [`DumpStateProvider`], which replays a
//! recorded [`StateDump`] from disk. The dump is not a convenience copy of the
//! RPC — it is what the RPC *answered*, so a fixture and a live run cannot
//! disagree about what the chain said without a test noticing.
//!
//! What neither may do: fill a gap. A missing account is
//! [`ProviderError::Missing`], which becomes
//! [`SimulationError::MissingState`][crate::error::SimulationError::MissingState],
//! and ends the run. Zero is not a default here — for a pool reserve it would
//! be a fabrication, and the difference between "the node pruned that block"
//! and "this account is empty" is the difference between an unusable state
//! source and an unusable opportunity.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use evm_chain::{normalize_address, BlockContext, ChainAdapter, RpcTraceSink};
use evm_core::{BlockNumber, ChainId};

use crate::acquisition::{BoundedDispatch, ConcurrencyReport, StateReadDescriptor};

/// The block a simulation is pinned to, and the header hash it must have.
///
/// The hash is not decoration. A number alone names a height, not a chain: two
/// forks share it, and a reorganised one replaces it. Every state read in this
/// crate happens under a pin, and [`crate::engine`] refuses to execute before
/// the provider proves it is serving *this* hash at *this* height.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BlockPin {
    pub number: BlockNumber,
    pub hash: B256,
}

impl BlockPin {
    pub const fn new(number: BlockNumber, hash: B256) -> Self {
        Self { number, hash }
    }
}

impl std::fmt::Display for BlockPin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.number.0, self.hash)
    }
}

/// An account header as the state source reports it.
///
/// No `code` field on purpose: bytecode is a separate read (§62 lists
/// `account`, `storage`, `code` apart), and a provider that charges for one
/// should not have to serve the other to answer a balance question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountState {
    pub balance: U256,
    pub nonce: u64,
    pub code_hash: B256,
}

impl AccountState {
    /// The header of an account that exists and holds nothing: no ether, no
    /// nonce, and the keccak of empty bytes where code would be.
    pub const EMPTY: AccountState = AccountState {
        balance: U256::ZERO,
        nonce: 0,
        code_hash: KECCAK_EMPTY,
    };

    pub fn is_empty_code(&self) -> bool {
        self.code_hash == KECCAK_EMPTY
    }
}

/// `keccak256([])` — the hash an account with no code carries.
pub const KECCAK_EMPTY: B256 = B256::new([
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xad, 0xf6, 0x01, 0x7b, 0x34, 0x34, 0x31,
]);

/// A state source could not answer, and the two reasons why are not the same.
///
/// The field is called `provider`, not `source`: thiserror treats a field named
/// `source` as an embedded error, and the name of the thing that came up short
/// is a string, not a cause.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ProviderError {
    /// The source works and reports that this piece of state is not there: a
    /// pruned block, an entry a recorded dump never captured.
    #[error("state is not available from {provider}: {what}")]
    Missing { provider: String, what: String },

    /// The source itself failed — node down, bad response, timeout. Nothing was
    /// learned about the state, only about the provider.
    #[error("state provider {provider} failed: {reason}")]
    Unavailable { provider: String, reason: String },
}

pub type ProviderResult<T> = std::result::Result<T, ProviderError>;

/// Read-only, block-pinned state access.
///
/// Deliberately an async trait over `&self`: the height is baked into the
/// implementation, not passed per call, so a caller cannot accidentally mix two
/// blocks. §70's warning that the historical transaction and the hypothetical
/// one are different objects is enforced here — the only state this can serve
/// is the post-block state at the pin.
#[async_trait]
pub trait StateProvider: Send + Sync {
    /// The chain this provider serves. A bare address is never an identity
    /// (§63), so this is part of every cache key and every comparison.
    fn chain_id(&self) -> ChainId;

    fn pin(&self) -> BlockPin;

    /// What this source is, in words that belong in a completion report
    /// (`ethRpc`, `dump:fixtures/…json`). §66 requires the state source to be
    /// recorded; this is where the string comes from.
    fn source(&self) -> String;

    /// `Ok(None)` means the account header is known to be absent *at a height
    /// the source can serve*; `Err(Missing)` means the height itself is not
    /// available. Only the first is a fact about the chain.
    async fn account(&self, address: Address) -> ProviderResult<Option<AccountState>>;

    async fn storage(&self, address: Address, slot: U256) -> ProviderResult<U256>;

    async fn code(&self, address: Address) -> ProviderResult<Bytes>;

    /// Several bytecodes the caller already knows it needs, as one dispatch.
    ///
    /// The default is the loop every provider has always been: one read after
    /// another, stopping at the first that fails. Only [`RpcStateProvider`] overrides
    /// it, and only to bound how many of the *same* independent reads are outstanding
    /// at once — M8.3.3 §5's dependency map is what makes a caller allowed to ask for
    /// a list at all, and §9's scheduler is what keeps the list from becoming an
    /// unbounded fan-out. Answers come back in input order, so a caller that names
    /// the address it wants in the same position it always did reads the same words.
    async fn codes(&self, addresses: &[Address]) -> ProviderResult<Vec<Bytes>> {
        let mut out = Vec::with_capacity(addresses.len());
        for address in addresses {
            out.push(self.code(*address).await?);
        }
        Ok(out)
    }

    /// The header this source is serving state *at*.
    ///
    /// Distinct from [`StateProvider::pin`]: a pin is what a caller asked for, a
    /// header is what the source answers with, and §20's check only means
    /// something if the two come from different places. The execution layer needs
    /// it for a second reason — §29 forbids inventing a gas price, and the base
    /// fee, the block gas limit and the randomness field are all header facts.
    async fn header(&self) -> ProviderResult<BlockContext>;

    /// Historical block hash, for `BLOCKHASH`. A source that cannot serve it
    /// returns `Ok(None)`; the EVM then reads zero, which is what the chain
    /// would give for a hash outside the 256-block window anyway.
    async fn block_hash(&self, number: BlockNumber) -> ProviderResult<Option<B256>>;

    /// Name the phase of the run that the reads issued below are part of (M8.4.1 §4).
    ///
    /// The default does nothing, and doing nothing is the honest answer for a source that
    /// issues no calls: a recorded dump answers from disk, so there is no request to
    /// attribute. Only [`RpcStateProvider`] can use this, because only it puts anything on
    /// the wire to be labelled.
    ///
    /// This is a *statement about the code that follows*, not about a span of time: the
    /// phase is what the caller is about to ask for, and the provider puts it on the calls
    /// it issues until the next name. A phase that is never named is not guessed at — the
    /// reads say they carry no phase, which is §28's 「没有证明就不能说」 applied to
    /// attribution.
    fn note_read_phase(&self, _phase: &str) {}

    /// This same source with a run's setup layered on top (§18, §57).
    ///
    /// A method on the trait rather than a builder on each type because the engine
    /// needs it at a point where it no longer knows the concrete type: setup is
    /// applied after the header has been read and the gas price resolved, since
    /// the native amount the test sender has to hold is the plan's gas ceiling
    /// *at that price*. Returning `Arc<dyn StateProvider>` is what lets that happen
    /// without a generic parameter on the run.
    ///
    /// Both implementations keep sharing what they record: an RPC source derived
    /// this way still writes into the same dump, so a fixture captured across a
    /// `with_setup` boundary remains one continuous record of the reads that were
    /// actually made.
    fn with_setup(&self, overrides: Vec<StateOverride>) -> Arc<dyn StateProvider>;
}

// ---------------------------------------------------------------------------
// Cache keys
// ---------------------------------------------------------------------------

/// Key of every read that is identified by one account: chain, height, address.
///
/// Chain and address because a bare address is not an identity (§63); the height
/// because bytecode and balances are facts *about a block*, and the two blocks of
/// one reorg share an address while disagreeing about what sits behind it. Keeping
/// the height in the key rather than inferring it from the provider's pin is what
/// makes the isolation a property of the data and not of who holds it (M8.3.1 §3):
/// a key that cannot name a second block is a key that cannot be tested against
/// one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StateReadKey {
    pub chain_id: ChainId,
    pub block: BlockNumber,
    pub address: Address,
}

impl StateReadKey {
    pub const fn new(chain_id: ChainId, block: BlockNumber, address: Address) -> Self {
        Self {
            chain_id,
            block,
            address,
        }
    }
}

/// Key of the storage cache: chain, block, address, slot (§64). All four,
/// because any three of them can be shared by two different words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StorageCacheKey {
    pub chain_id: ChainId,
    pub block: BlockNumber,
    pub address: Address,
    pub slot: U256,
}

impl StorageCacheKey {
    pub const fn new(chain_id: ChainId, block: BlockNumber, address: Address, slot: U256) -> Self {
        Self {
            chain_id,
            block,
            address,
            slot,
        }
    }
}

// ---------------------------------------------------------------------------
// Reuse accounting
// ---------------------------------------------------------------------------

/// One kind's reuse, as the run reports it.
///
/// The two numbers mean one thing and one thing only: a **miss** is a read this
/// boundary was asked about and had to send to the node, a **hit** is a read it
/// answered from a value the same simulation already paid for. So on a run with
/// reuse on, `misses` per kind equals that method's request count — which is what
/// lets §12's instrument be checked against a counter that counts bytes on the
/// wire rather than against itself.
///
/// A read that never came here is in neither number. With reuse off, an account
/// triple goes straight to the node without being looked up, and `reuse: false` in
/// the same row is what says so: absent measurements are reported as unmeasured,
/// not as zeroes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReuseTally {
    pub hits: usize,
    pub misses: usize,
}

impl ReuseTally {
    /// The requests this kind would have cost without the reuse.
    pub const fn saved(&self) -> usize {
        self.hits
    }
}

/// Reuse per read kind, for the evidence a report quotes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateReadStats {
    /// Whether account reads were looked up before going out. `false` is M8.2's
    /// HEAD: `code` and `storage` still pass through the boundary — they did
    /// before this milestone existed — while a balance, a nonce and the bytecode
    /// an account asks for each cost a request every time they are wanted.
    pub reuse: bool,
    pub code: ReuseTally,
    pub balance: ReuseTally,
    pub nonce: ReuseTally,
    pub storage: ReuseTally,
}

impl StateReadStats {
    pub const fn new(reuse: bool) -> Self {
        Self {
            reuse,
            code: ReuseTally { hits: 0, misses: 0 },
            balance: ReuseTally { hits: 0, misses: 0 },
            nonce: ReuseTally { hits: 0, misses: 0 },
            storage: ReuseTally { hits: 0, misses: 0 },
        }
    }

    pub const fn total_hits(&self) -> usize {
        self.code.hits + self.balance.hits + self.nonce.hits + self.storage.hits
    }

    pub const fn total_misses(&self) -> usize {
        self.code.misses + self.balance.misses + self.nonce.misses + self.storage.misses
    }

    /// Reads a node would otherwise have been asked for, counted across kinds.
    pub const fn requests_avoided(&self) -> usize {
        self.total_hits()
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "reuse": self.reuse,
            "code": { "hits": self.code.hits, "misses": self.code.misses },
            "balance": { "hits": self.balance.hits, "misses": self.balance.misses },
            "nonce": { "hits": self.nonce.hits, "misses": self.nonce.misses },
            "storage": { "hits": self.storage.hits, "misses": self.storage.misses },
            "total_hits": self.total_hits(),
            "total_misses": self.total_misses(),
        })
    }
}

/// What one simulation has already read.
///
/// Owned by one [`RpcStateProvider`] and dropped with it, which is the whole of
/// M8.3.1 §2: there is no process-wide map for a later run to find, and a height
/// this run never read cannot be served from somewhere else's read.
#[derive(Clone, Debug, PartialEq, Eq)]
struct StateReadCache {
    code: BTreeMap<StateReadKey, Bytes>,
    balance: BTreeMap<StateReadKey, U256>,
    nonce: BTreeMap<StateReadKey, u64>,
    storage: BTreeMap<StorageCacheKey, U256>,
    stats: StateReadStats,
}

impl StateReadCache {
    fn new(reuse: bool) -> Self {
        Self {
            code: BTreeMap::new(),
            balance: BTreeMap::new(),
            nonce: BTreeMap::new(),
            storage: BTreeMap::new(),
            stats: StateReadStats::new(reuse),
        }
    }

    /// A hit is counted by the lookup that found it, never by the caller, so the
    /// tally cannot drift from the map it describes.
    fn take_balance(&mut self, key: StateReadKey) -> Option<U256> {
        let value = self.balance.get(&key).copied()?;
        self.stats.balance.hits += 1;
        Some(value)
    }

    fn take_nonce(&mut self, key: StateReadKey) -> Option<u64> {
        let value = self.nonce.get(&key).copied()?;
        self.stats.nonce.hits += 1;
        Some(value)
    }

    fn take_code(&mut self, key: StateReadKey) -> Option<Bytes> {
        let value = self.code.get(&key).cloned()?;
        self.stats.code.hits += 1;
        Some(value)
    }

    fn take_storage(&mut self, key: StorageCacheKey) -> Option<U256> {
        let value = self.storage.get(&key).copied()?;
        self.stats.storage.hits += 1;
        Some(value)
    }

    fn put_balance(&mut self, key: StateReadKey, value: U256) {
        self.stats.balance.misses += 1;
        self.balance.insert(key, value);
    }

    fn put_nonce(&mut self, key: StateReadKey, value: u64) {
        self.stats.nonce.misses += 1;
        self.nonce.insert(key, value);
    }

    /// `tracked` is whether the read was looked up first: the reuse-off account
    /// path stores the bytecode it fetched exactly as M8.2's HEAD did, and a write
    /// nobody asked about is not a miss.
    fn put_code(&mut self, key: StateReadKey, value: &Bytes, tracked: bool) {
        if tracked {
            self.stats.code.misses += 1;
        }
        self.code.insert(key, value.clone());
    }

    fn put_storage(&mut self, key: StorageCacheKey, value: U256) {
        self.stats.storage.misses += 1;
        self.storage.insert(key, value);
    }
}

// ---------------------------------------------------------------------------
// Overrides
// ---------------------------------------------------------------------------

/// A state change that simulation setup is allowed to make.
///
/// §18 permits exactly this and nothing more like it: the test sender needs a
/// native balance to pay for gas and a token balance to spend, and neither of
/// those is a historical fact about the pool. Pool reserves, pool balances and
/// bytecode are never overridden — a simulation that edits the market it claims
/// to measure has stopped being a simulation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateOverride {
    pub address: Address,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<U256>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<Bytes>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<(U256, U256)>,
    /// Why this exists, in the report's words: `simulation setup: test sender
    /// needs gas money`, never an empty string.
    pub reason: String,
}

impl StateOverride {
    pub fn balance(address: Address, balance: U256, reason: impl Into<String>) -> Self {
        Self {
            address,
            balance: Some(balance),
            nonce: None,
            code: None,
            slots: Vec::new(),
            reason: reason.into(),
        }
    }

    pub fn slot(address: Address, slot: U256, value: U256, reason: impl Into<String>) -> Self {
        Self {
            address,
            balance: None,
            nonce: None,
            code: None,
            slots: vec![(slot, value)],
            reason: reason.into(),
        }
    }

    pub fn nonce(address: Address, nonce: u64, reason: impl Into<String>) -> Self {
        Self {
            address,
            balance: None,
            nonce: Some(nonce),
            code: None,
            slots: Vec::new(),
            reason: reason.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Dump (fixture) provider
// ---------------------------------------------------------------------------

/// One recorded account: header plus bytecode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DumpedAccount {
    pub balance: String,
    pub nonce: u64,
    /// Hex bytecode, `0x`-prefixed. Empty string of `0x` means no code.
    pub code: String,
}

/// Everything a provider read, in one serializable object.
///
/// The keys are lowercase hex strings and the maps are `BTreeMap`s so two dumps
/// of the same state serialize to identical bytes — determinism is a completion
/// gate (§37), and a fixture whose bytes move on every recording test is not a
/// fixture.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDump {
    pub chain_id: u64,
    pub block_number: u64,
    pub block_hash: String,
    /// `0xaddress` (lowercase) -> account.
    pub accounts: BTreeMap<String, DumpedAccount>,
    /// `0xaddress` -> `0xslot` (64 hex digits) -> `0xvalue`.
    pub storage: BTreeMap<String, BTreeMap<String, String>>,
    /// `number` -> `0xhash`, only the heights actually asked for.
    pub block_hashes: BTreeMap<String, String>,
    /// The header the recorded state belongs to, if it was read.
    ///
    /// A dump without one can still answer every account and storage question, and
    /// still be unusable: without the base fee there is no gas cost (§29), and
    /// without the hash there is nothing to check the pin against (§20). The engine
    /// therefore refuses it rather than filling in a plausible header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<RecordedHeader>,
    /// Reads the RPC served while this was recorded, in call order. Kept so a
    /// fixture can be audited against the request that produced it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<String>,
}

/// The header fields an execution needs, in the same string-for-number form the
/// rest of the dump uses so two recordings of one block serialize identically.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedHeader {
    pub timestamp: u64,
    pub gas_limit: u64,
    /// `None` when the block carries no EIP-1559 base fee, which is a fact about
    /// the chain's pricing model rather than a gap in the recording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_fee_per_gas: Option<u128>,
    /// EIP-4844's `excessBlobGas`, which a blob-era ruleset needs in its block
    /// environment. `None` is the recording saying it never saw the field — the
    /// engine then refuses a Cancun-or-later run instead of handing the EVM a zero
    /// nobody read off a header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excess_blob_gas: Option<u64>,
    pub beneficiary: Address,
    /// `mixHash`, which is the randomness field post-merge. A post-merge block
    /// without one cannot be executed against at all, so it stays optional here
    /// only because the chain's own header may genuinely not carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prevrandao: Option<B256>,
}

impl RecordedHeader {
    pub fn of_block(block: &BlockContext) -> Self {
        Self {
            timestamp: block.timestamp,
            gas_limit: block.gas_limit,
            base_fee_per_gas: block.base_fee_per_gas,
            excess_blob_gas: block.excess_blob_gas,
            beneficiary: block.beneficiary,
            prevrandao: block.prevrandao,
        }
    }

    /// Re-attach this header to the block it was read from.
    ///
    /// The chain, height and hash come from the dump rather than from here, so a
    /// fixture cannot state a header that disagrees with the state around it: the
    /// identity of the block is the part §20 checks.
    pub fn into_context(self, dump: &StateDump) -> ProviderResult<BlockContext> {
        let pin = dump.pin().ok_or_else(|| ProviderError::Missing {
            provider: format!("dump:{}", dump.block_number),
            what: format!("dump has no parsable block hash: {}", dump.block_hash),
        })?;
        Ok(BlockContext {
            chain_id: ChainId(dump.chain_id),
            number: pin.number,
            hash: pin.hash,
            timestamp: self.timestamp,
            gas_limit: self.gas_limit,
            base_fee_per_gas: self.base_fee_per_gas,
            excess_blob_gas: self.excess_blob_gas,
            beneficiary: self.beneficiary,
            prevrandao: self.prevrandao,
        })
    }
}

impl StateDump {
    pub fn empty(chain_id: ChainId, pin: BlockPin) -> Self {
        Self {
            chain_id: chain_id.0,
            block_number: pin.number.0,
            block_hash: format!("{:?}", pin.hash),
            accounts: BTreeMap::new(),
            storage: BTreeMap::new(),
            block_hashes: BTreeMap::new(),
            header: None,
            reads: Vec::new(),
        }
    }

    pub fn pin(&self) -> Option<BlockPin> {
        self.block_hash
            .parse::<B256>()
            .ok()
            .map(|hash| BlockPin::new(BlockNumber(self.block_number), hash))
    }

    pub fn chain_id(&self) -> ChainId {
        ChainId(self.chain_id)
    }

    pub fn insert_account(&mut self, address: Address, balance: U256, nonce: u64, code: &Bytes) {
        self.accounts.insert(
            hex_address(address),
            DumpedAccount {
                balance: balance.to_string(),
                nonce,
                code: format!("{code:?}"),
            },
        );
    }

    pub fn insert_storage(&mut self, address: Address, slot: U256, value: U256) {
        self.storage
            .entry(hex_address(address))
            .or_default()
            .insert(hex_slot(slot), value.to_string());
    }

    pub fn account(&self, address: Address) -> Option<&DumpedAccount> {
        self.accounts.get(&hex_address(address))
    }

    pub fn storage(&self, address: Address, slot: U256) -> Option<U256> {
        self.storage
            .get(&hex_address(address))
            .and_then(|words| words.get(&hex_slot(slot)))
            .and_then(|value| parse_u256(value))
    }

    pub fn from_file(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    }

    pub fn write_file(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        std::fs::write(path, format!("{text}\n"))
    }
}

/// State served from a [`StateDump`] on disk: §62's fixture provider.
#[derive(Clone, Debug)]
pub struct DumpStateProvider {
    chain_id: ChainId,
    pin: BlockPin,
    source: String,
    dump: StateDump,
    overrides: Vec<StateOverride>,
}

impl DumpStateProvider {
    pub fn new(dump: StateDump, source: impl Into<String>) -> Self {
        let chain_id = dump.chain_id();
        let pin = dump
            .pin()
            .unwrap_or(BlockPin::new(BlockNumber(dump.block_number), B256::ZERO));
        Self {
            chain_id,
            pin,
            source: source.into(),
            dump,
            overrides: Vec::new(),
        }
    }

    pub fn from_file(path: &Path) -> std::io::Result<Self> {
        let dump = StateDump::from_file(path)?;
        Ok(Self::new(dump, path.display().to_string()))
    }

    pub fn dump(&self) -> &StateDump {
        &self.dump
    }

    /// §18: setup scaffolding on top of recorded history, never instead of it.
    pub fn with_overrides(mut self, overrides: Vec<StateOverride>) -> Self {
        self.overrides = overrides;
        self
    }

    fn missing(&self, what: String) -> ProviderError {
        ProviderError::Missing {
            provider: self.source(),
            what,
        }
    }
}

#[async_trait]
impl StateProvider for DumpStateProvider {
    fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    fn pin(&self) -> BlockPin {
        self.pin
    }

    fn source(&self) -> String {
        self.source.clone()
    }

    async fn account(&self, address: Address) -> ProviderResult<Option<AccountState>> {
        let recorded = self.dump.account(address);
        let base = recorded.map(|account| AccountState {
            balance: parse_u256(&account.balance).unwrap_or(U256::ZERO),
            nonce: account.nonce,
            code_hash: code_hash_of(&parse_bytes(&account.code).unwrap_or_default()),
        });
        let over = header_override(&self.overrides, address);
        match apply_header_override(base, over.as_ref()) {
            Some(account) => Ok(Some(account)),
            None => Err(self.missing(format!("account {address}"))),
        }
    }

    async fn storage(&self, address: Address, slot: U256) -> ProviderResult<U256> {
        if let Some(value) = override_slot(&self.overrides, address, slot) {
            return Ok(value);
        }
        // An address the dump never read has no recorded value; inventing zero
        // would be inventing state, so this is a miss, not a default.
        self.dump
            .storage(address, slot)
            .ok_or_else(|| self.missing(format!("storage {address} slot {slot}")))
    }

    async fn header(&self) -> ProviderResult<BlockContext> {
        let recorded = self.dump.header.ok_or_else(|| {
            self.missing(
                "the fixture records no header, so the base fee, block gas limit and                  block identity it was read under are unknown".to_string(),
            )
        })?;
        recorded.into_context(&self.dump)
    }

    async fn code(&self, address: Address) -> ProviderResult<Bytes> {
        if let Some(over) = header_override(&self.overrides, address) {
            if let Some(code) = &over.code {
                return Ok(code.clone());
            }
        }
        let recorded = self
            .dump
            .account(address)
            .ok_or_else(|| self.missing(format!("code {address}")))?;
        parse_bytes(&recorded.code)
            .ok_or_else(|| self.missing(format!("unreadable code for {address}")))
    }

    async fn block_hash(&self, number: BlockNumber) -> ProviderResult<Option<B256>> {
        Ok(self
            .dump
            .block_hashes
            .get(&number.0.to_string())
            .and_then(|hash| hash.parse::<B256>().ok()))
    }

    fn with_setup(&self, overrides: Vec<StateOverride>) -> Arc<dyn StateProvider> {
        Arc::new(
            self.clone()
                .with_overrides(merge_setup(&self.overrides, overrides)),
        )
    }
}

// ---------------------------------------------------------------------------
// RPC provider
// ---------------------------------------------------------------------------

/// State read live from a node at one pinned height: §62's one real provider.
///
/// Every answer is recorded into a [`StateDump`] as it comes back, so a run
/// against a node can be committed as a fixture afterwards without re-reading
/// anything — the file and the run cannot drift, because they are the same
/// reads.
///
/// The reads that have already come back are kept in [`StateReadCache`] for as long
/// as this provider is, which makes the cache's lifetime exactly one simulation's
/// (M8.3.1 §2): nothing here is reachable from the next run, and nothing here
/// outlives the block this provider was pinned to.
pub struct RpcStateProvider {
    chain: Arc<dyn ChainAdapter>,
    chain_id: ChainId,
    pin: BlockPin,
    overrides: Vec<StateOverride>,
    cache: Arc<Mutex<StateReadCache>>,
    recorded: Arc<Mutex<StateDump>>,
    reuse: bool,
    dispatch: Arc<BoundedDispatch>,
    /// The sink this provider's own calls land in, taken from the adapter it reads
    /// through rather than handed in: a label belongs on the record of the call it
    /// describes, and the only way to be sure of that is to read it off the same
    /// handle the call goes out on. `None` is a normal value — a source that was
    /// never traced has nothing to label.
    trace: Option<RpcTraceSink>,
    /// The phase [`StateProvider::note_read_phase`] last named, shared across
    /// [`StateProvider::with_setup`] for the same reason the cache is: naming phases
    /// is one simulation's act, and a source derived from it is still that reader.
    phase: Arc<Mutex<String>>,
}

/// The stage this provider stamps its calls with: §8's rule that a report uses the
/// names the code already has. `simulation` is the stage the latency ladder calls this
/// run's state reads, and this type is only ever a simulation's state source.
const SIMULATION_STAGE: &str = "simulation";

/// One member of an account read, as a batch carries it.
///
/// A triple answers in three different types, so the three tasks that read them
/// need one type to be run together in; the tag is the kind, which is also the
/// [`StateReadDescriptor::kind`] this member's descriptor carries. The `bool` is the
/// one every single read has always returned: whether this member cost the node a
/// request (M8.3.1 §12).
///
/// Private to this module because nothing else hands a member to this batch.
enum AccountRead {
    Balance(U256, bool),
    Nonce(u64, bool),
    Code(Bytes, bool),
}

impl RpcStateProvider {
    pub fn new(chain: Arc<dyn ChainAdapter>, pin: BlockPin) -> Self {
        Self::with_state_read_reuse(chain, pin, true)
    }

    /// The same provider with account reads either looked up or not.
    ///
    /// `false` is M8.2's HEAD kept addressable, so an A/B run changes this one
    /// switch and nothing else (§6): the pin, the adapter, the overrides, the read
    /// order and the recording are the same code either way. It is not "no caching
    /// at all" — the bytecode and storage lookups that already existed at HEAD stay
    /// as they were, because they are not this milestone's variable.
    ///
    /// Concurrency 1, which is M8.3.2's HEAD and this build's default (§28).
    pub fn with_state_read_reuse(chain: Arc<dyn ChainAdapter>, pin: BlockPin, reuse: bool) -> Self {
        Self::with_state_read_concurrency(chain, pin, reuse, 1)
    }

    /// The same provider with a bound on how many of its independent reads may be
    /// outstanding at the node at once (M8.3.3 §9).
    ///
    /// `1` is the whole of the previous behaviour, and not a special case of the new
    /// code: the scheduler awaits its tasks one at a time and stops at the first
    /// failure exactly where the sequential statements it replaced did. §10 makes
    /// that a test rather than a reading of the source. A bound of `0` is read as
    /// `1` — a batch that can run nothing is a stalled run, not a setting.
    pub fn with_state_read_concurrency(
        chain: Arc<dyn ChainAdapter>,
        pin: BlockPin,
        reuse: bool,
        concurrency: usize,
    ) -> Self {
        let chain_id = chain.chain_id();
        let trace = chain.rpc_trace();
        Self {
            recorded: Arc::new(Mutex::new(StateDump::empty(chain_id, pin))),
            chain,
            chain_id,
            pin,
            overrides: Vec::new(),
            cache: Arc::new(Mutex::new(StateReadCache::new(reuse))),
            reuse,
            dispatch: Arc::new(BoundedDispatch::new(concurrency)),
            trace,
            phase: Arc::new(Mutex::new(String::new())),
        }
    }

    pub fn with_overrides(mut self, overrides: Vec<StateOverride>) -> Self {
        self.overrides = overrides;
        self
    }

    /// What this simulation's dispatch did, as §15 wants it reported: the bound that
    /// was configured, the high-water mark that was actually reached at the node, and
    /// every batch the scheduler was handed.
    ///
    /// Read after the run, like [`Self::state_read_stats`], and for the same reason:
    /// `configured` and `observed` have to describe one finished simulation, and a
    /// run that configured 4 while peaking at 1 is a run that never ran 4 at all.
    pub fn state_read_concurrency(&self) -> ConcurrencyReport {
        self.dispatch.report()
    }

    /// The reads made so far, in call order — what a fixture is written from.
    pub fn dump(&self) -> StateDump {
        self.recorded().clone()
    }

    /// What this simulation's reuse boundary has done, for the evidence (§12).
    ///
    /// A snapshot taken under the lock rather than a live handle: the numbers are
    /// quoted beside a request count that counted the wire, and both have to
    /// describe the same finished run.
    pub fn state_read_stats(&self) -> StateReadStats {
        self.cache().stats
    }

    /// The reuse map, poison-resistant: a poisoned lock still holds every read it
    /// made, and M8.3.1 §4's rule is that a value this simulation already paid for
    /// stays valid for it. So a panic somewhere else in the process must not turn a
    /// state read into a panic here.
    fn cache(&self) -> std::sync::MutexGuard<'_, StateReadCache> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn unavailable(&self, reason: String) -> ProviderError {
        ProviderError::Unavailable {
            provider: StateProvider::source(self),
            reason,
        }
    }

    /// The recorded dump, poison-resistant for the same reason [`Self::cache`] is: a
    /// panic somewhere else in the process must not turn a read this simulation already
    /// made into a panic here, and a dump that lost a marker would report fewer requests
    /// than the wire carried. Every lock of this field in this type goes through here, so
    /// no read path can regain a panicking acquire (M8.3.3 §40).
    fn recorded(&self) -> std::sync::MutexGuard<'_, StateDump> {
        self.recorded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A batch member's identity, in the four fields the RPC trace already publishes
    /// for the call it becomes — chain, height, address, and the method `kind` names.
    ///
    /// Deliberately not the trace's own `dedup_key` string: that format is
    /// `evm-chain`'s, and copying it here would give one key two authors. A reader
    /// joins a batch to its calls on these fields instead, which is the same
    /// identity and cannot go out of date.
    fn descriptor(&self, kind: &'static str, address: Address) -> StateReadDescriptor {
        StateReadDescriptor {
            kind,
            chain_id: self.chain_id.0,
            block: self.pin.number.0,
            address: normalize_address(&address.to_string()),
        }
    }

    /// Put the run's current phase on the call about to be issued.
    ///
    /// Called at the one point each read has in common — after the reuse boundary has said
    /// the node must be asked, before the request goes out — because that is the only place
    /// where the label and the call are the same event. Two consequences follow from it:
    ///
    /// A read answered from cache stamps nothing, because it issues nothing, and a label
    /// that outlived its phase would attribute a later call to the wrong caller. So when no
    /// phase has been named this clears rather than leaves the sink holding whatever the
    /// last phase was: the record then says `context_not_stamped`, which is §28's
    /// 「没有测到就记录 not observed」 for an attribution this run did not make.
    ///
    /// A phase that has not moved is not stamped again. [`RpcTraceSink::set_context`] moves
    /// the sink's stamp counter, and re-stamping the identical text while an earlier call of
    /// the same batch is still open is exactly the overlap the `context_restamped_mid_call`
    /// note exists to report — here it would be reported about a label that never changed.
    fn stamp_read(&self) {
        let Some(sink) = &self.trace else { return };
        let phase = self.current_phase();
        if phase.is_empty() {
            sink.clear_context();
            return;
        }
        let already = sink
            .context()
            .is_some_and(|held| held.stage == SIMULATION_STAGE && held.caller == phase);
        if !already {
            sink.set_context(SIMULATION_STAGE, phase);
        }
    }

    fn current_phase(&self) -> String {
        self.phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// One `eth_getBalance`, asked of the reuse boundary first when `ask` says to.
    ///
    /// The second half of the returned pair is whether this call cost the node a
    /// request — the dump's read markers are driven off it, so a fixture records the
    /// requests that were made and not the answers that were re-remembered. A failed
    /// read returns here before anything is stored (§4): a timeout is not a value,
    /// and caching one would let a later step of the same simulation be answered from
    /// a failure it never saw.
    async fn balance_at(&self, key: StateReadKey, ask: bool) -> ProviderResult<(U256, bool)> {
        let cached = if ask {
            self.cache().take_balance(key)
        } else {
            None
        };
        if let Some(value) = cached {
            return Ok((value, false));
        }
        // M8.3.3's high-water mark brackets a read that is outstanding at the node:
        // taken just before the request, released once the answer is in. A hit
        // returned above never had a request to have outstanding, and counting one
        // would put an overlap in the evidence that the wire did not have (§16).
        self.stamp_read();
        let _outstanding = self.dispatch.enter();
        let value = self
            .chain
            .get_balance(key.block, key.address)
            .await
            .map_err(|error| self.unavailable(error.to_string()))?;
        if ask {
            self.cache().put_balance(key, value);
        }
        Ok((value, true))
    }

    /// One `eth_getTransactionCount`. `ask` as in [`Self::balance_at`].
    ///
    /// The height is in the key, which is the only way this is safe: a nonce read at
    /// a pinned block is a fact about that block, and the same address at the next
    /// block is a different question (§10).
    async fn nonce_at(&self, key: StateReadKey, ask: bool) -> ProviderResult<(u64, bool)> {
        let cached = if ask {
            self.cache().take_nonce(key)
        } else {
            None
        };
        if let Some(value) = cached {
            return Ok((value, false));
        }
        self.stamp_read();
        let _outstanding = self.dispatch.enter();
        let value = self
            .chain
            .get_nonce(key.block, key.address)
            .await
            .map_err(|error| self.unavailable(error.to_string()))?;
        if ask {
            self.cache().put_nonce(key, value);
        }
        Ok((value, true))
    }

    /// One `eth_getCode`. `ask` is the milestone's own variable: the account path
    /// passes this provider's setting, while a direct bytecode read has always been
    /// looked up and keeps being looked up.
    async fn code_at(&self, key: StateReadKey, ask: bool) -> ProviderResult<(Bytes, bool)> {
        let cached = if ask {
            self.cache().take_code(key)
        } else {
            None
        };
        if let Some(code) = cached {
            return Ok((code, false));
        }
        self.stamp_read();
        let _outstanding = self.dispatch.enter();
        let code = self
            .chain
            .get_code(key.block, key.address)
            .await
            .map_err(|error| self.unavailable(error.to_string()))?;
        self.cache().put_code(key, &code, ask);
        Ok((code, true))
    }

    /// One bytecode read as both [`StateProvider::code`] and the batched
    /// [`StateProvider::codes`] do it: this run's override first, then the reuse
    /// boundary, then whether the answer cost the node a request.
    ///
    /// Shared rather than duplicated because the two paths must not be able to
    /// disagree about an override (§24): a setup value that one of them answered and
    /// the other sent to the node would make a fixture and a live run two accounts
    /// of the same read.
    async fn code_one(&self, address: Address) -> ProviderResult<(Bytes, bool)> {
        if let Some(over) = header_override(&self.overrides, address) {
            if let Some(code) = &over.code {
                return Ok((code.clone(), false));
            }
        }
        let key = StateReadKey::new(self.chain_id, self.pin.number, address);
        self.code_at(key, true).await
    }
}

#[async_trait]
impl StateProvider for RpcStateProvider {
    fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    fn pin(&self) -> BlockPin {
        self.pin
    }

    fn source(&self) -> String {
        format!("rpc:chain-{}", self.chain_id.0)
    }

    fn note_read_phase(&self, phase: &str) {
        *self
            .phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = phase.to_string();
    }

    async fn account(&self, address: Address) -> ProviderResult<Option<AccountState>> {
        let key = StateReadKey::new(self.chain_id, self.pin.number, address);
        // M8.3.3 §5's dependency map, resolved in this function rather than assumed by
        // it: the three reads of one account are asked of the same key computed once
        // above — chain, this provider's pinned height, this address — and no member's
        // request parameters are a function of another member's answer. That is what
        // makes them one hand. Whether more than one of them may be outstanding at the
        // node is the run's bound (§27), and `1` awaits them here in the order the
        // three sequential statements of M8.3.2's HEAD used: §10 makes that equality a
        // measurement, not a reading of this comment.
        let parts = self
            .dispatch
            .run(
                "account_triple",
                vec![
                    self.descriptor("balance", address),
                    self.descriptor("nonce", address),
                    self.descriptor("code", address),
                ],
                vec![
                    Box::pin(async move {
                        self.balance_at(key, self.reuse)
                            .await
                            .map(|(value, read)| AccountRead::Balance(value, read))
                    }),
                    Box::pin(async move {
                        self.nonce_at(key, self.reuse)
                            .await
                            .map(|(value, read)| AccountRead::Nonce(value, read))
                    }),
                    Box::pin(async move {
                        self.code_at(key, self.reuse)
                            .await
                            .map(|(value, read)| AccountRead::Code(value, read))
                    }),
                ],
            )
            .await;
        let mut balance = None;
        let mut nonce = None;
        let mut code = None;
        let mut failure = None;
        for part in parts {
            match part {
                Ok(AccountRead::Balance(value, read)) => balance = Some((value, read)),
                Ok(AccountRead::Nonce(value, read)) => nonce = Some((value, read)),
                Ok(AccountRead::Code(value, read)) => code = Some((value, read)),
                // Results come back in input order, so the first failure seen here is
                // the first one the sequential path would have stopped at — and, as
                // there, it ends the run rather than being smoothed over with a default
                // or a stale answer (§24).
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        let (Some(balance), Some(nonce), Some(code)) = (balance, nonce, code) else {
            return Err(self.unavailable(
                "the account dispatch answered fewer of its three members than it was \
                 given"
                    .to_string(),
            ));
        };
        let (balance, balance_read) = balance;
        let (nonce, nonce_read) = nonce;
        let (code, code_read) = code;
        if balance_read || nonce_read || code_read {
            // One marker per account read that cost the node something, which is
            // what `reads` has always counted: an answer reused from this
            // simulation's own earlier read asks the node nothing, and a dump that
            // claimed otherwise would record a request that never happened.
            let mut dump = self.recorded();
            dump.insert_account(address, balance, nonce, &code);
            dump.reads.push(format!("account {address}"));
        }
        let base = Some(AccountState {
            balance,
            nonce,
            code_hash: code_hash_of(&code),
        });
        Ok(apply_header_override(
            base,
            header_override(&self.overrides, address).as_ref(),
        ))
    }

    async fn storage(&self, address: Address, slot: U256) -> ProviderResult<U256> {
        if let Some(value) = override_slot(&self.overrides, address, slot) {
            return Ok(value);
        }
        let key = StorageCacheKey::new(self.chain_id, self.pin.number, address, slot);
        let cached = self.cache().take_storage(key);
        if let Some(value) = cached {
            return Ok(value);
        }
        self.stamp_read();
        let value = self
            .chain
            .get_storage_at(self.pin.number, address, slot)
            .await
            .map_err(|error| self.unavailable(error.to_string()))?;
        self.cache().put_storage(key, value);
        {
            let mut dump = self.recorded();
            dump.insert_storage(address, slot, value);
            dump.reads.push(format!("storage {address} {slot}"));
        }
        Ok(value)
    }

    async fn code(&self, address: Address) -> ProviderResult<Bytes> {
        let (code, read) = self.code_one(address).await?;
        if read {
            self.recorded().reads.push(format!("code {address}"));
        }
        Ok(code)
    }

    /// The route's bytecodes as one bounded hand (M8.3.3 §5, §9).
    ///
    /// Four reads the caller has already named, none of whose parameters depends on
    /// another's answer, and no answer of any of them is read before the batch is
    /// handed over — which is why this is the second and last site the scheduler is
    /// given a batch at. §7's rule that a dependency nobody proved keeps its order is
    /// why the storage reads the EVM demands one word at a time are not a third.
    ///
    /// Answers come back in input order and the dump's markers are pushed in input
    /// order once the hand has finished, so a fixture's bytes do not depend on which
    /// read answered first.
    async fn codes(&self, addresses: &[Address]) -> ProviderResult<Vec<Bytes>> {
        let reads = addresses
            .iter()
            .map(|address| self.descriptor("code", *address))
            .collect();
        let tasks = addresses
            .iter()
            .map(|address| {
                let address = *address;
                Box::pin(async move { self.code_one(address).await })
                    as BoxFuture<'_, ProviderResult<(Bytes, bool)>>
            })
            .collect();
        let answers = self.dispatch.run("touched_contracts", reads, tasks).await;
        let mut codes = Vec::with_capacity(addresses.len());
        let mut markers = Vec::new();
        let mut failure = None;
        for (address, answer) in addresses.iter().zip(answers) {
            match answer {
                Ok((code, read)) => {
                    if read {
                        markers.push(format!("code {address}"));
                    }
                    codes.push(code);
                }
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
            }
        }
        if !markers.is_empty() {
            self.recorded().reads.extend(markers);
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(codes)
    }

    async fn header(&self) -> ProviderResult<BlockContext> {
        self.stamp_read();
        let block = self
            .chain
            .get_block_context(self.pin.number)
            .await
            .map_err(|error| self.unavailable(error.to_string()))?;
        {
            let mut dump = self.recorded();
            dump.header = Some(RecordedHeader::of_block(&block));
            dump.reads.push(format!("header {}", block.number.0));
        }
        Ok(block)
    }

    async fn block_hash(&self, number: BlockNumber) -> ProviderResult<Option<B256>> {
        self.stamp_read();
        let block = self
            .chain
            .get_block(number)
            .await
            .map_err(|error| self.unavailable(error.to_string()))?;
        let hash = block.hash;
        self.recorded()
            .block_hashes
            .insert(number.0.to_string(), format!("{:?}", hash));
        Ok(Some(hash))
    }

    fn with_setup(&self, overrides: Vec<StateOverride>) -> Arc<dyn StateProvider> {
        Arc::new(Self {
            chain: Arc::clone(&self.chain),
            chain_id: self.chain_id,
            pin: self.pin,
            overrides: merge_setup(&self.overrides, overrides),
            // Shared, not copied: the derived source is the same reader, so its
            // reads land in the same dump and cost the same node no extra calls.
            // One simulation, one cache — §2's lifetime rule survives this hop
            // because nothing new is built here.
            cache: Arc::clone(&self.cache),
            recorded: Arc::clone(&self.recorded),
            reuse: self.reuse,
            // The same scheduler, not a fresh one: the bound is this simulation's, and
            // so is the high-water mark §15 reports. A derived source that restarted
            // the count would let one run report two peaks.
            // The same scheduler, not a fresh one: the bound is this simulation's, and
            // so is the high-water mark §15 reports. A derived source that restarted
            // the count would let one run report two peaks.
            dispatch: Arc::clone(&self.dispatch),
            // The sink is a handle to the adapter above, and this provider reads through
            // that same adapter, so the derived source labels the same timeline. The phase
            // is shared for the same reason: a run names its phases once, across a
            // `with_setup` boundary, and a label that reset to "no phase named" here would
            // strip the attribution off every read of every step that follows.
            trace: self.trace.clone(),
            phase: Arc::clone(&self.phase),
        })
    }
}

/// Setup already on a source, plus the setup a run is adding.
///
/// Appended rather than merged per address: the readers below take the *last*
/// declared override for a slot or header, so a run's own scaffolding wins over
/// whatever the caller had pinned without either one being silently dropped.
fn merge_setup(existing: &[StateOverride], added: Vec<StateOverride>) -> Vec<StateOverride> {
    let mut all = existing.to_vec();
    all.extend(added);
    all
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

pub(crate) fn hex_address(address: Address) -> String {
    address.to_string().to_lowercase()
}

/// Slot and value words are fixed width so ordering and comparison never
/// depend on how many leading zeros a printer happened to keep.
pub(crate) fn hex_slot(slot: U256) -> String {
    format!("0x{slot:064x}")
}

pub(crate) fn parse_u256(text: &str) -> Option<U256> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        U256::from_str_radix(hex, 16).ok()
    } else {
        text.parse::<U256>().ok()
    }
}

pub(crate) fn parse_bytes(text: &str) -> Option<Bytes> {
    let text = text.trim();
    if text.is_empty() {
        return Some(Bytes::new());
    }
    let hex = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))?;
    if hex.is_empty() {
        return Some(Bytes::new());
    }
    hex::decode(hex).ok().map(Bytes::from)
}

/// Hash the EVM itself uses for an account's code: keccak256 of the bytes, with
/// empty code collapsing to [`KECCAK_EMPTY`].
pub(crate) fn code_hash_of(code: &Bytes) -> B256 {
    if code.is_empty() {
        return KECCAK_EMPTY;
    }
    keccak256(code.as_ref())
}

/// Layer an override on top of a header the state source actually served.
///
/// An address the source has never heard of (`None` in) is created from
/// [`AccountState::EMPTY`] when — and only when — an override gives it a field:
/// §58's sender may be a fresh address the chain has never seen, and the
/// scaffolding that funds it has to work for an account that does not exist yet.
/// An override that adds nothing to a missing account still answers `None`, so a
/// hole in a fixture stays a hole rather than becoming a zero the run trusts.
fn apply_header_override(
    base: Option<AccountState>,
    over: Option<&StateOverride>,
) -> Option<AccountState> {
    let Some(over) = over else { return base };
    let mut account = base.unwrap_or(AccountState::EMPTY);
    if let Some(balance) = over.balance {
        account.balance = balance;
    }
    if let Some(nonce) = over.nonce {
        account.nonce = nonce;
    }
    if let Some(code) = &over.code {
        account.code_hash = code_hash_of(code);
    }
    if account == AccountState::EMPTY && base.is_none() {
        return None;
    }
    Some(account)
}

/// Storage slot a setup override pins, most recent override winning.
fn override_slot(overrides: &[StateOverride], address: Address, slot: U256) -> Option<U256> {
    overrides.iter().rev().find_map(|item| {
        (item.address == address)
            .then(|| {
                item.slots
                    .iter()
                    .rev()
                    .find_map(|(k, v)| (*k == slot).then_some(*v))
            })
            .flatten()
    })
}

/// Header fields the setup overrides pin at this address, last write winning.
///
/// Folded across every matching entry rather than taking the last one, per field:
/// a caller that pins a nonce and the engine that funds the same account are two
/// overrides on one address, and choosing a winner between them would silently
/// drop one of the two facts. The reasons survive the fold for the same reason —
/// a funding override that says nothing about who asked for it is not evidence.
fn header_override(overrides: &[StateOverride], address: Address) -> Option<StateOverride> {
    let mut seen: Option<StateOverride> = None;
    for item in overrides.iter().filter(|item| item.address == address) {
        let entry = seen.get_or_insert_with(|| StateOverride {
            address,
            balance: None,
            nonce: None,
            code: None,
            slots: Vec::new(),
            reason: String::new(),
        });
        if item.balance.is_some() {
            entry.balance = item.balance;
        }
        if item.nonce.is_some() {
            entry.nonce = item.nonce;
        }
        if item.code.is_some() {
            entry.code = item.code.clone();
        }
        if !item.reason.is_empty() {
            if !entry.reason.is_empty() {
                entry.reason.push_str(" + ");
            }
            entry.reason.push_str(&item.reason);
        }
    }
    seen.filter(|entry| entry.balance.is_some() || entry.nonce.is_some() || entry.code.is_some())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use alloy_primitives::{address, Address, Bytes, B256, U256};

    use super::{
        hex_address, override_slot, AccountState, BlockPin, DumpStateProvider, ProviderError,
        ReuseTally, RpcStateProvider, StateDump, StateOverride, StateProvider, StateReadKey,
        StateReadStats, StorageCacheKey, KECCAK_EMPTY,
    };
    use evm_chain::{
        BlockContext, BlockData, CallRequest, ChainAdapter, ChainBlock, ChainError, ChainLog,
        LogFilter,
    };
    use evm_core::{BlockNumber, ChainId};

    const CHAIN: ChainId = ChainId(91342);
    const OTHER_CHAIN: ChainId = ChainId(1);
    const PIN: u64 = 37_191_169;
    const POOL: Address = address!("0xf487d533cae6cddd0c7e7bbbac084dd04d876578");
    const WETH: Address = address!("0x4200000000000000000000000000000000000006");
    /// A third account that is neither pool nor token: the sender's role in §8's
    /// triple, and a distinct address is what makes a key collision visible.
    const CALLER: Address = address!("0x5b3c1e3fb6a97c0130ae015ff10f53a1a30c353e");

    fn pin() -> BlockPin {
        BlockPin::new(
            BlockNumber(PIN),
            "0x56c628d7102131d35d5bdf6a0ed2c6ec3d4d00edc4419a20641c7b396c48d670"
                .parse::<B256>()
                .expect("a 32-byte hash"),
        )
    }

    /// One recorded pool and one recorded token: the shape a real dump has.
    fn dump() -> StateDump {
        let mut dump = StateDump::empty(CHAIN, pin());
        dump.insert_account(WETH, U256::from(7u64), 3, &Bytes::from(vec![0x60, 0x00]));
        dump.insert_account(POOL, U256::ZERO, 1, &Bytes::from(vec![0xfe, 0x00]));
        dump.insert_storage(POOL, U256::from(6u64), U256::from(123_456_789u64));
        dump
    }

    /// §63 plus M8.3.1 §3: a key of (chain, height, address) is the smallest thing a
    /// balance, a nonce or a bytecode read can be remembered under. Dropping the
    /// chain serves one network's account for another's; dropping the height serves
    /// one block's account for the next block's, which is exactly the read a
    /// historical simulation must never answer from memory of a different height.
    #[test]
    fn state_read_key_carries_chain_block_and_address() {
        let addr = address!("0x0000000000000000000000000000000000000001");
        let keys = [
            StateReadKey::new(CHAIN, BlockNumber(PIN), addr),
            StateReadKey::new(OTHER_CHAIN, BlockNumber(PIN), addr),
            StateReadKey::new(CHAIN, BlockNumber(PIN + 1), addr),
            StateReadKey::new(
                CHAIN,
                BlockNumber(PIN),
                address!("0x0000000000000000000000000000000000000002"),
            ),
        ];
        let unique: BTreeSet<StateReadKey> = keys.iter().copied().collect();
        assert_eq!(
            unique.len(),
            4,
            "dropping any one of the three fields would collapse two different reads into one"
        );
        // One key type serves all three account reads, so the balance, nonce and
        // bytecode caches cannot disagree about what identifies an account.
        let mut cache: BTreeMap<StateReadKey, U256> = BTreeMap::new();
        for (index, key) in keys.iter().copied().enumerate() {
            cache.insert(key, U256::from(index));
        }
        assert_eq!(cache.len(), 4, "four identities, four entries");
    }

    /// §64: the same argument for storage, with the height in the key too, so
    /// the same slot at two blocks stays two entries.
    #[test]
    fn storage_cache_key_carries_chain_block_address_and_slot() {
        let addr = address!("0x0000000000000000000000000000000000000001");
        let slot = U256::from(6u64);
        let keys = [
            StorageCacheKey::new(CHAIN, BlockNumber(PIN), addr, slot),
            StorageCacheKey::new(CHAIN, BlockNumber(PIN + 1), addr, slot),
            StorageCacheKey::new(OTHER_CHAIN, BlockNumber(PIN), addr, slot),
            StorageCacheKey::new(CHAIN, BlockNumber(PIN), addr, U256::from(7u64)),
        ];
        let unique: BTreeSet<StorageCacheKey> = keys.iter().copied().collect();
        assert_eq!(
            unique.len(),
            4,
            "dropping any one of the four fields would collapse two reads into one"
        );
    }

    #[tokio::test]
    async fn dump_provider_serves_exactly_what_was_recorded() {
        let provider = DumpStateProvider::new(dump(), "test-dump");
        let account = provider
            .account(WETH)
            .await
            .expect("recorded")
            .expect("present");
        assert_eq!(account.balance, U256::from(7u64));
        assert_eq!(account.nonce, 3);
        assert_eq!(account.code_hash, alloy_primitives::keccak256([0x60, 0x00]));
        assert!(!account.is_empty_code());
        assert_eq!(
            provider.code(WETH).await.expect("recorded"),
            Bytes::from(vec![0x60, 0x00])
        );
        assert_eq!(
            provider
                .storage(POOL, U256::from(6u64))
                .await
                .expect("recorded"),
            U256::from(123_456_789u64)
        );
        assert_eq!(provider.chain_id(), CHAIN);
        assert_eq!(provider.pin(), pin());
    }

    /// A hole in a fixture is `Missing`, which the engine turns into
    /// `MissingState` — never a zero, and never the same error as a node that is
    /// simply down. §G wants those two apart, so they are apart here.
    #[tokio::test]
    async fn a_hole_in_the_fixture_is_a_miss_not_a_zero() {
        let provider = DumpStateProvider::new(dump(), "test-dump");
        let unknown = address!("0x0000000000000000000000000000000000000bad");
        assert!(matches!(
            provider.account(unknown).await,
            Err(ProviderError::Missing { .. })
        ));
        let error = provider
            .storage(unknown, U256::from(1u64))
            .await
            .expect_err("nothing was recorded for this slot");
        assert!(matches!(error, ProviderError::Missing { .. }));
        assert!(
            error.to_string().contains("test-dump"),
            "the message has to name the source that came up short: {error}"
        );
    }

    /// An override is allowed to fund an account the chain has never seen
    /// (§58's fresh sender) and to pin a slot — which is exactly why the set of
    /// slots a request may pin has to be the sender's own.
    #[tokio::test]
    async fn overrides_change_setup_state_only_where_they_are_declared() {
        let sender = address!("0x0000000000000000000000000000000000000fee");
        let provider = DumpStateProvider::new(dump(), "test-dump").with_overrides(vec![
            StateOverride::balance(sender, U256::from(1u64) << 18, "sender needs gas money"),
            StateOverride::slot(
                POOL,
                U256::from(6u64),
                U256::from(1u64),
                "pinned for a test",
            ),
        ]);
        let account = provider
            .account(sender)
            .await
            .expect("ok")
            .expect("the override makes it exist");
        assert_eq!(account.balance, U256::from(1u64) << 18);
        assert_eq!(account.code_hash, KECCAK_EMPTY, "funded, still no code");
        assert_eq!(
            provider.storage(POOL, U256::from(6u64)).await.expect("ok"),
            U256::from(1u64)
        );
        // A slot nobody pinned is still a miss; an override on slot 6 says
        // nothing about slot 4.
        let error = provider
            .storage(POOL, U256::from(4u64))
            .await
            .expect_err("not recorded");
        assert!(error.to_string().contains("storage"), "{error}");
    }

    /// The last declared override wins, so a request can correct itself without
    /// the earlier value silently surviving.
    #[test]
    fn the_last_override_for_a_slot_wins() {
        let overrides = vec![
            StateOverride::slot(POOL, U256::from(6u64), U256::from(1u64), "first"),
            StateOverride::slot(POOL, U256::from(6u64), U256::from(2u64), "second"),
        ];
        assert_eq!(
            override_slot(&overrides, POOL, U256::from(6u64)),
            Some(U256::from(2u64))
        );
        assert_eq!(override_slot(&overrides, POOL, U256::from(9u64)), None);
    }

    /// Two overrides on one address are two facts, not one winner: the engine funds
    /// the sender's balance in the same run that a caller pinned its nonce, and a
    /// fold that picked either entry would silently drop the other.
    #[tokio::test]
    async fn two_overrides_on_one_address_each_keep_their_own_field() {
        let sender = address!("0x00000000000000000000000000000000000005e1");
        let provider = DumpStateProvider::new(dump(), "test-dump").with_overrides(vec![
            StateOverride::nonce(sender, 4, "the sender's real nonce".to_string()),
            StateOverride::balance(sender, U256::from(99u64), "the run's gas money".to_string()),
        ]);
        let account = provider
            .account(sender)
            .await
            .expect("setup makes it exist")
            .expect("funded");
        assert_eq!(account.nonce, 4, "the pinned nonce survived the funding");
        assert_eq!(account.balance, U256::from(99u64));
        assert!(account.is_empty_code(), "funded, still no code");

        // Last write per field, so a request can correct itself without deleting
        // what it did not mean to change.
        let corrected = DumpStateProvider::new(dump(), "test-dump").with_overrides(vec![
            StateOverride::balance(sender, U256::from(1u64), "too little".to_string()),
            StateOverride::balance(sender, U256::from(2u64), "enough".to_string()),
            StateOverride::nonce(sender, 7, "and a nonce".to_string()),
        ]);
        let account = corrected
            .account(sender)
            .await
            .expect("setup")
            .expect("present");
        assert_eq!(account.balance, U256::from(2u64));
        assert_eq!(account.nonce, 7);
    }

    /// Setup layered on an RPC source keeps recording into the *same* dump: a
    /// fixture has to be one continuous record of the reads a run made, and the
    /// engine applies setup after it has already read the header.
    #[tokio::test]
    async fn with_setup_shares_the_recording_and_the_caches() {
        let chain = Arc::new(SpyChain::default());
        let base = RpcStateProvider::new(Arc::clone(&chain) as Arc<dyn ChainAdapter>, pin());
        base.header().await.expect("the spy answers");
        let sender = address!("0x00000000000000000000000000000000000005e1");
        let overridden = base.with_setup(vec![StateOverride::balance(
            sender,
            U256::from(500u64),
            "the run's gas money".to_string(),
        )]);
        let account = overridden
            .account(sender)
            .await
            .expect("served")
            .expect("the override makes it exist");
        assert_eq!(account.balance, U256::from(500u64));
        assert_eq!(overridden.pin(), base.pin());
        assert_eq!(overridden.chain_id(), CHAIN);
        assert_eq!(
            overridden.source(),
            base.source(),
            "a derived source is still the same source"
        );

        // One continuous record: the header read before the override is still there
        // alongside the one after it, and the setup read itself leaves no trace.
        let written = base.dump();
        assert!(written.header.is_some(), "the header read before setup");
        assert!(
            written.reads.iter().any(|read| read.starts_with("header ")),
            "{:?}",
            written.reads
        );
        assert_eq!(
            written.reads.len(),
            2,
            "header, then this account: {:?}",
            written.reads
        );

        // And the history stays the node's own answer: what the dump records for the
        // sender is the 42 the chain reported, not the 500 the run asked for. An
        // override that rewrote the fixture would be a fake historical state (§18)
        // wearing the clothes of evidence.
        assert_eq!(
            written
                .account(sender)
                .expect("the read itself is history")
                .balance,
            "42",
            "the override is applied on read, never written into the dump"
        );
    }

    /// The dump *is* the fixture format, so its round trip must be lossless and
    /// its bytes stable: two dumps of one state serialize identically (§37).
    #[test]
    fn state_dump_round_trips_through_json() {
        let original = dump();
        let text = serde_json::to_string(&original).expect("serializes");
        let back: StateDump = serde_json::from_str(&text).expect("deserializes");
        assert_eq!(back, original);
        assert_eq!(back.pin(), Some(pin()));
        assert_eq!(back.chain_id(), CHAIN);
        assert_eq!(serde_json::to_string(&back).expect("again"), text);
    }

    /// Slot keys are fixed width, so map order is numeric order and a fixture
    /// diff between two runs is readable.
    #[test]
    fn storage_keys_are_fixed_width_and_ordered() {
        let mut dump = StateDump::empty(CHAIN, pin());
        for slot in [1u64, 256, 65_537] {
            dump.insert_storage(POOL, U256::from(slot), U256::from(9u64));
        }
        let keys: Vec<String> = dump
            .storage
            .get(&hex_address(POOL))
            .expect("recorded")
            .keys()
            .cloned()
            .collect();
        assert!(keys.iter().all(|key| key.len() == 66), "{keys:?}");
        assert_eq!(
            keys.first().expect("sorted first").as_str(),
            "0x0000000000000000000000000000000000000000000000000000000000000001"
        );
    }

    /// A provider cannot be asked for a block: the height is fixed when it is
    /// built, and this asserts the reads really go out at that height (§49 —
    /// `latest` is not reachable through this interface).
    #[tokio::test]
    async fn rpc_reads_go_out_at_the_pin_and_are_recorded() {
        let chain = Arc::new(SpyChain::default());
        let provider = RpcStateProvider::new(chain.clone(), pin());
        provider
            .account(POOL)
            .await
            .expect("spy answers")
            .expect("spy answers with a header");
        provider
            .storage(POOL, U256::from(6u64))
            .await
            .expect("spy answers");

        assert_eq!(chain.seen_blocks(), vec![PIN, PIN, PIN, PIN]);
        let written = provider.dump();
        assert!(written.accounts.contains_key(&hex_address(POOL)));
        assert_eq!(
            written.storage(POOL, U256::from(6u64)),
            Some(U256::from(7u64))
        );
        assert_eq!(written.block_number, PIN);
        assert_eq!(written.chain_id, CHAIN.0);
        assert_eq!(written.reads.len(), 2, "{:?}", written.reads);
        assert!(
            written.reads[0].starts_with("account "),
            "{:?}",
            written.reads
        );
        assert!(
            written.reads[1].starts_with("storage "),
            "{:?}",
            written.reads
        );
    }

    /// The second read of one slot must not reach the node again: §63/§64 permit
    /// caching, and a cache that never hits is not one.
    #[tokio::test]
    async fn storage_reads_are_cached_by_full_identity() {
        let chain = Arc::new(SpyChain::default());
        let provider = RpcStateProvider::new(chain.clone(), pin());
        for _ in 0..3 {
            provider
                .storage(POOL, U256::from(6u64))
                .await
                .expect("spy answers");
        }
        assert_eq!(chain.storage_calls.load(Ordering::SeqCst), 1);
        // A different slot is a different read, and a different address too.
        provider
            .storage(POOL, U256::from(5u64))
            .await
            .expect("spy answers");
        provider
            .storage(WETH, U256::from(6u64))
            .await
            .expect("spy answers");
        assert_eq!(chain.storage_calls.load(Ordering::SeqCst), 3);
    }

    /// An override pinned at the provider level is not recorded as history: it
    /// never reaches the node, so it must never reach the dump either.
    #[tokio::test]
    async fn an_override_read_leaves_no_trace_in_the_dump() {
        let chain = Arc::new(SpyChain::default());
        let provider =
            RpcStateProvider::new(chain.clone(), pin()).with_overrides(vec![StateOverride::slot(
                POOL,
                U256::from(6u64),
                U256::from(1u64),
                "sender allowance",
            )]);
        assert_eq!(
            provider.storage(POOL, U256::from(6u64)).await.expect("ok"),
            U256::from(1u64)
        );
        assert!(provider
            .dump()
            .storage
            .get(&hex_address(POOL))
            .is_none_or(|words| words.is_empty()));
        assert_eq!(chain.storage_calls.load(Ordering::SeqCst), 0);
    }

    /// A node that fails is `Unavailable`, which the engine reports as
    /// `ProviderError` — a different answer from `MissingState`, and the test
    /// that keeps them different lives in the engine.
    #[tokio::test]
    async fn a_failing_node_is_unavailable_not_missing() {
        let provider = RpcStateProvider::new(Arc::new(FailingChain), pin());
        let error = provider
            .storage(POOL, U256::from(6u64))
            .await
            .expect_err("the node is down");
        assert!(
            matches!(error, ProviderError::Unavailable { .. }),
            "{error}"
        );
    }

    /// What one request the spy answered was, in the spy's own words.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct SpyCall {
        method: &'static str,
        address: String,
        block: u64,
    }

    /// A minimal [`ChainAdapter`] that counts reads and answers with fixed
    /// values. This is a transport double for tests, not the third state
    /// provider §62 warns about.
    ///
    /// The answers are keyed so that a read served from the wrong key is a wrong
    /// *value*, not merely an extra row in a counter: balance and nonce both move
    /// with the height, at `PIN` the number a caller would expect. That is what lets
    /// the cross-block tests below fail loudly instead of quietly.
    struct SpyChain {
        storage_calls: AtomicUsize,
        seen: Mutex<Vec<u64>>,
        calls: Mutex<Vec<SpyCall>>,
        chain_id: u64,
        /// Methods this spy refuses, empty unless a test asked for one. A read that
        /// failed still reached the transport, so it is recorded before it is refused.
        failing: Mutex<Vec<&'static str>>,
        /// How long a request waits before the spy answers it. Zero keeps a spy that
        /// answers the moment it is polled, which is what most of the tests above want;
        /// M8.3.3's overlap tests set it, because a transport that never yields makes
        /// `observed_peak` a statement about the spy rather than about the run.
        delay: Duration,
    }

    impl Default for SpyChain {
        fn default() -> Self {
            Self {
                storage_calls: AtomicUsize::new(0),
                seen: Mutex::new(Vec::new()),
                calls: Mutex::new(Vec::new()),
                chain_id: CHAIN.0,
                failing: Mutex::new(Vec::new()),
                delay: Duration::ZERO,
            }
        }
    }

    impl SpyChain {
        /// The same spy naming a different chain, for §9's fourth column: one
        /// address number on two networks must stay two reads.
        fn on(chain_id: ChainId) -> Self {
            Self {
                chain_id: chain_id.0,
                ..Default::default()
            }
        }

        /// A spy that holds every request for `delay` before answering, so several of
        /// them are genuinely outstanding at once and the dispatch can be observed
        /// overlapping.
        fn slowing(delay: Duration) -> Self {
            Self {
                delay,
                ..Default::default()
            }
        }

        /// A spy that answers, except that the named methods refuse every request with
        /// a reason carrying the method's own name. That is what lets a test ask which
        /// failure a batch reported when two of its members fail in different places.
        fn refusing(methods: &[&'static str]) -> Self {
            Self {
                failing: Mutex::new(methods.to_vec()),
                ..Default::default()
            }
        }

        /// Hold the request for this spy's own delay, then refuse it if this method was
        /// named when the spy was built.
        async fn answering(&self, method: &'static str) -> Result<(), ChainError> {
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            let refuses = self.failing.lock().expect("spy lock").contains(&method);
            if refuses {
                Err(ChainError::Rpc(format!("{method} refused")))
            } else {
                Ok(())
            }
        }

        fn seen_blocks(&self) -> Vec<u64> {
            self.seen.lock().expect("spy lock").clone()
        }

        /// Every state request, in the order it arrived — the counter the reuse
        /// boundary's own tally is checked against (§12).
        fn log(&self) -> Vec<SpyCall> {
            self.calls.lock().expect("spy lock").clone()
        }

        fn count(&self, method: &str) -> usize {
            self.log()
                .iter()
                .filter(|call| call.method == method)
                .count()
        }

        fn methods(&self) -> Vec<&'static str> {
            self.log().iter().map(|call| call.method).collect()
        }

        fn note(&self, at: BlockNumber) {
            self.seen.lock().expect("spy lock").push(at.0);
        }

        fn record(&self, method: &'static str, at: BlockNumber, address: Address) {
            self.note(at);
            self.calls.lock().expect("spy lock").push(SpyCall {
                method,
                address: hex_address(address),
                block: at.0,
            });
        }
    }

    #[async_trait::async_trait]
    impl ChainAdapter for SpyChain {
        fn chain_id(&self) -> ChainId {
            ChainId(self.chain_id)
        }

        async fn latest_block(&self) -> Result<BlockNumber, ChainError> {
            Ok(BlockNumber(PIN))
        }

        async fn get_block(&self, number: BlockNumber) -> Result<ChainBlock, ChainError> {
            Ok(ChainBlock {
                chain_id: CHAIN,
                number,
                hash: pin().hash,
                parent_hash: B256::ZERO,
                timestamp: 1_790_536_285,
                transaction_count: 35,
            })
        }

        async fn get_block_data(&self, _number: BlockNumber) -> Result<BlockData, ChainError> {
            Err(ChainError::MissingData("not used here".to_string()))
        }

        async fn get_block_context(&self, number: BlockNumber) -> Result<BlockContext, ChainError> {
            Ok(BlockContext {
                chain_id: CHAIN,
                number,
                hash: pin().hash,
                timestamp: 1_790_536_285,
                gas_limit: 60_000_000,
                base_fee_per_gas: Some(300),
                excess_blob_gas: Some(0),
                beneficiary: address!("0x4200000000000000000000000000000000000011"),
                prevrandao: None,
            })
        }

        async fn get_logs(&self, _filter: LogFilter) -> Result<Vec<ChainLog>, ChainError> {
            Ok(Vec::new())
        }

        async fn call(
            &self,
            _at: BlockNumber,
            _request: &CallRequest,
        ) -> Result<Bytes, ChainError> {
            Err(ChainError::MissingData("not used here".to_string()))
        }

        /// Bytecode is keyed on the address in this spy, so a cross-block bytecode
        /// read is proven by the request count, not by a differing value.
        async fn get_code(&self, at: BlockNumber, address: Address) -> Result<Bytes, ChainError> {
            self.record("eth_getCode", at, address);
            self.answering("eth_getCode").await?;
            Ok(Bytes::from(if address == POOL {
                vec![0xfe, 0x00]
            } else {
                vec![0x60, 0x00]
            }))
        }

        async fn get_balance(&self, at: BlockNumber, address: Address) -> Result<U256, ChainError> {
            self.record("eth_getBalance", at, address);
            self.answering("eth_getBalance").await?;
            Ok(U256::from(42u64 + at.0.saturating_sub(PIN)))
        }

        async fn get_storage_at(
            &self,
            at: BlockNumber,
            address: Address,
            slot: U256,
        ) -> Result<U256, ChainError> {
            self.storage_calls.fetch_add(1, Ordering::SeqCst);
            self.record("eth_getStorageAt", at, address);
            self.answering("eth_getStorageAt").await?;
            Ok(slot + U256::from(1u64 + at.0.saturating_sub(PIN)))
        }

        async fn get_nonce(&self, at: BlockNumber, address: Address) -> Result<u64, ChainError> {
            self.record("eth_getTransactionCount", at, address);
            self.answering("eth_getTransactionCount").await?;
            Ok(at.0.saturating_sub(PIN))
        }
    }

    // ---- M8.3.1: state read reuse -------------------------------------------------
    //
    // One task, one variable (§13): every test below differs from another only in
    // `with_state_read_reuse`, in the pin, or in which address and slot are asked
    // for. The spy is the counter, so each assertion names requests a transport
    // actually received rather than a number the boundary said about itself.

    fn provider_on(chain: &Arc<SpyChain>, pin: BlockPin, reuse: bool) -> RpcStateProvider {
        RpcStateProvider::with_state_read_reuse(
            Arc::clone(chain) as Arc<dyn ChainAdapter>,
            pin,
            reuse,
        )
    }

    /// The same provider with a bound on its dispatch (M8.3.3 §27). `1` is exactly
    /// [`provider_on`], and §10's baseline claim is that the two are the same run.
    fn provider_at(chain: &Arc<SpyChain>, bound: usize) -> RpcStateProvider {
        RpcStateProvider::with_state_read_concurrency(
            Arc::clone(chain) as Arc<dyn ChainAdapter>,
            pin(),
            true,
            bound,
        )
    }

    fn pin_at(height: u64) -> BlockPin {
        BlockPin::new(BlockNumber(height), pin().hash)
    }

    /// One `account` read, unwrapped: the spy always answers, and nine of the tests
    /// below would otherwise spend their lines on the same two `expect`s.
    async fn account_of(provider: &RpcStateProvider, address: Address) -> AccountState {
        provider
            .account(address)
            .await
            .expect("the spy answers")
            .expect("the spy answers with a header")
    }

    /// §8: inside one simulation the sender's triple is paid for once. The first
    /// read goes out, the second is answered from this simulation's own cache — and
    /// the third line of the assertion is the part that matters: the values are the
    /// ones the node gave at this pin, not defaults.
    #[tokio::test]
    async fn the_sender_triple_costs_three_reads_then_none() {
        let chain = Arc::new(SpyChain::default());
        let provider = provider_on(&chain, pin(), true);

        let first = account_of(&provider, CALLER).await;
        assert_eq!(
            chain.methods(),
            vec!["eth_getBalance", "eth_getTransactionCount", "eth_getCode"],
            "the order the reads go out in is the order M8.2 measured"
        );

        let second = account_of(&provider, CALLER).await;
        assert_eq!(chain.count("eth_getBalance"), 1, "the balance was reused");
        assert_eq!(
            chain.count("eth_getTransactionCount"),
            1,
            "the nonce was reused"
        );
        assert_eq!(chain.count("eth_getCode"), 1, "the bytecode was reused");
        assert_eq!(first, second, "a reused answer is the same answer");

        let stats = provider.state_read_stats();
        assert!(stats.reuse);
        assert_eq!(
            (stats.balance.hits, stats.balance.misses),
            (1, 1),
            "one read remembered, one read paid for"
        );
        assert_eq!((stats.nonce.hits, stats.nonce.misses), (1, 1));
        assert_eq!((stats.code.hits, stats.code.misses), (1, 1));
    }

    /// §6: arm A has to be the run M8.2 measured, or the A/B is comparing two
    /// experiments instead of one variable. With reuse off the same sequence costs
    /// six requests — the duplicate the task book counted 41 times over three runs.
    #[tokio::test]
    async fn the_same_triple_costs_six_reads_with_reuse_off() {
        let chain = Arc::new(SpyChain::default());
        let provider = provider_on(&chain, pin(), false);
        let first = account_of(&provider, CALLER).await;
        let second = account_of(&provider, CALLER).await;
        assert_eq!(chain.log().len(), 6);
        assert_eq!(first, second, "the answer never depended on caching");
        let stats = provider.state_read_stats();
        assert!(!stats.reuse);
        assert_eq!(
            (stats.balance.hits, stats.balance.misses),
            (0, 0),
            "arm A asks the boundary nothing, so it reports nothing — which §12 reads as \
             not measured, not as zero hits on a cache that worked"
        );
    }

    /// §16: one boundary, not two halves of one. The bytecode an account read
    /// remembers is the bytecode a later direct `code()` call is served from, and
    /// the other way round — otherwise the run would be part cached, part not, and
    /// the reduction would be an artifact of which path the engine happened to take.
    #[tokio::test]
    async fn an_account_read_and_a_direct_code_read_share_one_boundary() {
        let chain = Arc::new(SpyChain::default());
        let provider = provider_on(&chain, pin(), true);
        account_of(&provider, CALLER).await;
        let from_account = provider.code(CALLER).await.expect("the read is remembered");
        assert_eq!(
            chain.count("eth_getCode"),
            1,
            "the account read already paid for this bytecode"
        );

        account_of(&provider, WETH).await;
        assert_eq!(chain.count("eth_getCode"), 2, "a new account is a new read");
        assert_eq!(
            from_account,
            Bytes::from(vec![0x60, 0x00]),
            "the spy's answer for an address that is not the pool"
        );
        let stats = provider.state_read_stats();
        assert_eq!((stats.code.hits, stats.code.misses), (1, 2));
    }

    /// §9, the negative half: storage has no duplicates in the live sample, so its
    /// cache is only proven by the reads it must NOT serve. Each row below changes
    /// exactly one field of the key and must therefore cost one more request.
    #[tokio::test]
    async fn storage_reuse_needs_every_field_of_its_key() {
        let chain = Arc::new(SpyChain::default());
        let provider = provider_on(&chain, pin(), true);
        let slot = U256::from(6u64);
        let other_slot = U256::from(7u64);
        let other_pool = WETH;

        for (what, address, asked) in [
            ("same key twice", POOL, slot),
            ("different slot", POOL, other_slot),
            ("same slot, different address", other_pool, slot),
        ] {
            provider.storage(address, asked).await.expect(what);
            provider.storage(address, asked).await.expect(what);
        }
        assert_eq!(
            chain.count("eth_getStorageAt"),
            3,
            "one read per distinct key, never one per call"
        );
        assert_eq!(
            provider.state_read_stats().storage,
            ReuseTally { hits: 3, misses: 3 },
        );

        // Same slot, same address, one block later: a different fact about a
        // different block, and the spy answers it with a different word.
        let next = provider_on(&chain, pin_at(PIN + 1), true);
        assert_eq!(
            next.storage(POOL, slot).await.expect("the next block"),
            slot + U256::from(2u64),
            "the height is part of the answer, not just of the key"
        );
        assert_eq!(chain.count("eth_getStorageAt"), 4);

        // Same everything but the chain.
        let other_chain = Arc::new(SpyChain::on(OTHER_CHAIN));
        let elsewhere = provider_on(&other_chain, pin(), true);
        assert_eq!(
            elsewhere
                .storage(POOL, slot)
                .await
                .expect("the other chain"),
            slot + U256::from(1u64)
        );
        assert_eq!(other_chain.count("eth_getStorageAt"), 1);
    }

    /// §10: the boundary a historical simulation most needs. Two heights, one
    /// address — each provider asks for its own block's balance and nonce, and
    /// neither is answered from the other's read.
    #[tokio::test]
    async fn a_read_at_one_block_is_not_the_read_at_the_next() {
        let chain = Arc::new(SpyChain::default());
        let here = provider_on(&chain, pin(), true);
        let there = provider_on(&chain, pin_at(PIN + 1), true);

        let here_account = account_of(&here, CALLER).await;
        let there_account = account_of(&there, CALLER).await;
        assert_eq!(chain.count("eth_getBalance"), 2);
        assert_eq!(chain.count("eth_getTransactionCount"), 2);
        assert_eq!(
            here_account.balance,
            U256::from(42u64),
            "block {PIN} as the node answered it"
        );
        assert_eq!(
            there_account.balance,
            U256::from(43u64),
            "the next block is a different balance, and no cache gets to say otherwise"
        );
        assert_eq!(there_account.nonce, 1);
        assert_eq!(here_account.nonce, 0);
        for stats in [here.state_read_stats(), there.state_read_stats()] {
            assert_eq!(
                (stats.balance.hits, stats.nonce.hits),
                (0, 0),
                "two heights, no reuse between them"
            );
        }
    }

    /// §11: two simulations of the same block still pay twice. The cache is the
    /// provider's, and a provider is one simulation — there is no second one for it
    /// to leak into, which is the property §2 asks for rather than a policy someone
    /// remembers to follow.
    #[tokio::test]
    async fn two_simulations_never_share_a_read() {
        let chain = Arc::new(SpyChain::default());
        let first = provider_on(&chain, pin(), true);
        let second = provider_on(&chain, pin(), true);
        account_of(&first, CALLER).await;
        account_of(&second, CALLER).await;
        assert_eq!(
            chain.log().len(),
            6,
            "one simulation's read cannot be the next simulation's answer"
        );
        assert_eq!(first.state_read_stats().balance.hits, 0);
        assert_eq!(second.state_read_stats().balance.hits, 0);

        // The same provider, asked twice, is the reuse case — and it is the only one.
        account_of(&first, CALLER).await;
        assert_eq!(chain.log().len(), 6);
        assert_eq!(first.state_read_stats().balance.hits, 1);
    }

    /// §4: a read that failed is not a value. Nothing is stored on the error path,
    /// so the next step of the same simulation asks again instead of inheriting a
    /// timeout — which would change what the simulation does, and §13's one variable
    /// ends there.
    #[tokio::test]
    async fn a_failed_read_leaves_nothing_to_reuse() {
        let provider = RpcStateProvider::with_state_read_reuse(
            Arc::new(FailingChain) as Arc<dyn ChainAdapter>,
            pin(),
            true,
        );
        for attempt in 1..=2 {
            let error = provider
                .account(CALLER)
                .await
                .expect_err("the node is down");
            assert!(
                matches!(error, ProviderError::Unavailable { .. }),
                "attempt {attempt}"
            );
        }
        let stats = provider.state_read_stats();
        assert_eq!(
            (stats.balance.hits, stats.nonce.hits, stats.code.hits),
            (0, 0, 0),
            "a failure cannot be a hit"
        );
        assert_eq!(
            (stats.balance.misses, stats.nonce.misses, stats.code.misses),
            (0, 0, 0),
            "and a failure is not a read that succeeded and got remembered either"
        );
        assert!(provider
            .storage(POOL, U256::from(6u64))
            .await
            .is_err_and(|error| matches!(error, ProviderError::Unavailable { .. })));
        assert_eq!(provider.state_read_stats().storage.misses, 0);
    }

    /// §12: the tally is checked against the transport, not against itself. Every
    /// miss is a request the spy counted, every hit is a request it did not get.
    #[tokio::test]
    async fn the_tally_reconciles_with_the_requests() {
        let chain = Arc::new(SpyChain::default());
        let provider = provider_on(&chain, pin(), true);
        for address in [CALLER, WETH, POOL, CALLER, WETH] {
            account_of(&provider, address).await;
        }
        provider.storage(POOL, U256::from(6u64)).await.expect("ok");
        provider.storage(POOL, U256::from(6u64)).await.expect("ok");
        provider.storage(WETH, U256::from(6u64)).await.expect("ok");

        let stats = provider.state_read_stats();
        for (tally, method) in [
            (&stats.balance, "eth_getBalance"),
            (&stats.nonce, "eth_getTransactionCount"),
            (&stats.code, "eth_getCode"),
            (&stats.storage, "eth_getStorageAt"),
        ] {
            assert_eq!(
                tally.misses,
                chain.count(method),
                "{method}: a miss is exactly one request"
            );
        }
        assert_eq!(
            stats.total_misses(),
            chain.log().len(),
            "a miss is a request, and the spy is the authority on requests"
        );
        assert_eq!(
            stats.total_hits(),
            7,
            "two repeated account reads x three kinds, plus the slot asked for twice"
        );
    }

    /// §5, the state half: reuse changes how many requests a simulation makes and
    /// nothing about what the node said. The recorded state — the part a fixture is
    /// written from and the part the EVM executes on — is equal across arms; only
    /// the log of requests differs, and it differs because fewer were made.
    #[tokio::test]
    async fn both_arms_record_the_same_state_and_different_requests() {
        let sequence = |reuse: bool| async move {
            let chain = Arc::new(SpyChain::default());
            let provider = provider_on(&chain, pin(), reuse);
            for address in [CALLER, POOL, CALLER] {
                account_of(&provider, address).await;
            }
            provider.storage(POOL, U256::from(6u64)).await.expect("ok");
            provider.storage(POOL, U256::from(6u64)).await.expect("ok");
            (chain.log().len(), provider.dump())
        };
        let (uncached_reads, uncached) = sequence(false).await;
        let (cached_reads, cached) = sequence(true).await;

        // Ten, not eleven: HEAD already remembered that slot.
        assert_eq!(uncached_reads, 10, "3 accounts x 3 reads + 1 storage");
        assert_eq!(cached_reads, 7, "one whole account read remembered");
        assert_eq!(uncached.accounts, cached.accounts, "same state recorded");
        assert_eq!(uncached.storage, cached.storage, "same words recorded");
        assert_eq!(uncached.block_number, cached.block_number);
        assert_eq!(uncached.chain_id, cached.chain_id);
        assert!(
            cached.reads.len() < uncached.reads.len(),
            "the cached arm made fewer requests, and its dump says so: {:?}",
            (uncached.reads.len(), cached.reads.len())
        );
    }

    /// The pin is not a suggestion: reuse happens at the height the provider was
    /// built with, and this is the read that would notice if it drifted.
    #[tokio::test]
    async fn reuse_reads_at_the_pin_and_never_at_a_later_height() {
        let chain = Arc::new(SpyChain::default());
        let provider = provider_on(&chain, pin_at(PIN + 5), true);
        account_of(&provider, CALLER).await;
        account_of(&provider, CALLER).await;
        assert_eq!(
            chain.seen_blocks(),
            vec![PIN + 5, PIN + 5, PIN + 5],
            "three reads out, at the height this provider was pinned to, and nothing after"
        );
    }

    // ---- M8.3.3 §37: the same claims at a bound above one -------------------------
    //
    // M8.3.1 proved these five things — order, pin, cache locality, nothing cached on
    // failure, the tally reconciled against the transport — for a build that could only
    // run one read at a time. The rows below repeat them with the dispatch bound turned
    // up, because the milestone's claim is that the bound is the only variable. Each test
    // differs from its M8.3.1 neighbour in `provider_at`'s fourth argument and nothing
    // else.

    /// One mixed pass: an account's triple, a slot the EVM demands after it has the
    /// account, and two bytecodes the route has already named. This is the shape §5's
    /// dependency map found — two hands whose members do not depend on one another, and
    /// one read that is never in a hand — so the tests below compare a sequence a
    /// simulation actually runs rather than one invented for a scheduler.
    async fn mixed_pass(provider: &RpcStateProvider) {
        account_of(provider, CALLER).await;
        provider
            .storage(POOL, U256::from(6u64))
            .await
            .expect("the spy answers the pool's slot");
        provider
            .codes(&[WETH, POOL])
            .await
            .expect("the spy answers both bytecodes");
    }

    /// §37's Dependency row, the ordered half, on a transport that holds every request
    /// long enough for a bound above one to mean something. Six requests in six named
    /// methods, at every bound, in the same order: the storage read still arrives after
    /// the triple it depends on, and the two bytecodes the route named still arrive after
    /// both. What changed is how many were outstanding at once, which is the next test's
    /// business, not this one's.
    #[tokio::test]
    async fn a_bound_reorders_no_request_the_caller_had_decided() {
        const HOLD: Duration = Duration::from_millis(1);
        let expected = [
            "eth_getBalance",
            "eth_getTransactionCount",
            "eth_getCode",
            "eth_getStorageAt",
            "eth_getCode",
            "eth_getCode",
        ];
        let mut logs = Vec::new();
        for bound in [1usize, 2, 4] {
            let chain = Arc::new(SpyChain::slowing(HOLD));
            let provider = provider_at(&chain, bound);
            mixed_pass(&provider).await;
            assert_eq!(
                chain.methods(),
                expected,
                "bound {bound} sent the reads in another order"
            );
            assert_eq!(chain.log().len(), 6, "bound {bound}");
            logs.push(chain.log());
        }
        assert_eq!(
            logs[0], logs[2],
            "a request the serial arm made is missing from, or different in, the arm that \
             configured four"
        );
    }

    /// §37's Dependency row, the overlapping half, and the one place the provider's own
    /// instrument is checked rather than the raw scheduler's: the triple of one account
    /// really does have more than one member outstanding at the node once the bound
    /// allows it, and the number never exceeds the bound.
    ///
    /// The bound of four peaks at three and not at four because this hand has three
    /// members — the instrument reports the widest instant reached, which is the point of
    /// §15 publishing it beside the configured value.
    #[tokio::test]
    async fn a_providers_independent_reads_overlap_up_to_the_bound() {
        const HOLD: Duration = Duration::from_millis(1);
        for bound in [1usize, 2, 4] {
            let chain = Arc::new(SpyChain::slowing(HOLD));
            let provider = provider_at(&chain, bound);
            account_of(&provider, CALLER).await;

            let report = provider.state_read_concurrency();
            assert_eq!(
                report.observed_peak,
                bound.min(3),
                "three reads were handed over at the bound of {bound}"
            );
            assert_eq!(
                report.configured, bound,
                "and the report still says what was asked for, separately"
            );
            assert_eq!(report.serial, bound == 1);
            assert_eq!(
                chain.log().len(),
                3,
                "overlapping is not batching: three reads are still three requests at any \
                 bound, which is §3's ban on a JSON-RPC batch measured at the transport"
            );
        }
    }

    /// §7's other half, read out of the scheduler's own log: only the two hands the
    /// dependency analysis proved are ever opened, no batch ever carries a storage read,
    /// and every descriptor names this provider's pinned height.
    #[tokio::test]
    async fn only_the_two_proved_hands_are_ever_opened() {
        for bound in [1usize, 2, 4] {
            let chain = Arc::new(SpyChain::default());
            let provider = provider_at(&chain, bound);
            mixed_pass(&provider).await;

            let report = provider.state_read_concurrency();
            let sites: Vec<&str> = report
                .batch_dispatches
                .iter()
                .map(|batch| batch.site)
                .collect();
            assert_eq!(
                sites,
                vec!["account_triple", "touched_contracts"],
                "bound {bound} opened a batch the dependency map does not name"
            );
            assert_eq!(
                report.batched_reads, 5,
                "three of the triple plus two bytecodes"
            );
            for batch in &report.batch_dispatches {
                assert_eq!(
                    batch.limit, bound,
                    "the log records the bound that was in force, per hand"
                );
                for read in &batch.reads {
                    assert_ne!(
                        read.kind, "storage",
                        "a slot the EVM demands one at a time was handed to the scheduler, \
                         which would be §7's unproved dependency run concurrently"
                    );
                    assert_eq!(read.block, PIN, "§8: a descriptor names this pin");
                    assert_eq!(read.chain_id, CHAIN.0);
                }
            }
        }
    }

    /// §37's Historical block row: with the bound at two and at four as much as at one,
    /// every request the transport received named this provider's height, and nothing
    /// asked for a later one. §8 calls this the highest-priority rule, so it is checked at
    /// every arm rather than once.
    #[tokio::test]
    async fn every_request_at_every_bound_names_the_pin() {
        for bound in [1usize, 2, 4] {
            let chain = Arc::new(SpyChain::default());
            let provider = provider_at(&chain, bound);
            mixed_pass(&provider).await;
            // One more read at a deliberately different height, through a provider built
            // for it: the bound must not be able to borrow an answer across heights.
            let next = provider_on(&chain, pin_at(PIN + 1), true);
            account_of(&next, CALLER).await;

            let seen = chain.seen_blocks();
            assert_eq!(seen.len(), 9, "bound {bound}");
            let at_the_pin = seen.iter().filter(|height| **height == PIN).count();
            assert_eq!(at_the_pin, 6, "six of the nine reads were this provider's");
            assert!(
                seen.iter()
                    .all(|height| *height == PIN || *height == PIN + 1),
                "a request named a height that is neither provider's pin: {seen:?}"
            );
        }
    }

    /// §37's Cache row: `C1`, `C2` and `C4` change nothing about what the reuse boundary
    /// remembers. The same mixed pass run twice at each bound gives the same tally, the
    /// same recorded state and the same number of requests, for both values of `reuse` —
    /// the two switches stay orthogonal instead of one quietly turning the other off.
    #[tokio::test]
    async fn a_bound_changes_nothing_the_cache_remembers() {
        let pass = |bound: usize, reuse: bool| async move {
            let chain = Arc::new(SpyChain::default());
            let provider = RpcStateProvider::with_state_read_concurrency(
                Arc::clone(&chain) as Arc<dyn ChainAdapter>,
                pin(),
                reuse,
                bound,
            );
            mixed_pass(&provider).await;
            // The second pass is where a bound could smuggle in a difference: by then the
            // triple is entirely cache, so its hand has nothing left to overlap.
            mixed_pass(&provider).await;
            (
                chain.log().len(),
                provider.state_read_stats(),
                provider.dump(),
            )
        };

        let mut baseline: Option<(usize, StateReadStats, StateDump)> = None;
        let mut baseline_a: Option<(usize, StateReadStats, StateDump)> = None;
        for bound in [1usize, 2, 4] {
            let cached = pass(bound, true).await;
            assert_eq!(
                cached.0, 6,
                "bound {bound}: six reads the first time, none again"
            );
            assert_eq!(
                (cached.1.total_hits(), cached.1.total_misses()),
                (6, 6),
                "bound {bound} remembered a different number of reads than the serial arm"
            );
            if let Some((requests, stats, dump)) = baseline.clone() {
                assert_eq!(
                    cached.1, stats,
                    "the arm that configured {bound} hit and missed differently"
                );
                assert_eq!(cached.0, requests);
                assert_eq!(cached.2.accounts, dump.accounts);
                assert_eq!(cached.2.storage, dump.storage);
            }
            baseline = Some(cached);

            // Arm A at a raised bound: nine requests, and the balance and the nonce are
            // neither hit nor missed, because that switch is the one `reuse` controls.
            // Nine, not twelve — the bytecode hand and the slot keep the lookups M8.2's
            // HEAD already had, which [`RpcStateProvider::with_state_read_reuse`] spells
            // out and this line is the measurement of.
            let (requests, stats, dump) = pass(bound, false).await;
            assert_eq!(requests, 9, "bound {bound}");
            assert_eq!(
                (stats.balance.hits, stats.balance.misses),
                (0, 0),
                "arm A asks the boundary nothing about balances, so it reports nothing \
                 about them — §12's not-measured, at a bound of {bound}"
            );
            assert_eq!((stats.nonce.hits, stats.nonce.misses), (0, 0));
            assert!(
                !stats.reuse,
                "and the arm is labelled by what it was told, not by what it peaked at"
            );
            // The state is the same state either way, which is §17 at the transport level.
            if let Some(serial_a) = baseline_a.clone() {
                assert_eq!(
                    (requests, stats),
                    (serial_a.0, serial_a.1),
                    "arm A differed between bounds, so the bound moved something besides \
                     how many reads were outstanding"
                );
                assert_eq!(dump.accounts, serial_a.2.accounts);
                assert_eq!(dump.storage, serial_a.2.storage);
            }
            baseline_a = Some((requests, stats, dump));
        }

        // And the cache stays one simulation's at a raised bound: two providers, one
        // block, twelve requests between them because neither may answer from the other.
        let chain = Arc::new(SpyChain::default());
        let first = provider_at(&chain, 4);
        let second = provider_at(&chain, 4);
        mixed_pass(&first).await;
        mixed_pass(&second).await;
        assert_eq!(
            chain.log().len(),
            12,
            "M8.3.1 §11 held at the bound of four: a second simulation pays for its own \
             reads"
        );
        assert_eq!(first.state_read_stats().total_hits(), 0);
        assert_eq!(second.state_read_stats().total_hits(), 0);
    }

    /// §37's Errors row, first half: a node that is down is down at the bound of four too.
    /// Both accounts fail, nothing is stored from either attempt, and the tally stays at
    /// zero in both directions — a failure is neither a hit nor a read that succeeded.
    #[tokio::test]
    async fn a_failed_read_caches_nothing_at_a_bound_of_four() {
        let provider = RpcStateProvider::with_state_read_concurrency(
            Arc::new(FailingChain) as Arc<dyn ChainAdapter>,
            pin(),
            true,
            4,
        );
        for attempt in 1..=2 {
            let error = provider
                .account(CALLER)
                .await
                .expect_err("the node is down at any bound");
            assert!(
                matches!(error, ProviderError::Unavailable { .. }),
                "attempt {attempt}"
            );
        }
        let stats = provider.state_read_stats();
        assert_eq!(
            (stats.balance.hits, stats.nonce.hits, stats.code.hits),
            (0, 0, 0),
            "a failure cannot be a hit"
        );
        assert_eq!(
            (stats.balance.misses, stats.nonce.misses, stats.code.misses),
            (0, 0, 0),
            "and a failure is not a read that succeeded and got remembered either"
        );
        assert_eq!(
            provider.state_read_concurrency().batches,
            2,
            "§24's rule that an error ends the run did not stop the scheduler from being \
             asked again by the caller's second read"
        );
    }

    /// §37's Errors row, second half: one failed task propagates, and it propagates as
    /// the failure the sequential code would have reported. Two members of the same hand
    /// refuse, with reasons naming which one; the run answers with the earlier one in
    /// input order at both bounds, and §24's no-default, no-stale rule holds because the
    /// account simply is not returned.
    ///
    /// The two bounds differ in one way and the difference is stated rather than
    /// smoothed over: at one, the run stops after the balance request; at four, the whole
    /// first chunk has already left, so the nonce and code reads the serial arm never made
    /// are counted here. M8.3.3 §10's extra-read allowance is exactly this, and a failing
    /// batch that hid it would put calls on the wire with no trace event behind them.
    #[tokio::test]
    async fn the_earliest_failure_of_a_hand_is_the_one_the_run_reports() {
        for (bound, reads) in [(1usize, 1usize), (4, 3)] {
            let chain = Arc::new(SpyChain::refusing(&["eth_getBalance", "eth_getCode"]));
            let provider = provider_at(&chain, bound);
            let error = provider
                .account(CALLER)
                .await
                .expect_err("two members of the hand refuse");
            assert!(
                matches!(error, ProviderError::Unavailable { .. }),
                "{error}"
            );
            assert!(
                error.to_string().contains("eth_getBalance"),
                "bound {bound} reported the wrong failure: {error}"
            );
            assert_eq!(
                chain.log().len(),
                reads,
                "bound {bound} should have asked the node for {reads} of the hand's three \
                 reads before stopping"
            );
            let stats = provider.state_read_stats();
            assert_eq!(
                (stats.balance.hits, stats.balance.misses),
                (0, 0),
                "and the failed read left nothing behind"
            );
        }
    }

    /// The same, but every read fails: a node that is down, not a state that is
    /// absent.
    struct FailingChain;

    #[async_trait::async_trait]
    impl ChainAdapter for FailingChain {
        fn chain_id(&self) -> ChainId {
            CHAIN
        }

        async fn latest_block(&self) -> Result<BlockNumber, ChainError> {
            Err(ChainError::Rpc("node is down".to_string()))
        }

        async fn get_block(&self, _number: BlockNumber) -> Result<ChainBlock, ChainError> {
            Err(ChainError::Rpc("node is down".to_string()))
        }

        async fn get_block_data(&self, _number: BlockNumber) -> Result<BlockData, ChainError> {
            Err(ChainError::Rpc("node is down".to_string()))
        }

        async fn get_block_context(
            &self,
            _number: BlockNumber,
        ) -> Result<BlockContext, ChainError> {
            Err(ChainError::Rpc("node is down".to_string()))
        }

        async fn get_logs(&self, _filter: LogFilter) -> Result<Vec<ChainLog>, ChainError> {
            Err(ChainError::Rpc("node is down".to_string()))
        }

        async fn call(
            &self,
            _at: BlockNumber,
            _request: &CallRequest,
        ) -> Result<Bytes, ChainError> {
            Err(ChainError::Rpc("node is down".to_string()))
        }

        async fn get_code(&self, _at: BlockNumber, _address: Address) -> Result<Bytes, ChainError> {
            Err(ChainError::Rpc("node is down".to_string()))
        }

        async fn get_balance(
            &self,
            _at: BlockNumber,
            _address: Address,
        ) -> Result<U256, ChainError> {
            Err(ChainError::Rpc("node is down".to_string()))
        }

        async fn get_storage_at(
            &self,
            _at: BlockNumber,
            _address: Address,
            _slot: U256,
        ) -> Result<U256, ChainError> {
            Err(ChainError::Rpc("node is down".to_string()))
        }

        async fn get_nonce(&self, _at: BlockNumber, _address: Address) -> Result<u64, ChainError> {
            Err(ChainError::Rpc("node is down".to_string()))
        }
    }
}
