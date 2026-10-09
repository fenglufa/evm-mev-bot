//! M9.4 §22/§23/§35/§38: the transport behind a preconfirmation view, as its own
//! seam.
//!
//! Two things this separation is for, both of which the task book asks for by name:
//!
//! * **A decoder must not know how its bytes arrived.** [`FrameSource`] answers
//!   "give me the next pending payload" and returns JSON exactly as the provider
//!   wrote it; [`crate::preconf_decode`] is the only place that reads a key. A
//!   provider that changed its wire shape therefore changes one file, and the
//!   decoder's fail-closed behaviour is what turns that change into an error rather
//!   than into a wrong number.
//! * **The official endpoint has to stay swappable** (§35/§38). SequencerDirect and
//!   any self-hosted builder are not implemented here and are not reachable from
//!   here; what is implemented is one HTTP polling source that reads the same
//!   `pending` tag the canonical path already reads, and one replay source that
//!   re-serves a committed capture so the evidence gate is deterministic and never
//!   touches the network.
//!
//! §34's rule — instrumentation must not create a new production RPC — is enforced
//! by [`FrameSource::supports_pending_receipts`]: the polling source answers `false`,
//! because one more RPC *method* on the live path is precisely what that rule forbids,
//! while the replay source answers `true` since its receipts were already read once,
//! by the capture, and are now just bytes on disk.

use async_trait::async_trait;
use serde_json::Value;

use evm_chain::HeadReader;

use crate::preconf::PreconfError;

/// One read of the preconfirmation surface.
#[async_trait]
pub trait FrameSource: Send {
    /// Which endpoint this is, as a digest (§33: the URL itself never travels into
    /// an evidence file, and a digest is enough to answer "same provider or not").
    fn endpoint_id(&self) -> &str;

    /// The transport, in one word, for the evidence line that has to say what kind
    /// of read a claim rests on.
    fn transport(&self) -> &'static str;

    /// Whether this supply ends. A node does not; a recording does, and the
    /// difference is what lets a replay run finish by itself instead of being
    /// stopped by a timeout.
    fn is_finite(&self) -> bool {
        false
    }

    /// Whether a finite supply has *already* run out. `next_view` answering `None`
    /// is not enough to know that: a live node also answers `None` when its pending
    /// block is empty, and treating that as the end of a recording would truncate a
    /// replay. Only checked when [`FrameSource::is_finite`] says `true`.
    fn finished(&self) -> bool {
        false
    }

    /// The next `pending` payload, verbatim. `Ok(None)` means the provider answered
    /// null — the same distinction the canonical sources keep, because "no answer"
    /// and "no block" are different findings.
    async fn next_view(&mut self) -> Result<Option<Value>, PreconfError>;

    /// Whether this source can also hand over receipts for a block that has not
    /// sealed. See the module comment: a live source must answer `false`.
    fn supports_pending_receipts(&self) -> bool {
        false
    }

    /// Receipts for the current pending block, when the source is allowed to have
    /// any. The default refuses, so a new transport cannot silently claim a
    /// capability it never probed (§52: `UNKNOWN`, not `SUPPORTED`).
    async fn next_receipts(&mut self) -> Result<Option<Value>, PreconfError> {
        Ok(None)
    }
}

/// The official-endpoint source: the same [`HeadReader`] the M5 candidate source
/// already uses, and therefore no new HTTP machinery and no new RPC method.
///
/// It asks for [`HeadReader::pending_full_transactions`] — `full: true` on the method
/// the candidate path already calls — because the radar's decoder names contracts, and
/// a pending block whose transactions are bare hashes names nothing
/// (`PreconfError::HashOnlyTransaction`). M12-B §6 chose the read shape over the
/// decoder for that reason; the shared `pending_raw()` stayed on `full: false` so the
/// candidate observer keeps paying a fifth of the bytes for the shape fact it reads.
pub struct PollingFrameSource<R> {
    reader: R,
    endpoint_id: String,
}

impl<R> PollingFrameSource<R> {
    pub fn new(reader: R, endpoint_id: String) -> Self {
        Self {
            reader,
            endpoint_id,
        }
    }
}

#[async_trait]
impl<R: HeadReader + Send> FrameSource for PollingFrameSource<R> {
    fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }

    fn transport(&self) -> &'static str {
        "http-poll-pending"
    }

    async fn next_view(&mut self) -> Result<Option<Value>, PreconfError> {
        self.reader
            .pending_full_transactions()
            .await
            .map_err(|error| PreconfError::Transport(error.to_string()))
    }
}

/// A committed capture, re-served in order.
///
/// Each entry is one provider answer with the wall clock that *the capture* stamped
/// on it, so a replay run reproduces the original sequence's timings as data rather
/// than as a fresh measurement (§43: real order, no clock games, and the HashMap
/// ordering ban applies to the frames themselves).
#[derive(Clone, Debug)]
pub struct RecordedFrame {
    pub view: Value,
    pub observed_at_unix_ms: u64,
    /// Receipts captured against the same height, if the window read any.
    pub receipts: Option<Value>,
}

pub struct ReplayFrameSource {
    frames: Vec<RecordedFrame>,
    cursor: usize,
    endpoint_id: String,
}

impl ReplayFrameSource {
    pub fn new(frames: Vec<RecordedFrame>, endpoint_id: String) -> Self {
        Self {
            frames,
            cursor: 0,
            endpoint_id,
        }
    }

    /// Frames still unserved — the count a determinism test compares two runs on.
    pub const fn remaining(&self) -> usize {
        self.frames.len() - self.cursor
    }
}

