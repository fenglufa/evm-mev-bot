use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use alloy_primitives::{Address, Bytes, B256, U256};
use async_trait::async_trait;
use serde_json::{json, Value};

use evm_core::{BlockNumber, ChainId, LogIndex, TxHash, TxIndex};

use crate::adapter::ChainAdapter;
use crate::error::{ChainError, Result};
use crate::rpc_trace::{
    bounded_detail, describe_call, RpcAttempt, RpcCallEvent, RpcTraceSink, CLASS_DECODE_FAILED,
    CLASS_HTTP_STATUS, CLASS_NODE_REJECTED, CLASS_NON_JSON, CLASS_OK, CLASS_SEND_FAILED,
    RPC_TRACE_SCHEMA,
};
use crate::types::{
    BlockContext, BlockData, CallRequest, ChainBlock, ChainLog, ChainReceipt, ChainTransaction,
    LogFilter,
};

/// JSON-RPC over HTTP. The only place in the codebase that knows the wire
/// format of a provider response.
///
/// `Clone` shares the [`reqwest::Client`], and with it the connection pool: a
/// caller that needs this adapter both as a [`ChainAdapter`] and as a reader in
/// another task gets two handles to one endpoint, not two endpoints (§62).
///
/// `trace` is M8.2's observation handle and nothing else. Every adapter built by
/// [`HttpChainAdapter::connect`] holds `None`, and every recording branch below sits
/// behind that `None`, so an untraced run makes the same requests, in the same order,
/// with the same single retry, as it did before the field existed (§19).
#[derive(Clone)]
pub struct HttpChainAdapter {
    http: reqwest::Client,
    url: String,
    chain_id: ChainId,
    trace: Option<RpcTraceSink>,
}

/// What one HTTP attempt ended with.
///
/// A logical call can hold two of these, because the retry loop below holds two
/// tries. The split matters for §25: a 20 s call that was one slow answer and a 20 s
/// call that was a timed-out connection plus a second request are different
/// bottlenecks, and only the attempt list can tell them apart.
enum Attempt {
    /// The node answered — including the case of answering with a JSON-RPC error,
    /// which is a response and not a transport failure.
    Answered(Value),
    /// The attempt did not produce a result. `terminal` marks the two failures the
    /// loop has always returned on immediately (a node rejection, a payload with no
    /// result) as distinct from the three it retries, so wrapping the loop in a
    /// recorder cannot change when a second try happens.
    Failed {
        class: &'static str,
        detail: String,
        terminal: bool,
    },
}

/// Turn a failed attempt back into the error this module has always returned.
///
/// Keyed on the class rather than carried alongside it because the class *is* the
/// distinction the error types already draw: `Rpc` for what never answered,
/// `RpcRejected` for what answered "no", `Decode` for an answer with no value in it.
fn wire_error(class: &'static str, detail: String) -> ChainError {
    match class {
        CLASS_NODE_REJECTED => ChainError::RpcRejected(detail),
        CLASS_DECODE_FAILED => ChainError::Decode(detail),
        _ => ChainError::Rpc(detail),
    }
}

impl HttpChainAdapter {
    pub async fn connect(url: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        // This call is what *learns* the chain id, so it cannot be described with one:
        // it is recorded, when traced, against `chain-unknown` rather than a number
        // this adapter does not hold yet.
        let raw = Self::request_with(&http, url, "eth_chainId", json!([]), None, None).await?;
        let chain_id = ChainId(parse_u64(&raw, "eth_chainId")?);
        Ok(Self {
            http,
            url: url.to_owned(),
            chain_id,
            trace: None,
        })
    }

    /// This adapter again, with its calls recorded into `sink`.
    ///
    /// The clone keeps `self.http`, so a traced run talks to the same endpoint through
    /// the same connection pool as the untraced run it was derived from — this adds a
    /// listener, not a second client (§62). The sink learns the endpoint here so a
    /// transport error, which quotes the URL it was given, can be scrubbed before it
    /// is stored.
    pub fn with_rpc_trace(&self, sink: RpcTraceSink) -> Self {
        let mut traced = self.clone();
        traced.trace = Some(sink.with_endpoint(&self.url));
        traced
    }

    /// Whether this adapter's calls are being recorded.
    ///
    /// A caller that wants to report "this source cannot be traced" — which is not the
    /// same statement as "this source made no calls" — asks this rather than inferring
    /// it from an empty event list.
    pub fn is_rpc_traced(&self) -> bool {
        self.trace.is_some()
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_traced(method, params).await
    }

