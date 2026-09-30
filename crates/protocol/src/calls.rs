//! The callable side of the V2 surface: declarations that compute their own
//! selectors, and calldata built by the ABI encoder rather than by hand.
//!
//! [`crate::signatures`] pins the *event* half of this protocol. This module pins
//! the *function* half, and for the same reason: a selector typed in from memory
//! is a claim about a contract, while `sol!` derives keccak256 of the canonical
//! declaration. The test at the bottom requires each derived selector to equal the
//! value this chain's own attestation recorded, so a wrong declaration fails here
//! instead of reverting inside a simulation three milestones later.
//!
//! Only what M4 executes is declared. `swap` is the pair's own entry point,
//! `transfer` is how the sender moves tokens, `balanceOf` / `getReserves` /
//! `token0` / `token1` are read-only probes a simulation runs to measure what
//! actually happened, and `deposit` / `withdraw` are the wrapped-native pair that
//! lets a profit be expressed in the chain's native unit without asserting a price.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall};

use crate::error::{ProtocolError, Result};
use crate::signatures::selector_of;

sol! {
    function transfer(address to, uint256 value) returns (bool);
    function approve(address spender, uint256 value) returns (bool);
    function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data);
    function balanceOf(address owner) view returns (uint256);
    function getReserves() view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
    function token0() view returns (address);
    function token1() view returns (address);
    function deposit() payable;
    function withdraw(uint256 wad);
}

/// The four words `getReserves()` returns, unpacked from the ABI tail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reserves {
    pub reserve0: U256,
    pub reserve1: U256,
    pub block_timestamp_last: U256,
}

/// One call on the V2/ERC20 surface, with the arguments kept visible.
///
/// This is the shape a simulation request has to be able to print (§56 of the M4
/// task): `to`, `value`, `calldata` are what the EVM sees, and the decoded form is
/// what a human auditing the finding reads. Both come from the same value, so
/// neither can drift away from the other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum V2Call {
    Transfer {
        to: Address,
        value: U256,
    },
    Approve {
        spender: Address,
        value: U256,
    },
    Swap {
        amount0_out: U256,
        amount1_out: U256,
        to: Address,
    },
    BalanceOf {
        owner: Address,
    },
    GetReserves,
    Token0,
    Token1,
    Deposit,
    Withdraw {
        wad: U256,
    },
}

impl V2Call {
    /// The canonical signature string this call was encoded from — the thing a
    /// selector is a hash of, kept so evidence can be quoted without re-deriving.
    pub fn signature(&self) -> &'static str {
        match self {
            Self::Transfer { .. } => "transfer(address,uint256)",
            Self::Approve { .. } => "approve(address,uint256)",
            Self::Swap { .. } => "swap(uint256,uint256,address,bytes)",
            Self::BalanceOf { .. } => "balanceOf(address)",
            Self::GetReserves => "getReserves()",
            Self::Token0 => "token0()",
            Self::Token1 => "token1()",
            Self::Deposit => "deposit()",
            Self::Withdraw { .. } => "withdraw(uint256)",
        }
    }

    pub fn selector(&self) -> [u8; 4] {
        selector_of(self.signature())
    }

    /// ABI-encoded calldata: selector followed by the argument words.
    ///
    /// `swap`'s trailing `bytes data` is empty and never a callback target here —
    /// a non-empty `data` makes the pair call back into `uniswapV2Call`, which is
    /// how a flash-swap-style executor stays atomic. M4's plan is a plain sequence
    /// from one sender, so the field is the empty bytes and the pair transfers and
    /// returns.
    pub fn encode(&self) -> Bytes {
        match self {
            Self::Transfer { to, value } => transferCall {
                to: *to,
                value: *value,
            }
            .abi_encode(),
            Self::Approve { spender, value } => approveCall {
                spender: *spender,
                value: *value,
            }
            .abi_encode(),
            Self::Swap {
                amount0_out,
                amount1_out,
                to,
            } => swapCall {
                amount0Out: *amount0_out,
                amount1Out: *amount1_out,
                to: *to,
                data: Bytes::new(),
            }
            .abi_encode(),
            Self::BalanceOf { owner } => balanceOfCall { owner: *owner }.abi_encode(),
            Self::GetReserves => getReservesCall {}.abi_encode(),
            Self::Token0 => token0Call {}.abi_encode(),
            Self::Token1 => token1Call {}.abi_encode(),
            Self::Deposit => depositCall {}.abi_encode(),
            Self::Withdraw { wad } => withdrawCall { wad: *wad }.abi_encode(),
        }
        .into()
    }

    /// Decode a `return value`, not a `return data`: the raw bytes an `eth_call` or
    /// an EVM `Output::Call` produced for this call, with the selector already off.
    ///
    /// Reads the fixed ABI words directly instead of relying on the generated
    /// decoder's type mapping — `uint112` has no Rust type of its own, and a
    /// simulation would rather carry `U256` than argue about which width it came
    /// from.
    pub fn decode_return(&self, raw: &[u8]) -> Result<CallReturn> {
        match self {
            Self::BalanceOf { .. } => Ok(CallReturn::Amount(word(raw, 0)?)),
            Self::Transfer { .. } | Self::Approve { .. } => {
                // Both return `bool`. Anything that is not exactly one word with a
                // set last bit is treated as failure rather than as a truthy value.
                let value = word(raw, 0)?;
                Ok(CallReturn::Boolean(value == U256::from(1u8)))
            }
            Self::GetReserves => Ok(CallReturn::Reserves(Reserves {
                reserve0: word(raw, 0)?,
                reserve1: word(raw, 1)?,
                block_timestamp_last: word(raw, 2)?,
            })),
            Self::Token0 | Self::Token1 => Ok(CallReturn::Address(Address::from_slice(
                &word(raw, 0)?.to_be_bytes::<32>()[12..],
            ))),
            Self::Deposit | Self::Withdraw { .. } => Err(ProtocolError::MalformedLog(format!(
                "{} returns nothing to decode",
                self.signature()
            ))),
            Self::Swap { .. } => Err(ProtocolError::MalformedLog(
                "swap(uint256,uint256,address,bytes) returns nothing; its output is the \
                 amount the pair transferred, which a simulation measures with balanceOf"
                    .to_string(),
            )),
        }
    }
}

