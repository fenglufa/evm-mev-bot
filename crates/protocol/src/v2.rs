//! The V2-shaped constant-product pair: `Sync` publishes reserves, `Swap`
//! publishes flow.
//!
//! Two rules drive this adapter:
//!
//! 1. An emitter is only treated as a pool when the registry attests it. A
//!    `Sync`-shaped log from an unattested address is `Ok(None)`, never an
//!    implicit pool registration — event shape alone is not identity evidence,
//!    and this chain demonstrably contains non-V2 AMMs that emit the same shape.
//! 2. Only `Sync` produces reserves. `Swap` decodes to flow and can never be
//!    turned into a reserve by this code path.

use alloy_primitives::{Bytes, U256};

use evm_chain::ChainLog;
use evm_core::{PoolId, ProtocolId};

use crate::adapter::ProtocolAdapter;
use crate::error::{ProtocolError, Result};
use crate::event::{LogPosition, ProtocolEvent, SwapEvent, SyncEvent};
use crate::registry::{PoolAttestation, Registry};
use crate::signatures::{topic_address, word, V2Topics};

/// Identity of the protocol family this adapter decodes.
pub const PROTOCOL_NAME: &str = "v2-compatible";

/// `Sync(uint112 reserve0, uint112 reserve1)`: one topic, two words.
const SYNC_DATA_LEN: usize = 64;
/// `Swap(address,uint256,uint256,uint256,uint256,address)`: four words.
const SWAP_DATA_LEN: usize = 128;
/// The number of leading zero bytes a 32-byte word must have to be a `uint112`.
const UINT112_PADDING: usize = 18;

pub struct V2Adapter {
    protocol: ProtocolId,
    topics: V2Topics,
    registry: Registry,
}

impl V2Adapter {
    pub fn new(registry: Registry) -> Self {
        Self {
            protocol: ProtocolId::new(PROTOCOL_NAME),
            topics: V2Topics::default(),
            registry,
        }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    fn attested(&self, log: &ChainLog) -> Option<&PoolAttestation> {
        self.registry.get(PoolId::new(log.chain_id, log.address))
    }

    fn position(log: &ChainLog) -> LogPosition {
        LogPosition {
            chain_id: log.chain_id,
            block_number: log.block_number,
            tx_hash: log.tx_hash,
            tx_index: log.tx_index,
            log_index: log.log_index,
        }
    }

    /// A `uint112` value occupies the low 14 bytes of its word; anything in the
    /// other 18 bytes means this log is not the event it claims to be.
    fn word_u112(data: &Bytes, index: usize) -> Result<U256> {
        let value = word(data, index)?;
        let start = index * 32;
        if data[start..start + UINT112_PADDING] != [0u8; UINT112_PADDING] {
            return Err(ProtocolError::MalformedLog(format!(
                "word {index} does not fit uint112: {value}"
            )));
        }
        Ok(value)
    }

    fn decode_sync(&self, log: &ChainLog) -> Result<Option<ProtocolEvent>> {
        if self.attested(log).is_none() {
            return Ok(None);
        }
        if log.topics.len() != 1 {
            return Err(ProtocolError::MalformedLog(format!(
                "Sync carries 1 topic, log has {}",
                log.topics.len()
            )));
        }
        if log.data.len() != SYNC_DATA_LEN {
            return Err(ProtocolError::MalformedLog(format!(
                "Sync carries {} bytes, log has {}",
                SYNC_DATA_LEN,
                log.data.len()
            )));
        }
        Ok(Some(ProtocolEvent::Sync(SyncEvent {
            position: Self::position(log),
            pool: PoolId::new(log.chain_id, log.address),
            reserve0: Self::word_u112(&log.data, 0)?,
            reserve1: Self::word_u112(&log.data, 1)?,
        })))
    }

    fn decode_swap(&self, log: &ChainLog) -> Result<Option<ProtocolEvent>> {
        if self.attested(log).is_none() {
            return Ok(None);
        }
        if log.topics.len() != 3 {
            return Err(ProtocolError::MalformedLog(format!(
                "Swap carries 3 topics, log has {}",
                log.topics.len()
            )));
        }
        if log.data.len() != SWAP_DATA_LEN {
            return Err(ProtocolError::MalformedLog(format!(
                "Swap carries {} bytes, log has {}",
                SWAP_DATA_LEN,
                log.data.len()
            )));
        }
        Ok(Some(ProtocolEvent::Swap(SwapEvent {
            position: Self::position(log),
            pool: PoolId::new(log.chain_id, log.address),
            sender: Some(topic_address(&log.topics, 1)?),
            to: Some(topic_address(&log.topics, 2)?),
            amount0_in: word(&log.data, 0)?,
            amount1_in: word(&log.data, 1)?,
            amount0_out: word(&log.data, 2)?,
            amount1_out: word(&log.data, 3)?,
        })))
    }
}

impl ProtocolAdapter for V2Adapter {
    fn protocol_id(&self) -> ProtocolId {
        self.protocol.clone()
    }

