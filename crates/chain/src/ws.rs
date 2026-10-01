//! JSON-RPC over one WebSocket connection (§3.1A).
//!
//! Two measured facts decide what this file is for. `eth_subscribe` answers
//! `-32603 Internal error` for every kind on this provider — and that is a
//! different answer from the `-32601 rpc method is not whitelisted` a blocked
//! method gets, so the subscription call reaches the node and fails there. The
//! upgrade itself does work, but only at the `/ws` path. So a connection is worth
//! having for what it guarantees rather than for what it pushes: one TCP
//! connection pins one backend, which requests through this provider's load
//! balancer do not (see `data/evidence/m5/probe-ws-pending-single-connection.json`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::error::{ChainError, Result};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Sink = futures_util::stream::SplitSink<Socket, Message>;
type Stream = futures_util::stream::SplitStream<Socket>;

/// What the connection layer is doing, in the words §48 asks a session record to
/// carry. A live run that never mentions a disconnect is not honest about one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum ConnStatus {
    Connected {
        attempt: u32,
    },
    Disconnected {
        reason: String,
    },
    Reconnecting {
        attempt: u32,
        backoff_ms: u64,
    },
    /// Retries are exhausted. The caller decides whether that ends the run; this
    /// layer does not quietly keep trying forever.
    Failed {
        reason: String,
        attempts: u32,
    },
    /// Nothing received for longer than the watchdog allows.
    Silent {
        quiet_ms: u64,
    },
}

/// Tunables, all of them here rather than inline, because how long you were
/// willing to wait decides which class a failure belongs to (§53).
#[derive(Clone, Copy, Debug)]
pub struct WsOptions {
    pub request_timeout_ms: u64,
    pub heartbeat_ms: u64,
    /// No frame for this long counts as silence.
    pub watchdog_ms: u64,
    pub max_reconnect_attempts: u32,
    pub backoff_initial_ms: u64,
    pub backoff_max_ms: u64,
}

impl Default for WsOptions {
    fn default() -> Self {
        Self {
            request_timeout_ms: 10_000,
            heartbeat_ms: 15_000,
            watchdog_ms: 45_000,
            max_reconnect_attempts: 8,
            backoff_initial_ms: 250,
            backoff_max_ms: 4_000,
        }
    }
}

struct Outbound {
    id: u64,
    frame: String,
    reply: oneshot::Sender<Result<Value>>,
}

/// One request/response stream over one connection, with heartbeat and reconnect.
///
/// The socket is owned by exactly one task, so no ordering question can arise
/// inside this client: replies are matched by JSON-RPC id, a reply that never
/// arrives is a timeout rather than a hang, and a disconnect fails the requests
/// it was carrying instead of silently re-sending them.
pub struct WsRpcClient {
    url: String,
    outbound: mpsc::UnboundedSender<Outbound>,
    notifications: broadcast::Receiver<Value>,
    status: broadcast::Receiver<ConnStatus>,
    shutdown: mpsc::UnboundedSender<()>,
    next_id: AtomicU64,
    timeout: Duration,
}

