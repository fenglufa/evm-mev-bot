//! §47's command line, tested as the contract it is.
//!
//! `live` is the only way anyone reaches this pipeline, and three of M5's rules
//! land here rather than in the engine: an endpoint comes from a flag or the
//! environment and never from the code (§44), the source that supplies canonical
//! blocks is decided by what was actually given (§32's parity run and §62's live
//! run are the *same binary*), and a run that the flags cannot describe exits with
//! a stated reason instead of starting a session that would misreport itself (§8).
//!
//! The mapping tests call [`evm_cli::parse_live`] with the argv a person would
//! type, so they hold for the flags as spelled, not for a struct literal that
//! clap never saw. The ones that need a real process — the environment variables,
//! the help text — spawn the built binary, which is also the only way to read an
//! environment variable without racing every other test in this file for it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use evm_cli::{default_registry_dirs, parse_live, DEFAULT_LATENCY_DIR};
use evm_execution::ExecutionMode;
use evm_pipeline::config::{CanonicalSource, QueueConfig};

/// One `live` invocation, spelled the way it would be typed.
fn live(extra: &[&str]) -> Result<evm_pipeline::config::PipelineConfig, String> {
    let mut argv = vec!["evm-mev-bot", "live"];
    argv.extend_from_slice(extra);
    let args = parse_live(&argv).map_err(|error| format!("the flags were refused: {error}"))?;
    args.to_config()
}

#[test]
fn a_recording_needs_no_endpoint_at_all() {
    // §32's parity acceptance runs on a directory of real blocks. Requiring an
    // RPC URL there would be requiring a second, different way to say "read these
    // files", and the point of the shared engine is that there isn't one.
    let config = live(&["--replay-dir", "fixtures/live-m5/arbitrage-window"]).expect("a config");
    assert!(
        matches!(
            config.canonical_source,
            CanonicalSource::Replay { ref directory }
                if directory.to_str() == Some("fixtures/live-m5/arbitrage-window")
        ),
        "--replay-dir alone should mean replay, got {:?}",
        config.canonical_source
    );
    assert_eq!(config.rpc_url, None, "a replay contacts nothing");
    assert_eq!(config.ws_url, None);

    // The same source named explicitly, because §75's report has to be able to
    // say which producer supplied the blocks a claim rests on.
    let explicit = live(&[
        "--source",
        "replay",
        "--replay-dir",
        "some/dir",
        "--rpc-url",
        "http://127.0.0.1:1",
    ])
    .expect("an explicit replay");
    assert!(matches!(
        explicit.canonical_source,
        CanonicalSource::Replay { .. }
    ));
    assert_eq!(
        explicit.rpc_url.as_deref(),
        Some("http://127.0.0.1:1"),
        "a recording of blocks is not a recording of state: an endpoint stays useful here"
    );
}

#[test]
fn a_source_that_cannot_be_served_is_refused_with_what_is_missing() {
    // Each refusal names the flag that would fix it, because the alternative is an
    // exit code 2 and a stack trace.
    assert_eq!(
        live(&["--source", "replay"]).unwrap_err(),
        "--source replay needs --replay-dir"
    );
    let refused = live(&["--source", "websocket", "--rpc-url", "http://a:1"]).unwrap_err();
    assert!(refused.contains("--ws-url"), "{refused}");
}

#[test]
fn an_inferred_source_follows_the_endpoints_given() {
    let ws = live(&["--rpc-url", "http://a:1", "--ws-url", "ws://a:2"]).expect("a websocket run");
    assert_eq!(ws.canonical_source, CanonicalSource::WebSocket);

    let poll = live(&["--rpc-url", "http://a:1"]).expect("a polling run");
    assert_eq!(
        poll.canonical_source,
        CanonicalSource::HttpPoll,
        "with no WebSocket endpoint the run polls, and the session record says so"
    );

    let forced = live(&[
        "--rpc-url",
        "http://a:1",
        "--ws-url",
        "ws://a:2",
        "--source",
        "http-poll",
    ])
    .expect("a forced poll");
    assert_eq!(
        forced.canonical_source,
        CanonicalSource::HttpPoll,
        "an explicit source outranks an idle URL"
    );

    // A WebSocket run without a WebSocket endpoint cannot be silently served by
    // HTTP polling: the §6 gap recovery would then be reading a different
    // connection pool than the one that named the head.
    let refused = live(&["--rpc-url", "http://a:1", "--source", "websocket"]).unwrap_err();
    assert!(refused.contains("--ws-url"), "{refused}");
}