    fn decode_log(&self, log: &ChainLog) -> Result<Option<ProtocolEvent>> {
        let topic0 = match log.topics.first() {
            Some(topic) => *topic,
            // An anonymous log cannot be attributed to a protocol at all.
            None => return Ok(None),
        };
        if topic0 == self.topics.sync {
            self.decode_sync(log)
        } else if topic0 == self.topics.swap {
            self.decode_swap(log)
        } else {
            // Mint / Burn / Transfer / Approval and anything else in the same
            // event family are not state sources in v0.1. `PairCreated` is
            // included here: no factory has been attested, so this adapter has
            // no authority to register pools from a creation log.
            Ok(None)
        }
    }

    fn is_pool(&self, pool: PoolId) -> bool {
        self.registry.is_pool(pool)
    }

    fn attestation(&self, pool: PoolId) -> Option<PoolAttestation> {
        self.registry.get(pool).cloned()
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, b256, Address, B256};

    use super::*;
    use crate::registry::AttestationEvidence;
    use evm_core::{
        BlockNumber, ChainId, EvidenceRef, EvidenceSource, LogIndex, PoolType, TokenId, TxHash,
        TxIndex,
    };

    const CHAIN: ChainId = ChainId(91342);
    const POOL: Address = address!("0x3978e57bbceb7666d54a03551c03691f897f6092");
    const TOKEN0: Address = address!("0x304912af0ce0dd6479735634d567715107bdc0c6");
    const TOKEN1: Address = address!("0x4200000000000000000000000000000000000006");
    /// A second AMM on this chain that is *not* V2-shaped; it emits a
    /// `Sync(uint112,uint112)`-identical topic0 but is not attested.
    const STRANGER: Address = address!("0xad153c844ccac3d2ea991170624200e54730be74");

    fn evidence(signature: &str) -> Vec<EvidenceRef> {
        vec![EvidenceRef {
            source: EvidenceSource::ChainLog,
            block_number: Some(BlockNumber(37257235)),
            transaction_hash: None,
            log_index: None,
            signature: Some(signature.to_string()),
        }]
    }

    /// The attested pool, with evidence for all three claims.
    fn registry_with_attested_pool() -> Registry {
        let mut registry = Registry::default();
        registry.attest(PoolAttestation {
            protocol: ProtocolId::new(PROTOCOL_NAME),
            pool: PoolId::new(CHAIN, POOL),
            token0: TokenId::new(CHAIN, TOKEN0),
            token1: TokenId::new(CHAIN, TOKEN1),
            fee: None,
            pool_type: PoolType::ConstantProduct,
            evidence: AttestationEvidence {
                identity: evidence("getReserves() -> 3 words"),
                tokens: evidence("token0() / token1()"),
                state: evidence("Sync(uint112,uint112)"),
            },
        });
        registry
    }

    fn word_bytes(value: u128) -> [u8; 32] {
        U256::from(value).to_be_bytes::<32>()
    }

    fn log(address: Address, topics: Vec<B256>, data: Vec<u8>) -> ChainLog {
        ChainLog {
            chain_id: CHAIN,
            block_number: BlockNumber(37258929),
            tx_hash: TxHash(b256!(
                "0x1111111111111111111111111111111111111111111111111111111111111111"
            )),
            tx_index: TxIndex(3),
            log_index: LogIndex(7),
            address,
            topics,
            data: Bytes::from(data),
        }
    }

    fn sync_log(address: Address, reserve0: u128, reserve1: u128) -> ChainLog {
        let mut data = Vec::new();
        data.extend_from_slice(&word_bytes(reserve0));
        data.extend_from_slice(&word_bytes(reserve1));
        log(address, vec![V2Topics::default().sync], data)
    }

    /// An `address` topic is left-padded to 32 bytes.
    fn topic_word(address: Address) -> B256 {
        let mut word = [0u8; 32];
        word[12..].copy_from_slice(&address.into_array());
        B256::from(word)
    }

