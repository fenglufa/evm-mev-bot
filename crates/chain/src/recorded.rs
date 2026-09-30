use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use alloy_primitives::Bytes;
use async_trait::async_trait;

use evm_core::{BlockNumber, ChainId};

use crate::adapter::ChainAdapter;
use crate::error::{ChainError, Result};
use crate::types::{BlockData, CallRequest, ChainBlock, ChainLog, LogFilter};

/// Offline replay input: the same normalized blocks a provider would return,
/// read from disk. Live and replay therefore share the pipeline below the
/// adapter boundary, differing only in this implementation.
///
/// Files are `<dir>/block-<number>.json`, each a serialized [`BlockData`].
pub struct RecordedChainAdapter {
    chain_id: ChainId,
    blocks: BTreeMap<BlockNumber, BlockData>,
}

impl RecordedChainAdapter {
    pub fn load(dir: &Path, chain_id: ChainId) -> Result<Self> {
        let mut blocks = BTreeMap::new();
        let mut entries: Vec<PathBuf> = Vec::new();
        let read = std::fs::read_dir(dir)
            .map_err(|e| ChainError::Io(format!("{}: {e}", dir.display())))?;
        for entry in read {
            let entry = entry.map_err(|e| ChainError::Io(e.to_string()))?;
            let path = entry.path();
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("block-") && n.ends_with(".json"))
            {
                entries.push(path);
            }
        }
        entries.sort();
        for path in entries {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| ChainError::Io(format!("{}: {e}", path.display())))?;
            let data: BlockData = serde_json::from_str(&text)
                .map_err(|e| ChainError::Decode(format!("{}: {e}", path.display())))?;
            Self::validate(&data, chain_id, &path)?;
            blocks.insert(data.block.number, data);
        }
        Ok(Self { chain_id, blocks })
    }

    /// Recorded data is treated as untrusted input: the file may list receipts
    /// and logs in any order, but every index inside it has to agree with the
    /// chain's own accounting. A block whose log numbers contradict transaction
    /// order was assembled, not observed, and is refused.
    fn validate(data: &BlockData, chain_id: ChainId, path: &Path) -> Result<()> {
        if data.block.chain_id != chain_id {
            return Err(ChainError::Inconsistent(format!(
                "{}: block carries chain id {:?}, expected {:?}",
                path.display(),
                data.block.chain_id,
                chain_id
            )));
        }

        let mut logs: Vec<&ChainLog> = Vec::new();
        for receipt in &data.receipts {
            if receipt.block_number != data.block.number {
                return Err(ChainError::Inconsistent(format!(
                    "{}: receipt {:?} claims block {:?}, block is {:?}",
                    path.display(),
                    receipt.tx_hash,
                    receipt.block_number,
                    data.block.number
                )));
            }
            for log in &receipt.logs {
                if log.chain_id != chain_id {
                    return Err(ChainError::Inconsistent(format!(
                        "{}: log at index {} carries chain id {:?}",
                        path.display(),
                        log.log_index.0,
                        log.chain_id
                    )));
                }
                if log.block_number != data.block.number {
                    return Err(ChainError::Inconsistent(format!(
                        "{}: log at index {} claims block {:?}",
                        path.display(),
                        log.log_index.0,
                        log.block_number
                    )));
                }
                if log.tx_index != receipt.tx_index || log.tx_hash != receipt.tx_hash {
                    return Err(ChainError::Inconsistent(format!(
                        "{}: log at index {} declares tx {:?} but sits under receipt tx {:?}",
                        path.display(),
                        log.log_index.0,
                        log.tx_index,
                        receipt.tx_index
                    )));
                }
                logs.push(log);
            }
        }

        // The chain gives each log one index per block, so ordering a block's
        // logs by (transaction, log) and by (log) must produce the same
        // sequence. A file that breaks that rule cannot be replayed honestly.
        let mut by_position = logs.clone();
        by_position.sort();
        let mut by_log_index = by_position.clone();
        by_log_index.sort_by_key(|log| log.log_index);
        if by_log_index != by_position {
            return Err(ChainError::Inconsistent(format!(
                "{}: log indexes disagree with transaction order",
                path.display()
            )));
        }
        for pair in by_log_index.windows(2) {
            if pair[0].log_index == pair[1].log_index {
                return Err(ChainError::Inconsistent(format!(
                    "{}: log index {} appears twice in one block",
                    path.display(),
                    pair[0].log_index.0
                )));
            }
        }
        Ok(())
    }

    pub fn available_blocks(&self) -> Vec<BlockNumber> {
        self.blocks.keys().copied().collect()
    }
}

