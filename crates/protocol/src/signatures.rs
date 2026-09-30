//! Protocol event signatures, derived from their canonical Solidity declaration
//! and pinned against what the chain actually emitted.
//!
//! The `assert_eq!` below is the point of this module: a topic0 constant copied
//! from memory is a claim, while `sol!` computes keccak256 of the signature
//! string. Requiring the computed value to equal an observed value is what makes
//! the identity a fact rather than an assumption.

use alloy_primitives::B256;
use alloy_sol_types::{sol, SolEvent};

use crate::error::{ProtocolError, Result};

sol! {
    event Sync(uint112 reserve0, uint112 reserve1);
    event Swap(address indexed sender, uint256 amount0In, uint256 amount1In, uint256 amount0Out, uint256 amount1Out, address indexed to);
    event Mint(address indexed sender, uint256 amount0, uint256 amount1);
    event Burn(address indexed sender, uint256 amount0, uint256 amount1, address indexed to);
    event PairCreated(address indexed token0, address indexed token1, address indexed pair, uint256 pairIndex);
    event Transfer(address indexed from, address indexed to, uint256 value);
}

/// keccak256("getReserves()")
pub static GET_RESERVES_SELECTOR: std::sync::LazyLock<[u8; 4]> =
    std::sync::LazyLock::new(|| selector_of("getReserves()"));
/// keccak256("token0()")
pub static TOKEN0_SELECTOR: std::sync::LazyLock<[u8; 4]> =
    std::sync::LazyLock::new(|| selector_of("token0()"));
/// keccak256("token1()")
pub static TOKEN1_SELECTOR: std::sync::LazyLock<[u8; 4]> =
    std::sync::LazyLock::new(|| selector_of("token1()"));
/// keccak256("balanceOf(address)")
pub static BALANCE_OF_SELECTOR: std::sync::LazyLock<[u8; 4]> =
    std::sync::LazyLock::new(|| selector_of("balanceOf(address)"));

