//! M12-B §4: what an endpoint is *for*, stated by the operator.
//!
//! The repository could already record two things about an endpoint — its URL, in the
//! session record, and a digest of it, in the RPC trace (`rpc_trace::endpoint_id`, one
//! rule shared by every evidence file). Neither of those answers the question §1 puts:
//! was this read taken from a node on this machine or from a service somebody else runs?
//! An evidence table that cannot say so cannot support "the canonical path is local now",
//! which is the claim M12 exists to make or refuse.
//!
//! The label is therefore *declared*, never derived. There is no function in this module
//! that takes a URL: a check on `127.0.0.1`, a port number, or a hostname would let the
//! bot decide that an endpoint is local on evidence that says nothing of the kind — a
//! public RPC reachable through a localhost proxy, or a `8545` on somebody else's host —
//! and §4.1 forbids exactly that inference. `Unknown` is the default for the same reason:
//! an absent declaration is not a declaration of "local", and treating silence as
//! permission is how §4.5's "a public endpoint is never labelled local" gets violated by
//! default rather than by mistake.

use serde::{Deserialize, Serialize};

/// Which role an endpoint serves for this bot, and whether the operator says it is
/// running on their own machine.
///
/// The two halves are one variant because §4's requirement 5 is about the pair: the
/// claim that matters in evidence is "a node *I run* is the canonical source", and a
/// label that recorded only one half would let a reader reconstruct the other half by
/// guessing — which is what this type exists to stop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointPurpose {
    /// A canonical RPC the operator states is their own node.
    LocalCanonicalRpc,
    /// A canonical RPC the operator states is a service.
    PublicCanonicalRpc,
    /// A Flashblocks / preconf endpoint the operator states is their own node.
    LocalFlashblocksRpc,
    /// A Flashblocks / preconf endpoint the operator states is a service.
    PublicFlashblocksRpc,
    /// Nobody said. Legal, and the default — it is what a run that was given an
    /// endpoint and no declaration records.
    #[default]
    Unknown,
}

impl EndpointPurpose {
    /// The one spelling this type uses, in a declaration, in a config value, and in
    /// evidence. Deliberately the same string everywhere: an operator who reads
    /// `"local_canonical_rpc"` in a session file can type that back into the flag, and
    /// a reader who sees `unknown` can search for a declaration rather than assume one.
    pub const fn label(self) -> &'static str {
        match self {
            Self::LocalCanonicalRpc => "local_canonical_rpc",
            Self::PublicCanonicalRpc => "public_canonical_rpc",
            Self::LocalFlashblocksRpc => "local_flashblocks_rpc",
            Self::PublicFlashblocksRpc => "public_flashblocks_rpc",
            Self::Unknown => "unknown",
        }
    }

    /// Every declaration this type accepts, in the order [`Self::LABELS`] lists them.
    ///
    /// Nothing here parses a URL, and nothing here maps a boolean or a host to a variant.
    pub const LABELS: [&'static str; 5] = [
        "local_canonical_rpc",
        "public_canonical_rpc",
        "local_flashblocks_rpc",
        "public_flashblocks_rpc",
        "unknown",
    ];

    /// Read one declared label. Anything else is `None`, which the caller turns into a
    /// configuration refusal — a near-miss spelling must not fall through to `Unknown`,
    /// because then a typo would silently downgrade a run that its operator believed was
    /// labelled.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "local_canonical_rpc" => Some(Self::LocalCanonicalRpc),
            "public_canonical_rpc" => Some(Self::PublicCanonicalRpc),
            "local_flashblocks_rpc" => Some(Self::LocalFlashblocksRpc),
            "public_flashblocks_rpc" => Some(Self::PublicFlashblocksRpc),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// Which role the label claims, so a flag can refuse a declaration about the wrong
    /// endpoint: `--rpc-endpoint-purpose local_flashblocks_rpc` would otherwise label the
    /// canonical node with a candidate endpoint's purpose, and the evidence would then
    /// say something no operator stated.
    pub const fn role(self) -> Option<EndpointRole> {
        match self {
            Self::LocalCanonicalRpc | Self::PublicCanonicalRpc => Some(EndpointRole::Canonical),
            Self::LocalFlashblocksRpc | Self::PublicFlashblocksRpc => {
                Some(EndpointRole::Flashblocks)
            }
            Self::Unknown => None,
        }
    }

    /// Locality as three values, because "not local" and "nobody said" are different
    /// facts and evidence must not be able to read silence as either.
    pub const fn is_local(self) -> Option<bool> {
        match self {
            Self::LocalCanonicalRpc | Self::LocalFlashblocksRpc => Some(true),
            Self::PublicCanonicalRpc | Self::PublicFlashblocksRpc => Some(false),
            Self::Unknown => None,
        }
    }
}

