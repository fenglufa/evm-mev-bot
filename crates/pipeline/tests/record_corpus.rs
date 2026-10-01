//! Records a contiguous run of real blocks into a directory the replay source can
//! read, so §32's parity claim has an input both paths can be pointed at.
//!
//! This is a deliberate, ignored test rather than part of the pipeline: it is the
//! one place in M5 that copies chain bytes to disk, and it runs only when asked.
//! The point is that a later replay over `fixtures/live-m5/corpus` and a live
//! poll over the same node must agree block for block — which is only a claim if
//! the same blocks are available to both, and an archive node keeps them so.
//!
//! The endpoint comes from the milestone's own evidence file
//! (`data/live-m5/source-evidence.json`), or `GIWA_RPC_URL` when set (§44: no URL
//! is invented here, and none is needed to build the binary).

use std::path::PathBuf;

use evm_chain::{ChainAdapter, HttpChainAdapter};
use evm_core::BlockNumber;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The node this milestone has already been measured against.
fn rpc_url() -> String {
    if let Ok(url) = std::env::var("GIWA_RPC_URL") {
        return url;
    }
    let evidence = workspace_root().join("data/live-m5/source-evidence.json");
    let raw = std::fs::read_to_string(&evidence)
        .unwrap_or_else(|error| panic!("{}: {error}", evidence.display()));
    serde_json::from_str::<serde_json::Value>(&raw)
        .expect("the evidence file is JSON")
        .pointer("/endpoints/rpc_http")
        .and_then(|value| value.as_str())
        .unwrap_or_else(|| panic!("{} names an rpc_http", evidence.display()))
        .to_string()
}

/// A quiet stretch of this chain still contains market movement: it emits an
/// attested `Sync` roughly every other block, so 48 is enough to exercise the
/// state and graph stages without making the corpus an unreadable blob.
const BLOCKS: u64 = 48;

/// Which corpus to write. `M5_CORPUS_DIR` names a subdirectory of
/// `fixtures/live-m5/`, so the parity corpus and a targeted window (the
/// arbitrage window below) live side by side and are each replayable on their own.
fn corpus_dir() -> PathBuf {
    let name = std::env::var("M5_CORPUS_DIR").unwrap_or_else(|_| "corpus".to_string());
    workspace_root().join("fixtures/live-m5").join(name)
}

/// `M5_CORPUS_RANGE=from:to` records exactly those blocks. Used for a window the
/// live chain produced once — a two-pool restatement, say — where a head-relative
/// range would record nothing.
fn explicit_range() -> Option<(u64, u64)> {
    let spec = std::env::var("M5_CORPUS_RANGE").ok()?;
    let (from, to) = spec.split_once(':')?;
    Some((
        from.parse().expect("range start is a number"),
        to.parse().expect("range end is a number"),
    ))
}

#[tokio::test]
#[ignore = "writes to the repository; run deliberately to refresh the parity corpus"]
async fn capture_contiguous_corpus() {
    let adapter = HttpChainAdapter::connect(&rpc_url())
        .await
        .expect("the recorded endpoint answers");

    let (first, last) = match explicit_range() {
        Some(range) => range,
        None => {
            let head = adapter.latest_block().await.expect("head reads back");
            // A margin below the head, so every block written is sealed and
            // archive readable by the time the file exists.
            let last = head.0.saturating_sub(20);
            (last.saturating_sub(BLOCKS - 1), last)
        }
    };
    let dir = corpus_dir();
    std::fs::create_dir_all(&dir).expect("create the corpus directory");

    let mut blocks_with_logs = 0u64;
    let mut logs = 0u64;
    for number in (first..=last).map(BlockNumber) {
        let data = adapter
            .get_block_data(number)
            .await
            .unwrap_or_else(|error| panic!("block {}: {error}", number.0));
        logs += data.receipts.iter().map(|r| r.logs.len()).sum::<usize>() as u64;
        if !data.receipts.is_empty() {
            blocks_with_logs += 1;
        }
        let path = dir.join(format!("block-{}.json", number.0));
        std::fs::write(&path, serde_json::to_string(&data).expect("serialize"))
            .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    }
    println!(
        "recorded {} blocks ({}..={}) into {}; {logs} logs across {blocks_with_logs} blocks \
         with transactions",
        last - first + 1,
        first,
        last,
        dir.display(),
    );
}