impl WsRpcClient {
    /// Connects, then starts the owning task. Failing to connect at all is an
    /// error rather than a retry loop: the caller knows whether the endpoint is
    /// supposed to be up.
    pub async fn connect(url: &str, options: WsOptions) -> Result<Self> {
        let (sink, stream) = connect_now(url).await?.split();
        let (notifications_tx, notifications) = broadcast::channel(512);
        let (status_tx, status) = broadcast::channel(64);
        let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = mpsc::unbounded_channel();
        let _ = status_tx.send(ConnStatus::Connected { attempt: 0 });
        tokio::spawn(
            ConnectionTask {
                url: url.to_owned(),
                options,
                outbound_rx,
                shutdown_rx,
                notifications: notifications_tx,
                status: status_tx,
                pending: HashMap::new(),
            }
            .with_connection(sink, stream),
        );
        Ok(Self {
            url: url.to_owned(),
            outbound: outbound_tx,
            notifications,
            status,
            shutdown: shutdown_tx,
            next_id: AtomicU64::new(1),
            timeout: Duration::from_millis(options.request_timeout_ms),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// `eth_subscription` notifications as they arrive, whichever subscription
    /// they belong to. A subscription id does not survive a reconnect, so the
    /// caller cannot assume this stream is continuous.
    pub fn notifications(&mut self) -> &mut broadcast::Receiver<Value> {
        &mut self.notifications
    }

    pub fn status(&mut self) -> &mut broadcast::Receiver<ConnStatus> {
        &mut self.status
    }

    pub async fn request<P: Serialize>(&mut self, method: &str, params: &P) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
        .to_string();
        let (tx, rx) = oneshot::channel();
        self.outbound
            .send(Outbound {
                id,
                frame,
                reply: tx,
            })
            .map_err(|_| ChainError::Rpc("websocket task is gone".to_string()))?;
        let reply = tokio::time::timeout(self.timeout, rx).await.map_err(|_| {
            ChainError::Rpc(format!(
                "{method} id {id} timed out after {} ms",
                self.timeout.as_millis()
            ))
        })?;
        reply
            .map_err(|_| {
                ChainError::Rpc(format!(
                    "{method} id {id} was carried by a connection that died"
                ))
            })?
            .map_err(|e| ChainError::Rpc(format!("{method}: {e}")))
    }

    /// Ask for a subscription and report honestly whether the provider can do it.
    /// The provider's own words are kept, because "unsupported" is a finding about
    /// this endpoint rather than a design choice.
    pub async fn subscribe(&mut self, kinds: &[&str]) -> SubscribeOutcome {
        let mut tried = Vec::new();
        for kind in kinds {
            match self.request("eth_subscribe", &[kind]).await {
                Ok(value) => {
                    let Some(id) = value.as_str().filter(|s| !s.is_empty()) else {
                        tried.push(SubscriptionAttempt {
                            kind: (*kind).to_owned(),
                            outcome: format!("no subscription id in {value}"),
                        });
                        continue;
                    };
                    return SubscribeOutcome::Subscribed {
                        kind: (*kind).to_owned(),
                        id: id.to_owned(),
                        attempts: tried,
                    };
                }
                Err(error) => tried.push(SubscriptionAttempt {
                    kind: (*kind).to_owned(),
                    outcome: error.to_string(),
                }),
            }
        }
        SubscribeOutcome::Unsupported(tried)
    }

    pub fn close(&self) {
        let _ = self.shutdown.send(());
    }
}

pub struct SubscriptionAttempt {
    pub kind: String,
    pub outcome: String,
}

pub enum SubscribeOutcome {
    Subscribed {
        kind: String,
        id: String,
        /// Kinds that failed before this one worked, if any.
        attempts: Vec<SubscriptionAttempt>,
    },
    /// None of the requested kinds worked: the caller falls back to polling and
    /// records these attempts as the reason.
    Unsupported(Vec<SubscriptionAttempt>),
}

impl SubscribeOutcome {
    pub fn is_subscribed(&self) -> bool {
        matches!(self, Self::Subscribed { .. })
    }

    /// One line for an evidence file: what was asked, what came back.
    pub fn describe(&self) -> String {
        match self {
            Self::Subscribed { kind, id, attempts } => format!(
                "subscribed to {kind} as id {id}, after {} rejected attempt(s)",
                attempts.len()
            ),
            Self::Unsupported(attempts) => attempts
                .iter()
                .map(|a| format!("{} -> {}", a.kind, a.outcome))
                .collect::<Vec<_>>()
                .join("; "),
        }
    }
}

struct ConnectionTask {
    url: String,
    options: WsOptions,
    outbound_rx: mpsc::UnboundedReceiver<Outbound>,
    shutdown_rx: mpsc::UnboundedReceiver<()>,
    notifications: broadcast::Sender<Value>,
    status: broadcast::Sender<ConnStatus>,
    pending: HashMap<u64, oneshot::Sender<Result<Value>>>,
}

impl ConnectionTask {
    /// Spawned handle: run the session loop for as long as the connection can be
    /// re-established.
    async fn with_connection(mut self, mut sink: Sink, mut stream: Stream) {
        let mut attempt: u32 = 0;
        loop {
            let reason = self.session(&mut sink, &mut stream).await;
            self.fail_pending(format!("{reason} (requests in flight were not retried)"));
            if self.shutdown_rx.try_recv().is_ok() {
                return;
            }
            let Some((next_sink, next_stream)) = self.reconnect(reason, &mut attempt).await else {
                return;
            };
            sink = next_sink;
            stream = next_stream;
        }
    }

    /// Returns the reason the connection ended.
    async fn session(&mut self, sink: &mut Sink, stream: &mut Stream) -> String {
        let mut heartbeat =
            tokio::time::interval(Duration::from_millis(self.options.heartbeat_ms.max(1)));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_frame = tokio::time::Instant::now();
        loop {
            tokio::select! {
                _ = self.shutdown_rx.recv() => {
                    let _ = sink.close().await;
                    return "closed on request".to_string();
                }
                out = self.outbound_rx.recv() => {
                    let Some(out) = out else {
                        return "client dropped".to_string();
                    };
                    let Outbound { id, frame, reply } = out;
                    if let Err(error) = sink.send(Message::Text(frame.into())).await {
                        let _ = reply.send(Err(ChainError::Rpc(format!("send failed: {error}"))));
                        return format!("send failed: {error}");
                    }
                    self.pending.insert(id, reply);
                }
                _ = heartbeat.tick() => {
                    let quiet_ms = last_frame.elapsed().as_millis() as u64;
                    if quiet_ms > self.options.watchdog_ms {
                        let _ = self.status.send(ConnStatus::Silent { quiet_ms });
                        return format!("no frame for {quiet_ms} ms");
                    }
                    if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                        return "heartbeat send failed".to_string();
                    }
                }
                inb = stream.next() => {
                    let frame = match inb {
                        Some(Ok(frame)) => frame,
                        Some(Err(error)) => return format!("read failed: {error}"),
                        None => return "connection closed by peer".to_string(),
                    };
                    last_frame = tokio::time::Instant::now();
                    match frame {
                        Message::Text(text) => self.deliver(text.as_str()),
                        Message::Ping(payload) => {
                            if sink.send(Message::Pong(payload)).await.is_err() {
                                return "pong send failed".to_string();
                            }
                        }
                        Message::Close(reason) => {
                            return format!(
                                "close frame: {}",
                                reason.map(|c| c.to_string()).unwrap_or_default()
                            );
                        }
                        // Message is non-exhaustive: a frame kind this version
                        // does not name is still a reason to stay connected.
                        _ => {}
                    }
                }
            }
        }
    }

    /// A reply for an id goes to its waiter; anything else is kept only if it is
    /// a subscription notification. An unparseable frame is reported, not
    /// swallowed (§52: nothing here panics and nothing here is silently dropped).
    fn deliver(&mut self, text: &str) {
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(error) => {
                let _ = self.notifications.send(json!({
                    "unknown_frame": {"reason": error.to_string(), "frame": clip(text, 512)},
                }));
                return;
            }
        };
        if let Some(id) = value.get("id").and_then(Value::as_u64) {
            if let Some(reply) = self.pending.remove(&id) {
                let result = match value.get("error") {
                    Some(error) => Err(ChainError::RpcRejected(error.to_string())),
                    None => value
                        .get("result")
                        .cloned()
                        .ok_or_else(|| ChainError::Decode("frame carries no result".to_string())),
                };
                let _ = reply.send(result);
                return;
            }
        }
        if value.get("method").and_then(Value::as_str) == Some("eth_subscription") {
            let _ = self.notifications.send(value);
        }
    }

