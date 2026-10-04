//! The V2-shaped constant-product pair: `Sync` publishes reserves, `Swap`
//! publishes flow, `PairCreated` names a candidate.
//!
//! Three rules drive this adapter:
//!
//! 1. An emitter is only treated as a pool when the registry attests it. A
//!    `Sync`-shaped log from an unattested address is `Ok(None)`, never an
//!    implicit pool registration — event shape alone is not identity evidence,
//!    and this chain demonstrably contains non-V2 AMMs that emit the same shape.
//! 2. Only `Sync` produces reserves. `Swap` decodes to flow and can never be
//!    turned into a reserve by this code path.
//! 3. `PairCreated` decodes to a *claim*, and a claim writes nothing. This is the
//!    one log whose emitter is not required to be attested — a factory announcing
//!    a pair is how discovery starts — and the event it produces is fed to
//!    `evm-discovery`, which has to verify the address before the registry ever
//!    hears of it. M9.1 §6: `Raw Log -> PairCreated -> CandidatePool`, never
//!    `Raw Log -> Registry.attest()`.

use alloy_primitives::{Bytes, U256};

use evm_chain::ChainLog;
use evm_core::{PoolId, ProtocolId, TokenId};

use crate::adapter::ProtocolAdapter;
use crate::error::{ProtocolError, Result};
use crate::event::{LogPosition, PoolCreatedEvent, ProtocolEvent, SwapEvent, SyncEvent};
use crate::registry::{PoolAttestation, Registry};
use crate::signatures::{data_address, padded_topic, topic_address, word, V2Topics};

/// Identity of the protocol family this adapter decodes.
pub const PROTOCOL_NAME: &str = "v2-compatible";

