use std::str::FromStr;

use alloy_primitives::{Address, Bytes, B256, U256};
use async_trait::async_trait;
use serde_json::{json, Value};

use evm_core::{BlockNumber, ChainId, LogIndex, TxHash, TxIndex};

use crate::adapter::ChainAdapter;
use crate::error::{ChainError, Result};
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
#[derive(Clone)]
pub struct HttpChainAdapter {
    http: reqwest::Client,
    url: String,
    chain_id: ChainId,
}

impl HttpChainAdapter {
    pub async fn connect(url: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        let raw = Self::request_with(&http, url, "eth_chainId", json!([])).await?;
        let chain_id = ChainId(parse_u64(&raw, "eth_chainId")?);
        Ok(Self {
            http,
            url: url.to_owned(),
            chain_id,
        })
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        Self::request_with(&self.http, &self.url, method, params).await
    }

    /// The same request, for a caller that needs the raw provider JSON rather
    /// than a normalized type. Only used to keep one header-parsing path between
    /// the HTTP adapter and any other transport (see [`crate::head`]).
    pub async fn request_raw(&self, method: &str, params: Value) -> Result<Value> {
        Self::request_with(&self.http, &self.url, method, params).await
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    async fn request_with(
        http: &reqwest::Client,
        url: &str,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let mut last = None;
        // A single retry: transient 5xx / connection resets are common on public nodes.
        for _ in 0..2 {
            let response = http
                .post(url)
                .json(&body)
                .send()
                .await
                .map_err(|e| ChainError::Rpc(e.to_string()));
            let response = match response {
                Ok(r) => r,
                Err(e) => {
                    last = Some(e);
                    continue;
                }
            };
            let status = response.status();
            let parsed: Value = match response.json().await {
                Ok(v) => v,
                Err(e) => {
                    last = Some(ChainError::Rpc(format!(
                        "non-json response (status {status}): {e}"
                    )));
                    continue;
                }
            };
            if !status.is_success() {
                last = Some(ChainError::Rpc(format!("http status {status}: {parsed}")));
                continue;
            }
            if let Some(error) = parsed.get("error") {
                return Err(ChainError::RpcRejected(error.to_string()));
            }
            return parsed
                .get("result")
                .cloned()
                .ok_or_else(|| ChainError::Decode("response has no result".to_string()));
        }
        Err(last.unwrap_or_else(|| ChainError::Rpc("request failed".to_string())))
    }
}

fn parse_u64(value: &Value, context: &str) -> Result<u64> {
    let text = value
        .as_str()
        .ok_or_else(|| ChainError::Decode(format!("{context} is not a hex string: {value}")))?;
    u64::from_str_radix(text.strip_prefix("0x").unwrap_or(text), 16)
        .map_err(|e| ChainError::Decode(format!("{context} `{text}`: {e}")))
}

fn parse_address(value: &Value, context: &str) -> Result<Address> {
    let text = value
        .as_str()
        .ok_or_else(|| ChainError::Decode(format!("{context} is not an address: {value}")))?;
    Address::from_str(text).map_err(|e| ChainError::Decode(format!("{context} `{text}`: {e}")))
}

fn parse_b256(value: &Value, context: &str) -> Result<B256> {
    let text = value
        .as_str()
        .ok_or_else(|| ChainError::Decode(format!("{context} is not a hash: {value}")))?;
    B256::from_str(text).map_err(|e| ChainError::Decode(format!("{context} `{text}`: {e}")))
}

fn parse_bytes(value: &Value, context: &str) -> Result<Bytes> {
    let text = value
        .as_str()
        .ok_or_else(|| ChainError::Decode(format!("{context} is not bytes: {value}")))?;
    Bytes::from_str(text).map_err(|e| ChainError::Decode(format!("{context} `{text}`: {e}")))
}

fn parse_u256(value: &Value, context: &str) -> Result<U256> {
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

fn normalize_log(chain_id: ChainId, raw: &Value) -> Result<ChainLog> {
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
        .map(|l| normalize_log(chain_id, l))
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
            .map(|l| normalize_log(self.chain_id, l))
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
