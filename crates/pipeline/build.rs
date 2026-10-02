//! §44 asks every latency file to name the commit it was built from, and the value is
//! read from the environment by `option_env!` in `src/latency.rs`.
//!
//! That combination needs this line: cargo has no way to know an environment variable
//! feeds a `option_env!`, so a build that happens with the variable unset can reuse an
//! artifact compiled with it set — or rebuild without it and silently turn a real
//! revision into `unrecorded`. Naming the variable is what makes the fingerprint track
//! it, which is the difference between a revision that is checkable and one that is a
//! coincidence of the last command typed.

fn main() {
    println!("cargo:rerun-if-env-changed=GIT_REVISION");
}
