//! M9.4 §58 — the architectural gate: a Flashblocks-derived preconfirmation state must
//! not be able to reach the canonical `StateStore`, and `PreconfirmationState` must be
//! isolated from `PoolState` at the type level (§4).
//!
//! This is a *negative* test, and a negative test has one failure mode that a positive
//! test does not: it passes because it looked at nothing. So every assertion here comes
//! paired with a control that is expected to report a hit — a synthetic line of code the
//! scanner must catch, and in two cases a real file in this repo the scanner does catch
//! (`crates/pipeline/Cargo.toml` for the dependency walk, `crates/live/src/preconf.rs`
//! for the name scan). `the_gate_has_exposure` additionally asserts the file sets are
//! non-empty and named, so renaming a module cannot quietly turn the gate into a no-op.
//!
//! Three layers, strongest first:
//!
//! 1. **Build graph.** `evm-live`'s dependency closure, walked transitively out of the
//!    workspace `Cargo.toml` files, does not contain `evm-state`, `evm-graph`,
//!    `evm-pathfinder`, or any other canonical crate. A crate that is not in the closure
//!    cannot be named: `use evm_state::StateStore` in `crates/live/src` is not a style
//!    violation, it is a compile error. This is the whole of §4's "the type layer must
//!    prevent it", discharged on the toolchain rather than on discipline.
//! 2. **Source scan.** The five `crates/live/src/preconf*.rs` files mention no canonical
//!    writer in code (comments are stripped and reported separately, because §45's
//!    prohibition on PathFinder has to be *said* somewhere).
//! 3. **Shape scan.** `PoolState` has no `source` field and `evm-core` has no
//!    `Flashblocks` variant (§4's forbidden design), and none of the preconf types carry
//!    a reserve, balance, or price field (§13's "no final reserves" rule, structurally).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The workspace root, from `crates/live`.
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/live sits at the workspace root")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = repo().join(relative);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} could not be read: {error}", path.display()));
    assert!(
        !text.is_empty(),
        "{} is empty, so a scan over it proves nothing",
        path.display()
    );
    text
}

/// Every `.rs` file under `crates/<name>/src`, sorted — the scan's exposure must not
/// depend on directory order (§43).
fn sources(crate_name: &str) -> Vec<PathBuf> {
    let root = repo().join("crates").join(crate_name).join("src");
    let mut out = Vec::new();
    let mut walk = vec![root];
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

/// Line and block comments dropped, string literals kept. The preconf modules use `///`
/// prose to name the very things they must not call (§45's prohibition has to be said
/// somewhere), so a scan that counted comments would be a scan of the prohibition rather
/// than of the code — while a scan that truncated at the first `//` *inside* a string
/// would quietly lose the rest of the line, which is the wrong direction for a gate.
fn code_lines(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut kept = String::with_capacity(text.len());
    let mut in_string = false;
    let mut i = 0;
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
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            if i < chars.len() {
                kept.push('\n');
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            let mut closed = false;
            while i + 1 < chars.len() {
                if chars[i] == '*' && chars[i + 1] == '/' {
                    closed = true;
                    break;
                }
                if chars[i] == '\n' {
                    kept.push('\n');
                }
                i += 1;
            }
            assert!(
                closed,
                "an unterminated block comment: the scanner would be guessing where the \
                 code resumes"
            );
            i += 2;
            continue;
        }
        kept.push(c);
        i += 1;
    }
    kept.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn token_hits(text: &str, tokens: &[&str]) -> Vec<String> {
    let mut hits = Vec::new();
    for line in code_lines(text) {
        for token in tokens {
            if line.contains(token) {
                hits.push(format!("{token} :: {line}"));
            }
        }
    }
    hits
}

/// The local crates this workspace is made of, as `directory -> package name`.
fn local_packages() -> BTreeMap<String, String> {
    let crates_dir = repo().join("crates");
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(&crates_dir).expect("crates/ exists") {
        let dir = entry.expect("a readable crates/ entry").path();
        let manifest = dir.join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&manifest).expect("a readable Cargo.toml");
        let name = package_name(&text).unwrap_or_else(|| {
            panic!("{} declares no package name", manifest.display());
        });
        out.insert(
            dir.file_name()
                .expect("a crate directory")
                .to_string_lossy()
                .into_owned(),
            name,
        );
    }
    assert!(out.len() >= 12, "only {out:?} looked like workspace crates");
    out
}

