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
use serde::{Deserialize, Serialize};
use thiserror::Error;

use evm_chain::{BlockContext, ChainAdapter};
use evm_core::{BlockNumber, ChainId};

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
    pub fn with_state_read_reuse(chain: Arc<dyn ChainAdapter>, pin: BlockPin, reuse: bool) -> Self {
        let chain_id = chain.chain_id();
        Self {
            recorded: Arc::new(Mutex::new(StateDump::empty(chain_id, pin))),
            chain,
            chain_id,
            pin,
            overrides: Vec::new(),
            cache: Arc::new(Mutex::new(StateReadCache::new(reuse))),
            reuse,
        }
    }

    pub fn with_overrides(mut self, overrides: Vec<StateOverride>) -> Self {
        self.overrides = overrides;
        self
    }

    /// The reads made so far, in call order — what a fixture is written from.
    pub fn dump(&self) -> StateDump {
        self.recorded.lock().expect("dump lock").clone()
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
        let code = self
            .chain
            .get_code(key.block, key.address)
            .await
            .map_err(|error| self.unavailable(error.to_string()))?;
        self.cache().put_code(key, &code, ask);
        Ok((code, true))
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

    async fn account(&self, address: Address) -> ProviderResult<Option<AccountState>> {
        let key = StateReadKey::new(self.chain_id, self.pin.number, address);
        // Sequential, not `join!`: §61 puts correctness above latency, the
        // recorded dump has to list reads in the order they were made or a
        // fixture's bytes would move between runs, and M8.3.1 §13 keeps
        // concurrency out of this milestone.
        let (balance, balance_read) = self.balance_at(key, self.reuse).await?;
        let (nonce, nonce_read) = self.nonce_at(key, self.reuse).await?;
        let (code, code_read) = self.code_at(key, self.reuse).await?;
        if balance_read || nonce_read || code_read {
            // One marker per account read that cost the node something, which is
            // what `reads` has always counted: an answer reused from this
            // simulation's own earlier read asks the node nothing, and a dump that
            // claimed otherwise would record a request that never happened.
            let mut dump = self.recorded.lock().expect("dump lock");
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
        let value = self
            .chain
            .get_storage_at(self.pin.number, address, slot)
            .await
            .map_err(|error| self.unavailable(error.to_string()))?;
        self.cache().put_storage(key, value);
        {
            let mut dump = self.recorded.lock().expect("dump lock");
            dump.insert_storage(address, slot, value);
            dump.reads.push(format!("storage {address} {slot}"));
        }
        Ok(value)
    }

    async fn code(&self, address: Address) -> ProviderResult<Bytes> {
        if let Some(over) = header_override(&self.overrides, address) {
            if let Some(code) = &over.code {
                return Ok(code.clone());
            }
        }
        let key = StateReadKey::new(self.chain_id, self.pin.number, address);
        let (code, read) = self.code_at(key, true).await?;
        if read {
            self.recorded
                .lock()
                .expect("dump lock")
                .reads
                .push(format!("code {address}"));
        }
        Ok(code)
    }

    async fn header(&self) -> ProviderResult<BlockContext> {
        let block = self
            .chain
            .get_block_context(self.pin.number)
            .await
            .map_err(|error| self.unavailable(error.to_string()))?;
        {
            let mut dump = self.recorded.lock().expect("dump lock");
            dump.header = Some(RecordedHeader::of_block(&block));
            dump.reads.push(format!("header {}", block.number.0));
        }
        Ok(block)
    }

    async fn block_hash(&self, number: BlockNumber) -> ProviderResult<Option<B256>> {
        let block = self
            .chain
            .get_block(number)
            .await
            .map_err(|error| self.unavailable(error.to_string()))?;
        let hash = block.hash;
        self.recorded
            .lock()
            .expect("dump lock")
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

    use alloy_primitives::{address, Address, Bytes, B256, U256};

    use super::{
        hex_address, override_slot, AccountState, BlockPin, DumpStateProvider, ProviderError,
        ReuseTally, RpcStateProvider, StateDump, StateOverride, StateProvider, StateReadKey,
        StorageCacheKey, KECCAK_EMPTY,
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
    }

    impl Default for SpyChain {
        fn default() -> Self {
            Self {
                storage_calls: AtomicUsize::new(0),
                seen: Mutex::new(Vec::new()),
                calls: Mutex::new(Vec::new()),
                chain_id: CHAIN.0,
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
            Ok(Bytes::from(if address == POOL {
                vec![0xfe, 0x00]
            } else {
                vec![0x60, 0x00]
            }))
        }

        async fn get_balance(&self, at: BlockNumber, address: Address) -> Result<U256, ChainError> {
            self.record("eth_getBalance", at, address);
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
            Ok(slot + U256::from(1u64 + at.0.saturating_sub(PIN)))
        }

        async fn get_nonce(&self, at: BlockNumber, address: Address) -> Result<u64, ChainError> {
            self.record("eth_getTransactionCount", at, address);
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
