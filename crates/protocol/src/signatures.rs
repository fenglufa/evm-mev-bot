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
    event Deposit(address indexed dst, uint256 wad);
    event Withdrawal(address indexed src, uint256 wad);
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

/// The topic0 values of the wrapped gas asset, computed from the two declarations above.
///
/// A wrap or an unwrap moves no `Transfer`. `deposit()` mints and `withdraw()` burns, and the
/// only chain-visible statement of either is these two single-topic logs — so an execution
/// audit that reads `Transfer` alone sees the wrapped token leave the wallet and never see it
/// come back, and calls that a disagreement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Weth9Topics {
    pub deposit: B256,
    pub withdrawal: B256,
}

impl Default for Weth9Topics {
    fn default() -> Self {
        Self {
            deposit: Deposit::SIGNATURE_HASH,
            withdrawal: Withdrawal::SIGNATURE_HASH,
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

/// Address carried by one exact 32-byte ABI word, padding included.
///
/// The strict sibling of [`topic_address`]: an `address` occupies the low 20
/// bytes of its word, so anything in the other 12 means this word is not an
/// address at all. `topic_address` truncates and moves on, which is fine for a
/// field nobody trusts, and wrong for one a candidate's identity will be built
/// from — so discovery decodes through here (M9.1 §6: malformed input is a
/// structured error, never a plausible-looking address).
pub fn address_word(word: &[u8]) -> Result<alloy_primitives::Address> {
    if word.len() != 32 {
        return Err(ProtocolError::MalformedLog(format!(
            "an ABI address word is 32 bytes, got {}",
            word.len()
        )));
    }
    if word[..32 - 20] != [0u8; 12] {
        return Err(ProtocolError::MalformedLog(format!(
            "address word is not left-padded: high 12 bytes are {:?}",
            &word[..12]
        )));
    }
    Ok(alloy_primitives::Address::from_slice(&word[12..]))
}

/// The 32-byte word at `index` of an ABI-encoded buffer, read as an address.
pub fn data_address(data: &[u8], index: usize) -> Result<alloy_primitives::Address> {
    let start = index * 32;
    let end = start + 32;
    if data.len() < end {
        return Err(ProtocolError::MalformedLog(format!(
            "log data is {} bytes, needs at least {end} for word {index}",
            data.len()
        )));
    }
    address_word(&data[start..end])
}

/// Address carried in a topic word, padding checked.
///
/// [`topic_address`] and [`address_word`] combined: a topic that exists but is
/// not a left-padded address is a malformed log, not an address with an
/// surprising prefix.
pub fn padded_topic(topics: &[B256], index: usize) -> Result<alloy_primitives::Address> {
    let topic = topics
        .get(index)
        .ok_or_else(|| ProtocolError::MalformedLog(format!("topic {index} is missing")))?;
    address_word(&topic[..])
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

    /// The wrapped gas asset's own two events, pinned the same way as `V2Topics`.
    ///
    /// Census over 5,000 blocks ending at head 37,549,725 on chain 91,342, `eth_getLogs`
    /// for 0x4200000000000000000000000000000000000006 alone — 24,970 logs, and exactly four
    /// topic0 families, each with its `(topic count, data bytes)` shape:
    /// - 0xe1fffcc4.. (2 topics, 32 bytes) — 12,701 logs.
    /// - 0xddf252ad.. (3 topics, 32 bytes) —  7,397 logs, the ERC-20 `Transfer`.
    /// - 0x7fcf532c.. (2 topics, 32 bytes) —  3,164 logs.
    /// - 0x8c5be1e5.. (3 topics, 32 bytes) —  1,708 logs, `Approval`.
    ///
    /// The first family is identified independently of its hash: transaction
    /// 0x3234d9009a924b657630b47715a69127c37850e8a788cb8e2b82e05166181330 (block 37,524,492)
    /// calls the token contract with value and its receipt holds one 2-topic log of 32 bytes
    /// with that topic0, and no `Transfer` — a `deposit()`. So the other 2-topic family is the
    /// burn side, and this test's point is that the burn's topic0 is what the
    /// `Withdrawal(address,uint256)` declaration computes, not what an ABI browser says.
    #[test]
    fn weth9_topic0s_match_what_the_token_contract_emitted() {
        let topics = Weth9Topics::default();
        assert_eq!(
            topics.deposit,
            b256!("0xe1fffcc4923d04b559f4d29a8bfc6cda04eb5b0d3c460751c2402c5c5cc9109c")
        );
        assert_eq!(
            topics.withdrawal,
            b256!("0x7fcf532c15f0a6db0bd6d0e038bea71d30d808c7d98cb3bf7268a95bf5081b65")
        );
        assert_ne!(
            topics.withdrawal,
            V2Topics::default().transfer,
            "an unwrap is not a Transfer, which is why a flow audit has to read both"
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

    /// The wrap/unwrap pair is the short shape — one indexed address and one word — and that
    /// is what the chain's log shape shows: 2 topics and 32 data bytes, where `Transfer` is
    /// 3 topics and `Swap` is 3 topics over 128 bytes.
    #[test]
    fn weth9_declarations_match_the_observed_log_shapes() {
        assert_eq!(Deposit::SIGNATURE, "Deposit(address,uint256)");
        assert_eq!(Withdrawal::SIGNATURE, "Withdrawal(address,uint256)");
        sol! {
            event WithdrawalBalances(address indexed src, uint256 wad, uint256 bal);
        }
        assert_ne!(
            WithdrawalBalances::SIGNATURE_HASH,
            Weth9Topics::default().withdrawal,
            "a Withdrawal that also reported the remaining balance is a different event, and \
             the chain's 32-byte data rules it out"
        );
    }

    fn word(bytes: [u8; 32]) -> B256 {
        B256::from(bytes)
    }

    fn padded(address_last20: u8) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[12..].fill(address_last20);
        bytes
    }

    #[test]
    fn a_left_padded_address_word_decodes() {
        let address = address_word(&padded(0xab)).expect("padded");
        assert_eq!(address.as_slice(), &[0xabu8; 20]);
    }

    #[test]
    fn an_address_word_with_high_bytes_is_malformed() {
        let mut bytes = padded(0xab);
        bytes[11] = 0x01;
        assert!(matches!(
            address_word(&bytes),
            Err(ProtocolError::MalformedLog(_))
        ));

        // `topic_address` is the lenient sibling and would hand back the low 20
        // bytes anyway; the strict helper is what discovery decodes through.
        let topic = word(bytes);
        assert_eq!(
            topic_address(&[topic], 0).expect("lenient read always succeeds"),
            address_word(&padded(0xab)).expect("padded")
        );
    }

    #[test]
    fn an_address_word_has_to_be_a_whole_word() {
        assert!(matches!(
            address_word(&[0u8; 31]),
            Err(ProtocolError::MalformedLog(_))
        ));
    }

    #[test]
    fn data_address_reads_the_word_at_its_index_and_no_further() {
        let data = [padded(0xcd), padded(0xef)].concat();
        assert_eq!(
            data_address(&data, 1).expect("in range"),
            address_word(&padded(0xef)).expect("padded")
        );
        // Asking for the third word of a two-word buffer is a bounds error, not a
        // zero address.
        assert!(matches!(
            data_address(&data, 2),
            Err(ProtocolError::MalformedLog(_))
        ));
    }

    #[test]
    fn padded_topic_rejects_a_missing_topic() {
        let topics = [word(padded(0xab))];
        assert!(matches!(
            padded_topic(&topics, 1),
            Err(ProtocolError::MalformedLog(_))
        ));
        assert_eq!(
            padded_topic(&topics, 0).expect("present and padded"),
            address_word(&padded(0xab)).expect("padded")
        );
    }
}
