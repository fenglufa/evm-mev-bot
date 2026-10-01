//! §19/§20: how far a run is allowed to go, decided before it starts and not by what
//! happens to be in the environment.
//!
//! The default is `BuildOnly`, and it is the default *implementation* too —
//! [`ExecutionMode::default`] is not `Submit`, so a binary started with no flag at all
//! cannot broadcast even though the private key may be sitting in the environment. That
//! is the whole of §19's requirement, and it lives here where a test can pin it.

use serde::{Deserialize, Serialize};

use crate::error::{ExecutionError, Result};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Build the transaction and stop. Requires no key and touches no sender.
    #[default]
    BuildOnly,
    /// Build, then sign locally. Still no broadcast: the signed bytes are handed back
    /// and never leave the process.
    SignOnly,
    /// Build, sign, and submit to the endpoint, then track the receipt. Must be asked
    /// for by name.
    Submit,
}

impl ExecutionMode {
    /// The mode's own name, for logs and evidence.
    pub fn name(self) -> &'static str {
        match self {
            Self::BuildOnly => "build-only",
            Self::SignOnly => "sign-only",
            Self::Submit => "submit",
        }
    }

    /// Whether a private key may be read at all. §19: `BuildOnly` must run on a
    /// machine with no key present, so the key read is behind this and not merely
    /// avoided by convention.
    pub fn may_read_key(self) -> bool {
        matches!(self, Self::SignOnly | Self::Submit)
    }

    /// Whether raw bytes may be sent to a node.
    pub fn may_submit(self) -> bool {
        matches!(self, Self::Submit)
    }

    /// Parse the flag value. Unrecognised words are an error rather than a silent
    /// fall-through to the safe mode, because "I typed `submmit` and it did nothing"
    /// should not look like "the run completed".
    pub fn parse(text: &str) -> Result<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "build-only" | "buildonly" | "build" => Ok(Self::BuildOnly),
            "sign-only" | "signonly" | "sign" => Ok(Self::SignOnly),
            "submit" => Ok(Self::Submit),
            other => Err(ExecutionError::ModeGate(format!(
                "`{other}` is not an execution mode; expected build-only, sign-only or submit"
            ))),
        }
    }
}

impl std::fmt::Display for ExecutionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_mode_cannot_read_a_key_or_send_anything() {
        let default = ExecutionMode::default();
        assert_eq!(default, ExecutionMode::BuildOnly);
        assert!(!default.may_read_key());
        assert!(!default.may_submit());
    }

    #[test]
    fn only_the_named_mode_can_broadcast() {
        for mode in [ExecutionMode::BuildOnly, ExecutionMode::SignOnly] {
            assert!(!mode.may_submit(), "{mode} must not broadcast");
        }
        assert!(ExecutionMode::Submit.may_submit());
    }

    #[test]
    fn an_unrecognised_mode_name_is_an_error_and_not_a_fall_back_to_safe() {
        // A typo that quietly became BuildOnly would look like a successful run that
        // simply found nothing to send.
        assert!(ExecutionMode::parse("submmit").is_err());
        assert!(ExecutionMode::parse("").is_err());
        assert_eq!(
            ExecutionMode::parse(" SUBMIT ").unwrap(),
            ExecutionMode::Submit
        );
    }
}
