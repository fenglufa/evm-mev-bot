//! §22/§23: the GIWA endpoint, as measured.
//!
//! What this file knows about GIWA is what `data/evidence/m6/probe-summary.md` measured:
//!
//! * the submission method is `eth_sendRawTransaction`, and it is whitelisted — the two
//!   decode errors it returns for a malformed payload are recorded verbatim there;
//! * no direct-sequencer protocol exists on it. `mev_sendBundle`, `engine_submitBlock`,
//!   `sequencer_submit`, `giwa_sendRawTransaction`,
//!   `eth_sendRawTransactionFlashblock`, `txpool_status` and `txpool_content` all answer
//!   `-32601 rpc method is not whitelisted`, and the deliberately misspelled control
//!   (`etch_sendRawTransaction`) answers the same way, which is what makes `-32601`
//!   evidence of absence rather than a generic error. §24's instruction for that case is
//!   `SequencerDirect = BLOCKED`, and [`GiwaSequencerDirect::direct_protocol`] states it;
//!   nothing here fakes a private path.
//! * blocks arrive about once a second, `baseFeePerGas` is present and tiny (371 wei
//!   measured), `eth_maxPriorityFeePerGas` answers 1 000 000 wei, and both legacy and
//!   EIP-1559 user transactions are in the chain's own history — so §12's "verify the fee
//!   type from the endpoint" is satisfied by reading a header, never by assuming.
//!
//! The HTTP client and the URL come from [`HttpChainAdapter`], which the rest of the
//! repository already uses and which §44 requires be configured rather than baked in.
//! This adapter adds no new transport, no new endpoint constant and no second connection
//! pool.

use alloy_primitives::{Address, ChainId, B256, U256};
use async_trait::async_trait;
use serde_json::{json, Value};

use evm_chain::{chain_block_from_value, chain_log_from_value, ChainAdapter, HttpChainAdapter};
use evm_core::BlockNumber;

use crate::chain_read::ChainReader;
use crate::error::{ExecutionError, Result};
use crate::fee::{FeePolicy, FeeReading, FeeSource};
use crate::mode::ExecutionMode;
use crate::nonce::{NonceReading, NonceSource};
use crate::receipt::Receipt;
use crate::submitter::{EndpointKind, SubmissionOutcome, TransactionSubmitter};
use crate::tx::{SignedTransaction, TransactionType};

/// The name §22 gives the GIWA adapter. It is the *submission* adapter: over the public
/// RPC, because that is the only path the endpoint accepts.
#[derive(Clone)]
pub struct GiwaSequencerDirect {
    http: HttpChainAdapter,
    mode: ExecutionMode,
    endpoint: EndpointKind,
    chain_id: ChainId,
}

impl GiwaSequencerDirect {
    /// §6: connect and verify the chain identity before anything else can be asked of
    /// the endpoint. A mismatch is a refusal, not a warning.
    pub async fn connect(
        url: &str,
        expected_chain_id: ChainId,
        mode: ExecutionMode,
        endpoint: EndpointKind,
    ) -> Result<Self> {
        let http = HttpChainAdapter::connect(url)
            .await
            .map_err(|e| ExecutionError::ChainRead(e.to_string()))?;
        // The adapter's chain id is `evm_core`'s newtype; this crate's transaction and
        // record types carry the plain quantity, so the conversion happens here and
        // nowhere else.
        let answered = http.chain_id().0;
        if answered != expected_chain_id {
            return Err(ExecutionError::ChainMismatch(format!(
                "the endpoint answers for chain {} and execution is configured for chain {}; \
                 nothing is built, signed or sent against a node that disagrees about which \
                 chain it is",
                answered, expected_chain_id
            )));
        }
        if !endpoint.may_broadcast() && mode.may_submit() {
            return Err(ExecutionError::ModeGate(format!(
                "{} is a read-only endpoint kind and Submit was asked of it",
                endpoint.name()
            )));
        }
        Ok(Self {
            http,
            mode,
            endpoint,
            chain_id: answered,
        })
    }

    pub fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    pub fn url(&self) -> &str {
        self.http.url()
    }

    pub fn mode(&self) -> ExecutionMode {
        self.mode
    }

    pub fn endpoint_kind(&self) -> EndpointKind {
        self.endpoint
    }

    /// §24, stated as a fact about the endpoint rather than as an unimplemented method.
    /// A caller that was asked for a private sequencer path reports this in the
    /// completion report instead of building a mock.
    pub fn direct_protocol() -> std::result::Result<(), String> {
        Err(
            "BLOCKED: no direct-sequencer protocol exists on the GIWA testnet endpoint. \
             `mev_sendBundle`, `engine_submitBlock`, `sequencer_submit`, \
             `giwa_sendRawTransaction`, `eth_sendRawTransactionFlashblock`, `txpool_status` \
             and `txpool_content` each return `-32601 rpc method is not whitelisted`; the \
             misspelled control `etch_sendRawTransaction` returns the same, so `-32601` means \
             absent. Evidence: data/evidence/m6/probe-method-whitelist.txt"
                .to_string(),
        )
    }