    /// The same request, for a caller that needs the raw provider JSON rather
    /// than a normalized type. Only used to keep one header-parsing path between
    /// the HTTP adapter and any other transport (see [`crate::head`]).
    pub async fn request_raw(&self, method: &str, params: Value) -> Result<Value> {
        self.request_traced(method, params).await
    }

    /// One call, recorded into this adapter's sink when it has one.
    ///
    /// Every state read in the repository reaches the wire through here, which is
    /// why the instrumentation sits at this layer rather than in a provider: a caller
    /// above the adapter asks for a `U256` and never says which method that costs, so
    /// a record taken there would be a guess about the wire in exactly the direction
    /// §5 forbids. §18 falls out of the same shape — this function already holds the
    /// method and the params, and asks the node for nothing further.
    async fn request_traced(&self, method: &str, params: Value) -> Result<Value> {
        let chain_id = self.chain_id.0;
        Self::request_with(
            &self.http,
            &self.url,
            method,
            params,
            Some(chain_id),
            self.trace.as_ref(),
        )
        .await
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Send one JSON-RPC request, optionally recording it.
    ///
    /// The last two arguments are the whole of M8.2's hook-up; with both `None` this
    /// is the function this repository had before it, request for request.
    async fn request_with(
        http: &reqwest::Client,
        url: &str,
        method: &str,
        params: Value,
        chain_id: Option<u64>,
        trace: Option<&RpcTraceSink>,
    ) -> Result<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        // Read off the params that are about to go out, so §12's key describes what the
        // node is actually asked for rather than what a caller says it asked for.
        let description = trace.map(|_| describe_call(method, &body["params"], chain_id));
        let rpc_id = trace.map(|sink| sink.next_rpc_id());
        let started_ns = trace.map(|sink| sink.mark(Instant::now()));
        let mut attempts: Vec<RpcAttempt> = Vec::new();
        let mut answer: Option<Value> = None;
        let mut failure: Option<(&'static str, String)> = None;
        // A single retry: transient 5xx / connection resets are common on public nodes.
        for _ in 0..2 {
            let attempt_started_ns = trace.map(|sink| sink.mark(Instant::now()));
            let outcome = Self::one_attempt(http, url, &body).await;
            if let (Some(sink), Some(attempt_started)) = (trace, attempt_started_ns) {
                let attempt_finished_ns = sink.mark(Instant::now());
                attempts.push(RpcAttempt {
                    started_ns: attempt_started,
                    finished_ns: attempt_finished_ns,
                    duration_ns: attempt_finished_ns.saturating_sub(attempt_started),
                    outcome: match &outcome {
                        Attempt::Answered(_) => CLASS_OK,
                        Attempt::Failed { class, .. } => class,
                    },
                });
            }
            match outcome {
                Attempt::Answered(value) => {
                    answer = Some(value);
                    break;
                }
                Attempt::Failed {
                    class,
                    detail,
                    terminal,
                } => {
                    failure = Some((class, detail));
                    if terminal {
                        break;
                    }
                }
            }
        }

        // The call as a whole, in the two forms the rest of this function needs: an
        // error to return and a class plus message to record. Derived from `answer`
        // first, because a retry that lands after a failure is a success.
        let (error_class, error_detail) = match (&answer, &failure) {
            (Some(_), _) => (None, None),
            (None, Some((class, detail))) => (Some(*class), Some(bounded_detail(detail))),
            // The loop's own floor, kept from the code before this function could
            // record: two tries ran and neither reported a class. It says so as a
            // send failure, which is the only thing it can honestly mean.
            (None, None) => (Some(CLASS_SEND_FAILED), Some("request failed".to_string())),
        };

        if let (Some(sink), Some(rpc_id), Some(started_ns)) = (trace, rpc_id, started_ns) {
            let finished_ns = sink.mark(Instant::now());
            let (block, target, slot, dedup_key, key_note) = match description {
                Some(seen) => (
                    seen.block,
                    seen.target,
                    seen.slot,
                    seen.dedup_key,
                    seen.key_note,
                ),
                None => (None, None, None, None, None),
            };
            sink.record(RpcCallEvent {
                trace_schema: RPC_TRACE_SCHEMA,
                rpc_id,
                method: method.to_owned(),
                block,
                target,
                slot,
                started_ns,
                finished_ns,
                duration_ns: finished_ns.saturating_sub(started_ns),
                success: answer.is_some(),
                error_class,
                error_detail,
                attempts,
                dedup_key,
                key_note,
            });
        }

        match answer {
            Some(value) => Ok(value),
            None => Err(match failure {
                Some((class, detail)) => wire_error(class, detail),
                None => ChainError::Rpc("request failed".to_string()),
            }),
        }
    }

    /// One POST and its reading: the attempt-level half of
    /// [`HttpChainAdapter::request_with`], so the loop above only has to decide what to
    /// do with a class, not how to obtain one.
    async fn one_attempt(http: &reqwest::Client, url: &str, body: &Value) -> Attempt {
        let response = match http.post(url).json(body).send().await {
            Ok(response) => response,
            Err(error) => {
                return Attempt::Failed {
                    class: CLASS_SEND_FAILED,
                    detail: error.to_string(),
                    terminal: false,
                }
            }
        };
        let status = response.status();
        let parsed: Value = match response.json().await {
            Ok(value) => value,
            Err(error) => {
                return Attempt::Failed {
                    class: CLASS_NON_JSON,
                    detail: format!("non-json response (status {status}): {error}"),
                    terminal: false,
                }
            }
        };
        if !status.is_success() {
            return Attempt::Failed {
                class: CLASS_HTTP_STATUS,
                detail: format!("http status {status}: {parsed}"),
                terminal: false,
            };
        }
        // A JSON-RPC error payload is the node answering, so it ends the call the way
        // it always did — returned, not retried.
        if let Some(error) = parsed.get("error") {
            return Attempt::Failed {
                class: CLASS_NODE_REJECTED,
                detail: error.to_string(),
                terminal: true,
            };
        }
        match parsed.get("result") {
            Some(result) => Attempt::Answered(result.clone()),
            None => Attempt::Failed {
                class: CLASS_DECODE_FAILED,
                detail: "response has no result".to_string(),
                terminal: true,
            },
        }
    }
}

/// The provider's hex-quantity and hex-bytes field rules.
///
/// Public for the same reason [`chain_block_from_value`] is: a reader of a single
/// transaction receipt (`crates/execution`) needs fields a block-shaped adapter never
/// asked for, and a second copy of these rules would be a second definition of what a
/// malformed quantity means on this repository's only chain interface.
pub fn parse_u64(value: &Value, context: &str) -> Result<u64> {
    let text = value
        .as_str()
        .ok_or_else(|| ChainError::Decode(format!("{context} is not a hex string: {value}")))?;
    u64::from_str_radix(text.strip_prefix("0x").unwrap_or(text), 16)
        .map_err(|e| ChainError::Decode(format!("{context} `{text}`: {e}")))
}

pub fn parse_address(value: &Value, context: &str) -> Result<Address> {
    let text = value
        .as_str()
        .ok_or_else(|| ChainError::Decode(format!("{context} is not an address: {value}")))?;
    Address::from_str(text).map_err(|e| ChainError::Decode(format!("{context} `{text}`: {e}")))
}

pub fn parse_b256(value: &Value, context: &str) -> Result<B256> {
    let text = value
        .as_str()
        .ok_or_else(|| ChainError::Decode(format!("{context} is not a hash: {value}")))?;
    B256::from_str(text).map_err(|e| ChainError::Decode(format!("{context} `{text}`: {e}")))
}

pub fn parse_bytes(value: &Value, context: &str) -> Result<Bytes> {
    let text = value
        .as_str()
        .ok_or_else(|| ChainError::Decode(format!("{context} is not bytes: {value}")))?;
    Bytes::from_str(text).map_err(|e| ChainError::Decode(format!("{context} `{text}`: {e}")))
}

pub fn parse_u256(value: &Value, context: &str) -> Result<U256> {
    let text = value
        .as_str()
        .ok_or_else(|| ChainError::Decode(format!("{context} is not a quantity: {value}")))?;
    U256::from_str_radix(text.strip_prefix("0x").unwrap_or(text), 16)
        .map_err(|e| ChainError::Decode(format!("{context} `{text}`: {e}")))
}

fn block_param(number: BlockNumber) -> String {
    format!("{:#x}", number.0)
}

/// A block header from a provider's own JSON.
///
/// One function rather than two copies, so a header read over HTTP, a header read
/// over one pinned WebSocket connection, and a header read out of a recording
/// normalize identically — which is the §9 requirement that Live and Replay share
/// one state semantics, applied one layer below state.
pub fn chain_block_from_value(chain_id: ChainId, raw: &Value) -> Result<ChainBlock> {
    let get = |key: &str| -> Result<&Value> {
        raw.get(key)
            .ok_or_else(|| ChainError::Decode(format!("block is missing `{key}`")))
    };
    Ok(ChainBlock {
        chain_id,
        number: BlockNumber(parse_u64(get("number")?, "block.number")?),
        hash: parse_b256(get("hash")?, "block.hash")?,
        parent_hash: parse_b256(get("parentHash")?, "block.parentHash")?,
        timestamp: parse_u64(get("timestamp")?, "block.timestamp")?,
        transaction_count: get("transactions")?
            .as_array()
            .ok_or_else(|| ChainError::Decode("block.transactions is not an array".to_string()))?
            .len(),
    })
}

/// A log from a provider's own JSON.
///
/// Public for the same reason [`chain_block_from_value`] is: the execution layer reads
/// a single transaction receipt, which the block-oriented [`ChainAdapter`] methods do
/// not serve, and a second copy of this parsing would be a second set of rules for what
/// a log is (§9's one state semantics, applied one layer below state again).
pub fn chain_log_from_value(chain_id: ChainId, raw: &Value) -> Result<ChainLog> {
    let get = |key: &str| -> Result<&Value> {
        raw.get(key)
            .ok_or_else(|| ChainError::Decode(format!("log is missing `{key}`: {raw}")))
    };
    let topics = get("topics")?
        .as_array()
        .ok_or_else(|| ChainError::Decode("topics is not an array".to_string()))?;
    let topics = topics
        .iter()
        .map(|t| parse_b256(t, "topic"))
        .collect::<Result<Vec<_>>>()?;
    Ok(ChainLog {
        chain_id,
        block_number: BlockNumber(parse_u64(get("blockNumber")?, "log.blockNumber")?),
        tx_hash: TxHash(parse_b256(get("transactionHash")?, "log.transactionHash")?),
        tx_index: TxIndex(parse_u64(get("transactionIndex")?, "log.transactionIndex")?),
        log_index: LogIndex(parse_u64(get("logIndex")?, "log.logIndex")?),
        address: parse_address(get("address")?, "log.address")?,
        topics,
        data: parse_bytes(get("data")?, "log.data")?,
    })
}

fn normalize_receipt(chain_id: ChainId, raw: &Value) -> Result<ChainReceipt> {
    let tx_hash = TxHash(parse_b256(
        raw.get("transactionHash")
            .ok_or_else(|| ChainError::Decode("receipt has no transactionHash".to_string()))?,
        "receipt.transactionHash",
    )?);
    let logs = raw
        .get("logs")
        .and_then(Value::as_array)
        .ok_or_else(|| ChainError::Decode("receipt has no logs array".to_string()))?;
    let mut logs = logs
        .iter()
        .map(|l| chain_log_from_value(chain_id, l))
        .collect::<Result<Vec<_>>>()?;
    logs.sort();
    let status = raw
        .get("status")
        .and_then(Value::as_str)
        .map(|s| s != "0x0" && !s.is_empty())
        .unwrap_or(true);
    Ok(ChainReceipt {
        tx_hash,
        tx_index: TxIndex(parse_u64(
            raw.get("transactionIndex")
                .ok_or_else(|| ChainError::Decode("receipt has no transactionIndex".to_string()))?,
            "receipt.transactionIndex",
        )?),
        block_number: BlockNumber(parse_u64(
            raw.get("blockNumber")
                .ok_or_else(|| ChainError::Decode("receipt has no blockNumber".to_string()))?,
            "receipt.blockNumber",
        )?),
        status,
        logs,
    })
}

fn optional_address(raw: &Value, key: &str) -> Result<Option<Address>> {
    match raw.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(parse_address(value, key)?)),
    }
}