    #[test]
    fn sync_from_an_attested_pool_becomes_reserves() {
        let adapter = V2Adapter::new(registry_with_attested_pool());
        let decoded = adapter
            .decode_log(&sync_log(POOL, 1_000, 2_000))
            .expect("decodable")
            .expect("this protocol");
        match decoded {
            ProtocolEvent::Sync(e) => {
                assert_eq!(e.pool, PoolId::new(CHAIN, POOL));
                assert_eq!(e.reserve0, U256::from(1_000u128));
                assert_eq!(e.reserve1, U256::from(2_000u128));
                assert_eq!(e.position.log_index, LogIndex(7));
                assert_eq!(e.position.tx_index, TxIndex(3));
            }
            other => panic!("expected Sync, got {other:?}"),
        }
    }

    #[test]
    fn sync_shaped_log_from_an_unattested_address_is_ignored() {
        let adapter = V2Adapter::new(registry_with_attested_pool());
        assert_eq!(
            adapter.decode_log(&sync_log(STRANGER, 5, 6)),
            Ok(None),
            "an event shape must not register a pool"
        );
        assert!(!adapter.is_pool(PoolId::new(CHAIN, STRANGER)));
    }

    #[test]
    fn the_same_address_on_another_chain_is_not_attested() {
        let adapter = V2Adapter::new(registry_with_attested_pool());
        let mut recorded = sync_log(POOL, 1, 2);
        recorded.chain_id = ChainId(1);
        assert_eq!(adapter.decode_log(&recorded), Ok(None));
    }

    #[test]
    fn sync_values_too_large_for_uint112_are_malformed() {
        let adapter = V2Adapter::new(registry_with_attested_pool());
        // 2^112 does not fit uint112, so the padding bytes are no longer zero.
        let decoded = adapter.decode_log(&sync_log(POOL, 1u128 << 112, 2));
        assert!(
            matches!(decoded, Err(ProtocolError::MalformedLog(_))),
            "{decoded:?}"
        );
    }

    #[test]
    fn sync_with_the_wrong_shape_is_malformed_not_ignored() {
        let adapter = V2Adapter::new(registry_with_attested_pool());
        let topics = V2Topics::default();

        // Right topic0, half the data.
        let truncated = log(POOL, vec![topics.sync], word_bytes(1_000).to_vec());
        assert!(matches!(
            adapter.decode_log(&truncated),
            Err(ProtocolError::MalformedLog(_))
        ));

        // Right data, an extra topic the declaration does not have.
        let with_extra_topic = log(
            POOL,
            vec![topics.sync, topics.sync],
            [word_bytes(1_000), word_bytes(2_000)].concat(),
        );
        assert!(matches!(
            adapter.decode_log(&with_extra_topic),
            Err(ProtocolError::MalformedLog(_))
        ));
    }

    #[test]
    fn swap_decodes_to_flow_and_never_to_reserves() {
        let adapter = V2Adapter::new(registry_with_attested_pool());
        let topics = V2Topics::default();
        let mut data = Vec::new();
        for amount in [10u128, 0, 0, 9] {
            data.extend_from_slice(&word_bytes(amount));
        }
        let decoded = adapter
            .decode_log(&log(
                POOL,
                vec![topics.swap, topic_word(TOKEN1), topic_word(TOKEN0)],
                data,
            ))
            .expect("decodable")
            .expect("this protocol");
        match decoded {
            ProtocolEvent::Swap(e) => {
                assert_eq!(e.amount0_in, U256::from(10u128));
                assert_eq!(e.amount1_out, U256::from(9u128));
                assert_eq!(e.sender, Some(TOKEN1));
                assert_eq!(e.to, Some(TOKEN0));
            }
            other => panic!("a Swap must not decode as {other:?}"),
        }
    }

    #[test]
    fn other_events_in_the_same_family_are_not_state_sources() {
        let adapter = V2Adapter::new(registry_with_attested_pool());
        let topics = V2Topics::default();
        for topic0 in [
            topics.mint,
            topics.burn,
            topics.transfer,
            topics.pair_created,
        ] {
            assert_eq!(
                adapter.decode_log(&log(POOL, vec![topic0], vec![])),
                Ok(None)
            );
        }
        assert_eq!(adapter.decode_log(&log(POOL, vec![], vec![])), Ok(None));
    }

    #[test]
    fn pool_meta_only_exists_for_attested_pools() {
        let adapter = V2Adapter::new(registry_with_attested_pool());
        let meta = adapter
            .attestation(PoolId::new(CHAIN, POOL))
            .expect("attested")
            .to_meta();
        assert_eq!(meta.token0.address, TOKEN0);
        assert_eq!(meta.token1.address, TOKEN1);
        assert_eq!(meta.protocol, ProtocolId::new(PROTOCOL_NAME));
        assert!(meta.fee.is_none(), "fee is not attested in M1");
        assert!(adapter.attestation(PoolId::new(CHAIN, STRANGER)).is_none());
    }
}