/// The half of [`EndpointPurpose`] a flag validates against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointRole {
    Canonical,
    Flashblocks,
}

impl EndpointRole {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Canonical => "canonical",
            Self::Flashblocks => "flashblocks",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vocabulary is closed and every label round-trips, including `unknown` — a
    /// declaration format that cannot express the default would push an undeclared run
    /// towards saying something it did not know.
    #[test]
    fn every_label_round_trips() {
        for label in EndpointPurpose::LABELS {
            let purpose = EndpointPurpose::parse(label)
                .unwrap_or_else(|| panic!("{label} is not a declared label"));
            assert_eq!(purpose.label(), label, "a label that does not print back");
        }
        assert_eq!(EndpointPurpose::default(), EndpointPurpose::Unknown);
        assert_eq!(
            serde_json::to_string(&EndpointPurpose::Unknown).unwrap(),
            "\"unknown\""
        );
        assert_eq!(
            serde_json::to_string(&EndpointPurpose::LocalCanonicalRpc).unwrap(),
            "\"local_canonical_rpc\""
        );
    }

    /// §4.1's negative control: the only input that produces a variant is a declared
    /// label. The three strings below are the guesses this module must never make — a
    /// loopback host, a conventional RPC port, and a hostname that reads like a local
    /// one. Each is refused rather than mapped, and the refusal is what makes an
    /// undeclared endpoint record `unknown` instead of a claim.
    #[test]
    fn no_url_shape_declares_anything() {
        for not_a_declaration in [
            "http://127.0.0.1:8545",
            "http://localhost:8545",
            "ws://0.0.0.0:9997",
            "https://rpc.giwa.io",
            "local",
            "LOCAL_CANONICAL_RPC",
            "local_canonical_rpc ",
        ] {
            assert_eq!(
                EndpointPurpose::parse(not_a_declaration),
                None,
                "{not_a_declaration} was read as a declaration"
            );
        }
    }

    /// Locality is three-valued: an undeclared endpoint is not "public", and a declared
    /// one answers.
    #[test]
    fn silence_is_not_a_claim_about_locality() {
        assert_eq!(EndpointPurpose::Unknown.is_local(), None);
        assert_eq!(EndpointPurpose::LocalCanonicalRpc.is_local(), Some(true));
        assert_eq!(
            EndpointPurpose::PublicFlashblocksRpc.is_local(),
            Some(false)
        );
    }

    /// §4.5, held at the type: there is no variant that is public in one half and local
    /// in the other, and the role check is what lets a flag refuse a declaration about
    /// the wrong endpoint before it reaches a config value.
    #[test]
    fn a_label_names_one_role_and_one_locality() {
        assert_eq!(
            EndpointPurpose::LocalCanonicalRpc.role(),
            Some(EndpointRole::Canonical)
        );
        assert_eq!(
            EndpointPurpose::PublicFlashblocksRpc.role(),
            Some(EndpointRole::Flashblocks)
        );
        assert_eq!(EndpointPurpose::Unknown.role(), None);
        assert_eq!(EndpointRole::Canonical.label(), "canonical");
    }

    /// A serde round trip is the evidence path: what the runner writes is what a reader
    /// of the session file gets, with no hand-written strings to drift.
    #[test]
    fn evidence_writes_the_same_string_the_flag_takes() {
        for label in EndpointPurpose::LABELS {
            let value = serde_json::json!(EndpointPurpose::parse(label).unwrap());
            assert_eq!(value.as_str(), Some(label));
            let back: EndpointPurpose = serde_json::from_value(value).unwrap();
            assert_eq!(back.label(), label);
        }
    }
}
