//! M12-D §7: the boundary the early view is not allowed to cross, and the radar's real
//! status, stated from the code rather than from a completion report.
//!
//! The task book puts three prohibitions on this file, and each one is checked as a
//! structural fact about the repository — a call site, a match arm, a manifest edge — not
//! as a re-reading of M9.4's or M12-B's prose:
//!
//! ```text
//! 不得因为 Pending 数据可解析，就认定它可以直接驱动执行
//! 不得将 Early Radar 直接接入 Signer 或 Submitter
//! Radar 尚未接入运行链路时，准确记录真实状态，不得声称端到端通过
//! ```
//!
//! The first two have a shared shape: the pending/early data reaches the process, and stops.
//! Where it stops is what the tests below locate, because "it stops" said in prose is worth
//! nothing next to a run that could put it in front of a signer.
//!
//! What this file deliberately does **not** duplicate:
//!
//! * [`crates/live/tests/preconf_isolation.rs`] (§58) walks `crates/{core,state,graph,
//!   pathfinder,discovery,pipeline}/src` for the radar's *type names* and proves the build
//!   graph from `evm-live` outward never reaches a canonical writer. The crate it does not
//!   walk is the one that owns a signer and a submitter — `crates/execution` — so the name
//!   scan here starts from that hole and runs in the opposite direction.
//! * [`crates/live/tests/pending_shape_compat.rs`] (M12-B §7) proves the `pending` shapes
//!   are parseable, and that the radar **refuses** a hashes-only frame rather than reporting
//!   an empty affected-pool set. That is the 「可解析」 half of the first prohibition; it is
//!   re-run by `cargo test --workspace`, and this file only adds the half that says where
//!   the parsed thing may then go.
//!
//! ```text
//! what is scanned      crates/*/src, comments dropped, strings kept — a doc comment that
//!                      names a forbidden type is prose, and prose has to be writable
//! what counts as a     a hit that is a definition inside crates/live/src, which is where
//! call site            the types live; anything else, anywhere, is a consumer
//! why the zeros are    every zero is paired with the same needle reported somewhere it
//! not vacuous          legitimately belongs (crates/live/src, crates/chain/src/head.rs, or
//!                      the Canonical arm of the same match), printed in the failure message
//! what it costs        no dependency: std only, and no request to any endpoint
//! ```
//!
//! §9's constraints hold throughout: nothing here runs a node, reads a real endpoint, signs
//! a transaction, or writes into an evidence directory.

use std::path::{Path, PathBuf};

/// The workspace root, from `crates/pipeline`.
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/pipeline sits under the workspace root")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = repo().join(relative);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} could not be read: {error}", path.display()));
    assert!(
        !text.is_empty(),
        "{relative} is empty, so a scan over it proves nothing"
    );
    text
}

