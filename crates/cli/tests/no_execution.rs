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

/// The §26 list, in the words a compiler would use.
///
/// Chosen to be *identifiers and RPC method names*, not English verbs: the word
/// "broadcast" appears in this workspace deliberately (`NO_BROADCAST` is a
/// constant the risk layer stamps on every decision), while `sendRawTransaction`
/// never should. A ban that trips on prose would be deleted the first time it
/// annoyed someone; one that trips on a method name will not.
const BANNED_IN_PRODUCTION_CODE: &[&str] = &[
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
];

/// Dependencies that would give this binary the ability §26 withholds from it.
const BANNED_DEPENDENCIES: &[&str] = &[
    "alloy-signer",
    "alloy-network",
    "alloy-provider",
    "alloy-transport",
    "alloy-rpc-client",
    "foundry-evm",
];

#[test]
fn no_production_code_can_send_a_transaction() {
    let mut offenders = Vec::new();
    for file in production_files() {
        let code = production_code(&file);
        for banned in BANNED_IN_PRODUCTION_CODE {
            if code.contains(banned) {
                offenders.push(format!("{} names `{banned}`", file.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "§26 stops a run at a risk decision; these lines would take it past that:\n{}",
        offenders.join("\n")
    );
    // The scan has to be reading something, or a green result is an empty input.
    assert!(
        production_files().len() > 30,
        "the scan found only {} source files",
        production_files().len()
    );
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
fn the_m5_path_uses_no_unsafe() {
    // §22's chosen option — one runtime per worker thread, driven from `main` —
    // exists so that `Send` is checked by the compiler rather than asserted here.
    // An `unsafe impl Send` would not be a style choice; it would be the claim
    // that the workers share state the compiler cannot see.
    let mut offenders = Vec::new();
    for crate_name in ["chain", "live", "pipeline", "metrics", "cli"] {
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
        "§22's simulation workers must not need `unsafe`: {}",
        offenders.join(", ")
    );
}