fn package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package && line.starts_with("name") {
            return line
                .split('=')
                .nth(1)
                .map(|rest| rest.trim().trim_matches('"').to_string());
        }
    }
    None
}

/// The `evm-*` crates listed in `[dependencies]` — not `[dev-dependencies]`, which are
/// compiled for tests and examples only and cannot appear in `src`.
fn production_deps(manifest: &str) -> Vec<String> {
    let mut inside = false;
    let mut out = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == "[dependencies]";
            continue;
        }
        if !inside || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(key) = line.split(['=', '.']).next() {
            let key = key.trim();
            if key.starts_with("evm-") {
                out.push(key.to_string());
            }
        }
    }
    out
}

/// Every crate reachable from `start` through production dependencies, transitively —
/// which is the point: a canonical crate smuggled in behind `evm-chain` would still be
/// nameable from `crates/live/src`.
fn closure(start: &str, packages: &BTreeMap<String, String>) -> BTreeSet<String> {
    let by_name: BTreeMap<&str, &str> = packages
        .iter()
        .map(|(dir, name)| (name.as_str(), dir.as_str()))
        .collect();
    let mut seen = BTreeSet::new();
    let mut queue = vec![start.to_string()];
    while let Some(crate_name) = queue.pop() {
        if !seen.insert(crate_name.clone()) {
            continue;
        }
        let Some(dir) = by_name.get(crate_name.as_str()) else {
            continue; // an crates.io dependency, not a workspace member
        };
        let manifest = repo()
            .join("crates")
            .join(dir)
            .join("Cargo.toml")
            .canonicalize()
            .unwrap_or_else(|_| panic!("{} has no Cargo.toml", dir));
        let text = std::fs::read_to_string(manifest).expect("a readable manifest");
        for dep in production_deps(&text) {
            if !seen.contains(&dep) {
                queue.push(dep);
            }
        }
    }
    seen
}

/// The canonical side of the boundary §3.1 and §45 draw.
const CANONICAL_CRATES: [&str; 8] = [
    "evm-state",
    "evm-graph",
    "evm-pathfinder",
    "evm-discovery",
    "evm-opportunity",
    "evm-simulation",
    "evm-pipeline",
    "evm-execution",
];

/// The identifiers §3.1 forbids inside the radar's own code.
const CANONICAL_WRITERS: [&str; 9] = [
    "evm_state",
    "StateStore",
    "StateUpdate",
    "PoolState",
    "GraphBuilder",
    "GraphSnapshot",
    "PathFinder",
    "evm_graph",
    "evm_pathfinder",
];

/// The names §58 forbids on the canonical side: nothing in the state, graph, pathfinder,
/// discovery or pipeline crates may even know the radar exists.
const RADAR_NAMES: [&str; 4] = ["Preconf", "preconf", "EarlyRadar", "RadarEvent"];

fn preconf_sources() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = sources("live")
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("preconf"))
        })
        .collect();
    files.sort();
    files
}

#[test]
fn the_build_graph_itself_is_the_isolation() {
    // §58's strongest form: the canonical crates are not reachable, so a call into the
    // store is not a review finding — it does not compile.
    let packages = local_packages();
    let reachable = closure("evm-live", &packages);
    let leaked: Vec<&str> = CANONICAL_CRATES
        .iter()
        .filter(|crate_name| reachable.contains(**crate_name))
        .copied()
        .collect();
    assert!(
        leaked.is_empty(),
        "crates/live reaches {leaked:?} through production dependencies; §3.1's \
         prohibition would then be a convention rather than a type error"
    );
    assert!(
        reachable.contains("evm-live"),
        "the walk starts where it claims to: {reachable:?}"
    );

    // Positive control: the same walker does find the canonical crates for the crate
    // that legitimately owns them. Without this row the assertion above would also pass
    // if `production_deps` returned nothing at all.
    let pipeline = closure("evm-pipeline", &packages);
    assert!(
        pipeline.contains("evm-state"),
        "the walker cannot see evm-state from evm-pipeline either, so it is not walking: \
         {pipeline:?}"
    );
    assert!(pipeline.contains("evm-graph"), "{pipeline:?}");

    // …and it reads dev-dependencies as exactly what they are: present in the manifest,
    // absent from the closure. evm-live has evm-state for its tests only.
    let live_manifest = read("crates/live/Cargo.toml");
    assert!(
        live_manifest.contains("[dev-dependencies]") && live_manifest.contains("evm-state"),
        "evm-live's test fixtures do build a canonical store; that is the point of the \
         distinction, and the gate must keep making it"
    );
    assert!(
        !production_deps(&live_manifest)
            .iter()
            .any(|dep| dep == "evm-state"),
        "evm-state is listed in evm-live's production dependencies, which is exactly the \
         line §58 forbids: {:?}",
        production_deps(&live_manifest)
    );
    assert!(
        !reachable.contains("evm-state"),
        "and the transitive walk agrees: {reachable:?}"
    );
}