/// `Sync(uint112 reserve0, uint112 reserve1)`: one topic, two words.
const SYNC_DATA_LEN: usize = 64;
/// `Swap(address,uint256,uint256,uint256,uint256,address)`: four words.
const SWAP_DATA_LEN: usize = 128;
/// The number of leading zero bytes a 32-byte word must have to be a `uint112`.
const UINT112_PADDING: usize = 18;
/// The declared declaration: `pair` is `indexed`, so it arrives as a topic and
/// only `pairIndex` is data.
const PAIR_CREATED_INDEXED_PAIR_TOPICS: usize = 4;
/// What every `PairCreated` log on this chain actually looks like: `pair` is a
/// data word and `pairIndex` the word after it.
/// `data/evidence/m7/probe-pair-created-log-shape.json` counted 1,030 of them and
/// zero of the declared shape. Indexed-ness is not part of topic0, so the hash
/// cannot tell the two apart and only the log itself can.
const PAIR_CREATED_INLINE_PAIR_TOPICS: usize = 3;

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

    /// `PairCreated` — the factory's claim that an address is one of its pairs.
    ///
    /// Deliberately *not* gated on the registry the way `Sync` and `Swap` are:
    /// the whole point of this event is to name an address nobody has verified
    /// yet. What it produces is a candidate, and every check that turns a
    /// candidate into a pool lives in `evm-discovery`. No emitter allowlist is
    /// consulted here either (M9.1 §8/§15): the factory address is kept as
    /// provenance, and provenance is not trust.
    fn decode_pair_created(&self, log: &ChainLog) -> Result<Option<ProtocolEvent>> {
        let token0 = padded_topic(&log.topics, 1)?;
        let token1 = padded_topic(&log.topics, 2)?;
        let (pair, pair_index) = match log.topics.len() {
            PAIR_CREATED_INLINE_PAIR_TOPICS => {
                if log.data.len() != 2 * 32 {
                    return Err(ProtocolError::MalformedLog(format!(
                        "PairCreated with the pair as a data word carries 64 bytes, log has {}",
                        log.data.len()
                    )));
                }
                (data_address(&log.data, 0)?, word(&log.data, 1)?)
            }
            PAIR_CREATED_INDEXED_PAIR_TOPICS => {
                if log.data.len() != 32 {
                    return Err(ProtocolError::MalformedLog(format!(
                        "PairCreated with an indexed pair carries 32 bytes, log has {}",
                        log.data.len()
                    )));
                }
                (padded_topic(&log.topics, 3)?, word(&log.data, 0)?)
            }
            other => {
                return Err(ProtocolError::MalformedLog(format!(
                    "PairCreated carries {} or {} topics, log has {other}",
                    PAIR_CREATED_INLINE_PAIR_TOPICS, PAIR_CREATED_INDEXED_PAIR_TOPICS
                )))
            }
        };
        Ok(Some(ProtocolEvent::PoolCreated(PoolCreatedEvent {
            position: Self::position(log),
            factory: log.address,
            pool: PoolId::new(log.chain_id, pair),
            token0: TokenId::new(log.chain_id, token0),
            token1: TokenId::new(log.chain_id, token1),
            pair_index,
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
        } else if topic0 == self.topics.pair_created {
            self.decode_pair_created(log)
        } else {
            // Mint / Burn / Transfer / Approval and anything else in the same
            // event family are not state sources in v0.1.
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
        for topic0 in [topics.mint, topics.burn, topics.transfer] {
            assert_eq!(
                adapter.decode_log(&log(POOL, vec![topic0], vec![])),
                Ok(None)
            );
        }
        assert_eq!(adapter.decode_log(&log(POOL, vec![], vec![])), Ok(None));
    }

    /// The shape every `PairCreated` on this chain emits: `pair` and `pairIndex`
    /// both in data, so three topics.
    fn pair_created_inline(
        factory: Address,
        token_a: Address,
        token_b: Address,
        pair: Address,
    ) -> ChainLog {
        let topics = V2Topics::default();
        let data = [topic_word(pair).to_vec(), word_bytes(41u128).to_vec()].concat();
        log(
            factory,
            vec![
                topics.pair_created,
                topic_word(token_a),
                topic_word(token_b),
            ],
            data,
        )
    }

    /// The declared shape: `pair` indexed as a fourth topic, only `pairIndex` in
    /// data.
    fn pair_created_indexed(
        factory: Address,
        token_a: Address,
        token_b: Address,
        pair: Address,
    ) -> ChainLog {
        let topics = V2Topics::default();
        log(
            factory,
            vec![
                topics.pair_created,
                topic_word(token_a),
                topic_word(token_b),
                topic_word(pair),
            ],
            word_bytes(41u128).to_vec(),
        )
    }

    #[test]
    fn pair_created_with_the_inline_pair_decodes() {
        let adapter = V2Adapter::new(registry_with_attested_pool());
        let decoded = adapter
            .decode_log(&pair_created_inline(STRANGER, TOKEN0, TOKEN1, POOL))
            .expect("decodable")
            .expect("this protocol");
        match decoded {
            ProtocolEvent::PoolCreated(e) => {
                assert_eq!(e.factory, STRANGER);
                assert_eq!(e.pool, PoolId::new(CHAIN, POOL));
                assert_eq!(e.token0, TokenId::new(CHAIN, TOKEN0));
                assert_eq!(e.token1, TokenId::new(CHAIN, TOKEN1));
                assert_eq!(e.pair_index, U256::from(41u128));
                assert_eq!(e.position.block_number, BlockNumber(37258929));
                assert_eq!(e.position.log_index, LogIndex(7));
            }
            other => panic!("a PairCreated must not decode as {other:?}"),
        }
    }

    #[test]
    fn pair_created_with_an_indexed_pair_decodes() {
        let adapter = V2Adapter::new(registry_with_attested_pool());
        let decoded = adapter
            .decode_log(&pair_created_indexed(STRANGER, TOKEN0, TOKEN1, POOL))
            .expect("decodable")
            .expect("this protocol");
        match decoded {
            ProtocolEvent::PoolCreated(e) => {
                assert_eq!(e.pool, PoolId::new(CHAIN, POOL));
                assert_eq!(e.pair_index, U256::from(41u128));
            }
            other => panic!("a PairCreated must not decode as {other:?}"),
        }
    }

    #[test]
    fn decoding_pair_created_registers_nothing() {
        // M9.1 §6: `Raw Log -> PairCreated -> CandidatePool`, never
        // `Raw Log -> Registry.attest()`. The adapter's registry is the only
        // place a pool can be known, and a creation log must leave it untouched.
        let adapter = V2Adapter::new(registry_with_attested_pool());
        let candidate = address!("0x5a15f14aa209d0eb8c9c4b779f1c8c9b0b6e0d21");
        let before = adapter.registry().pools.len();
        adapter
            .decode_log(&pair_created_inline(STRANGER, TOKEN0, TOKEN1, candidate))
            .expect("decodable");
        assert!(!adapter.is_pool(PoolId::new(CHAIN, candidate)));
        assert_eq!(adapter.registry().pools.len(), before);
        assert!(adapter.attestation(PoolId::new(CHAIN, candidate)).is_none());
    }

    #[test]
    fn malformed_pair_created_is_an_error_not_an_ignored_log() {
        let adapter = V2Adapter::new(registry_with_attested_pool());
        let topics = V2Topics::default();

        // Inline shape with only the pair word — the index is missing.
        let short_data = log(
            STRANGER,
            vec![topics.pair_created, topic_word(TOKEN0), topic_word(TOKEN1)],
            topic_word(POOL).to_vec(),
        );
        assert!(matches!(
            adapter.decode_log(&short_data),
            Err(ProtocolError::MalformedLog(_))
        ));

        // Indexed shape with the extra pair word still in data.
        let long_data = log(
            STRANGER,
            vec![
                topics.pair_created,
                topic_word(TOKEN0),
                topic_word(TOKEN1),
                topic_word(POOL),
            ],
            [word_bytes(41u128), word_bytes(42u128)].concat(),
        );
        assert!(matches!(
            adapter.decode_log(&long_data),
            Err(ProtocolError::MalformedLog(_))
        ));

        // Neither shape: two topics cannot name both tokens and a pair.
        let thin = log(
            STRANGER,
            vec![topics.pair_created, topic_word(TOKEN0)],
            word_bytes(41u128).to_vec(),
        );
        assert!(matches!(
            adapter.decode_log(&thin),
            Err(ProtocolError::MalformedLog(_))
        ));

        // An address topic with garbage in the padding bytes is not an address.
        let mut unpadded = topic_word(TOKEN1);
        unpadded[11] = 0xff;
        let dirty = log(
            STRANGER,
            vec![
                topics.pair_created,
                topic_word(TOKEN0),
                unpadded,
                topic_word(POOL),
            ],
            word_bytes(41u128).to_vec(),
        );
        assert!(matches!(
            adapter.decode_log(&dirty),
            Err(ProtocolError::MalformedLog(_))
        ));
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