    /// The head as one atomic read: height and hash from the same header, because a
    /// binding check built from two reads can straddle a block.
    pub async fn head(&self) -> Result<(BlockNumber, B256)> {
        let raw = self
            .http
            .request_raw("eth_getBlockByNumber", json!(["latest", false]))
            .await
            .map_err(read_error)?;
        let block =
            chain_block_from_value(evm_core::ChainId(self.chain_id), &raw).map_err(read_error)?;
        Ok((block.number, block.hash))
    }

    /// The hash the endpoint holds at `number`, or `None` when it has no such block.
    pub async fn block_hash_at(&self, number: BlockNumber) -> Result<Option<B256>> {
        match self.http.get_block(number).await {
            Ok(block) => Ok(Some(block.hash)),
            Err(evm_chain::ChainError::MissingData(_)) => Ok(None),
            Err(error) => Err(read_error(error)),
        }
    }

    /// §32's `chain_id` leg, read rather than remembered.
    pub async fn endpoint_chain_id(&self) -> Result<ChainId> {
        let raw = self
            .http
            .request_raw("eth_chainId", json!([]))
            .await
            .map_err(read_error)?;
        quantity_u64(&raw, "eth_chainId")
    }

    /// `eth_maxPriorityFeePerGas`, unprocessed.
    async fn suggested_tip_raw(&self) -> Result<Option<U256>> {
        match self
            .http
            .request_raw("eth_maxPriorityFeePerGas", json!([]))
            .await
        {
            Ok(value) => {
                let tip = evm_chain::rpc::parse_u256(&value, "eth_maxPriorityFeePerGas")
                    .map_err(read_error)?;
                Ok(Some(tip))
            }
            // A node that does not answer the question is recorded as "no suggestion"
            // rather than as a zero suggestion; `FeePolicy::BaseFeeHeadroom` then refuses,
            // which is the honest path (§12).
            Err(evm_chain::ChainError::RpcRejected(_)) => Ok(None),
            Err(error) => Err(read_error(error)),
        }
    }

    /// The base fee of one block, `None` when the block has no such field (a pre-1559
    /// section is legacy-priced, which §12 requires be recorded rather than invented).
    async fn base_fee_at(&self, number: BlockNumber) -> Result<Option<U256>> {
        let context = self
            .http
            .get_block_context(number)
            .await
            .map_err(read_error)?;
        Ok(context.base_fee_per_gas.map(U256::from))
    }
}

/// §32's balance leg, at a pinned block.
impl GiwaSequencerDirect {
    pub async fn native_balance(&self, address: Address, at: BlockNumber) -> Result<U256> {
        self.http
            .get_balance(at, address)
            .await
            .map_err(|e| ExecutionError::ChainRead(format!("{address} at {}: {e}", at.0)))
    }
}

/// §32's two canonical reads, answered by this endpoint.
///
/// Both methods delegate to the inherent reads above rather than re-describing them: the
/// trait exists so [`crate::gate`]'s binding leg can be evaluated against a scripted
/// endpoint in the tests, not so the same question has two answers. The comparison that
/// turns these reads into a [`crate::gate::BlockBinding`] is
/// [`crate::chain_read::read_binding`], and it runs for this adapter and for a test double
/// through one implementation.
#[async_trait]
impl ChainReader for GiwaSequencerDirect {
    async fn block_hash_at(&self, number: BlockNumber) -> Result<Option<B256>> {
        Self::block_hash_at(self, number).await
    }

    async fn endpoint_chain_id(&self) -> Result<u64> {
        Self::endpoint_chain_id(self).await
    }
}

#[async_trait]
impl FeeSource for GiwaSequencerDirect {
    /// Read the pinned block's base fee and the node's suggested tip, and apply `policy`.
    ///
    /// The block is named by number, not by `latest`: a fee read against a moving head is
    /// a fee read against no state, and §31's staleness question would become
    /// unanswerable.
    async fn fee_reading(
        &self,
        block_number: u64,
        block_hash: B256,
        tx_type: TransactionType,
        policy: &FeePolicy,
    ) -> Result<FeeReading> {
        let base = self.base_fee_at(BlockNumber(block_number)).await?;
        let tip = self.suggested_tip_raw().await?;
        let reading = policy.apply(self.chain_id, block_number, block_hash, base, tip, tx_type)?;
        Ok(reading)
    }

    async fn suggested_tip(&self) -> Result<Option<U256>> {
        self.suggested_tip_raw().await
    }

    async fn balance(&self, address: Address, block_number: u64) -> Result<U256> {
        self.native_balance(address, BlockNumber(block_number))
            .await
    }
}