fn normalize_transaction(chain_id: ChainId, raw: &Value) -> Result<ChainTransaction> {
    let _ = chain_id;
    let hash = TxHash(parse_b256(
        raw.get("hash")
            .ok_or_else(|| ChainError::Decode("transaction has no hash".to_string()))?,
        "tx.hash",
    )?);
    Ok(ChainTransaction {
        hash,
        tx_index: TxIndex(parse_u64(
            raw.get("transactionIndex")
                .ok_or_else(|| ChainError::Decode("transaction has no index".to_string()))?,
            "tx.transactionIndex",
        )?),
        from: optional_address(raw, "tx.from")?,
        to: optional_address(raw, "tx.to")?,
        value: raw
            .get("value")
            .map(|v| parse_u256(v, "tx.value"))
            .transpose()?
            .unwrap_or(U256::ZERO),
        input: raw
            .get("input")
            .map(|v| parse_bytes(v, "tx.input"))
            .transpose()?
            .unwrap_or_default(),
    })
}

#[async_trait]
impl ChainAdapter for HttpChainAdapter {
    fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    fn with_rpc_trace(&self, sink: RpcTraceSink) -> Option<Arc<dyn ChainAdapter>> {
        Some(Arc::new(Self::with_rpc_trace(self, sink)))
    }

