//! Reading *which canonical blocks exist*, from three kinds of provider, through
//! one trait.
//!
//! This is the seam that lets §9 and §32 be satisfied by construction. A live
//! endpoint and a recorded directory both answer `head()` and `block_at()`;
//! everything above them — ordering, the state engine, the graph — sees one
//! shape and cannot tell which it is talking to. A source that could not be
//! written this way would be a second pipeline, which is what §9 forbids.
//!
//! `Ok(None)` from [`HeadReader::block_at`] is a fact about the chain (this
//! number is not here yet), not a failure of the read. Collapsing the two would
//! turn a provider outage into a gap, and a gap into an outage.

use alloy_primitives::B256;
use async_trait::async_trait;
use serde_json::{json, Value};

use evm_core::{BlockNumber, ChainId};

use crate::adapter::ChainAdapter as _;
use crate::error::{ChainError, Result};
use crate::recorded::RecordedChainAdapter;
use crate::rpc::{chain_block_from_value, HttpChainAdapter};
use crate::types::ChainBlock;
use crate::ws::WsRpcClient;

/// Which blocks a provider will hand over, and their headers.
#[async_trait]
pub trait HeadReader: Send {
    /// The transport, for the evidence line (§41) that has to say which provider
    /// answer a claim rests on.
    fn transport(&self) -> &'static str;

    /// The highest canonical block this provider currently claims.
    async fn head(&mut self) -> Result<BlockNumber>;

    /// Whether this transport's supply of blocks has an end.
    ///
    /// A node does not: the next block has not happened yet, and a cycle that
    /// produced nothing means "wait". A recording does, and the difference is
    /// what lets a replay run stop by itself instead of being timed out.
    fn is_finite(&self) -> bool {
        false
    }

    /// A header by number. `None` means the provider does not have that block.
    async fn block_at(&mut self, number: BlockNumber) -> Result<Option<ChainBlock>>;

    /// The header of the block a `pending` read would describe, if this transport
    /// answers `pending` at all. Only the flashblock source uses it, and only to
    /// observe a candidate — never to advance state (§27).
    async fn pending_header(&mut self) -> Result<Option<ChainBlock>> {
        Ok(None)
    }

    /// The `pending` header exactly as the provider returned it, with its
    /// transactions as *hashes only* (`eth_getBlockByNumber(["pending", false])`).
    ///
    /// Only the candidate source uses it, and only because a pending block's
    /// *shape* is the observation (§30): which number, which hash, how many
    /// transactions and how much gas it has accumulated between two reads. A
    /// normalized [`ChainBlock`] would drop the two fields the claim rests on.
    async fn pending_raw(&mut self) -> Result<Option<Value>> {
        Ok(None)
    }

    /// The same `pending` block with its transactions spelled out
    /// (`eth_getBlockByNumber(["pending", true])`).
    ///
    /// A second read rather than a bolder default, because the two consumers need
    /// opposite payloads. §30's fact is how *many* transactions a pending view holds,
    /// which `full: false` answers; the preconfirmation radar's fact is *which
    /// contracts* moved, which a list of hashes cannot name — it refuses such a frame
    /// outright. Measured on the committed M9.4 capture, where every transaction
    /// entry of both windows is a full object: a pending read costs about seven times
    /// the bytes of the same read with bare hashes. The per-window totals and the
    /// integer ratio are in
    /// `data/evidence/m12/b/pending-shape-compat.json` (`size_stats`), not here, so
    /// this line cannot drift from them. Making the shared read heavy to serve the
    /// radar would tax the candidate observer for bytes it never opens.
    ///
    /// A transport that has not been shown to answer this shape returns `Err` rather
    /// than the light payload. That is deliberate: falling back to `full: false` would
    /// let every frame arrive as a hash list, so the radar would emit nothing but
    /// refusals while the transport looked healthy.
    async fn pending_full_transactions(&mut self) -> Result<Option<Value>> {
        Err(ChainError::MissingData(format!(
            "{} transport has not been shown to answer eth_getBlockByNumber([\"pending\", true])",
            self.transport()
        )))
    }