#[test]
fn a_live_run_without_an_endpoint_says_which_variable_it_wants() {
    // §44: the code holds no default. The refusal has to teach the reader where to
    // put the URL, not merely that one is missing.
    let refused = live(&["--source", "http-poll"]).unwrap_err();
    assert!(refused.contains("--rpc-url"), "{refused}");
    assert!(refused.contains("GIWA_RPC_URL"), "{refused}");
}

#[test]
fn a_bad_address_is_named_rather_than_replaced_by_none() {
    let good = live(&[
        "--replay-dir",
        "d",
        "--wrapped-native",
        "0x4200000000000000000000000000000000000006",
    ])
    .expect("a funded run");
    assert_eq!(
        good.wrapped_native.map(|a| a.to_string()),
        Some("0x4200000000000000000000000000000000000006".to_string())
    );

    let absent = live(&["--replay-dir", "d"]).expect("an unfunded run");
    assert_eq!(
        absent.wrapped_native, None,
        "no wrapped native is a legal choice: every finding then declines with a stated \
         reason (§57) rather than being handed a manufactured balance"
    );

    let refused = live(&["--replay-dir", "d", "--wrapped-native", "0xnope"]).unwrap_err();
    assert!(refused.contains("0xnope"), "{refused}");
}

#[test]
fn the_queue_and_clock_knobs_reach_the_config_or_stay_defaulted() {
    let defaults = QueueConfig::default();
    let plain = live(&["--replay-dir", "d"]).expect("a defaulted run");
    assert_eq!(plain.queues, defaults);
    assert_eq!(plain.duration, std::time::Duration::from_secs(60));
    assert_eq!(plain.start_block, None, "absent means read the head (§7)");
    assert_eq!(plain.max_blocks, None);
    assert!(plain.progress, "progress lines are the default (§75)");
    assert_eq!(plain.source.poll_interval_ms, 900);
    assert_eq!(plain.flashblocks.poll_interval_ms, 250);
    assert_eq!(plain.registry_dirs, default_registry_dirs());

    let tuned = live(&[
        "--replay-dir",
        "d",
        "--event-capacity",
        "8",
        "--simulation-capacity",
        "9",
        "--outcome-capacity",
        "10",
        "--simulation-workers",
        "3",
        "--duration",
        "5",
        "--start-block",
        "100",
        "--max-blocks",
        "7",
        "--poll-interval-ms",
        "111",
        "--flashblock-poll-interval-ms",
        "222",
        "--quiet",
        "--minimum-net-profit-wei",
        "1000",
        "--maximum-gas",
        "900000",
        "--registry-dir",
        "one",
        "--registry-dir",
        "two",
        "--evidence-dir",
        "somewhere",
        "--state-dump",
        "dump.json",
        "--flashblocks-url",
        "http://cand:3",
    ])
    .expect("a tuned run");
    assert_eq!(
        tuned.queues,
        QueueConfig {
            event_capacity: 8,
            simulation_capacity: 9,
            outcome_capacity: 10,
            simulation_workers: 3,
        }
    );
    assert_eq!(tuned.duration, std::time::Duration::from_secs(5));
    assert_eq!(tuned.start_block, Some(100));
    assert_eq!(tuned.max_blocks, Some(7));
    assert_eq!(tuned.source.poll_interval_ms, 111);
    assert_eq!(tuned.flashblocks.poll_interval_ms, 222);
    assert!(!tuned.progress, "--quiet only turns off the progress lines");
    assert_eq!(tuned.risk.minimum_net_profit_wei, 1000);
    assert_eq!(tuned.risk.maximum_gas, Some(900_000));
    assert_eq!(
        tuned
            .registry_dirs
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>(),
        ["one", "two"],
        "repeatable, in the order given, merged by the engine (§46)"
    );
    assert_eq!(tuned.evidence_dir.display().to_string(), "somewhere");
    assert_eq!(tuned.state_dump.unwrap().display().to_string(), "dump.json");
    assert_eq!(tuned.flashblocks_url.as_deref(), Some("http://cand:3"));
}