    async fn latest_block(&self) -> Result<BlockNumber> {
        let raw = self.request("eth_blockNumber", json!([])).await?;
        Ok(BlockNumber(parse_u64(&raw, "eth_blockNumber")?))
    }

    async fn get_block(&self, number: BlockNumber) -> Result<ChainBlock> {
        let raw = self
            .request("eth_getBlockByNumber", json!([block_param(number), false]))
            .await?;
        if raw.is_null() {
            return Err(ChainError::MissingData(format!(
                "block {} not available",
                number.0
            )));
        }
        chain_block_from_value(self.chain_id, &raw)
    }

    async fn get_block_context(&self, number: BlockNumber) -> Result<BlockContext> {
        let raw = self
            .request("eth_getBlockByNumber", json!([block_param(number), false]))
            .await?;
        if raw.is_null() {
            return Err(ChainError::MissingData(format!(
                "block {} not available",
                number.0
            )));
        }
        let get = |key: &str| -> Result<&Value> {
            raw.get(key)
                .ok_or_else(|| ChainError::Decode(format!("block is missing `{key}`")))
        };
        // A legacy-pricing block simply has no baseFeePerGas field; that is
        // recorded as None rather than as zero, because the two mean different
        // things when the effective gas price is computed.
        let base_fee_per_gas = match raw.get("baseFeePerGas") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                u128::try_from(parse_u256(v, "block.baseFeePerGas")?).map_err(|_| {
                    ChainError::Decode("block.baseFeePerGas does not fit in u128".to_string())
                })?,
            ),
        };
        // An absent `excessBlobGas` is recorded as None, not as zero: the two mean
        // different things to a blob-era ruleset, and inventing the field would be
        // the same mistake §60 forbids for bytecode.
        let excess_blob_gas = match raw.get("excessBlobGas") {
            None | Some(Value::Null) => None,
            Some(v) => Some(parse_u64(v, "block.excessBlobGas")?),
        };
        Ok(BlockContext {
            chain_id: self.chain_id,
            number: BlockNumber(parse_u64(get("number")?, "block.number")?),
            hash: parse_b256(get("hash")?, "block.hash")?,
            timestamp: parse_u64(get("timestamp")?, "block.timestamp")?,
            gas_limit: parse_u64(get("gasLimit")?, "block.gasLimit")?,
            base_fee_per_gas,
            excess_blob_gas,
            beneficiary: parse_address(get("miner")?, "block.miner")?,
            prevrandao: raw
                .get("mixHash")
                .map(|v| parse_b256(v, "block.mixHash"))
                .transpose()?,
        })
    }

    async fn get_block_data(&self, number: BlockNumber) -> Result<BlockData> {
        let block = self.get_block(number).await?;
        let raw_block = self
            .request("eth_getBlockByNumber", json!([block_param(number), true]))
            .await?;
        let raw_txs = raw_block
            .get("transactions")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ChainError::Decode("hydrated block has no transactions array".to_string())
            })?;
        let mut transactions = Vec::with_capacity(raw_txs.len());
        for raw in raw_txs {
            if !raw.is_object() {
                return Err(ChainError::Decode(
                    "provider returned bare transaction hashes although hydration was requested"
                        .to_string(),
                ));
            }
            transactions.push(normalize_transaction(self.chain_id, raw)?);
        }
        transactions.sort_by_key(|t| t.tx_index);
        if transactions
            .iter()
            .map(|t| t.tx_index.0)
            .collect::<Vec<_>>()
            != (0..transactions.len() as u64).collect::<Vec<_>>()
        {
            return Err(ChainError::Inconsistent(format!(
                "block {} transaction indices are not contiguous",
                block.number.0
            )));
        }

        let raw_receipts = self
            .request("eth_getBlockReceipts", json!([block_param(number)]))
            .await?;
        let raw_receipts = raw_receipts.as_array().ok_or_else(|| {
            ChainError::Decode("eth_getBlockReceipts did not return an array".to_string())
        })?;
        if raw_receipts.len() != transactions.len() {
            return Err(ChainError::Inconsistent(format!(
                "block {}: {} transactions but {} receipts",
                number.0,
                transactions.len(),
                raw_receipts.len()
            )));
        }
        let mut receipts = raw_receipts
            .iter()
            .map(|r| normalize_receipt(self.chain_id, r))
            .collect::<Result<Vec<_>>>()?;
        receipts.sort();

        // Cross-check: every receipt must belong to this block and line up with
        // the hydrated transaction list. A provider that mixes up blocks would
        // otherwise silently corrupt state.
        for (tx, receipt) in transactions.iter().zip(receipts.iter()) {
            if tx.hash != receipt.tx_hash || tx.tx_index != receipt.tx_index {
                return Err(ChainError::Inconsistent(format!(
                    "block {}: transaction {:?} at index {:?} does not match receipt {:?}",
                    number.0, tx.hash, tx.tx_index, receipt.tx_hash
                )));
            }
            if let Some(log) = receipt.logs.first() {
                if log.block_number != number {
                    return Err(ChainError::Inconsistent(format!(
                        "receipt {:?} carries logs from block {:?}",
                        receipt.tx_hash, log.block_number
                    )));
                }
            }
        }

        Ok(BlockData {
            block,
            transactions,
            receipts,
        })
    }

    async fn get_logs(&self, filter: LogFilter) -> Result<Vec<ChainLog>> {
        let topics: Vec<Value> = filter
            .topics
            .iter()
            .map(|slot| match slot {
                Some(list) => Value::Array(list.iter().map(|t| json!(t.to_string())).collect()),
                None => Value::Null,
            })
            .collect();
        let mut params = json!([{
            "fromBlock": block_param(filter.from_block),
            "toBlock": block_param(filter.to_block),
            "topics": topics,
        }]);
        if !filter.addresses.is_empty() {
            let addresses: Vec<Value> = filter
                .addresses
                .iter()
                .map(|a| json!(a.to_string()))
                .collect();
            params[0]["address"] = Value::Array(addresses);
        }
        let raw = self.request("eth_getLogs", params).await?;
        let raw = raw
            .as_array()
            .ok_or_else(|| ChainError::Decode("eth_getLogs did not return an array".to_string()))?;
        let mut logs = raw
            .iter()
            .map(|l| chain_log_from_value(self.chain_id, l))
            .collect::<Result<Vec<_>>>()?;
        logs.sort();
        Ok(logs)
    }

    async fn call(&self, at: BlockNumber, request: &CallRequest) -> Result<Bytes> {
        let raw = self
            .request(
                "eth_call",
                json!([{"to": request.to.to_string(), "data": request.data.to_string()}, block_param(at)]),
            )
            .await?;
        parse_bytes(&raw, "eth_call result")
    }

    async fn get_code(&self, at: BlockNumber, address: Address) -> Result<Bytes> {
        let raw = self
            .request("eth_getCode", json!([address.to_string(), block_param(at)]))
            .await?;
        parse_bytes(&raw, "eth_getCode result")
    }

    async fn get_balance(&self, at: BlockNumber, address: Address) -> Result<U256> {
        let raw = self
            .request(
                "eth_getBalance",
                json!([address.to_string(), block_param(at)]),
            )
            .await?;
        parse_u256(&raw, "eth_getBalance result")
    }

    async fn get_storage_at(&self, at: BlockNumber, address: Address, slot: U256) -> Result<U256> {
        let raw = self
            .request(
                "eth_getStorageAt",
                json!([
                    address.to_string(),
                    format!("0x{slot:064x}"),
                    block_param(at)
                ]),
            )
            .await?;
        parse_u256(&raw, "eth_getStorageAt result")
    }

    async fn get_nonce(&self, at: BlockNumber, address: Address) -> Result<u64> {
        let raw = self
            .request(
                "eth_getTransactionCount",
                json!([address.to_string(), block_param(at)]),
            )
            .await?;
        parse_u64(&raw, "eth_getTransactionCount result")
    }
}