/// Every `.rs` file under `crates/<name>/src`, sorted, so exposure cannot depend on
/// directory order.
fn sources(crate_name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut walk = vec![repo().join("crates").join(crate_name).join("src")];
    while let Some(dir) = walk.pop() {
        if !dir.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&dir).expect("a directory we just checked") {
            let path = entry.expect("a readable directory entry").path();
            if path.is_dir() {
                walk.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Line and block comments dropped, string literals kept.
///
/// The reason is the same one `preconf_isolation.rs` gives: these modules use `///` prose to
/// name what they forbid, so a scan that counted comments would measure the prohibition
/// rather than the code — and a scan that truncated at the first `//` inside a string would
/// quietly lose the tail of a real call site, which fails in the wrong direction.
fn code_only(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut kept = String::with_capacity(text.len());
    let mut in_string = false;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            kept.push(c);
            i += 1;
            if c == '\\' && i < chars.len() {
                kept.push(chars[i]);
                i += 1;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            kept.push(c);
            i += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            let mut depth = 1usize;
            while i < chars.len() && depth > 0 {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        kept.push(c);
        i += 1;
    }
    kept
}

fn occurrences(text: &str, needle: &str) -> usize {
    text.matches(needle).count()
}

/// The names that only the radar's own crate may say: the types, the constructors, and the
/// config structs that a caller would have to touch to start the early view.
const RADAR_VOCABULARY: [&str; 11] = [
    "EarlyRadar",
    "PreconfLink",
    "PollingFrameSource",
    "ReplayFrameSource",
    "RadarConfig",
    "RadarInput",
    "CanonicalDigest",
    "PoolSet",
    "PreconfirmationFrame",
    "frame_from_pending_value",
    "LinkConfig",
];

/// The four calls that actually put the radar on a socket.
const RADAR_CONSTRUCTORS: [&str; 4] = [
    "EarlyRadar::new",
    "PreconfLink::new",
    "PollingFrameSource::new",
    "ReplayFrameSource::new",
];

/// The crates that own a signature, a submission, a simulation, or a run loop.
const DOWNSTREAM_CRATES: [&str; 4] = ["execution", "simulation", "pipeline", "cli"];

/// The `pending` *block* reads — the ones that would hand an unsealed view to a state reader.
const PENDING_STATE_READS: [&str; 3] =
    ["pending_raw", "pending_header", "pending_full_transactions"];

#[test]
fn no_source_outside_the_live_crate_speaks_the_radars_vocabulary() {
    // §7's 「不得将 Early Radar 直接接入 Signer 或 Submitter」, read as a name scan: a crate
    // that cannot spell `EarlyRadar` cannot feed one to a signer.
    let mut scanned_files = 0usize;
    let mut offenders: Vec<String> = Vec::new();
    for crate_name in [
        "core",
        "chain",
        "protocol",
        "state",
        "graph",
        "pathfinder",
        "discovery",
        "opportunity",
        "simulation",
        "risk",
        "execution",
        "metrics",
        "pipeline",
        "cli",
        "replay",
    ] {
        for path in sources(crate_name) {
            scanned_files += 1;
            let text = code_only(
                &std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display())),
            );
            let hits: Vec<&str> = RADAR_VOCABULARY
                .iter()
                .copied()
                .filter(|needle| occurrences(&text, needle) > 0)
                .collect();
            if !hits.is_empty() {
                offenders.push(format!("{}: {hits:?}", path.display()));
            }
        }
    }
    assert!(
        scanned_files > 60,
        "only {scanned_files} files were scanned, so this is not the sweep it claims to be"
    );
    assert!(
        offenders.is_empty(),
        "a crate outside crates/live names the radar in code, which means an early view \
         could be consumed there: {offenders:?}"
    );

    // Positive control, and it is the load-bearing half: the identical needles are reported
    // in the crate that owns them. Without this row the zeros above would also be produced
    // by needles that match nothing anywhere.
    let in_live: usize = sources("live")
        .iter()
        .map(|path| {
            let text = code_only(
                &std::fs::read_to_string(path)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display())),
            );
            RADAR_VOCABULARY
                .iter()
                .map(|needle| occurrences(&text, needle))
                .sum::<usize>()
        })
        .sum();
    assert!(
        in_live > 50,
        "the needles matched {in_live} times in crates/live/src, which is not what a live \
         radar's own code would look like — the needles, not the scan, are the suspect"
    );
}

/// Where each occurrence of `needle` starts, so a hit can be classified by position rather
/// than excused by name.
fn positions(text: &str, needle: &str) -> Vec<usize> {
    text.match_indices(needle)
        .map(|(offset, _)| offset)
        .collect()
}