#[test]
fn no_radar_source_file_names_a_canonical_writer() {
    let files = preconf_sources();
    assert_eq!(files.len(), 5, "the five M9.4 modules: {files:?}");
    for path in &files {
        let text = std::fs::read_to_string(path).expect("a readable module");
        let name = path
            .file_name()
            .expect("a file")
            .to_string_lossy()
            .into_owned();
        let hits = token_hits(&text, &CANONICAL_WRITERS);
        assert!(
            hits.is_empty(),
            "crates/live/src/{name} calls into the canonical side in code: {hits:?}"
        );
    }
}

#[test]
fn no_canonical_source_file_names_the_radar() {
    // The other direction, which is the half that actually protects M9.2 and M9.3: if no
    // canonical crate can name a preconf type, no canonical code path can read one.
    let mut scanned = 0usize;
    for crate_name in [
        "core",
        "state",
        "graph",
        "pathfinder",
        "discovery",
        "pipeline",
    ] {
        for path in sources(crate_name) {
            scanned += 1;
            let text = std::fs::read_to_string(&path).expect("a readable module");
            let hits = token_hits(&text, &RADAR_NAMES);
            assert!(
                hits.is_empty(),
                "{} mentions the radar, so a canonical path could consume it: {hits:?}",
                path.display()
            );
        }
    }
    assert!(scanned > 30, "only {scanned} files were scanned");

    // Positive control: the same scan over the radar's own module does report the names,
    // so the zero above is about the canonical crates and not about a scan that never
    // matches anything.
    let control = token_hits(&read("crates/live/src/preconf.rs"), &RADAR_NAMES);
    assert!(
        !control.is_empty(),
        "the name scan found nothing even in the file that defines the types"
    );
}

#[test]
fn pool_state_has_no_source_field_and_core_has_no_flashblocks_variant() {
    // §4's forbidden design, checked as a field list rather than as a vibe:
    // `PoolState { source: Rpc | Flashblocks }` is what would let a candidate be read as
    // a confirmed state, and it would be indistinguishable in every downstream table.
    let pool = read("crates/core/src/pool.rs");
    let fields = struct_fields(&pool, "pub struct PoolState");
    assert!(
        !fields.iter().any(|field| field == "source"),
        "PoolState gained a `source` field: {fields:?}"
    );
    assert_eq!(
        fields,
        vec![
            "pool".to_string(),
            "reserve0".to_string(),
            "reserve1".to_string(),
            "block_number".to_string(),
            "log_index".to_string(),
        ],
        "PoolState's shape is a claim this gate pins down; a change here is a §4 decision, \
         not a refactor"
    );
    for crate_name in ["core", "state", "graph"] {
        for path in sources(crate_name) {
            let text = std::fs::read_to_string(&path).expect("a readable module");
            let hits = token_hits(&text, &["Flashblocks", "Flashblock"]);
            assert!(
                hits.is_empty(),
                "{} grew a Flashblock variant on a canonical type: {hits:?}",
                path.display()
            );
        }
    }

    // Positive control: the field extractor and the variant scan both do report the
    // forbidden shape when it is written out.
    let hypothetical = "pub struct PoolState {\n    pub pool: PoolId,\n    pub source: Venue,\n}\n";
    assert_eq!(
        struct_fields(hypothetical, "pub struct PoolState"),
        vec!["pool".to_string(), "source".to_string()]
    );
    assert_eq!(
        token_hits("pub enum Source { Rpc, Flashblocks }\n", &["Flashblocks"]).len(),
        1
    );
}

