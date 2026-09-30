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

/// Key of the bytecode cache: chain plus address, never address alone (§63).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CodeKey {
    pub chain_id: ChainId,
    pub address: Address,
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
pub struct RpcStateProvider {
    chain: Arc<dyn ChainAdapter>,
    chain_id: ChainId,
    pin: BlockPin,
    overrides: Vec<StateOverride>,
    code_cache: Arc<Mutex<BTreeMap<CodeKey, Bytes>>>,
    storage_cache: Arc<Mutex<BTreeMap<StorageCacheKey, U256>>>,
    recorded: Arc<Mutex<StateDump>>,
}

impl RpcStateProvider {
    pub fn new(chain: Arc<dyn ChainAdapter>, pin: BlockPin) -> Self {
        let chain_id = chain.chain_id();
        Self {
            recorded: Arc::new(Mutex::new(StateDump::empty(chain_id, pin))),
            chain,
            chain_id,
            pin,
            overrides: Vec::new(),
            code_cache: Arc::new(Mutex::new(BTreeMap::new())),
            storage_cache: Arc::new(Mutex::new(BTreeMap::new())),
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

    fn unavailable(&self, reason: String) -> ProviderError {
        ProviderError::Unavailable {
            provider: StateProvider::source(self),
            reason,
        }
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
        let block = self.pin.number;
        // Sequential, not `join!`: §61 puts correctness above latency, and the
        // recorded dump has to list reads in the order they were made or a
        // fixture's bytes would move between runs.
        let balance = self
            .chain
            .get_balance(block, address)
            .await
            .map_err(|error| self.unavailable(error.to_string()))?;
        let nonce = self
            .chain
            .get_nonce(block, address)
            .await
            .map_err(|error| self.unavailable(error.to_string()))?;
        let code = self
            .chain
            .get_code(block, address)
            .await
            .map_err(|error| self.unavailable(error.to_string()))?;
        {
            let mut dump = self.recorded.lock().expect("dump lock");
            dump.insert_account(address, balance, nonce, &code);
            dump.reads.push(format!("account {address}"));
        }
        self.code_cache.lock().expect("code cache lock").insert(
            CodeKey {
                chain_id: self.chain_id,
                address,
            },
            code.clone(),
        );
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
        let key = StorageCacheKey {
            chain_id: self.chain_id,
            block: self.pin.number,
            address,
            slot,
        };
        if let Some(value) = self
            .storage_cache
            .lock()
            .expect("storage cache lock")
            .get(&key)
            .copied()
        {
            return Ok(value);
        }
        let value = self
            .chain
            .get_storage_at(self.pin.number, address, slot)
            .await
            .map_err(|error| self.unavailable(error.to_string()))?;
        let mut cache = self.storage_cache.lock().expect("storage cache lock");
        cache.insert(key, value);
        let mut dump = self.recorded.lock().expect("dump lock");
        dump.insert_storage(address, slot, value);
        dump.reads.push(format!("storage {address} {slot}"));
        Ok(value)
    }

    async fn code(&self, address: Address) -> ProviderResult<Bytes> {
        if let Some(over) = header_override(&self.overrides, address) {
            if let Some(code) = &over.code {
                return Ok(code.clone());
            }
        }
        let key = CodeKey {
            chain_id: self.chain_id,
            address,
        };
        if let Some(code) = self
            .code_cache
            .lock()
            .expect("code cache lock")
            .get(&key)
            .cloned()
        {
            return Ok(code);
        }
        let code = self
            .chain
            .get_code(self.pin.number, address)
            .await
            .map_err(|error| self.unavailable(error.to_string()))?;
        self.code_cache
            .lock()
            .expect("code cache lock")
            .insert(key, code.clone());
        self.recorded
            .lock()
            .expect("dump lock")
            .reads
            .push(format!("code {address}"));
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
            code_cache: Arc::clone(&self.code_cache),
            storage_cache: Arc::clone(&self.storage_cache),
            recorded: Arc::clone(&self.recorded),
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
        hex_address, override_slot, BlockPin, CodeKey, DumpStateProvider, ProviderError,
        RpcStateProvider, StateDump, StateOverride, StateProvider, StorageCacheKey, KECCAK_EMPTY,
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

    /// §63: a bytecode cache keyed on the address alone would serve one chain's
    /// bytecode for another chain's identical address. The key carries the chain.
    #[test]
    fn code_cache_key_carries_the_chain() {
        let addr = address!("0x0000000000000000000000000000000000000001");
        let on_this_chain = CodeKey {
            chain_id: CHAIN,
            address: addr,
        };
        let on_another = CodeKey {
            chain_id: OTHER_CHAIN,
            address: addr,
        };
        assert_ne!(on_this_chain, on_another);
        let mut cache: BTreeMap<CodeKey, Bytes> = BTreeMap::new();
        cache.insert(on_this_chain, Bytes::from(vec![0x01]));
        cache.insert(on_another, Bytes::from(vec![0x02]));
        assert_eq!(cache.len(), 2, "two chains, two entries");
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

    /// A minimal [`ChainAdapter`] that counts reads and answers with fixed
    /// values. This is a transport double for tests, not the third state
    /// provider §62 warns about.
    #[derive(Default)]
    struct SpyChain {
        storage_calls: AtomicUsize,
        seen: Mutex<Vec<u64>>,
    }

    impl SpyChain {
        fn seen_blocks(&self) -> Vec<u64> {
            self.seen.lock().expect("spy lock").clone()
        }

        fn note(&self, at: BlockNumber) {
            self.seen.lock().expect("spy lock").push(at.0);
        }
    }

    #[async_trait::async_trait]
    impl ChainAdapter for SpyChain {
        fn chain_id(&self) -> ChainId {
            CHAIN
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

        async fn get_code(&self, at: BlockNumber, address: Address) -> Result<Bytes, ChainError> {
            self.note(at);
            Ok(Bytes::from(if address == POOL {
                vec![0xfe, 0x00]
            } else {
                vec![0x60, 0x00]
            }))
        }

        async fn get_balance(
            &self,
            at: BlockNumber,
            _address: Address,
        ) -> Result<U256, ChainError> {
            self.note(at);
            Ok(U256::from(42u64))
        }

        async fn get_storage_at(
            &self,
            at: BlockNumber,
            _address: Address,
            _slot: U256,
        ) -> Result<U256, ChainError> {
            self.storage_calls.fetch_add(1, Ordering::SeqCst);
            self.note(at);
            Ok(U256::from(7u64))
        }

        async fn get_nonce(&self, at: BlockNumber, _address: Address) -> Result<u64, ChainError> {
            self.note(at);
            Ok(0)
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