#[async_trait]
impl FrameSource for ReplayFrameSource {
    fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }

    fn transport(&self) -> &'static str {
        "replay-recording"
    }

    fn is_finite(&self) -> bool {
        true
    }

    fn finished(&self) -> bool {
        self.cursor >= self.frames.len()
    }

    fn supports_pending_receipts(&self) -> bool {
        true
    }

    async fn next_view(&mut self) -> Result<Option<Value>, PreconfError> {
        if self.cursor >= self.frames.len() {
            return Ok(None);
        }
        let frame = self.frames[self.cursor].clone();
        self.cursor += 1;
        Ok(Some(frame.view))
    }

    async fn next_receipts(&mut self) -> Result<Option<Value>, PreconfError> {
        // The receipts belong to the frame just served: `next_view` advanced the
        // cursor, so index `cursor - 1` is the payload this read describes.
        if self.cursor == 0 {
            return Ok(None);
        }
        Ok(self.frames[self.cursor - 1].receipts.clone())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    struct StubReader(Value);

    #[async_trait]
    impl HeadReader for StubReader {
        fn transport(&self) -> &'static str {
            "stub"
        }

        async fn head(&mut self) -> evm_chain::Result<evm_core::BlockNumber> {
            Ok(evm_core::BlockNumber(0))
        }

        async fn block_at(
            &mut self,
            _number: evm_core::BlockNumber,
        ) -> evm_chain::Result<Option<evm_chain::ChainBlock>> {
            Ok(None)
        }

        async fn pending_full_transactions(&mut self) -> evm_chain::Result<Option<Value>> {
            Ok(Some(self.0.clone()))
        }
    }

    /// A reader that only ever answers the light shape, `["pending", false]`.
    struct LightOnlyReader(Value);

    #[async_trait]
    impl HeadReader for LightOnlyReader {
        fn transport(&self) -> &'static str {
            "stub-light-only"
        }

        async fn head(&mut self) -> evm_chain::Result<evm_core::BlockNumber> {
            Ok(evm_core::BlockNumber(0))
        }

        async fn block_at(
            &mut self,
            _number: evm_core::BlockNumber,
        ) -> evm_chain::Result<Option<evm_chain::ChainBlock>> {
            Ok(None)
        }

        async fn pending_raw(&mut self) -> evm_chain::Result<Option<Value>> {
            Ok(Some(self.0.clone()))
        }
    }

    #[tokio::test]
    async fn a_polling_source_never_offers_receipts() {
        let reader = StubReader(json!({"number": "0x1"}));
        let mut source = PollingFrameSource::new(reader, "rpc-0000000000000000".to_string());
        assert!(!source.supports_pending_receipts());
        assert!(!source.is_finite());
        let view = source
            .next_view()
            .await
            .expect("the stub always answers")
            .expect("a payload");
        assert_eq!(view["number"], json!("0x1"));
        assert!(
            source
                .next_receipts()
                .await
                .expect("refusal is an answer, not an error")
                .is_none(),
            "§34: the live path reads no extra method"
        );
    }

    /// §6's choice, witnessed as a refusal rather than as prose: a transport that has
    /// not been shown to answer `full: true` does not get to feed the radar the light
    /// payload instead. A silent fallback would keep the run alive and fill it with
    /// `HashOnlyTransaction` refusals that read as a provider problem.
    #[tokio::test]
    async fn a_reader_that_only_answers_the_light_shape_is_refused_not_downgraded() {
        let mut source = PollingFrameSource::new(
            LightOnlyReader(json!({"number": "0x1", "transactions": ["0xaa"]})),
            "rpc-2222222222222222".to_string(),
        );
        let error = source
            .next_view()
            .await
            .expect_err("an unprobed read shape is a refusal, not a lighter answer");
        assert!(matches!(error, PreconfError::Transport(_)), "{error}");
        let text = error.to_string();
        assert!(
            text.contains("stub-light-only") && text.contains("\"pending\", true"),
            "the refusal has to name both the transport and the params it never answered: {text}"
        );
    }

    #[tokio::test]
    async fn a_replay_source_serves_its_recording_in_order_and_ends() {
        let mut source = ReplayFrameSource::new(
            vec![
                RecordedFrame {
                    view: json!({"number": "0x1"}),
                    observed_at_unix_ms: 10,
                    receipts: None,
                },
                RecordedFrame {
                    view: json!({"number": "0x2"}),
                    observed_at_unix_ms: 20,
                    receipts: Some(json!([{"transactionHash": format!("0x{}", "aa".repeat(32))}])),
                },
            ],
            "rpc-1111111111111111".to_string(),
        );
        assert!(source.is_finite());
        assert!(source.supports_pending_receipts());
        assert_eq!(source.remaining(), 2);
        let first = source
            .next_view()
            .await
            .expect("recording")
            .expect("one frame");
        assert_eq!(first["number"], json!("0x1"));
        assert!(
            source
                .next_receipts()
                .await
                .expect("no receipts on the first frame")
                .is_none(),
            "receipts belong to the frame just served"
        );
        let second = source
            .next_view()
            .await
            .expect("recording")
            .expect("two frames");
        assert_eq!(second["number"], json!("0x2"));
        assert_eq!(
            source
                .next_receipts()
                .await
                .expect("receipts")
                .expect("the second frame has receipts")
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(source.remaining(), 0);
        assert!(
            source
                .next_view()
                .await
                .expect("an ended recording is an answer")
                .is_none(),
            "a finite supply ends by itself, without a timeout"
        );
    }
}
