//! §26, §44 and §45, checked against the source text rather than against memory.
//!
//! These three rules are all prohibitions, and a prohibition is only worth what
//! its enforcement is worth: a paragraph in a report saying "nothing is sent" is
//! a claim, while a test that fails the moment a `sendRawTransaction` appears is
//! a fact. The point of this file is that the next change — by anyone, including
//! a future run of this same session — cannot quietly turn a decision-support
//! tool into an executor.
//!
//! The scan is over *production* code: comments are dropped because the codebase
//! states these bans in prose (`no signer, no private key` in a module doc is the
//! rule being followed, not broken), and everything from `#[cfg(test)]` onward is
//! dropped because a test may legitimately name a real endpoint or a real chain id
//! in order to assert something about it.
//!
//! M6 changed the shape of the ban, not its strength. §17–§23 put the signing and
//! submission surface in exactly one crate (`evm-execution`), behind a mode that
//! defaults to not using it and a gate that must be passed before it is. So the
//! keyword list below is now enforced against *every other crate*: the pipeline, the
//! live path, the metrics path and the CLI may hand a risk decision to the execution
//! stage, but must stay unable to name a submission method, a key type, or the
//! environment variable a key lives in — even by accident. What the execution crate
//! is allowed to do is pinned by the three guards after that list — no key material
//! in code, no `unsafe`, and no dependency that could send on its own.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every `crates/*/src/**/*.rs` file in the workspace.
fn production_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let crates = workspace_root().join("crates");
    let entries =
        std::fs::read_dir(&crates).unwrap_or_else(|error| panic!("{}: {error}", crates.display()));
    for entry in entries {
        let src = entry.expect("a crate directory").path().join("src");
        if !src.is_dir() {
            continue;
        }
        collect(&src, &mut files);
    }
    files.sort();
    files
}

/// Everything except the one crate M6 gave this ability to.
fn non_execution_production_files() -> Vec<PathBuf> {
    production_files()
        .into_iter()
        .filter(|path| !in_execution_crate(path))
        .collect()
}