/// Every `.rs` file under one directory, sorted, recursing into subdirectories.
fn rust_files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut walk = vec![root.to_path_buf()];
    while let Some(dir) = walk.pop() {
        if !dir.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&dir).expect("a directory we just checked") {
            let path = entry.expect("a readable directory entry").path();
            if path.is_dir() {
                walk.push(path);
            } else if path.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn the_radar_is_constructed_only_by_tests_and_its_own_test_module() {
    // The honest status §7 asks for, measured as call sites rather than asserted as a
    // sentence: this repository has a radar, and nothing in it runs one.
    let crates_root = repo().join("crates");
    let mut production_sites: Vec<String> = Vec::new();
    let mut test_sites = 0usize;
    for crate_dir in std::fs::read_dir(&crates_root)
        .expect("crates exists")
        .flatten()
    {
        for path in rust_files_under(&crate_dir.path().join("src")) {
            let text = code_only(
                &std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display())),
            );
            // The test module of a source file is code the binary never builds, so a
            // constructor below its `#[cfg(test)]` is counted as a test site — and only
            // below it, which is why this is a position and not a filename.
            let marker = text.find("#[cfg(test)]");
            for needle in RADAR_CONSTRUCTORS {
                for offset in positions(&text, needle) {
                    if marker.is_some_and(|test_module| offset > test_module) {
                        test_sites += 1;
                    } else {
                        production_sites.push(format!("{} :: {needle}", path.display()));
                    }
                }
            }
        }
        for path in rust_files_under(&crate_dir.path().join("tests")) {
            let text = code_only(
                &std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display())),
            );
            test_sites += RADAR_CONSTRUCTORS
                .iter()
                .map(|needle| positions(&text, needle).len())
                .sum::<usize>();
        }
    }
    assert!(
        production_sites.is_empty(),
        "a radar or one of its frame sources is constructed in code that a binary builds — \
         that is an early view entering the run link, and §7 forbids it: {production_sites:?}"
    );
    // The same four needles do have call sites, all of them tests: that count is the proof
    // that the scan above counts something. Measured 17 — five `PreconfLink::new`, four
    // `EarlyRadar::new`, six `PollingFrameSource::new`, two `ReplayFrameSource::new` — of
    // which four are this file's own `RADAR_CONSTRUCTORS` string literals, kept because the
    // scan keeps strings, and three are in `preconf_provider.rs` below its `#[cfg(test)]`.
    // The bound below is a slack on the number, not a guess at it.
    assert!(
        test_sites >= 8,
        "only {test_sites} construction sites were found anywhere, including crates/live/tests, \
         so these needles are not live"
    );

    // And the one harness that would put the radar on a real endpoint is `#[ignore]`d, so
    // `cargo test --workspace` never enters it and no committed run can claim it ran.
    let live_giwa = read("crates/live/tests/preconf_live_giwa.rs");
    assert!(
        live_giwa.contains("#[ignore]"),
        "the real-endpoint radar harness is no longer ignored, so an ordinary test run now \
         spends requests and the status recorded in M12-D §7 is out of date"
    );
}

#[test]
fn the_run_loop_records_a_candidate_and_acts_only_on_a_sealed_block() {
    // 「不得因为 Pending 数据可解析，就认定它可以直接驱动执行」, checked where it would
    // actually matter: the arm of the match that consumes the event stream. A pending view
    // that arrives here is written to the candidates file; only `Canonical` reaches the
    // engine and the simulation dispatch.
    let runner = code_only(&read("crates/pipeline/src/runner.rs"));
    let candidate = match_arm(&runner, "MarketEvent::Candidate(candidate) =>")
        .expect("the candidate arm of the event match");
    let canonical = match_arm(&runner, "MarketEvent::Canonical(announcement) =>")
        .expect("the canonical arm of the event match");

    for forbidden in ["engine", "dispatch", "sim", "send("] {
        assert!(
            !candidate.contains(forbidden),
            "the candidate arm reaches `{forbidden}`, so an unsealed view drives work: {candidate}"
        );
    }
    assert!(
        candidate.contains("evidence") && candidate.contains("Candidates"),
        "the candidate arm's whole job is the record; if it stopped recording, §51's \
         'reported, never silently forgotten' would be false: {candidate}"
    );

    // Positive control: the extractor is not returning an empty slice, and the sibling arm
    // that is allowed to act does act. Without this the four zeros above would pass on a
    // match arm the scanner never found.
    assert!(
        canonical.contains("on_canonical") && canonical.contains("dispatch"),
        "the canonical arm no longer reaches the engine and the dispatch, which means this \
         test has lost its comparison: {canonical}"
    );

    // The pending *state* reads are the other half of the same ban: they exist, they are
    // used by the candidate source, and no crate that owns execution or simulation calls
    // one.
    for crate_name in DOWNSTREAM_CRATES {
        for path in sources(crate_name) {
            let text = code_only(
                &std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display())),
            );
            let hits: Vec<&str> = PENDING_STATE_READS
                .iter()
                .copied()
                .filter(|needle| occurrences(&text, needle) > 0)
                .collect();
            assert!(
                hits.is_empty(),
                "{} reads a pending block, which would put an unsealed view under a state \
                 reader: {hits:?}",
                path.display()
            );
        }
    }
    let used_in_live = sources("live")
        .iter()
        .filter(|path| {
            let text = code_only(
                &std::fs::read_to_string(path)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display())),
            );
            PENDING_STATE_READS
                .iter()
                .any(|needle| occurrences(&text, needle) > 0)
        })
        .count();
    assert!(
        used_in_live >= 2,
        "the pending-read needles matched in {used_in_live} live source files; the candidate \
         source and the polling frame source both call them, so a lower count means the \
         needles are broken, not that the ban holds"
    );
}