    async fn reconnect(&mut self, reason: String, attempt: &mut u32) -> Option<(Sink, Stream)> {
        *attempt = 0;
        loop {
            *attempt += 1;
            if *attempt > self.options.max_reconnect_attempts {
                let _ = self.status.send(ConnStatus::Failed {
                    reason,
                    attempts: *attempt - 1,
                });
                return None;
            }
            let backoff = (self.options.backoff_initial_ms * 2u64.pow(*attempt - 1))
                .min(self.options.backoff_max_ms);
            let _ = self.status.send(ConnStatus::Reconnecting {
                attempt: *attempt,
                backoff_ms: backoff,
            });
            tokio::time::sleep(Duration::from_millis(backoff)).await;
            if self.shutdown_rx.try_recv().is_ok() {
                return None;
            }
            match connect_now(&self.url).await {
                Ok(socket) => {
                    let _ = self
                        .status
                        .send(ConnStatus::Connected { attempt: *attempt });
                    *attempt = 0;
                    let (sink, stream) = socket.split();
                    return Some((sink, stream));
                }
                Err(error) => {
                    let _ = self.status.send(ConnStatus::Disconnected {
                        reason: error.to_string(),
                    });
                }
            }
        }
    }

    fn fail_pending(&mut self, reason: String) {
        for (_, reply) in self.pending.drain() {
            let _ = reply.send(Err(ChainError::Rpc(reason.clone())));
        }
    }
}