fn in_execution_crate(path: &Path) -> bool {
    path.display().to_string().contains("crates/execution/src")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("a readable source directory") {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The code a guard may hold this file to: no comment lines, and nothing below
/// the inline test module.
///
/// `#[cfg(test)]` is only ever used here on a whole module at the end of a file,
/// which is the shape `cargo new` produces and this workspace keeps; if that ever
/// stops being true this function, not the tests, is what needs to change.
fn production_code(path: &Path) -> String {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let code = text.split("\n#[cfg(test)]").next().unwrap_or_default();
    code.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// §26's list, in the words a compiler would use, enforced on every crate that is not
/// the execution layer.
///
/// Chosen to be *identifiers and RPC method names*, not English verbs: the word
/// "broadcast" appears in this workspace deliberately (`NO_BROADCAST` is a
/// constant the risk layer stamps on every decision), while `sendRawTransaction`
/// never should. A ban that trips on prose would be deleted the first time it
/// annoyed someone; one that trips on a method name will not.
const BANNED_OUTSIDE_THE_EXECUTION_CRATE: &[&str] = &[
    "sendRawTransaction",
    "eth_sendBundle",
    "mev_send",
    "flashbots",
    "SequencerDirect",
    "SigningKey",
    "PrivateSigner",
    "private_key",
    "secret_key",
    "keystore",
    // M6's own surface, named as precisely as it is defined: the key type, the one
    // constructor that takes a key's bytes, the one call that reads the key from the
    // environment, and the environment variable's name. The pipeline and the CLI hand
    // a decision to `ExecutionStage` and are not allowed to need any of these — if a
    // future change reaches for a key outside the crate that gates it, §17's promise
    // that exactly one place can hold one has already been broken.
    "ExecutionKey",
    "from_secret_bytes",
    "Signer::from_env",
    "GIWA_EXECUTION_PRIVATE_KEY",
];

/// Dependencies that would give this binary the ability §26 withholds from it.
///
/// Unchanged by M6, and that is the point: `evm-execution` signs with `k256` directly
/// and talks to the node through the same HTTP adapter M1–M5 already used, so no crate
/// that bundles a signer, a provider, or a transaction manager entered the workspace. A
/// dependency ban is the guard a keyword ban cannot be — an `alloy-signer` type would
/// let any crate construct a signer without ever naming a key.
const BANNED_DEPENDENCIES: &[&str] = &[
    "alloy-signer",
    "alloy-network",
    "alloy-provider",
    "alloy-transport",
    "alloy-rpc-client",
    "foundry-evm",
];

#[test]
fn no_code_outside_the_execution_crate_can_send_a_transaction() {
    let mut offenders = Vec::new();
    for file in non_execution_production_files() {
        let code = production_code(&file);
        for banned in BANNED_OUTSIDE_THE_EXECUTION_CRATE {
            if code.contains(banned) {
                offenders.push(format!("{} names `{banned}`", file.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "§26 stops a run at a risk decision and §17 keeps what comes after it in one crate; \
         these lines put that ability somewhere else:\n{}",
        offenders.join("\n")
    );
    // The scan has to be reading something, or a green result is an empty input. The
    // exception then has to stay one crate wide: if the pipeline could name a signer
    // type, the mode gate would be decorative.
    let scanned = non_execution_production_files().len();
    assert!(
        scanned > 30,
        "the scan found only {scanned} source files outside the execution crate"
    );
    let execution = production_files().len() - scanned;
    // M6 opened this crate with 19 files (signer, tx/rlp codec, submitter, receipt,
    // lifecycle, gate, stage, …). M7 added seven more — `cost.rs`, `preflight.rs`,
    // `profit.rs`, `market.rs`, `sequence.rs`, `giwa/reads.rs`,
    // `giwa/preflight_facts.rs` — because §35–§39's cost model, §26's thirteen
    // checks, §17's asset deltas, §51's market label and §54's six-step ladder are
    // all facts about *sending*, so they belong on this side of the wall, not in the
    // pipeline's. 26 today; the ceiling is 32 with one milestone of headroom, so a
    // jump to 40 still means what it says: the exception stopped being one layer.
    assert!(
        (4..=32).contains(&execution),
        "the execution crate holds {execution} source files, which is not the shape of one \
         gated layer — either this exception grew into the rest of the workspace or the \
         filter stopped matching it"
    );
}

/// §17: the private key is read from `GIWA_EXECUTION_PRIVATE_KEY` and never written
/// down.
///
/// A 64-hex-digit literal is what a key looks like, and every legitimate 32-byte value
/// in *shipped* code here is computed (a hash, a digest) rather than typed, so the shape
/// is a reliable signal outside a test module.
///
/// Inside the execution crate the scan goes further and reads the test modules too: §40
/// requires those tests to sign with a synthetic key, so a real 32-byte literal there
/// would be the user's wallet written into a fixture. Other crates' test modules are not
/// scanned, because an expected keccak hash in a simulation test has the same shape and
/// is not a secret.
#[test]
fn no_private_key_is_written_into_the_code() {
    let mut offenders = Vec::new();
    for file in production_files() {
        // The execution crate is read whole; everything else only above `#[cfg(test)]`.
        let text = if in_execution_crate(&file) {
            std::fs::read_to_string(&file).expect("a readable source file")
        } else {
            production_code(&file)
        };
        offenders.extend(hex_literal_lines(&text, &file));
    }
    for file in execution_test_files() {
        let text = std::fs::read_to_string(&file).expect("a readable test file");
        offenders.extend(hex_literal_lines(&text, &file));
    }
    assert!(
        offenders.is_empty(),
        "§17 puts the key in `GIWA_EXECUTION_PRIVATE_KEY` and nowhere else; a literal in the \
         source would outlive the run that needed it:\n{}",
        offenders.join("\n")
    );
    // A guard that reads nothing is green for the wrong reason, so name what it must
    // have opened: the real-data codec test is the file §14 lives in.
    let scanned = execution_test_files();
    assert!(
        scanned
            .iter()
            .any(|path| path.file_name().is_some_and(|name| name == "real_codec.rs")),
        "the key scan did not open the execution crate's integration tests: {scanned:?}"
    );
}

fn hex_literal_lines(text: &str, file: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        for token in line.split(|c: char| !c.is_ascii_hexdigit() && c != '_') {
            let digits = token.trim_start_matches('0');
            if digits.len() == 64 && digits.chars().all(|c| c.is_ascii_hexdigit()) {
                out.push(format!(
                    "{} carries a 32-byte hex literal: {}",
                    file.display(),
                    line.trim()
                ));
            }
        }
    }
    out
}

/// The execution crate's `tests/*.rs`, which §40 holds to the same rule as its source.
fn execution_test_files() -> Vec<PathBuf> {
    let dir = workspace_root()
        .join("crates")
        .join("execution")
        .join("tests");
    let mut files = Vec::new();
    if dir.is_dir() {
        collect(&dir, &mut files);
    }
    // The fixtures directory holds chain data, not keys, and a real transaction's `r`
    // and `s` are the same shape as a key — so it is deliberately not scanned.
    files.retain(|path| !path.display().to_string().contains("fixtures"));
    files.sort();
    files
}

#[test]
fn no_dependency_could_add_sending_ability() {
    let mut offenders = Vec::new();
    for manifest in crate_manifests() {
        let text = std::fs::read_to_string(&manifest).expect("a manifest");
        for banned in BANNED_DEPENDENCIES {
            // `[dependencies] alloy-signer = ...`, and the same name in a comment
            // explaining why it is absent must not count; a manifest line always
            // starts the name at the beginning of a token.
            for line in text.lines() {
                let line = line.trim();
                if line.starts_with('#') {
                    continue;
                }
                if line.starts_with(&format!("{banned} "))
                    || line.starts_with(&format!("{banned}="))
                    || line.starts_with(&format!("{banned} ="))
                {
                    offenders.push(format!("{} pulls {banned}", manifest.display()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "§26's ban is also a ban on the dependencies that would make sending \
         possible:\n{}",
        offenders.join("\n")
    );
}

fn crate_manifests() -> Vec<PathBuf> {
    let mut all = vec![workspace_root().join("Cargo.toml")];
    let crates = workspace_root().join("crates");
    for entry in std::fs::read_dir(&crates).expect("the crates directory") {
        let manifest = entry.expect("an entry").path().join("Cargo.toml");
        if manifest.is_file() {
            all.push(manifest);
        }
    }
    all
}

#[test]
fn no_endpoint_and_no_chain_identity_is_baked_into_the_code() {
    // §44: URLs come from flags or environment only. §45: the chain is what the
    // endpoint answers for, so the number 91342 is not a condition anywhere above
    // the adapter.
    let mut offenders = Vec::new();
    for file in production_files() {
        let code = production_code(&file);
        for scheme in ["https://", "http://", "wss://", "ws://"] {
            if code.contains(scheme) {
                offenders.push(format!("{} hardcodes a {scheme} URL", file.display()));
            }
        }
        if code.contains("91342") {
            offenders.push(format!("{} names the chain id 91342", file.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "pointing this binary at another node has to stay a flag, not a code change \
         (§44, §45):\n{}",
        offenders.join("\n")
    );
}

#[test]
fn no_unsafe_in_the_data_or_execution_path() {
    // §22's chosen option — one runtime per worker thread, driven from `main` —
    // exists so that `Send` is checked by the compiler rather than asserted here.
    // An `unsafe impl Send` would not be a style choice; it would be the claim
    // that the workers share state the compiler cannot see.
    let mut offenders = Vec::new();
    for crate_name in ["chain", "live", "pipeline", "metrics", "cli", "execution"] {
        let src = workspace_root().join("crates").join(crate_name).join("src");
        let mut files = Vec::new();
        collect(&src, &mut files);
        for file in files {
            let code = production_code(&file);
            if code.contains("unsafe") {
                offenders.push(file.display().to_string());
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "§22's simulation workers and §17's signer must not need `unsafe`: {}",
        offenders.join(", ")
    );
}