#[test]
fn the_lanes_only_pending_read_is_a_nonce_and_is_named_as_one() {
    // The distinction the last test's zero depends on. The execution lane *does* read a
    // pending view, and it must stay that way: `eth_getTransactionCount(addr, "pending")` is
    // what the nonce is, and §26's preflight says so. A gate that banned the word "pending"
    // would either be red on the correct code or would have to exempt it, and an exemption
    // written that loosely would also exempt a pending block.
    let sequencer = code_only(&read("crates/execution/src/giwa/sequencer_direct.rs"));
    assert!(
        sequencer.contains("eth_getTransactionCount") && sequencer.contains("\"pending\""),
        "the lane's pending nonce read has changed shape; this test's distinction between a \
         pending account view and a pending block view needs a re-read, not a re-assertion"
    );
    assert!(
        !sequencer.contains("eth_getBlockByNumber([\"pending\"")
            && !PENDING_STATE_READS
                .iter()
                .any(|needle| sequencer.contains(needle)),
        "the same file that reads a pending nonce now reads a pending block too, which is a \
         different permission and a different risk"
    );
}

#[test]
fn the_signer_and_the_radar_are_two_crates_that_never_meet_in_a_manifest() {
    // §7's ban stated as a build fact rather than a review habit: the crate that signs cannot
    // compile against the crate that holds the radar, in either direction.
    //
    // `preconf_isolation.rs` walks this edge from `evm-live` outward (§58). The direction
    // that guards the signer is the other one, and it is checked here.
    let execution_manifest = read("crates/execution/Cargo.toml");
    let live_manifest = read("crates/live/Cargo.toml");
    let pipeline_manifest = read("crates/pipeline/Cargo.toml");
    let production_deps = |text: &str| {
        let start = text
            .find("[dependencies]")
            .expect("a production dependency section");
        let tail = &text[start..];
        let end = tail[1..]
            .find("\n[")
            .map(|offset| offset + 1)
            .unwrap_or(tail.len());
        tail[..end].to_string()
    };

    assert!(
        !production_deps(&execution_manifest).contains("evm-live"),
        "evm-execution gained a production dependency on evm-live: a signer would be able to \
         reach a preconfirmation frame"
    );
    assert!(
        !production_deps(&live_manifest).contains("evm-execution"),
        "evm-live gained a production dependency on evm-execution: the radar would be able to \
         reach a submitter"
    );
    // Positive control: the reader does parse manifests, and the crate that owns the run link
    // legitimately depends on both — which is exactly why the type-level ban above is not
    // enough on its own and the candidate arm is the load-bearing check.
    let pipeline_deps = production_deps(&pipeline_manifest);
    assert!(
        pipeline_deps.contains("evm-live") && pipeline_deps.contains("evm-execution"),
        "evm-pipeline no longer depends on both crates, so this test's control is broken: {pipeline_deps}"
    );
}

/// The body of a `match` arm, by brace counting from the pattern.
fn match_arm(text: &str, pattern: &str) -> Option<String> {
    let start = text.find(pattern)? + pattern.len();
    let open = text[start..].find('{')? + start;
    let mut depth = 0usize;
    let mut body = String::new();
    for c in text[open..].chars() {
        match c {
            '{' => {
                depth += 1;
                if depth == 1 {
                    continue;
                }
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(body);
                }
            }
            _ => {}
        }
        body.push(c);
    }
    None
}
