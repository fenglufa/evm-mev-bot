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
//! clap never saw. The two that need a real process — the environment variable and
//! the help text — spawn the built binary, which is also the only way to read an
//! environment variable without racing every other test in this file for it.

use std::process::{Command, Output};

use evm_cli::{default_registry_dirs, parse_live};
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

/// The built binary, with none of the endpoint variables inherited — the tests
/// that care about §44 have to know what the environment was.
fn binary(args: &[&str]) -> Output {
    Command::new(
        std::env::var("CARGO_BIN_EXE_evm-mev-bot")
            .expect("cargo gives an integration test its package's binary"),
    )
    .args(args)
    .env_remove("GIWA_RPC_URL")
    .env_remove("GIWA_WS_URL")
    .env_remove("GIWA_FLASHBLOCKS_URL")
    .output()
    .expect("the binary runs")
}
