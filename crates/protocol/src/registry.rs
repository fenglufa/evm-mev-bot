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
    #[error("pool {0} is attested twice with different content ({1})")]
    Conflict(String, String),
    #[error("pool {0} attests the same token on both sides ({1})")]
    DegeneratePair(String, String),
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

    /// Every `*.json` in the directory, merged into one registry.
    ///
    /// Files are read in sorted order so the merged result never depends on
    /// directory order. A pool that two files attest differently is refused:
    /// two contradictory attestations of one address is a data bug, and merging
    /// them silently would pick a winner by filename.
    pub fn load_dir(dir: &Path) -> std::result::Result<Self, RegistryError> {
        let read = std::fs::read_dir(dir)
            .map_err(|e| RegistryError::Io(format!("{}: {e}", dir.display())))?;
        let mut files: Vec<_> = read
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| path.extension().is_some_and(|e| e == "json"))
            .collect();
        files.sort();
        let mut merged = Self::default();
        for path in files {
            merged.merge(Self::load(&path)?)?;
        }
        Ok(merged)
    }

    pub fn merge(&mut self, other: Self) -> std::result::Result<(), RegistryError> {
        for (id, attestation) in other.pools {
            match self.pools.get(&id) {
                Some(existing) if *existing == attestation => {}
                Some(existing) => {
                    return Err(RegistryError::Conflict(
                        format!("{id:?}"),
                        format!(
                            "one file attests {} [{:?}], another attests {:?} [{:?}]",
                            existing.protocol,
                            existing.token0.address,
                            attestation.protocol,
                            attestation.token0.address
                        ),
                    ))
                }
                None => {
                    self.pools.insert(id, attestation);
                }
            }
        }
        Ok(())
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
            if attestation.token0 == attestation.token1 {
                return Err(RegistryError::DegeneratePair(
                    format!("{id:?}"),
                    format!("{:?}", attestation.token0.address),
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

    #[test]
    fn a_pool_whose_two_tokens_are_the_same_is_refused() {
        let mut a = attestation(true);
        a.token1 = a.token0;
        let pool = a.pool;
        let registry = Registry {
            pools: [(pool, a)].into_iter().collect(),
        };
        assert!(matches!(
            registry.validate(),
            Err(RegistryError::DegeneratePair(_, _))
        ));
    }

    /// A scratch directory holding registry files, named so two test threads
    /// never share one.
    fn registry_dir(tag: &str, files: &[(&str, &[PoolAttestation])]) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("evm-registry-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create registry directory");
        for (name, pools) in files {
            let value = serde_json::json!({ "pools": pools });
            std::fs::write(
                dir.join(name),
                serde_json::to_string_pretty(&value).expect("serialize"),
            )
            .expect("write registry file");
        }
        dir
    }

    #[test]
    fn several_files_merge_into_one_registry() {
        let left = attestation(true);
        let mut right = attestation(true);
        right.pool = PoolId::new(
            ChainId(91342),
            address!("0xcaafb95fc292c10a526f03fa480407bb438dac67"),
        );
        let dir = registry_dir(
            "merge",
            &[
                ("a.json", std::slice::from_ref(&left)),
                ("b.json", std::slice::from_ref(&right)),
            ],
        );
        let merged = Registry::load_dir(&dir).expect("merge two files");
        assert_eq!(merged.pools.len(), 2);
        assert!(merged.is_pool(left.pool));
        assert!(merged.is_pool(right.pool));
        std::fs::remove_dir_all(&dir).ok();

        // One attestation repeated across files is a duplicate, not a conflict.
        let dir = registry_dir(
            "duplicate",
            &[
                ("a.json", std::slice::from_ref(&left)),
                ("b.json", std::slice::from_ref(&left)),
            ],
        );
        let merged = Registry::load_dir(&dir).expect("duplicate merges");
        assert_eq!(merged.pools.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn two_files_that_attest_one_pool_differently_are_refused() {
        let left = attestation(true);
        let mut right = attestation(true);
        // Same pool address, different token side — the conflict a silent merge
        // would resolve by filename order.
        right.token0 = TokenId::new(
            ChainId(91342),
            address!("0x0000000000000000000000000000000000000111"),
        );
        let dir = registry_dir("conflict", &[("a.json", &[left]), ("b.json", &[right])]);
        let err = Registry::load_dir(&dir).expect_err("conflicting files refused");
        assert!(
            matches!(err, RegistryError::Conflict(_, _)),
            "expected a conflict, got {err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_committed_registry_files_all_validate() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/protocols")
            .canonicalize()
            .expect("registry directory");
        let registry = Registry::load_dir(&dir).expect("committed registry loads");
        assert_eq!(
            registry.pools.len(),
            4,
            "one attestation per evidenced pool, nothing more"
        );

        // Counted, not assumed: each attestation stands on its own evidence and
        // each names a distinct pair on one chain.
        let mut pairs = std::collections::BTreeSet::new();
        for attestation in registry.pools.values() {
            assert!(
                attestation.evidence.is_complete(),
                "{} lacks evidence",
                attestation.pool.address
            );
            for refs in [
                &attestation.evidence.identity,
                &attestation.evidence.tokens,
                &attestation.evidence.state,
            ] {
                assert!(
                    !refs.is_empty() && refs.iter().all(|e| e.block_number.is_some()),
                    "{} has an unevidenced claim",
                    attestation.pool.address
                );
            }
            assert_eq!(attestation.token0.chain_id, attestation.pool.chain_id);
            assert_eq!(attestation.token1.chain_id, attestation.pool.chain_id);
            assert_ne!(attestation.token0, attestation.token1);
            assert!(
                pairs.insert((attestation.token0, attestation.token1)),
                "two pools attest the same pair"
            );
        }
    }
}