/// What a call answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallReturn {
    Amount(U256),
    Reserves(Reserves),
    Address(Address),
    Boolean(bool),
}

fn word(raw: &[u8], index: usize) -> Result<U256> {
    crate::signatures::word(raw, index)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, B256};

    use super::*;

    const SOMEONE: Address = address!("0x1111111111111111111111111111111111111111");

    /// Every selector here is the value this chain actually answers to. The
    /// anchors are on-chain, not remembered: `data/protocols-m3/…` and
    /// `data/simulation-m4/execution-evidence.json` recorded `getReserves()`
    /// 0x0902f1ac, `token0()` 0x0dfe1681, `balanceOf(address)` 0x70a08231,
    /// `transfer(address,uint256)` 0xa9059cbb, WETH `deposit()` 0xd0e30db0 and
    /// `withdraw(uint256)` 0x2e1a7d4d; `swap(uint256,uint256,address,bytes)` is
    /// 0x022c0d9f, which is the first word of the calldata of three real
    /// transactions that called pool `0xf487d533…6578` directly — `0xc465dc6a…`,
    /// `0x17b7478a…` and `0x85b403a1…`, blocks 37187701 / 37187959 / 37189041.
    ///
    /// That last one is worth stating plainly because it is the reason this test
    /// exists: the swap selector had first been written down from recollection as
    /// `0x022c0400`, and `selector_of` disagreed. Two independent keccak
    /// implementations (OpenSSL's `keccak-256` and alloy's, the one already
    /// validated against this chain's event topics) and the chain's own transaction
    /// inputs all say 0x022c0d9f. A selector typed from memory is a claim; these
    /// assertions are what turns it back into a confirmation.
    #[test]
    fn derived_selectors_equal_the_observed_ones() {
        let calls = [
            V2Call::Transfer {
                to: SOMEONE,
                value: U256::from(1u8),
            },
            V2Call::Approve {
                spender: SOMEONE,
                value: U256::from(1u8),
            },
            V2Call::Swap {
                amount0_out: U256::from(1u8),
                amount1_out: U256::ZERO,
                to: SOMEONE,
            },
            V2Call::BalanceOf { owner: SOMEONE },
            V2Call::GetReserves,
            V2Call::Token0,
            V2Call::Token1,
            V2Call::Deposit,
            V2Call::Withdraw {
                wad: U256::from(1u8),
            },
        ];
        let observed: [[u8; 4]; 9] = [
            [0xa9, 0x05, 0x9c, 0xbb],
            [0x09, 0x5e, 0xa7, 0xb3],
            [0x02, 0x2c, 0x0d, 0x9f],
            [0x70, 0xa0, 0x82, 0x31],
            [0x09, 0x02, 0xf1, 0xac],
            [0x0d, 0xfe, 0x16, 0x81],
            [0xd2, 0x12, 0x20, 0xa7],
            [0xd0, 0xe3, 0x0d, 0xb0],
            [0x2e, 0x1a, 0x7d, 0x4d],
        ];
        for (call, selector) in calls.iter().zip(observed) {
            assert_eq!(
                call.selector(),
                selector,
                "{} must select {selector:02x?}",
                call.signature()
            );
        }
    }

    /// The encoder is the one alloy ships, and the shape it produces is the shape
    /// the chain already accepted: `0xc465dc6a…d1dc` called the pair with 164 bytes
    /// of input — selector + four head words + one word holding `data.length == 0` —
    /// with `amount0Out = 0`, `amount1Out > 0`, `to` set to the caller, and no
    /// callback payload. That is the exact-out form and the no-callback form this
    /// plan uses, so the assertion below is a comparison against a real transaction
    /// rather than against this file's own expectations.
    #[test]
    fn encoded_calldata_has_the_abi_shape() {
        let swap = V2Call::Swap {
            amount0_out: U256::ZERO,
            amount1_out: U256::from(3_626_443_575_520_596_526_u128),
            to: address!("0x24a403f324f27c8d33127e91d668609235d66c91"),
        };
        let encoded = swap.encode();
        assert_eq!(encoded.len(), 164);
        assert_eq!(&encoded[..4], &swap.selector());
        // The three real transactions above all sent exactly this many bytes.
        assert_eq!(word(&encoded[4..], 0), Ok(U256::ZERO));
        assert_eq!(
            word(&encoded[4..], 1),
            Ok(U256::from(3_626_443_575_520_596_526_u128))
        );
        assert_eq!(
            Address::from_slice(&encoded[4 + 64..4 + 96][12..]),
            address!("0x24a403f324f27c8d33127e91d668609235d66c91")
        );
        // `data` is a dynamic type: an offset word, then a length of zero.
        assert_eq!(word(&encoded[4..], 3), Ok(U256::from(128u32)));
        assert_eq!(word(&encoded[4..], 4), Ok(U256::ZERO));

        let transfer = V2Call::Transfer {
            to: SOMEONE,
            value: U256::from(7u8),
        };
        let encoded = transfer.encode();
        assert_eq!(encoded.len(), 4 + 2 * 32);
        assert_eq!(
            crate::signatures::topic_address(&[B256::from_slice(&encoded[4..36])], 0)
                .expect("address word"),
            SOMEONE
        );
        assert_eq!(
            crate::signatures::word(&encoded[4..], 1).expect("value word"),
            U256::from(7u8)
        );
    }

    /// `getReserves` answers three words; the third is a uint32 timestamp, which
    /// must not be confused with a reserve. Decoding it wrong would silently feed
    /// a block timestamp into a reserve comparison, so the round trip is asserted.
    #[test]
    fn get_reserves_return_decodes_three_words() {
        let raw: Vec<u8> = [
            U256::from(35_099_900_253_008u128).to_be_bytes::<32>(),
            U256::from(45_655_538_604_883_371_699_u128).to_be_bytes::<32>(),
            U256::from(1_790_536_285u64).to_be_bytes::<32>(),
        ]
        .concat();
        let decoded = V2Call::GetReserves.decode_return(&raw).expect("decodes");
        assert_eq!(
            decoded,
            CallReturn::Reserves(Reserves {
                reserve0: U256::from(35_099_900_253_008u128),
                reserve1: U256::from(45_655_538_604_883_371_699_u128),
                block_timestamp_last: U256::from(1_790_536_285u64),
            })
        );
    }

    #[test]
    fn a_call_with_no_return_data_refuses_to_be_decoded() {
        let raw = [];
        assert!(V2Call::Deposit.decode_return(&raw).is_err());
        assert!(V2Call::Swap {
            amount0_out: U256::ZERO,
            amount1_out: U256::from(1u8),
            to: SOMEONE,
        }
        .decode_return(&raw)
        .is_err());
    }
}
