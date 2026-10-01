//! §18/§20 and §23, read out of the source rather than asserted in prose.
//!
//! Both rules are about what a *call site* is allowed to say, which is the kind of
//! rule a test can only hold by looking at the text: no amount of running one
//! recorded block proves that a later change won't ask the node for `latest`, or
//! that the ingestion loop won't one day await a full simulation queue.
//!
//! A scan like this is only as good as its ability to see, so each check here is
//! paired with something the same scan has to find — the `pending` tag, the
//! `try_send` that is the allowed way in. An absence plus a presence is evidence;
//! an absence alone could be a broken glob.

use std::path::{Path, PathBuf};

fn crate_src(crate_name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("crates")
        .join(crate_name)
        .join("src")
}

/// Production code: comment lines dropped (the codebase states these bans in
/// prose, and prose is not a call site), and everything from `#[cfg(test)]` on,
/// which is where a test is allowed to name a thing in order to refuse it.
fn production_code(path: &Path) -> String {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let code = text.split("\n#[cfg(test)]").next().unwrap_or_default();
    code.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `.rs` file under the given crate's `src`.
fn source_files(crate_name: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk(&crate_src(crate_name), &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "the scan of {crate_name} found no source files at {}",
        crate_src(crate_name).display()
    );
    files
}

fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
    {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_stage_ever_asks_for_the_latest_state() {
    // §20: 「不允许使用 "latest" 模糊状态」. The only legitimate tags on this path
    // are a block number and, for a candidate read that never advances state,
    // `pending` — and both of those are visible to this same scan.
    let mut offenders = Vec::new();
    let mut saw_a_block_tag = false;
    for crate_name in [
        "chain", "state", "replay", "live", "pipeline", "metrics", "cli",
    ] {
        for file in source_files(crate_name) {
            let code = production_code(&file);
            if code.contains("\"latest\"") {
                offenders.push(format!("{} asks for state by tag `latest`", file.display()));
            }
            if code.contains("\"pending\"") {
                saw_a_block_tag = true;
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a finding's simulation must read the state it was priced against (§18, §19):\n{}",
        offenders.join("\n")
    );
    assert!(
        saw_a_block_tag,
        "the scan saw no block tag at all, which means it is not reading the code that \
         builds them — the absence of `latest` would be an empty-input result"
    );
}

#[test]
fn ingestion_never_waits_for_a_simulation_slot() {
    // §23 and §74: a full simulation queue is the pipeline refusing to slow down
    // for REVM, and the refusal has to be a `try_send` that returns immediately.
    // An `.await` on this queue would put the market clock behind the EVM, which
    // is how a stale finding gets priced as if it were current.
    let runner = crate_src("pipeline").join("runner.rs");
    let code = production_code(&runner);
    assert!(
        code.contains("try_send"),
        "the dispatch site should be offering jobs with `try_send`"
    );
    let awaited = code
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains(".send("))
        .map(|(index, _)| index + 1)
        .collect::<Vec<_>>();
    assert!(
        awaited.is_empty(),
        "runner.rs awaits a queue at lines {awaited:?}; the market path must only ever \
         offer a job non-blockingly (§23)"
    );
}

#[test]
fn a_declined_finding_is_written_down_rather_than_lost() {
    // §51 and §57: every way a finding can stop has to leave a line. This is the
    // inventory of the rules the pipeline is allowed to decline for — a new one
    // added to `plan_job` or `dispatch` without a test is what this catches.
    let rules = [
        "state_unavailable",
        "state_pin_mismatch",
        "state_version_mismatch",
        "funding_unavailable",
        "no_wrapped_native_configured",
        "route_not_executable",
        "request_refused",
        "simulation_queue_full",
    ];
    let mut sources = String::new();
    for name in ["runner.rs", "sim.rs"] {
        sources.push_str(&production_code(&crate_src("pipeline").join(name)));
    }
    for rule in rules {
        assert!(
            sources.contains(&format!("\"{rule}\"")),
            "`{rule}` is no longer a stated decline reason: either the code stopped \
             declining for it, or it now declines silently (§51)"
        );
    }
}