async fn connect_now(url: &str) -> Result<Socket> {
    let (socket, _response) = connect_async(url)
        .await
        .map_err(|e| ChainError::Rpc(format!("websocket connect {url}: {e}")))?;
    Ok(socket)
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    //! The connection layer's own rules, tested without a socket: which frame
    //! counts as an answer, which counts as a rejection, and which is a frame this
    //! layer cannot name (§3.1A/B/C, §52).

    use super::*;
    use tokio::sync::broadcast::error::TryRecvError;

    fn task() -> (ConnectionTask, broadcast::Receiver<Value>) {
        let (notifications_tx, notifications) = broadcast::channel(8);
        let (status_tx, _status) = broadcast::channel(8);
        let (_outbound_tx, outbound_rx) = mpsc::unbounded_channel();
        let (_shutdown_tx, shutdown_rx) = mpsc::unbounded_channel();
        (
            ConnectionTask {
                url: "ws://test/ws".to_string(),
                options: WsOptions::default(),
                outbound_rx,
                shutdown_rx,
                notifications: notifications_tx,
                status: status_tx,
                pending: HashMap::new(),
            },
            notifications,
        )
    }

    #[tokio::test]
    async fn a_rejected_request_answers_with_the_providers_own_words() {
        // The measured `-32603` for `eth_subscribe` has to survive as *text* the
        // report can quote. A generic "request failed" would let a BLOCKED
        // capability look like a design choice (§73).
        let (mut task, _notifications) = task();
        let (reply, waiter) = oneshot::channel();
        task.pending.insert(7, reply);
        task.deliver(
            r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32603,"message":"eth_subscribe blocked"}}"#,
        );
        let answer = waiter.await.expect("the waiter was answered");
        match answer {
            Err(ChainError::RpcRejected(text)) => {
                assert!(text.contains("-32603"), "{text}");
                assert!(text.contains("eth_subscribe"), "{text}");
            }
            other => panic!("an error payload is a rejection, got {other:?}"),
        }
        assert!(
            task.pending.is_empty(),
            "the request was retired, not left hanging"
        );
    }

    #[tokio::test]
    async fn a_successful_reply_goes_to_the_request_that_asked_for_it() {
        let (mut task, _notifications) = task();
        let (reply, waiter) = oneshot::channel();
        task.pending.insert(9, reply);
        task.deliver(r#"{"jsonrpc":"2.0","id":9,"result":"0x23b1a41"}"#);
        let answer = waiter
            .await
            .expect("answered")
            .expect("a result frame is a success");
        assert_eq!(answer, json!("0x23b1a41"));
    }

    #[tokio::test]
    async fn a_reply_with_no_result_is_an_error_not_a_timeout() {
        // Distinguishing "the provider answered nothing meaningful" from "the
        // provider did not answer" is what §53's classes are for.
        let (mut task, _notifications) = task();
        let (reply, waiter) = oneshot::channel();
        task.pending.insert(11, reply);
        task.deliver(r#"{"jsonrpc":"2.0","id":11}"#);
        match waiter.await.expect("answered") {
            Err(ChainError::Decode(text)) => assert!(text.contains("no result"), "{text}"),
            other => panic!("expected a decode error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_frame_that_is_not_json_is_reported_rather_than_dropped() {
        // §52: an unnamed frame is an event with a reason in it, not a silence.
        let (mut task, mut notifications) = task();
        task.deliver("upstream said this without braces");
        let forwarded = notifications
            .recv()
            .await
            .expect("the unparsable frame is forwarded");
        assert!(forwarded["unknown_frame"]["reason"].is_string());
        assert_eq!(
            forwarded["unknown_frame"]["frame"],
            "upstream said this without braces"
        );
    }

    #[tokio::test]
    async fn only_subscription_notifications_reach_the_consumer() {
        let (mut task, mut notifications) = task();
        task.deliver(
            r#"{"jsonrpc":"2.0","method":"eth_subscription","params":{"result":{"number":"0x10"}}}"#,
        );
        task.deliver(r#"{"jsonrpc":"2.0","id":404,"result":"ignored"}"#);
        let forwarded = notifications
            .recv()
            .await
            .expect("the subscription frame is forwarded");
        assert_eq!(forwarded["method"], "eth_subscription");
        assert_eq!(forwarded["params"]["result"]["number"], "0x10");
        assert!(
            matches!(
                notifications.try_recv(),
                Err(TryRecvError::Empty) | Err(TryRecvError::Lagged(_))
            ),
            "a reply to a request nobody made is not a market event"
        );
    }

    #[test]
    fn the_heartbeat_is_early_enough_to_be_an_answer_and_late_enough_to_be_a_ping() {
        // The watchdog classifies a connection as silent on the *heartbeat*
        // schedule, so a ping that fires after the deadline can never be the thing
        // that proves the link is alive.
        let options = WsOptions::default();
        assert!(
            options.heartbeat_ms < options.watchdog_ms,
            "a ping scheduled after the silence deadline is not a heartbeat"
        );
        assert!(
            options.request_timeout_ms <= options.watchdog_ms,
            "a request that outlives the watchdog is decided by the watchdog, not twice"
        );
        assert!(options.max_reconnect_attempts > 0);
        assert!(options.backoff_initial_ms <= options.backoff_max_ms);
    }

    #[test]
    fn an_unsupported_subscription_names_every_kind_and_the_answer_each_got() {
        // This is the shape §73 asks for when a provider cannot push: every kind
        // tried, and the provider's own words for each.
        let outcome = SubscribeOutcome::Unsupported(vec![
            SubscriptionAttempt {
                kind: "newHeads".to_string(),
                outcome: "rpc returned an error payload: {\"code\":-32603}".to_string(),
            },
            SubscriptionAttempt {
                kind: "newBlockHeaders".to_string(),
                outcome: "rpc returned an error payload: {\"code\":-32603}".to_string(),
            },
        ]);
        assert!(!outcome.is_subscribed());
        let described = outcome.describe();
        for kind in ["newHeads", "newBlockHeaders"] {
            assert!(described.contains(kind), "{described}");
        }
        assert_eq!(described.split("; ").count(), 2, "{described}");
        assert!(described.contains("-32603"), "{described}");
    }

    #[test]
    fn a_subscribed_answer_says_what_it_had_to_get_through_first() {
        let outcome = SubscribeOutcome::Subscribed {
            kind: "newHeads".to_string(),
            id: "0x6a2f".to_string(),
            attempts: vec![SubscriptionAttempt {
                kind: "logs".to_string(),
                outcome: "no subscription id in null".to_string(),
            }],
        };
        assert!(outcome.is_subscribed());
        let described = outcome.describe();
        assert!(described.contains("newHeads"), "{described}");
        assert!(described.contains("0x6a2f"), "{described}");
        assert!(described.contains("1 rejected attempt"), "{described}");
    }
}