#[test]
fn the_binary_itself_refuses_a_run_it_cannot_describe() {
    // The exit code is part of §47's contract: a script that runs a session must
    // be able to tell "the flags were wrong" (2) from "the chain would not align"
    // (1) without reading stderr.
    let output = binary(&["live", "--source", "http-poll"]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("GIWA_RPC_URL"),
        "the run that did not start still said where the endpoint belongs: {stderr}"
    );
}

#[test]
fn the_help_text_states_the_ban_it_is_under() {
    let output = binary(&["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("never signs") && help.contains("§26"),
        "{help}"
    );
    assert!(
        !help.contains("eth_sendRawTransaction"),
        "help is where a reader looks for what a tool can do; a sending method named here \
         would be the §26 ban broken in public"
    );
    // M6 widened what the binary may be asked to do, so the same test now pins the
    // two things the widening did not touch: the lane has to be named by a flag that
    // takes a mode word and not a key, and §34's limit has to be in the public
    // description rather than only in a source comment.
    assert!(
        help.contains("--execution-mode") && help.contains("§34"),
        "a reader of --help has to learn both that there is a lane and what it still may not \
         do: {help}"
    );
    assert!(
        !help.contains("--private-key") && !help.contains("--key"),
        "no flag may take key material (§17): {help}"
    );
}

#[test]
fn the_execution_lane_is_absent_unless_named_and_takes_no_key() {
    // §20: no flag at all means M5's run. This is acceptance U in its command-line
    // form — the binary that *can* be told to sign is the one that will not unless
    // a reader spells it out.
    let plain = live(&["--rpc-url", "http://127.0.0.1:1"]).expect("a config");
    assert!(
        plain.execution.is_none(),
        "no --execution-mode means no lane, not a lane that defaults to something"
    );

    for (flag, expected) in [
        ("build-only", ExecutionMode::BuildOnly),
        ("sign-only", ExecutionMode::SignOnly),
        ("submit", ExecutionMode::Submit),
    ] {
        let armed = live(&["--rpc-url", "http://127.0.0.1:1", "--execution-mode", flag])
            .expect("a lane config");
        let setup = armed.execution.expect("the lane the flag asked for");
        assert_eq!(setup.mode, expected);
        assert_eq!(armed.rpc_url.as_deref(), Some("http://127.0.0.1:1"));
    }

    // §20's other half: a typo is refused where a person can see it, rather than
    // quietly becoming the safe mode.
    let typo = live(&[
        "--rpc-url",
        "http://127.0.0.1:1",
        "--execution-mode",
        "submmit",
    ])
    .expect_err("a misspelled mode is not a run");
    assert!(typo.contains("submmit"), "{typo}");

    // And a lane with no endpoint is refused at the flag stage: the lane prices a
    // fee and reads a nonce from a node, which a recording does not have.
    let stranded = live(&["--replay-dir", "some/dir", "--execution-mode", "sign-only"])
        .expect_err("a replay has no endpoint for the lane to send through");
    assert!(stranded.contains("--rpc-url"), "{stranded}");
}

/// M8.1 §40 and §41, in flag form. The default run must reach the pipeline with no
/// latency directory at all — that absence is what makes the instrumented code path a
/// no-op rather than a cheap path — and either flag alone must turn tracing on.
#[test]
fn latency_tracing_is_off_until_a_flag_names_it() {
    // A replay: the traced and untraced runs are the same recording, so the only thing
    // the flags can change is where this run's latencies go.
    let traced_with = |extra: &[&str]| -> Option<PathBuf> {
        let mut argv = vec!["--replay-dir", "fixtures/live-m5/arbitrage-window"];
        argv.extend_from_slice(extra);
        live(&argv).expect("a config").latency_dir
    };

    assert_eq!(
        traced_with(&[]),
        None,
        "§40: no flag means the run exactly as M5–M7 wrote it, with no recorder to feed"
    );
    assert_eq!(
        traced_with(&["--latency-trace"]),
        Some(PathBuf::from(DEFAULT_LATENCY_DIR)),
        "--latency-trace alone has to name a directory, since it is the flag that says \
         this run measures something"
    );
    assert_eq!(
        traced_with(&["--latency-output", "target/latency-live"]),
        Some(PathBuf::from("target/latency-live")),
        "§41: naming a directory is itself an instruction to trace — a run that traced \
         nowhere would be a summary line and no evidence"
    );
    assert_eq!(
        traced_with(&["--latency-trace", "--latency-output", "target/latency-live"]),
        Some(PathBuf::from("target/latency-live")),
        "the directory the caller spelled out wins over the default"
    );

    // The flag reaches that directory and nothing else: no decision a live run makes is
    // a function of it (§2.1).
    let plain = live(&["--replay-dir", "fixtures/live-m5/arbitrage-window"]).expect("a config");
    let switched = live(&[
        "--replay-dir",
        "fixtures/live-m5/arbitrage-window",
        "--latency-trace",
    ])
    .expect("a config");
    assert_eq!(plain.max_blocks, switched.max_blocks);
    assert_eq!(plain.duration, switched.duration);
    assert_eq!(plain.queues, switched.queues);
    assert_eq!(plain.canonical_source, switched.canonical_source);
    assert!(switched.execution.is_none());
}

/// M12-B §3's freshness pair, plus §16 D3's interval defaults, both read off the
/// flags rather than off a struct literal.
#[test]
fn a_run_judges_head_freshness_only_when_the_operator_supplies_both_halves() {
    use evm_chain::HeadFreshnessPolicy;

    // The default is the absence, not a number this file invented: §3 forbids a
    // hardcoded tolerance with no basis, and a single node cannot report its own lag.
    let plain = live(&["--rpc-url", "http://node:8545"]).expect("a config");
    assert_eq!(plain.readiness, HeadFreshnessPolicy::NotJudged);

    let both = live(&[
        "--rpc-url",
        "http://node:8545",
        "--require-head-reference",
        "37224031",
        "--allow-head-lag",
        "5",
    ])
    .expect("a config");
    assert_eq!(
        both.readiness,
        HeadFreshnessPolicy::AgainstReference {
            reference_head: 37_224_031,
            tolerance_blocks: 5,
        }
    );

    // Each half alone is refused. The tempting completion — a lone tolerance read as
    // "no tolerance", a lone reference read as "any lag" — would each be a rule nobody
    // typed, and a run's record would then claim a strictness or a slack that no flag
    // asked for.
    let no_tolerance = live(&[
        "--rpc-url",
        "http://node:8545",
        "--require-head-reference",
        "37224031",
    ])
    .unwrap_err();
    assert!(no_tolerance.contains("--allow-head-lag"), "{no_tolerance}");
    let no_reference =
        live(&["--rpc-url", "http://node:8545", "--allow-head-lag", "5"]).unwrap_err();
    assert!(
        no_reference.contains("--require-head-reference"),
        "{no_reference}"
    );

    // A reference of zero is a check that can never hold anything: every head is at or
    // behind it, so it would report "judged against the network head" while asking the
    // node for nothing.
    let vacuous = live(&[
        "--rpc-url",
        "http://node:8545",
        "--require-head-reference",
        "0",
        "--allow-head-lag",
        "5",
    ])
    .unwrap_err();
    assert!(vacuous.contains("asks nothing of the node"), "{vacuous}");

    // And a recording has no node to judge, so the pair is refused rather than
    // silently dropped — dropping it would let the session record a readiness policy
    // that was never applied to anything.
    let replay = live(&[
        "--replay-dir",
        "fixtures/live-m5/arbitrage-window",
        "--require-head-reference",
        "37224031",
        "--allow-head-lag",
        "5",
    ])
    .unwrap_err();
    assert!(replay.contains("replays a recording"), "{replay}");
}

/// M12-B §4: an endpoint's purpose is something the operator says, and the flags are
/// the only place this repository learns it.
#[test]
fn an_endpoint_purpose_is_declared_or_it_is_recorded_as_unknown() {
    use evm_chain::EndpointPurpose;

    // Absent is `unknown`, and it stays `unknown` for an address that every
    // localhost-sniffing implementation would have called local. That is §4.1's rule
    // measured at the boundary: a URL of `127.0.0.1:8545` is the exact shape a guesser
    // would turn into `local_canonical_rpc`, so if the label ever came out anything but
    // `unknown` here, an inference would have happened somewhere in between.
    let undeclared = live(&["--rpc-url", "http://127.0.0.1:8545"]).expect("a config");
    assert_eq!(undeclared.canonical_purpose, EndpointPurpose::Unknown);
    assert_eq!(undeclared.flashblocks_purpose, EndpointPurpose::Unknown);

    let declared = live(&[
        "--rpc-url",
        "http://127.0.0.1:8545",
        "--rpc-endpoint-purpose",
        "local_canonical_rpc",
    ])
    .expect("a config");
    assert_eq!(
        declared.canonical_purpose,
        EndpointPurpose::LocalCanonicalRpc,
        "a declaration the operator typed has to reach the config it describes"
    );
    // …and says nothing about the other endpoint, which stays undeclared (§4: the two
    // roles are declared separately because they can genuinely be different providers).
    assert_eq!(declared.flashblocks_purpose, EndpointPurpose::Unknown);

    let public_candidate = live(&[
        "--rpc-url",
        "https://sepolia-rpc.example",
        "--flashblocks-url",
        "wss://flashblocks.example",
        "--flashblocks-endpoint-purpose",
        "public_flashblocks_rpc",
    ])
    .expect("a config");
    assert_eq!(
        public_candidate.flashblocks_purpose,
        EndpointPurpose::PublicFlashblocksRpc
    );
    assert_eq!(public_candidate.canonical_purpose, EndpointPurpose::Unknown);

    // `unknown` is also a thing an operator can say out loud, which is how §4.6's
    // "legal state, never a silent default to local" gets a spelling in a config file.
    let said_so = live(&[
        "--rpc-url",
        "https://sepolia-rpc.example",
        "--rpc-endpoint-purpose",
        "unknown",
    ])
    .expect("a config");
    assert_eq!(said_so.canonical_purpose, EndpointPurpose::Unknown);

    // Three refusals, each a way a label could end up meaning something nobody said.
    // A typo must not fall through to `unknown`: the operator would believe the session
    // record says "local" or "public" about a run that recorded an absence.
    let typo = live(&[
        "--rpc-url",
        "http://node:8545",
        "--rpc-endpoint-purpose",
        "local_canonical",
    ])
    .unwrap_err();
    assert!(typo.contains("is not an endpoint purpose"), "{typo}");
    // A label about the other role, refused rather than re-pointed.
    let wrong_role = live(&[
        "--rpc-url",
        "http://node:8545",
        "--rpc-endpoint-purpose",
        "local_flashblocks_rpc",
    ])
    .unwrap_err();
    assert!(
        wrong_role.contains("not what this flag describes") && wrong_role.contains("canonical"),
        "{wrong_role}"
    );
    // A purpose for an endpoint the run does not have.
    let no_endpoint = live(&[
        "--rpc-url",
        "http://node:8545",
        "--flashblocks-endpoint-purpose",
        "local_flashblocks_rpc",
    ])
    .unwrap_err();
    assert!(
        no_endpoint.contains("an endpoint this run does not have"),
        "{no_endpoint}"
    );
    // And the public/c_local split is never spelled by a hostname: the only accepted
    // spellings are the five labels, so `localhost` as a value is a refusal.
    let host_as_purpose = live(&[
        "--rpc-url",
        "http://localhost:8545",
        "--rpc-endpoint-purpose",
        "localhost",
    ])
    .unwrap_err();
    assert!(
        host_as_purpose.contains("is not an endpoint purpose"),
        "{host_as_purpose}"
    );
}

/// §16 D3: the interval a run uses comes from one place, and this test is what keeps
/// it that place. Reading the number off `Default` rather than typing it again is the
/// point — a test that repeated the literal would pass happily while the fallback and
/// the struct disagreed.
#[test]
fn the_interval_fallback_reads_the_same_default_the_run_uses() {
    use evm_live::{FlashblockConfig, SourceConfig};

    let defaults = live(&["--rpc-url", "http://node:8545"]).expect("a config");
    assert_eq!(
        defaults.source.poll_interval_ms,
        SourceConfig::default().poll_interval_ms,
        "no flag means the source's own default, not a second copy of the number"
    );
    assert_eq!(
        defaults.flashblocks.poll_interval_ms,
        FlashblockConfig::default().poll_interval_ms
    );

    let explicit = live(&[
        "--rpc-url",
        "http://node:8545",
        "--poll-interval-ms",
        "37",
        "--flashblock-poll-interval-ms",
        "41",
    ])
    .expect("a config");
    assert_eq!(explicit.source.poll_interval_ms, 37);
    assert_eq!(explicit.flashblocks.poll_interval_ms, 41);
    // Everything else in the two structs is still whatever the default says, so an
    // interval flag cannot quietly retune an unrelated knob beside it.
    assert_eq!(
        explicit.source,
        SourceConfig {
            poll_interval_ms: 37,
            ..SourceConfig::default()
        }
    );

    let non_numeric = parse_live(&["evm-mev-bot", "live", "--poll-interval-ms", "soon"]);
    assert!(
        non_numeric.is_err(),
        "an interval that is not a number is refused by the parser, not defaulted"
    );
}

/// The built binary, with none of the endpoint variables inherited — the tests
/// that care about §44 have to know what the environment was.
///
/// Both purpose variables are scrubbed as well as the three URLs: §4's refusals read
/// a declaration against the endpoints a run has, so a purpose left over in the shell
/// would change which refusal this file sees without changing a single flag it passes.
fn binary(args: &[&str]) -> Output {
    command(args, &[]).output().expect("the binary runs")
}

/// The same scrubbed run, with the listed variables added back.
fn binary_with_env(added: &[(&str, &str)], args: &[&str]) -> Output {
    command(args, added).output().expect("the binary runs")
}

fn command(args: &[&str], env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(
        std::env::var("CARGO_BIN_EXE_evm-mev-bot")
            .expect("cargo gives an integration test its package's binary"),
    );
    command
        .args(args)
        .env_remove("GIWA_RPC_URL")
        .env_remove("GIWA_WS_URL")
        .env_remove("GIWA_FLASHBLOCKS_URL")
        .env_remove("GIWA_RPC_ENDPOINT_PURPOSE")
        .env_remove("GIWA_FLASHBLOCKS_ENDPOINT_PURPOSE");
    for (key, value) in env {
        command.env(key, value);
    }
    command
}

/// The flags §7's probe run under: a replay asks for no canonical endpoint, so the
/// only thing that can stop it is the candidate declaration — and the directory is
/// one the run cannot open, which is where a run that got past its flags stops.
const PROBE_ARGS: &[&str] = &[
    "live",
    "--replay-dir",
    "m12b-absent-replay-dir",
    "--flashblocks-endpoint-purpose",
    "local_flashblocks_rpc",
    "--evidence-dir",
    "target/m12b-cli-probe",
];

/// §7's normalized name, read by a real process rather than by `try_parse_from`.
///
/// The label is the probe, and the probe has both answers in it. A run that gives the
/// purpose without giving the endpoint is refused by §4's third rule, so:
/// * with `GIWA_FLASHBLOCKS_URL` in the environment and no matching flag, a run that
///   reached the replay directory proves the environment name was read into the same
///   field the flag writes;
/// * the same flags with the variable absent must stop at the label, which is what
///   keeps the first half from passing because the bot never reads any variable and
///   never refuses anything.
///
/// This is also the check that the rename did not turn the candidate endpoint into a
/// default: the run still needs someone to name it, and with nothing named it says so.
#[test]
fn the_candidate_endpoint_comes_from_one_environment_variable() {
    let honored = binary_with_env(
        &[("GIWA_FLASHBLOCKS_URL", "http://127.0.0.1:1")],
        PROBE_ARGS,
    );
    let honored = String::from_utf8_lossy(&honored.stderr);
    assert!(
        !honored.contains("no flashblocks URL"),
        "GIWA_FLASHBLOCKS_URL is the name the flag documents, so a run started with it \
         in the environment cannot be told it has no candidate endpoint: {honored}"
    );
    assert!(
        honored.contains("m12b-absent-replay-dir"),
        "past its flags, the run stops at the directory it was pointed at: {honored}"
    );

    let refused = binary(PROBE_ARGS);
    let refused = String::from_utf8_lossy(&refused.stderr);
    assert!(
        refused.contains("no flashblocks URL"),
        "with the variable scrubbed the same flags have nothing to declare, which is the \
         half that makes the first half a measurement rather than a tautology: {refused}"
    );
}

/// §7's acceptance criterion, as M12-A stated it: after the rename, grepping the
/// workspace's own code finds one spelling.
///
/// The scan counts both spellings and asserts on each. A guard that only counted
/// hits on the old name would go green on a workspace where the new name appears
/// nowhere either, so the canonical spelling has to be found where it is declared —
/// `crates/cli/src/lib.rs` is where the flag's `env` is written.
///
/// Reverting any one file to the pre-rename spelling puts a number into the first
/// assertion, which is the planted defect this test is for. The exemption below is
/// exactly one file — this one, which has to spell both names in order to look for
/// them — and it costs little, because the name production actually reads is pinned
/// apart from the grep by [`the_candidate_endpoint_comes_from_one_environment_variable`],
/// which asks a real process for the value under the canonical spelling.
///
/// The scan covers `crates/` and nothing else, on purpose: the old spelling still
/// appears in `docs/v0.1/M12-A Repo Audit.md`, which is the record of the defect, and
/// in `data/evidence/m7/probe-sequencer-direct.json`, which is a committed artifact of
/// a run that happened. Rewriting either to satisfy a grep would make the evidence
/// describe a configuration that was never used.
#[test]
fn one_spelling_of_the_candidate_endpoint_variable_names_the_code() {
    let old = "GIWA_FLASHBLOCKS_RPC_URL";
    let canonical = "GIWA_FLASHBLOCKS_URL";
    let this_file =
        std::fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/live_args.rs"))
            .expect("this test file is where it says it is");
    let mut stale: Vec<String> = Vec::new();
    let mut canonical_files: Vec<String> = Vec::new();
    for file in rust_files(&workspace_root().join("crates")) {
        let text = std::fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
        let named = std::fs::canonicalize(&file)
            .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
        if named == this_file {
            continue;
        }
        if text.contains(old) {
            stale.push(file.display().to_string());
        }
        if text.contains(canonical) {
            canonical_files.push(file.display().to_string());
        }
    }
    assert!(
        stale.is_empty(),
        "{old} is the retired spelling (§7); these files still say it: {stale:?}"
    );
    assert!(
        canonical_files
            .iter()
            .any(|path| path.ends_with("crates/cli/src/lib.rs")),
        "the scan found {canonical} nowhere near the flag that documents it, so it would \
         also have found nothing had the rename been dropped: {canonical_files:?}"
    );

    // And "near the flag" means the flag itself. The name also appears in prose —
    // module documentation, this file — so a scan that stopped at "some file says the
    // word" would stay green while the `#[arg(env = …)]` spelled it differently, which
    // is the one line the unification is actually about.
    let cli = std::fs::read_to_string(workspace_root().join("crates/cli/src/lib.rs"))
        .expect("the CLI source is readable");
    let declaration = format!("env = \"{canonical}\"");
    assert!(
        cli.contains(&declaration),
        "{canonical} is not declared as an environment source in the CLI: no \
         `#[arg(env = …)]` line carries it"
    );
    assert!(
        !cli.contains(&format!("env = \"{old}\"")),
        "the CLI still reads {old}, which is the split §7 closed"
    );
}

/// §7's other half: one name does not make the two endpoints the same thing.
///
/// The candidate endpoint is spelled one way now, and a run that has only it still
/// cannot say where its canonical blocks come from. If the unification had quietly
/// merged the two fields — or made the candidate URL serve as a fallback — this run
/// would start instead of naming the variable the canonical role needs.
#[test]
fn the_candidate_endpoint_never_stands_in_for_the_canonical_one() {
    let output = binary_with_env(
        &[("GIWA_FLASHBLOCKS_URL", "http://127.0.0.1:1")],
        &["live", "--source", "http-poll"],
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "a run with no canonical endpoint is a flag error, whatever else is set: stderr {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("GIWA_RPC_URL"),
        "and it asks for the canonical variable by name, not for either of them at once: \
         {stderr}"
    );
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every `.rs` file below a directory, sorted so a failure names them in one order.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_rust_files(dir, &mut files);
    files.sort();
    files
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
    {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            // A `cargo` run inside a crate leaves a build directory there; the scan
            // is about what the workspace's own code says, not what a dependency
            // was copied into.
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            collect_rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}