/// keccak256 of a Solidity signature string. `keccak256` from alloy is the same
/// function the EVM uses, so the result is the selector/topic the chain uses.
pub fn selector_of(signature: &str) -> [u8; 4] {
    use alloy_primitives::keccak256;
    let hash = keccak256(signature.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

/// The topic0 values this adapter can recognize, each derived from the
/// declarations above.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V2Topics {
    pub sync: B256,
    pub swap: B256,
    pub mint: B256,
    pub burn: B256,
    pub pair_created: B256,
    pub transfer: B256,
}

impl Default for V2Topics {
    fn default() -> Self {
        Self {
            sync: Sync::SIGNATURE_HASH,
            swap: Swap::SIGNATURE_HASH,
            mint: Mint::SIGNATURE_HASH,
            burn: Burn::SIGNATURE_HASH,
            pair_created: PairCreated::SIGNATURE_HASH,
            transfer: Transfer::SIGNATURE_HASH,
        }
    }
}

/// Extract up to 32 bytes of a big-endian ABI word as `U256`.
pub fn word(data: &[u8], index: usize) -> Result<alloy_primitives::U256> {
    let start = index * 32;
    let end = start + 32;
    if data.len() < end {
        return Err(ProtocolError::MalformedLog(format!(
            "log data is {} bytes, needs at least {end} for word {index}",
            data.len()
        )));
    }
    Ok(alloy_primitives::U256::from_be_slice(&data[start..end]))
}

/// Address carried in a topic word.
pub fn topic_address(topics: &[B256], index: usize) -> Result<alloy_primitives::Address> {
    let topic = topics
        .get(index)
        .ok_or_else(|| ProtocolError::MalformedLog(format!("topic {index} is missing")))?;
    Ok(alloy_primitives::Address::from_slice(&topic[12..]))
}

#[cfg(test)]
mod tests {
    use alloy_primitives::b256;

    use super::*;

    /// Every topic0 below is a value observed in the recorded capture; none is
    /// copied from an ABI database. Census over the 95,670 receipt logs
    /// (54,686 transactions, 2,575 contract addresses) in
    /// `/Volumes/superfs/giwa-mev/data/evidence/v0.4.3.1/semantic-events.json`,
    /// chain 91342:
    /// - `Sync`     : 2,345 logs, from three different addresses —
    ///   0xad153c84.. (1,629), 0x3978e57b.. (698), 0xcaafb95f.. (18).
    ///   Example: contract 0x3978e57b.., tx
    ///   0x0021b1b3197efe3219b2db23729dc50253e205eaed303f1d03070b1c240d71ae.
    /// - `Swap`     :   371 logs. Emitters 0x3978e57b.. (353), 0xcaafb95f.. (18).
    /// - `Mint`     :   173 logs, only from 0x3978e57b...
    /// - `Burn`     :   172 logs, only from 0x3978e57b...
    /// - `Transfer` : 22,035 logs chain-wide, 517 of them from 0x3978e57b...
    ///
    /// The point of pinning these: all three `Sync` emitters share one topic0,
    /// yet 0xad153c84.. — the majority of them — emits no Swap, Mint, Burn or
    /// Transfer at all, and answering `token0()` at block 37257255 reverts.
    /// Same topic0 does not mean same protocol, so identity comes from the
    /// registry, not from event shape.
    #[test]
    fn computed_topic0_matches_what_the_chain_emitted() {
        let topics = V2Topics::default();
        assert_eq!(
            topics.sync,
            b256!("0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1")
        );
        assert_eq!(
            topics.swap,
            b256!("0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822")
        );
        assert_eq!(
            topics.mint,
            b256!("0x4c209b5fc8ad50758f13e2e1088ba56a560dff690a1c6fef26394f4c03821c4f")
        );
        assert_eq!(
            topics.burn,
            b256!("0xdccd412f0b1252819cb1fd330b93224ca42612892bb3f4f789976e6d81936496")
        );
        assert_eq!(
            topics.transfer,
            b256!("0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef")
        );
    }

    #[test]
    fn sync_is_strictly_the_uint112_declaration() {
        // If the chain had used `Sync(uint256,uint256)` the topic0 would differ.
        let observed = b256!("0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1");
        assert_eq!(Sync::SIGNATURE_HASH, observed, "uint112 declaration");
        sol! {
            event SyncUint256(uint256 reserve0, uint256 reserve1);
        }
        assert_ne!(
            SyncUint256::SIGNATURE_HASH,
            observed,
            "uint256 declaration must NOT match the observed topic0"
        );
    }

    #[test]
    fn function_selectors_are_derived_not_assumed() {
        assert_eq!(
            selector_of("getReserves()"),
            [0x09, 0x02, 0xf1, 0xac],
            "the pool answers 0x0902f1ac"
        );
        assert_eq!(selector_of("token0()"), [0x0d, 0xfe, 0x16, 0x81]);
        assert_eq!(selector_of("token1()"), [0xd2, 0x12, 0x20, 0xa7]);
        assert_eq!(selector_of("balanceOf(address)"), [0x70, 0xa0, 0x82, 0x31]);
    }

    /// The recorded logs are `(topic count, data length)` pairs, and every
    /// declaration above has to agree with them: Sync (1 topic, 64 bytes),
    /// Swap (3, 128), Mint (2, 64), Burn (3, 64).
    ///
    /// `Burn` is the reason this test exists: the plain
    /// `Burn(address,uint256,uint256)` declaration hashes to 0x49995e5d.., which
    /// is not what this chain emits. The chain's Burn logs carry a third topic,
    /// so the declaration must end with one more address parameter — only the
    /// data could say that.
    #[test]
    fn declarations_match_the_observed_log_shapes() {
        assert_eq!(Sync::SIGNATURE, "Sync(uint112,uint112)");
        assert_eq!(
            Swap::SIGNATURE,
            "Swap(address,uint256,uint256,uint256,uint256,address)"
        );
        assert_eq!(Mint::SIGNATURE, "Mint(address,uint256,uint256)");
        assert_eq!(Burn::SIGNATURE, "Burn(address,uint256,uint256,address)");
        assert_eq!(
            selector_of("Burn(address,uint256,uint256)"),
            [0x49, 0x99, 0x5e, 0x5d],
            "the shorter Burn declaration is a different event"
        );
    }
}