    /// Ask whether a candidate hash is readable at all — the question §73 has to
    /// answer before a candidate could ever become state. The provider's answer
    /// comes back as given: `Value::Null` is "there is no block at this hash",
    /// which is an answer, not a transport failure.
    async fn candidate_read(&mut self, hash: B256) -> Result<Value> {
        Err(ChainError::MissingData(format!(
            "{} transport does not address a candidate by {hash:#x}",
            self.transport()
        )))
    }

    /// Blocks the provider volunteered without being asked — a subscription
    /// notification, if this transport has one. A hint may only ever widen the
    /// range of numbers the ingestion owes; the block itself is still read
    /// through [`HeadReader::block_at`] in chain order (§4).
    fn take_hints(&mut self) -> Vec<BlockNumber> {
        Vec::new()
    }

    /// Notification frames this transport received that it could not name.
    /// Counted rather than dropped (§52).
    fn unknown_hints(&self) -> u64 {
        0
    }
}

fn hex_number(value: &Value) -> Result<u64> {
    let text = value
        .as_str()
        .ok_or_else(|| ChainError::Decode(format!("block number is not a hex string: {value}")))?;
    u64::from_str_radix(text.strip_prefix("0x").unwrap_or(text), 16)
        .map_err(|e| ChainError::Decode(format!("block number `{text}`: {e}")))
}

fn hex_param(number: BlockNumber) -> String {
    format!("0x{:x}", number.0)
}