#[async_trait]
impl NonceSource for GiwaSequencerDirect {
    /// Both views, taken around one head read.
    ///
    /// The confirmed view is pinned to the height that head returned rather than asked as
    /// `"latest"`, so the pair describes one moment as closely as two HTTP calls can
    /// describe anything, and the `source` string says exactly what was done. A build that
    /// cannot tell where a nonce came from cannot tell whether it is still valid.
    async fn nonce(&self, address: Address) -> Result<NonceReading> {
        let (head, _) = self.head().await?;
        let confirmed = self
            .http
            .get_nonce(head, address)
            .await
            .map_err(|e| ExecutionError::NonceUnavailable(format!("confirmed view: {e}")))?;
        let raw = self
            .http
            .request_raw(
                "eth_getTransactionCount",
                json!([address.to_string(), "pending"]),
            )
            .await
            .map_err(|e| ExecutionError::NonceUnavailable(format!("pending view: {e}")))?;
        let pending = evm_chain::rpc::parse_u64(&raw, "eth_getTransactionCount(pending)")
            .map_err(read_error)?;
        Ok(NonceReading {
            address,
            confirmed,
            pending,
            at_block: head.0,
            source: format!(
                "eth_getTransactionCount at #{} for the confirmed view and \
                 \"pending\" for the pending view, both read behind one \
                 eth_getBlockByNumber(\"latest\")",
                head.0
            ),
        })
    }
}

#[async_trait]
impl TransactionSubmitter for GiwaSequencerDirect {
    fn endpoint(&self) -> EndpointKind {
        self.endpoint
    }

    fn may_submit(&self) -> bool {
        self.mode.may_submit() && self.endpoint.may_broadcast()
    }

    /// §21/§23/§25: hand the raw bytes over once, and classify the answer.
    ///
    /// There is no retry in this function, and none in its callers: an `Unknown` answer
    /// keeps the nonce lane reserved ([`crate::lifecycle::ExecutionLane`]) until a receipt
    /// or a re-read resolves it. The transaction hash is computed locally from the bytes
    /// and compared against the node's answer, so a node that names a different hash
    /// cannot redirect what we track (§27).
    async fn submit(&self, transaction: &SignedTransaction) -> Result<SubmissionOutcome> {
        if !self.may_submit() {
            return Err(ExecutionError::ModeGate(format!(
                "submission was asked of a process running in {} mode; §20 makes Submit an \
                 explicit choice and the default is BuildOnly",
                self.mode.name()
            )));
        }
        let local_hash = transaction.hash();
        let raw = transaction.raw();
        let payload = format!("0x{}", hex::encode(raw.as_ref()));
        let response = self
            .http
            .request_raw("eth_sendRawTransaction", json!([payload]))
            .await;
        Ok(match response {
            Ok(value) => {
                let text = value.as_str().unwrap_or_default();
                let returned = if text.len() == 66 {
                    text.parse::<B256>().ok()
                } else {
                    None
                };
                match returned {
                    Some(hash) => SubmissionOutcome::Accepted {
                        transaction_hash: Some(hash),
                        hash_matches_local: hash == local_hash,
                        endpoint: self.endpoint,
                        detail: format!("eth_sendRawTransaction returned {text}"),
                    },
                    None => SubmissionOutcome::Unknown {
                        reason: format!(
                            "eth_sendRawTransaction answered with something that is not a \
                             32-byte hash: {value}"
                        ),
                        endpoint: self.endpoint,
                    },
                }
            }
            // An explicit JSON-RPC error object is the node saying no. §25 lets only this
            // answer release the lane.
            Err(evm_chain::ChainError::RpcRejected(error)) => SubmissionOutcome::Rejected {
                reason: format!("eth_sendRawTransaction refused the payload: {error}"),
                endpoint: self.endpoint,
            },
            // A transport failure, a non-JSON body, or an HTTP status that is not a
            // refusal: we do not know, and knowing that is the point.
            Err(error) => SubmissionOutcome::Unknown {
                reason: format!("no answer from eth_sendRawTransaction: {error}"),
                endpoint: self.endpoint,
            },
        })
    }

    /// `eth_getTransactionReceipt`. A null result is a legitimate "not yet" (§26's
    /// `Pending`), and a receipt that cannot be parsed is an error rather than a missing
    /// one.
    async fn receipt(&self, transaction_hash: B256) -> Result<Option<Receipt>> {
        let raw = self
            .http
            .request_raw(
                "eth_getTransactionReceipt",
                json!([transaction_hash.to_string()]),
            )
            .await
            .map_err(read_error)?;
        if raw.is_null() {
            return Ok(None);
        }
        Ok(Some(parse_receipt(
            self.chain_id,
            self.endpoint,
            &raw,
            &format!("{transaction_hash:#x}"),
        )?))
    }
}