#[async_trait]
impl ChainAdapter for RecordedChainAdapter {
    fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    async fn latest_block(&self) -> Result<BlockNumber> {
        self.blocks
            .keys()
            .next_back()
            .copied()
            .ok_or_else(|| ChainError::MissingData("no recorded blocks".to_string()))
    }

    async fn get_block(&self, number: BlockNumber) -> Result<ChainBlock> {
        self.blocks
            .get(&number)
            .map(|b| b.block.clone())
            .ok_or_else(|| {
                ChainError::MissingData(format!("recorded block {} is absent", number.0))
            })
    }

    async fn get_block_data(&self, number: BlockNumber) -> Result<BlockData> {
        self.blocks.get(&number).cloned().ok_or_else(|| {
            ChainError::MissingData(format!("recorded block {} is absent", number.0))
        })
    }

    async fn get_logs(&self, filter: LogFilter) -> Result<Vec<ChainLog>> {
        let mut logs = Vec::new();
        for (number, block) in self.blocks.range(filter.from_block..=filter.to_block) {
            let _ = number;
            for receipt in &block.receipts {
                for log in &receipt.logs {
                    if !filter.addresses.is_empty() && !filter.addresses.contains(&log.address) {
                        continue;
                    }
                    if !filter
                        .topics
                        .iter()
                        .enumerate()
                        .all(|(i, slot)| match slot {
                            None => true,
                            Some(list) => log.topics.get(i).is_some_and(|t| list.contains(t)),
                        })
                    {
                        continue;
                    }
                    logs.push(log.clone());
                }
            }
        }
        logs.sort();
        Ok(logs)
    }

