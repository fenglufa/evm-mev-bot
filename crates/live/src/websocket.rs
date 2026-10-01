//! The §3.1A source: one connection, subscription first if the provider can
//! manage it, polling over that same connection if it cannot.
//!
//! What this file does *not* do is present a fallback as a feature. The
//! subscription attempt is made, the provider's own words are captured, and
//! `capability()` carries them into the report — which is the difference between
//! §31's "state BLOCKED honestly" and a claim that flashblocks-style push
//! ingestion exists here.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::mpsc;

use evm_chain::WsHeadReader;
use evm_core::{BlockNumber, ChainId};

use crate::error::{LiveError, LiveResult};
use crate::event::{MarketEvent, SourceKind, SourceStatus};
use crate::source::{MarketDataSource, RunReport, SourceConfig};

pub struct WebSocketSource {
    inner: crate::source::PollingSource<WsHeadReader>,
    /// Learned from `eth_chainId` at connect, never from a config file (§45: the
    /// chain identity is a fact about the endpoint, not an assumption).
    chain_id: ChainId,
    /// Filled in by the attempt made inside `run`, because a subscription can
    /// only be probed against a live connection.
    subscription: Option<serde_json::Value>,
}

impl WebSocketSource {
    /// Connects eagerly: a run should fail at once on a bad URL rather than
    /// after the state engine has been built.
    pub async fn connect(url: &str, config: SourceConfig) -> LiveResult<Self> {
        let reader = WsHeadReader::connect(url)
            .await
            .map_err(|e| LiveError::Connection(format!("{url}: {e}")))?;
        let chain_id = reader.chain_id();
        let inner =
            crate::source::PollingSource::new(reader, chain_id, SourceKind::WebSocket, config);
        Ok(Self {
            inner,
            chain_id,
            subscription: None,
        })
    }

    pub const fn chain_id(&self) -> ChainId {
        self.chain_id
    }
}

#[async_trait]
impl MarketDataSource for WebSocketSource {
    fn kind(&self) -> SourceKind {
        SourceKind::WebSocket
    }

    async fn head(&mut self) -> LiveResult<BlockNumber> {
        self.inner.head().await
    }

    async fn run(
        &mut self,
        start_after: BlockNumber,
        sink: mpsc::Sender<MarketEvent>,
        stop: Arc<AtomicBool>,
    ) -> LiveResult<RunReport> {
        let outcome = self
            .inner
            .reader()
            .client()
            .subscribe(&["newHeads", "newBlockHeaders"])
            .await;
        let described = outcome.describe();
        let subscribed = outcome.is_subscribed();
        self.subscription = Some(json!({
            "attempted": ["newHeads", "newBlockHeaders"],
            "subscribed": subscribed,
            "provider_answer": described,
            "consequence": if subscribed {
                "notifications advance the known head; blocks are still read in order"
            } else {
                "polling eth_blockNumber over the same connection; §73 records this as BLOCKED"
            },
        }));
        sink.send(MarketEvent::Status(SourceStatus::Subscription {
            source: SourceKind::WebSocket,
            outcome: described,
        }))
        .await
        .map_err(|_| LiveError::QueueClosed)?;
        self.inner.run(start_after, sink, stop).await
    }

    fn capability(&self) -> serde_json::Value {
        json!({
            "transport": "websocket",
            "probed": true,
            "subscription": self.subscription,
        })
    }

    fn report(&self) -> Option<RunReport> {
        self.inner.report()
    }
}