/// Read one receipt's JSON into [`Receipt`].
///
/// Every field §26 lists is required and a malformed one is an error; the OP-stack
/// additions are optional, because they exist only on this chain's receipts and an
/// absent one is a fact about the endpoint rather than a decode failure.
///
/// This is public because it is the endpoint contract rather than a step of the ladder:
/// §41's evidence is a receipt the node actually answered with, and a test that reads
/// that answer has to turn it into a `Receipt` the same way the live lane does — not with
/// a second decoder that could drift from the first.
pub fn parse_receipt(
    chain_id: ChainId,
    endpoint: EndpointKind,
    raw: &Value,
    context: &str,
) -> Result<Receipt> {
    let required = |key: &str| -> Result<&Value> {
        raw.get(key).filter(|v| !v.is_null()).ok_or_else(|| {
            ExecutionError::ReceiptBinding(format!("receipt for {context} has no `{key}`"))
        })
    };
    let quantity = |key: &str| -> Result<u64> {
        evm_chain::rpc::parse_u64(required(key)?, &format!("receipt.{key}")).map_err(read_error)
    };
    let big = |key: &str| -> Result<U256> {
        evm_chain::rpc::parse_u256(required(key)?, &format!("receipt.{key}")).map_err(read_error)
    };
    let optional_big = |key: &str| -> Result<Option<U256>> {
        match raw.get(key).filter(|v| !v.is_null()) {
            Some(value) => Ok(Some(
                evm_chain::rpc::parse_u256(value, &format!("receipt.{key}")).map_err(read_error)?,
            )),
            None => Ok(None),
        }
    };
    let status = quantity("status")?;
    if status > 1 {
        return Err(ExecutionError::ReceiptBinding(format!(
            "receipt for {context} reports status {status}, which is neither success nor \
             failure; a value this code cannot name must not be read as either"
        )));
    }
    let logs = required("logs")?
        .as_array()
        .ok_or_else(|| {
            ExecutionError::ReceiptBinding(format!("receipt for {context} has no logs array"))
        })?
        .iter()
        .map(|log| chain_log_from_value(evm_core::ChainId(chain_id), log).map_err(read_error))
        .collect::<Result<Vec<_>>>()?;
    let to = match raw.get("to").filter(|v| !v.is_null()) {
        Some(value) => {
            Some(evm_chain::rpc::parse_address(value, "receipt.to").map_err(read_error)?)
        }
        None => None,
    };
    let contract_address = match raw.get("contractAddress").filter(|v| !v.is_null()) {
        Some(value) => Some(
            evm_chain::rpc::parse_address(value, "receipt.contractAddress").map_err(read_error)?,
        ),
        None => None,
    };
    let tx_type = match raw.get("type").filter(|v| !v.is_null()) {
        Some(value) => Some(evm_chain::rpc::parse_u64(value, "receipt.type").map_err(read_error)?),
        None => None,
    };
    Ok(Receipt {
        transaction_hash: evm_chain::rpc::parse_b256(
            required("transactionHash")?,
            "receipt.transactionHash",
        )
        .map_err(read_error)?,
        block_number: quantity("blockNumber")?,
        block_hash: evm_chain::rpc::parse_b256(required("blockHash")?, "receipt.blockHash")
            .map_err(read_error)?,
        transaction_index: quantity("transactionIndex")?,
        success: status == 1,
        gas_used: quantity("gasUsed")?,
        effective_gas_price: big("effectiveGasPrice")?,
        cumulative_gas_used: optional_big("cumulativeGasUsed")?,
        from: evm_chain::rpc::parse_address(required("from")?, "receipt.from")
            .map_err(read_error)?,
        to,
        contract_address,
        tx_type,
        logs,
        l1_fee: optional_big("l1Fee")?,
        l1_gas_price: optional_big("l1GasPrice")?,
        l1_gas_used: optional_big("l1GasUsed")?,
        l1_base_fee_scalar: optional_big("l1BaseFeeScalar")?,
        l1_blob_base_fee: optional_big("l1BlobBaseFee")?,
        l1_blob_base_fee_scalar: optional_big("l1BlobBaseFeeScalar")?,
        provenance: format!(
            "eth_getTransactionReceipt over {} ({})",
            "the configured GIWA RPC URL",
            endpoint.name()
        ),
    })
}

/// A chain read that failed. §39 keeps this distinct from a submission answer: a read
/// error says nothing about whether a transaction is in flight.
fn read_error(error: evm_chain::ChainError) -> ExecutionError {
    ExecutionError::ChainRead(error.to_string())
}

/// A hex quantity from a provider answer, with the field named in the error.
fn quantity_u64(value: &Value, context: &str) -> Result<u64> {
    evm_chain::rpc::parse_u64(value, context).map_err(read_error)
}