    async fn call(&self, _at: BlockNumber, request: &CallRequest) -> Result<Bytes> {
        Err(ChainError::MissingData(format!(
            "recorded data cannot serve eth_call for {:?}",
            request.to
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use alloy_primitives::{address, Address, B256};

    use evm_core::{LogIndex, TxHash, TxIndex};

    use super::*;
    use crate::types::{ChainReceipt, ChainTransaction};

    const CHAIN: ChainId = ChainId(91342);
    const POOL: Address = address!("0x3978e57bbceb7666d54a03551c03691f897f6092");

    fn tx_hash(index: u64) -> B256 {
        B256::left_padding_from(&index.to_be_bytes())
    }

    fn log(block: u64, tx_index: u64, log_index: u64) -> ChainLog {
        ChainLog {
            chain_id: CHAIN,
            block_number: BlockNumber(block),
            tx_hash: TxHash(tx_hash(tx_index)),
            tx_index: TxIndex(tx_index),
            log_index: LogIndex(log_index),
            address: POOL,
            topics: vec![],
            data: Bytes::new(),
        }
    }

    fn receipt(block: u64, tx_index: u64, logs: Vec<ChainLog>) -> ChainReceipt {
        ChainReceipt {
            tx_hash: TxHash(tx_hash(tx_index)),
            tx_index: TxIndex(tx_index),
            block_number: BlockNumber(block),
            status: true,
            logs,
        }
    }

    fn block_data(number: u64, receipts: Vec<ChainReceipt>) -> BlockData {
        let transactions: Vec<ChainTransaction> = receipts
            .iter()
            .map(|r| ChainTransaction {
                hash: r.tx_hash,
                tx_index: r.tx_index,
                from: None,
                to: Some(POOL),
                value: alloy_primitives::U256::ZERO,
                input: Bytes::new(),
            })
            .collect();
        BlockData {
            block: ChainBlock {
                chain_id: CHAIN,
                number: BlockNumber(number),
                hash: tx_hash(number),
                parent_hash: tx_hash(number - 1),
                timestamp: 0,
                transaction_count: transactions.len(),
            },
            transactions,
            receipts,
        }
    }

    fn validate(data: &BlockData) -> Result<()> {
        RecordedChainAdapter::validate(data, CHAIN, Path::new("in-memory.json"))
    }

    #[test]
    fn a_block_stored_in_any_order_is_still_a_valid_block() {
        // Execution order is carried by the indexes, not by the arrays, so a
        // file whose receipts are scrambled is honest data.
        let in_order = vec![
            receipt(7, 0, vec![log(7, 0, 4)]),
            receipt(7, 1, vec![log(7, 1, 9), log(7, 1, 10)]),
            receipt(7, 2, vec![log(7, 2, 15)]),
        ];
        let scrambled = vec![
            in_order[2].clone(),
            in_order[0].clone(),
            in_order[1].clone(),
        ];
        assert!(validate(&block_data(7, in_order)).is_ok());
        assert!(validate(&block_data(7, scrambled)).is_ok());
    }

    #[test]
    fn a_log_index_used_twice_in_one_block_is_refused() {
        let data = block_data(
            7,
            vec![
                receipt(7, 0, vec![log(7, 0, 4)]),
                receipt(7, 1, vec![log(7, 1, 4)]),
            ],
        );
        assert!(matches!(validate(&data), Err(ChainError::Inconsistent(_))));
    }

    #[test]
    fn log_indexes_have_to_follow_transaction_order() {
        // Second transaction reporting an earlier log index than the first:
        // impossible on a real chain, so this file was assembled, not observed.
        let data = block_data(
            7,
            vec![
                receipt(7, 0, vec![log(7, 0, 5)]),
                receipt(7, 1, vec![log(7, 1, 3)]),
            ],
        );
        assert!(matches!(validate(&data), Err(ChainError::Inconsistent(_))));
    }

    #[test]
    fn a_log_cannot_claim_a_different_transaction_than_its_receipt() {
        let mut moved = log(7, 1, 5);
        moved.tx_hash = TxHash(tx_hash(9));
        let data = block_data(7, vec![receipt(7, 1, vec![moved])]);
        assert!(matches!(validate(&data), Err(ChainError::Inconsistent(_))));
    }

    #[test]
    fn records_from_another_chain_or_block_are_refused() {
        let mut foreign_log = log(7, 0, 0);
        foreign_log.chain_id = ChainId(1);
        let data = block_data(7, vec![receipt(7, 0, vec![foreign_log])]);
        assert!(matches!(validate(&data), Err(ChainError::Inconsistent(_))));

        let mut foreign_receipt = receipt(8, 0, vec![log(8, 0, 0)]);
        foreign_receipt.block_number = BlockNumber(8);
        let data = block_data(7, vec![foreign_receipt]);
        assert!(matches!(validate(&data), Err(ChainError::Inconsistent(_))));

        let data = block_data(7, vec![receipt(7, 0, vec![log(7, 0, 0)])]);
        assert!(matches!(
            RecordedChainAdapter::validate(&data, ChainId(1), Path::new("in-memory.json")),
            Err(ChainError::Inconsistent(_))
        ));
    }

    #[tokio::test]
    async fn a_written_block_round_trips_and_orders_its_logs() {
        let dir = std::env::temp_dir().join(format!("evm-chain-recorded-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let data = block_data(
            7,
            vec![
                receipt(7, 1, vec![log(7, 1, 9), log(7, 1, 10)]),
                receipt(7, 0, vec![log(7, 0, 4)]),
            ],
        );
        std::fs::write(
            dir.join("block-7.json"),
            serde_json::to_string(&data).expect("serialize"),
        )
        .expect("write");

        let adapter = RecordedChainAdapter::load(&dir, CHAIN).expect("loads");
        assert_eq!(adapter.chain_id(), CHAIN);
        assert_eq!(adapter.available_blocks(), vec![BlockNumber(7)]);
        assert_eq!(adapter.latest_block().await.unwrap(), BlockNumber(7));
        let read = adapter.get_block_data(BlockNumber(7)).await.expect("read");
        let indexes: Vec<u64> = read.ordered_logs().iter().map(|l| l.log_index.0).collect();
        assert_eq!(indexes, vec![4, 9, 10]);

        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_missing_directory_is_an_io_error() {
        let dir = std::env::temp_dir().join("evm-chain-recorded-definitely-absent");
        assert!(matches!(
            RecordedChainAdapter::load(&dir, CHAIN),
            Err(ChainError::Io(_))
        ));
    }
}
