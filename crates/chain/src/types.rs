use alloy_primitives::{Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};

use evm_core::{BlockNumber, ChainId, LogIndex, TxHash, TxIndex};

/// A block header, normalized. Provider specific block types never leave the
/// chain layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainBlock {
    pub chain_id: ChainId,
    pub number: BlockNumber,
    pub hash: B256,
    pub parent_hash: B256,
    pub timestamp: u64,
    pub transaction_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainTransaction {
    pub hash: TxHash,
    pub tx_index: TxIndex,
    pub from: Option<Address>,
    pub to: Option<Address>,
    pub value: U256,
    /// Full calldata, including the 4-byte selector.
    pub input: Bytes,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainReceipt {
    pub tx_hash: TxHash,
    pub tx_index: TxIndex,
    pub block_number: BlockNumber,
    pub status: bool,
    /// Already ordered by [`evm_core::LogIndex`].
    pub logs: Vec<ChainLog>,
}

/// One log plus its exact on-chain execution position.
///
/// `Ord` is the on-chain execution order: block, then transaction, then log.
/// It is deliberately not derived, so no caller can accidentally order by hash
/// or by the order a provider returned records in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainLog {
    pub chain_id: ChainId,
    pub block_number: BlockNumber,
    pub tx_hash: TxHash,
    pub tx_index: TxIndex,
    pub log_index: LogIndex,
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Bytes,
}

impl ChainLog {
    /// (block, transaction, log) — the only order state may be applied in.
    pub fn position(&self) -> (BlockNumber, TxIndex, LogIndex) {
        (self.block_number, self.tx_index, self.log_index)
    }
}

impl Ord for ChainLog {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.position().cmp(&other.position())
    }
}

impl PartialOrd for ChainLog {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ChainReceipt {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.block_number, self.tx_index).cmp(&(other.block_number, other.tx_index))
    }
}

impl PartialOrd for ChainReceipt {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Everything the pipeline needs about one block, in execution order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockData {
    pub block: ChainBlock,
    pub transactions: Vec<ChainTransaction>,
    pub receipts: Vec<ChainReceipt>,
}

impl BlockData {
    /// Logs flattened in strict on-chain execution order.
    pub fn ordered_logs(&self) -> Vec<&ChainLog> {
        let mut logs: Vec<&ChainLog> = self.receipts.iter().flat_map(|r| r.logs.iter()).collect();
        logs.sort();
        logs
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogFilter {
    pub from_block: BlockNumber,
    pub to_block: BlockNumber,
    pub addresses: Vec<Address>,
    /// `None` at position i means "any topic at that position".
    pub topics: Vec<Option<Vec<B256>>>,
}

impl Default for LogFilter {
    fn default() -> Self {
        Self {
            from_block: BlockNumber(0),
            to_block: BlockNumber(0),
            addresses: Vec::new(),
            topics: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallRequest {
    pub to: Address,
    pub data: Bytes,
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, b256};

    use super::*;
    use evm_core::ChainId;

    fn log(block: u64, tx: u64, idx: u64, addr: Address) -> ChainLog {
        ChainLog {
            chain_id: ChainId(91342),
            block_number: BlockNumber(block),
            tx_hash: TxHash(b256!(
                "0x1111111111111111111111111111111111111111111111111111111111111111"
            )),
            tx_index: TxIndex(tx),
            log_index: LogIndex(idx),
            address: addr,
            topics: vec![],
            data: Bytes::new(),
        }
    }

    #[test]
    fn ordering_is_transaction_index_then_log_index() {
        let a = log(
            100,
            1,
            2,
            address!("0x0000000000000000000000000000000000000001"),
        );
        let b = log(
            100,
            1,
            3,
            address!("0x0000000000000000000000000000000000000002"),
        );
        let c = log(
            100,
            2,
            0,
            address!("0x0000000000000000000000000000000000000003"),
        );
        let mut v = [c.clone(), a.clone(), b.clone()];
        v.sort();
        assert_eq!(v, [a, b, c]);
    }

    #[test]
    fn ordered_logs_ignores_input_order() {
        let l0 = log(
            7,
            0,
            1,
            address!("0x0000000000000000000000000000000000000001"),
        );
        let l1 = log(
            7,
            0,
            0,
            address!("0x0000000000000000000000000000000000000002"),
        );
        let block = BlockData {
            block: ChainBlock {
                chain_id: ChainId(91342),
                number: BlockNumber(7),
                hash: b256!("0x2222222222222222222222222222222222222222222222222222222222222222"),
                parent_hash: b256!(
                    "0x3333333333333333333333333333333333333333333333333333333333333333"
                ),
                timestamp: 0,
                transaction_count: 1,
            },
            transactions: vec![],
            receipts: vec![
                ChainReceipt {
                    tx_hash: l0.tx_hash,
                    tx_index: l0.tx_index,
                    block_number: l0.block_number,
                    status: true,
                    logs: vec![l0.clone()],
                },
                ChainReceipt {
                    tx_hash: l1.tx_hash,
                    tx_index: l1.tx_index,
                    block_number: l1.block_number,
                    status: true,
                    logs: vec![l1.clone()],
                },
            ],
        };
        assert_eq!(block.ordered_logs(), vec![&l1, &l0]);
    }
}
