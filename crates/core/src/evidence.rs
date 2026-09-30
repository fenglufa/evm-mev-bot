use serde::{Deserialize, Serialize};

use crate::identity::{BlockNumber, TxHash};

/// Where a fact was observed. Every claim about a pool's identity must be
/// traceable to one of these, otherwise it is an assumption.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EvidenceSource {
    /// `eth_getLogs` / `eth_getBlockReceipts` on a finalized block.
    ChainLog,
    /// `eth_call` against contract state.
    EthCall,
    /// `eth_getCode`.
    Bytecode,
    /// `eth_getTransactionByHash` calldata.
    TransactionInput,
    /// A locally recorded capture (raw historical data).
    RecordedCapture,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub source: EvidenceSource,
    pub block_number: Option<BlockNumber>,
    pub transaction_hash: Option<TxHash>,
    /// Global log index within the block, as reported by the chain.
    pub log_index: Option<u64>,
    /// The 4-byte selector or topic0 this evidence is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl EvidenceRef {
    pub fn new(source: EvidenceSource) -> Self {
        Self {
            source,
            block_number: None,
            transaction_hash: None,
            log_index: None,
            signature: None,
        }
    }
}

/// One confirmed fact about a pool plus the evidence behind it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Attested<A> {
    pub value: A,
    pub evidence: Vec<EvidenceRef>,
}

impl<A> Attested<A> {
    pub fn new(value: A, evidence: Vec<EvidenceRef>) -> Self {
        Self { value, evidence }
    }

    pub fn require_evidence(&self) -> Option<&A> {
        if self.evidence.is_empty() {
            None
        } else {
            Some(&self.value)
        }
    }
}