#[async_trait]
impl HeadReader for HttpChainAdapter {
    fn transport(&self) -> &'static str {
        "http"
    }

    async fn head(&mut self) -> Result<BlockNumber> {
        self.latest_block().await
    }

    async fn block_at(&mut self, number: BlockNumber) -> Result<Option<ChainBlock>> {
        match self.get_block(number).await {
            Ok(block) => Ok(Some(block)),
            // The adapter's own words for "this provider answered null".
            Err(ChainError::MissingData(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn pending_header(&mut self) -> Result<Option<ChainBlock>> {
        let raw = self.pending_raw().await?;
        match raw {
            Some(raw) => Ok(Some(chain_block_from_value(self.chain_id(), &raw)?)),
            None => Ok(None),
        }
    }

    async fn pending_raw(&mut self) -> Result<Option<Value>> {
        let raw = self
            .request_raw("eth_getBlockByNumber", json!(["pending", false]))
            .await?;
        Ok(if raw.is_null() { None } else { Some(raw) })
    }

    async fn pending_full_transactions(&mut self) -> Result<Option<Value>> {
        let raw = self
            .request_raw("eth_getBlockByNumber", json!(["pending", true]))
            .await?;
        Ok(if raw.is_null() { None } else { Some(raw) })
    }

    async fn candidate_read(&mut self, hash: B256) -> Result<Value> {
        self.request_raw("eth_getBlockByHash", json!([format!("{hash:#x}"), false]))
            .await
    }
}

/// The same reads over one WebSocket connection.
///
/// Worth the extra type: a pinned connection is the only way measured on this
/// provider to be sure two reads came from the same node, and §6's gap recovery
/// is meaningless if the answer to "is block N there?" comes from a different
/// backend than the answer to "what is the head?".
pub struct WsHeadReader {
    client: WsRpcClient,
    chain_id: ChainId,
    unknown_hints: u64,
}

impl WsHeadReader {
    pub async fn connect(url: &str) -> Result<Self> {
        let mut client = WsRpcClient::connect(url, crate::ws::WsOptions::default()).await?;
        let raw = client.request("eth_chainId", &Vec::<String>::new()).await?;
        let chain_id = ChainId(hex_number(&raw)?);
        Ok(Self {
            client,
            chain_id,
            unknown_hints: 0,
        })
    }

    pub const fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    pub fn client(&mut self) -> &mut WsRpcClient {
        &mut self.client
    }

    async fn header(&mut self, tag: &str) -> Result<Option<ChainBlock>> {
        let raw = self
            .client
            .request("eth_getBlockByNumber", &json!([tag, false]))
            .await?;
        if raw.is_null() {
            return Ok(None);
        }
        Ok(Some(chain_block_from_value(self.chain_id, &raw)?))
    }
}

#[async_trait]
impl HeadReader for WsHeadReader {
    fn transport(&self) -> &'static str {
        "websocket"
    }

    async fn head(&mut self) -> Result<BlockNumber> {
        let raw = self
            .client
            .request("eth_blockNumber", &Vec::<String>::new())
            .await?;
        Ok(BlockNumber(hex_number(&raw)?))
    }

    async fn block_at(&mut self, number: BlockNumber) -> Result<Option<ChainBlock>> {
        self.header(&hex_param(number)).await
    }

    async fn pending_header(&mut self) -> Result<Option<ChainBlock>> {
        self.header("pending").await
    }

    /// `["pending", false]`, and deliberately no [`HeadReader::pending_full_transactions`]
    /// override: every pending capture this repository holds was taken over HTTP, so the
    /// trait's fail-closed default is the accurate answer for this transport.
    async fn pending_raw(&mut self) -> Result<Option<Value>> {
        let raw = self
            .client
            .request("eth_getBlockByNumber", &json!(["pending", false]))
            .await?;
        Ok(if raw.is_null() { None } else { Some(raw) })
    }

    async fn candidate_read(&mut self, hash: B256) -> Result<Value> {
        self.client
            .request("eth_getBlockByHash", &json!([format!("{hash:#x}"), false]))
            .await
    }

    /// Drain whatever the subscription pushed since the last cycle.
    ///
    /// A notification's own header is used only for its number: the block is read
    /// back through `block_at`, so a pushed payload can never become a second,
    /// unverified source of block facts.
    fn take_hints(&mut self) -> Vec<BlockNumber> {
        let mut hints = Vec::new();
        loop {
            match self.client.notifications().try_recv() {
                Ok(value) => {
                    let number = value
                        .pointer("/params/result/number")
                        .and_then(|v| hex_number(v).ok())
                        .or_else(|| value.get("result").and_then(|v| hex_number(v).ok()));
                    match number {
                        Some(number) => hints.push(BlockNumber(number)),
                        None => {
                            if value.get("unknown_frame").is_none() {
                                // A notification for something else, or a payload
                                // with no parsable number. Counted, not dropped.
                                self.unknown_hints += 1;
                            }
                        }
                    }
                }
                Err(_) => return hints,
            }
        }
    }

    fn unknown_hints(&self) -> u64 {
        self.unknown_hints
    }
}

/// Recorded blocks answering the live trait. The point is not convenience: it is
/// that the parity acceptance in §32 cannot be satisfied by a mock, because the
/// replay side runs the identical ingestion code against a directory of real
/// recordings.
#[async_trait]
impl HeadReader for RecordedChainAdapter {
    fn transport(&self) -> &'static str {
        "recorded"
    }

    /// A directory has a last block, and nothing above it will ever arrive. That
    /// is what lets a replay run end by itself instead of being timed out.
    fn is_finite(&self) -> bool {
        true
    }

    async fn head(&mut self) -> Result<BlockNumber> {
        self.available_blocks().into_iter().max().ok_or_else(|| {
            ChainError::MissingData("recorded directory holds no blocks".to_string())
        })
    }

    async fn block_at(&mut self, number: BlockNumber) -> Result<Option<ChainBlock>> {
        match self.get_block(number).await {
            Ok(block) => Ok(Some(block)),
            Err(ChainError::MissingData(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }
}
