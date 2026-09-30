use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use evm_core::{EvidenceRef, Fee, PoolId, PoolMeta, PoolType, ProtocolId, TokenId};

/// The three separate things an attestation has to justify. A pool entry that
/// cannot name evidence for all three is not a verified pool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestationEvidence {
    pub identity: Vec<EvidenceRef>,
    pub tokens: Vec<EvidenceRef>,
    pub state: Vec<EvidenceRef>,
}

impl AttestationEvidence {
    pub fn is_complete(&self) -> bool {
        !(self.identity.is_empty() || self.tokens.is_empty() || self.state.is_empty())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolAttestation {
    pub protocol: ProtocolId,
    pub pool: PoolId,
    pub token0: TokenId,
    pub token1: TokenId,
    pub fee: Option<Fee>,
    pub pool_type: PoolType,
    pub evidence: AttestationEvidence,
}

impl PoolAttestation {
    pub fn to_meta(&self) -> PoolMeta {
        PoolMeta {
            id: self.pool,
            protocol: self.protocol.clone(),
            token0: self.token0,
            token1: self.token1,
            fee: self.fee,
            pool_type: self.pool_type,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("registry file could not be read: {0}")]
    Io(String),
    #[error("registry file is not valid: {0}")]
    Format(String),
    #[error("pool {0} is attested without evidence ({1})")]
    Unevidenced(String, String),
}

/// Pools the project has verified, and why. Loaded from `data/protocols/*.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    pub pools: HashMap<PoolId, PoolAttestation>,
}

impl Registry {
    pub fn load(path: &Path) -> std::result::Result<Self, RegistryError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| RegistryError::Io(format!("{}: {e}", path.display())))?;
        let file: RegistryFile = serde_json::from_str(&text)
            .map_err(|e| RegistryError::Format(format!("{}: {e}", path.display())))?;
        let registry = Self {
            pools: file.pools.into_iter().map(|a| (a.pool, a)).collect(),
        };
        registry.validate()?;
        Ok(registry)
    }

    /// Refuses to load a registry that asserts a pool with no evidence behind it.
    pub fn validate(&self) -> std::result::Result<(), RegistryError> {
        for (id, attestation) in &self.pools {
            if !attestation.evidence.is_complete() {
                return Err(RegistryError::Unevidenced(
                    format!("{id:?}"),
                    format!(
                        "identity={} tokens={} state={}",
                        attestation.evidence.identity.len(),
                        attestation.evidence.tokens.len(),
                        attestation.evidence.state.len()
                    ),
                ));
            }
            if attestation.token0.chain_id != id.chain_id
                || attestation.token1.chain_id != id.chain_id
            {
                return Err(RegistryError::Unevidenced(
                    format!("{id:?}"),
                    "token chain id does not match pool chain id".to_string(),
                ));
            }
        }
        Ok(())
    }

    pub fn get(&self, pool: PoolId) -> Option<&PoolAttestation> {
        self.pools.get(&pool)
    }

    pub fn is_pool(&self, pool: PoolId) -> bool {
        self.pools.contains_key(&pool)
    }

    pub fn attest(&mut self, attestation: PoolAttestation) {
        self.pools.insert(attestation.pool, attestation);
    }
}

#[derive(Deserialize)]
struct RegistryFile {
    pools: Vec<PoolAttestation>,
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, b256};

    use super::*;
    use evm_core::{BlockNumber, ChainId, EvidenceSource, TxHash};

    fn attestation(complete: bool) -> PoolAttestation {
        let chain = ChainId(91342);
        let pool = PoolId::new(
            chain,
            address!("0x3978e57bbceb7666d54a03551c03691f897f6092"),
        );
        let evidence = AttestationEvidence {
            identity: if complete {
                vec![EvidenceRef {
                    source: EvidenceSource::Bytecode,
                    block_number: Some(BlockNumber(37257235)),
                    transaction_hash: None,
                    log_index: None,
                    signature: Some("0x0902f1ac".to_string()),
                }]
            } else {
                vec![]
            },
            tokens: vec![EvidenceRef {
                source: EvidenceSource::EthCall,
                block_number: Some(BlockNumber(37257235)),
                transaction_hash: None,
                log_index: None,
                signature: Some("token0()".to_string()),
            }],
            state: vec![EvidenceRef {
                source: EvidenceSource::ChainLog,
                block_number: Some(BlockNumber(37257255)),
                transaction_hash: Some(TxHash(b256!(
                    "0x0021b1b3197efe3219b2db23729dc50253e205eaed303f1d03070b1c240d71ae"
                ))),
                log_index: Some(181),
                signature: Some("Sync(uint112,uint112)".to_string()),
            }],
        };
        PoolAttestation {
            protocol: ProtocolId::new("v2-compatible"),
            pool,
            token0: TokenId::new(
                chain,
                address!("0x304912af0ce0dd6479735634d567715107bdc0c6"),
            ),
            token1: TokenId::new(
                chain,
                address!("0x4200000000000000000000000000000000000006"),
            ),
            fee: None,
            pool_type: PoolType::ConstantProduct,
            evidence,
        }
    }

    #[test]
    fn unevidenced_identity_is_rejected() {
        let registry = Registry {
            pools: [(attestation(false).pool, attestation(false))]
                .into_iter()
                .collect(),
        };
        assert!(matches!(
            registry.validate(),
            Err(RegistryError::Unevidenced(_, _))
        ));
    }

    #[test]
    fn complete_attestation_passes() {
        let a = attestation(true);
        let registry = Registry {
            pools: [(a.pool, a)].into_iter().collect(),
        };
        assert!(registry.validate().is_ok());
        assert!(registry.is_pool(PoolId::new(
            ChainId(91342),
            address!("0x3978e57bbceb7666d54a03551c03691f897f6092")
        )));
        assert!(!registry.is_pool(PoolId::new(
            ChainId(1),
            address!("0x3978e57bbceb7666d54a03551c03691f897f6092")
        )));
    }
}