#[test]
fn the_radar_types_carry_no_reserves_or_prices() {
    // §13: a flashblock may say which pool changed, never what the pool's reserves became.
    // A reserve field on any of these types would be the second state representation the
    // task book forbids, and would make `Verified != Canonical` unauditable.
    let preconf = read("crates/live/src/preconf.rs");
    let banned = [
        "reserve",
        "balance",
        "price",
        "amount_in",
        "amount_out",
        "sqrt_price",
    ];
    for name in [
        "pub struct PreconfirmationState",
        "pub struct PreconfirmationFrame",
        "pub struct AffectedPool",
        "pub struct PreconfTransaction",
        "pub struct PreconfReceipt",
        "pub struct PreconfLog",
        "pub struct PreconfIdentity",
    ] {
        let fields = struct_fields(&preconf, name);
        assert!(!fields.is_empty(), "{name} was not found as written");
        let leaks: Vec<&String> = fields
            .iter()
            .filter(|field| banned.iter().any(|bad| field.contains(bad)))
            .collect();
        assert!(leaks.is_empty(), "{name} carries {leaks:?}");
    }

    // Positive control.
    assert_eq!(
        struct_fields("pub struct X {\n pub reserve0: U256,\n}\n", "pub struct X"),
        vec!["reserve0".to_string()]
    );
}

#[test]
fn the_canonical_bridge_is_identity_only_and_one_way() {
    // The single place the two sides touch: `note_canonical(&CanonicalDigest)`. It takes
    // an identity type defined inside this crate, and returns events — so canonical data
    // flows in and findings flow out, and nothing flows back into the store.
    let radar = read("crates/live/src/preconf_radar.rs");
    assert!(
        radar.contains("pub fn note_canonical("),
        "the reconciliation entry point the report names is gone"
    );
    let digest = struct_fields(&radar, "pub struct CanonicalDigest");
    let banned = ["reserve", "balance", "price", "source"];
    let leaks: Vec<&String> = digest
        .iter()
        .filter(|field| banned.iter().any(|bad| field.contains(bad)))
        .collect();
    assert!(
        leaks.is_empty(),
        "CanonicalDigest carries {leaks:?} — an identity type must not become a state type"
    );
    assert!(
        digest.contains(&"transaction_hashes".to_string()),
        "and it must still carry what §16's content check reads: {digest:?}"
    );
}

#[test]
fn the_gate_has_exposure() {
    // A negative gate over a set that is empty passes for the wrong reason. These are the
    // counts the other tests are standing on, restated where a rename would break them.
    let names: Vec<String> = preconf_sources()
        .iter()
        .map(|path| {
            path.file_name()
                .expect("a file")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        names,
        vec![
            "preconf.rs",
            "preconf_decode.rs",
            "preconf_loop.rs",
            "preconf_provider.rs",
            "preconf_radar.rs",
        ]
    );
    assert!(!sources("state").is_empty());
    assert!(!sources("pathfinder").is_empty());

    // The comment stripper is load-bearing (prose in the radar names PathFinder, and the
    // canonical crates carry block comments), so its three behaviours are pinned here
    // rather than assumed.
    assert_eq!(
        token_hits(
            "// use evm_state::StateStore;\n/// §45 forbids PathFinder\n/* GraphBuilder */\nfn f() {}\n",
            &["evm_state", "PathFinder", "GraphBuilder"]
        ),
        Vec::<String>::new(),
        "a comment of either shape was counted as a call"
    );
    assert_eq!(
        token_hits("let store = StateStore::new();\n", &["StateStore"]).len(),
        1,
        "and code is not invisible either"
    );
    assert_eq!(
        token_hits(
            "let marker = \"rpc//placeholder\"; Store::note(marker);\n",
            &["Store"]
        )
        .len(),
        1,
        "a `//` inside a string must not blind the scanner to the rest of the line"
    );
}

/// The field names of `signature { … }`, in declaration order. Deliberately dumb: it
/// finds the struct by its literal header and reads `pub <name>:` lines until the
/// closing brace, so what the gate claims to inspect is what it inspects.
fn struct_fields(text: &str, signature: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let Some(start) = text.find(signature) else {
        return fields;
    };
    let body = &text[start..];
    // The header line carries the opening brace, so the body starts inside it.
    for line in body.lines().skip(1) {
        let line = line.trim();
        if line.starts_with("//") || line.is_empty() || line == "{" {
            continue;
        }
        if line.starts_with('}') {
            break;
        }
        if let Some(rest) = line.strip_prefix("pub ") {
            if let Some(field) = rest.split(':').next() {
                let field = field.trim();
                if !field.is_empty() {
                    fields.push(field.to_string());
                }
            }
        }
    }
    fields
}
