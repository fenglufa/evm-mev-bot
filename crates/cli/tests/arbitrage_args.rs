//! §57's command line, tested as the contract it is.
//!
//! `arbitrage` is the only way to reach M7's route runner, and the rules that make M7 a
//! milestone rather than a demo land here, in flag form: one candidate named by the caller
//! because §27 forbids a gate that re-searches; `--market` and `--market-evidence` required
//! because §51's REAL_MARKET/CONTROLLED_FIXTURE split is worth nothing if a run can be
//! unlabelled; `--fee-evidence` required because §5 makes *proving the fee* part of what an
//! opportunity is; and `--execution-mode` required because §32's default is the rung that
//! sends nothing.
//!
//! What there is no test for is a flag that would set a reserve, a balance, an allowance, a
//! transfer tax, a gas figure or an L1 fee — the two tests that read `--help` and reject an
//! unknown argument are the ones that keep those absent. The mapping tests call
//! [`evm_cli::parse_arbitrage`] with the argv a person would type, so they hold for the flags
//! as spelled; the tests that need a real process spawn the built binary.
//!
//! None of them touches a node, prices a pool, or signs anything: every path here ends at
//! [`evm_cli::ArbitrageArgs::to_config`], which refuses on the command line and costs nothing.

use std::path::PathBuf;
use std::process::Output;

use alloy_primitives::Address;
use evm_cli::{
    default_registry_dirs, parse_arbitrage, parse_live, parse_validate, ArbitrageArgs,
    DEFAULT_DIAGNOSIS_DIR, DEFAULT_LATENCY_DIR,
};
use evm_execution::{ExecutionMode, MarketKind};
use evm_pipeline::ArbitrageConfig;

/// The candidate M7 measured, spelled as it would be typed: WETH in, one mid token, the two
/// venues that disagree about its price, at 0.0001 ETH — §48's smallest size.
const WETH: &str = "0x4200000000000000000000000000000000000006";
const MID: &str = "0x07D4af6E2bc8DD82beb06b4FD279DF4c9028F26f";
const VENUE_A: &str = "0x2a3ceafbA30f6626170CBB0CD67392eFb94BD9A4";
const VENUE_B: &str = "0x5b3C1E3Fb6A97c0130aE015ff10f53A1A30C353e";
const SENDER: &str = "0xd450630c1c55b1c7df1ebf7eeaee1fffb45e520c";
const FEE_EVIDENCE: &str = "data/evidence/m7/candidate-fee-measurement.json";
const MARKET_EVIDENCE: &str = "reserves read at the live head by this run; the registry \
                               attests both pools";

/// A complete, cheap command line: `build-only`, so even a mistaken run forms an intent and
/// stops there.
fn complete() -> Vec<&'static str> {
    vec![
        "--rpc-url",
        "http://127.0.0.1:1",
        "--execution-mode",
        "build-only",
        "--sender",
        SENDER,
        "--input-token",
        WETH,
        "--candidate-mid",
        MID,
        "--candidate-pool",
        VENUE_A,
        "--candidate-pool",
        VENUE_B,
        "--input-wei",
        "100000000000000",
        "--fee-num",
        "997",
        "--fee-den",
        "1000",
        "--fee-evidence",
        FEE_EVIDENCE,
        "--market",
        "real-market",
        "--market-evidence",
        MARKET_EVIDENCE,
    ]
}

/// The config one flag list describes, or the reason it describes none.
fn build(flags: &[&'static str]) -> Result<ArbitrageConfig, String> {
    let argv = argv(flags);
    let args: ArbitrageArgs =
        parse_arbitrage(&argv).map_err(|error| format!("the flags were refused: {error}"))?;
    args.to_config()
}

/// Every occurrence of one flag and its value, removed. `--candidate-pool` is repeatable, so
/// this takes all of them: a test that means to change a venue list rebuilds it rather than
/// assuming one of the two survived.
fn without(flags: &[&'static str], name: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::with_capacity(flags.len());
    let mut skip_next = false;
    for flag in flags {
        if skip_next {
            skip_next = false;
            continue;
        }
        if *flag == name {
            skip_next = true;
            continue;
        }
        out.push(flag);
    }
    out
}

/// The line with one option's value replaced — which has to mean removed and re-added, since
/// clap refuses a repeated single-value option outright, and a test that changed two things
/// by accident would be asserting about neither.
fn replaced(flags: &[&'static str], name: &'static str, value: &'static str) -> Vec<&'static str> {
    let mut out = without(flags, name);
    out.extend([name, value]);
    out
}

/// Flags with extra ones appended, used only for `--candidate-pool`, the one repeatable
/// option on this command line.
fn with_extra(flags: &[&'static str], extra: &[&'static str]) -> Vec<&'static str> {
    let mut out = flags.to_vec();
    out.extend_from_slice(extra);
    out
}

/// The whole argv, program name and subcommand included.
fn argv(flags: &[&'static str]) -> Vec<&'static str> {
    let mut argv = vec!["evm-mev-bot", "arbitrage"];
    argv.extend_from_slice(flags);
    argv
}

#[test]
fn a_route_run_has_to_say_how_far_it_may_go_and_which_kind_of_market_it_is() {
    // §32 and §51 in flag form. Each of these four is required with no default, and each
    // refusal names the flag, because the alternative — a silent default — is how a run ends
    // up labelled REAL_MARKET or rung `submit` without anyone typing it.
    for flag in [
        "--execution-mode",
        "--market",
        "--market-evidence",
        "--fee-evidence",
    ] {
        let refused = build(&without(&complete(), flag)).unwrap_err();
        assert!(
            refused.contains(flag),
            "removing {flag} must produce a refusal that names it, got: {refused}"
        );
    }

    // §44's rule, unchanged from M5 and M6: the endpoint is a flag or an environment
    // variable, never a URL sitting in the code.
    let stranded = build(&without(&complete(), "--rpc-url")).unwrap_err();
    assert!(
        stranded.contains("--rpc-url") && stranded.contains("GIWA_RPC_URL"),
        "{stranded}"
    );

    // And §51's two spellings and §20's three rungs are checked, not forgiven: a third
    // category that sounds plausible is refused rather than rounded up to the expensive one.
    let refused = build(&replaced(&complete(), "--market", "real-market-ish")).unwrap_err();
    assert!(
        refused.contains("real-market-ish"),
        "a made-up §51 category names what it rejected: {refused}"
    );
    let refused = build(&replaced(&complete(), "--execution-mode", "submmit")).unwrap_err();
    assert!(
        refused.contains("submmit"),
        "a misspelled rung is a refusal, not a default: {refused}"
    );
}

#[test]
fn evidence_that_names_nothing_is_no_evidence() {
    // The requirement is not that *a* string was typed. A blank `--market-evidence` is §1's
    // fabrication with the label still on it, so it is refused at the same place that its
    // absence is.
    assert!(
        build(&replaced(&complete(), "--market-evidence", "   ")).is_err(),
        "a blank §51 evidence string describes no evidence"
    );
    assert!(
        build(&replaced(&complete(), "--fee-evidence", "")).is_err(),
        "a blank fee citation is the unproved fee §5 refuses"
    );
}

#[test]
fn a_candidate_is_exactly_two_venues_over_one_pair() {
    // §5's definition of an opportunity, in the shape of a flag count. One venue is a price
    // with nothing to disagree with; three is a route this runner does not have; two
    // spellings of the same pool is a loop, not a pair.
    let one = {
        let mut flags = without(&complete(), "--candidate-pool");
        flags.extend(["--candidate-pool", VENUE_A]);
        flags
    };
    let refused = build(&one).unwrap_err();
    assert!(
        refused.contains("twice") && refused.contains("1 times"),
        "one venue has to be told the count it should have been: {refused}"
    );

    let three = with_extra(&complete(), &["--candidate-pool", VENUE_A]);
    let refused = build(&three).unwrap_err();
    assert!(
        refused.contains("3 times"),
        "three venues is not §5's pair either: {refused}"
    );

    let same_twice = {
        let mut flags = without(&complete(), "--candidate-pool");
        flags.extend(["--candidate-pool", VENUE_A, "--candidate-pool", VENUE_A]);
        flags
    };
    let refused = build(&same_twice).unwrap_err();
    assert!(
        refused.contains("same pool"),
        "two venues that are one pool are not two prices: {refused}"
    );

    // §16's shape: the mid token is what the venues disagree about, so it cannot be the
    // asset the route starts and ends in.
    let refused = build(&replaced(&complete(), "--candidate-mid", WETH)).unwrap_err();
    assert!(refused.contains("--candidate-mid"), "{refused}");

    // An address-shaped string is required, not requested: a typo in a venue address would
    // otherwise read some other account's `getReserves()` and call the answer a market.
    let mistyped = {
        let mut flags = without(&complete(), "--candidate-pool");
        flags.extend([
            "--candidate-pool",
            VENUE_A,
            "--candidate-pool",
            "0xnot-an-address",
        ]);
        flags
    };
    let refused = build(&mistyped).unwrap_err();
    assert!(
        refused.contains("not-an-address"),
        "the refusal repeats what it could not parse: {refused}"
    );
}

#[test]
fn a_fee_is_a_measured_ratio_not_a_free_parameter() {
    // §5's "fee proven" has a numeric side: a ratio with no denominator is not a
    // measurement, and a fee that takes more than the whole input is not a fee at all. Both
    // are refused before a block is read, which is the only order that costs nothing.
    let no_den = build(&without(&complete(), "--fee-den")).unwrap_err();
    assert!(no_den.contains("--fee-den"), "{no_den}");
    let no_num = build(&without(&complete(), "--fee-num")).unwrap_err();
    assert!(no_num.contains("--fee-num"), "{no_num}");
    assert!(
        build(&replaced(&complete(), "--fee-den", "0")).is_err(),
        "a zero denominator is refused, not read as an infinite fee"
    );

    let more_than_the_trade = replaced(&replaced(&complete(), "--fee-num", "2"), "--fee-den", "1");
    let refused = build(&more_than_the_trade).unwrap_err();
    assert!(
        refused.contains("more than the whole input"),
        "a fee above 100% is a typo or a fabrication: {refused}"
    );
}

#[test]
fn an_input_that_trades_nothing_describes_no_route() {
    let absent = build(&without(&complete(), "--input-wei")).unwrap_err();
    assert!(absent.contains("--input-wei"), "{absent}");
    assert!(
        build(&replaced(&complete(), "--input-wei", "0")).is_err(),
        "0 wei must not become a free simulation whose profit is 0"
    );
}

#[test]
fn a_complete_command_line_becomes_a_config_and_nothing_else() {
    // The mapping test: what the flags say is what the runner is handed. The size is asserted
    // in wei because §11's single-denomination rule starts at the input, and a silently
    // converted 0.0001 ETH is not the number the profit line will be divided by.
    let config = build(&complete()).expect("the flags describe a run");
    assert_eq!(config.rpc_url, "http://127.0.0.1:1");
    assert_eq!(config.setup.mode, ExecutionMode::BuildOnly);
    assert_eq!(config.sender.to_string().to_lowercase(), SENDER);
    assert_eq!(config.candidate.input_amount.to_string(), "100000000000000");
    assert_eq!(
        config.candidate.venues,
        [
            VENUE_A.parse::<Address>().unwrap(),
            VENUE_B.parse::<Address>().unwrap()
        ]
    );
    assert_eq!(config.candidate.fee.numerator, 997);
    assert_eq!(config.candidate.fee.denominator, 1000);
    assert_eq!(config.candidate.fee_evidence, FEE_EVIDENCE);
    assert_eq!(
        config.candidate.chain_id, 0,
        "0 defers to the endpoint (§45), which is the default rather than a guess"
    );
    assert_eq!(
        config.market,
        MarketKind::RealMarket {
            attested_by: MARKET_EVIDENCE.to_string()
        },
    );
    assert!(config.market.counts_as_real_arbitrage());
    assert_eq!(config.tolerance.numerator, 1);
    assert_eq!(config.tolerance.denominator, 100);
    assert_eq!(config.risk.minimum_net_profit_wei, 0);
    assert_eq!(config.risk.maximum_gas, None);
    assert_eq!(config.registry_dirs, default_registry_dirs());
    assert_eq!(
        config.evidence_dir,
        PathBuf::from("data/evidence/m7/route"),
        "the default is this repository's own evidence path, not an endpoint"
    );
}

#[test]
fn a_fixture_is_labelled_as_a_fixture_and_still_refuses_to_be_a_real_route() {
    // §51's whole purpose is one call: `counts_as_real_arbitrage`. A run that says
    // CONTROLLED_FIXTURE gets the label, the evidence string it typed, and a verdict that
    // cannot be counted — and it still has to *say* which kind it is, and name its proof.
    let flags = replaced(
        &replaced(&complete(), "--market", "controlled-fixture"),
        "--market-evidence",
        "execution system works",
    );
    let config = build(&flags).expect("a fixture is runnable, just not countable");
    assert_eq!(config.market.name(), "CONTROLLED_FIXTURE");
    assert!(!config.market.counts_as_real_arbitrage());
    assert!(config.market.describe().contains("execution system works"));

    let unproven = without(
        &replaced(&complete(), "--market", "controlled-fixture"),
        "--market-evidence",
    );
    assert!(
        build(&unproven).is_err(),
        "CONTROLLED_FIXTURE still has to name what it proves"
    );
}

#[test]
fn the_three_paths_stay_three_paths() {
    // §26's separation, checked at the parser: a `live` session cannot acquire a route and a
    // `validate` run cannot name a pool. The only way to be sure a market session will not
    // send is that its flags cannot describe a candidate at all.
    let mut line = vec!["evm-mev-bot", "live"];
    line.extend(complete());
    assert!(
        parse_live(&line).is_err(),
        "a route belongs to `arbitrage`, not to a session"
    );

    let mut line = vec!["evm-mev-bot", "validate"];
    line.extend(complete());
    assert!(
        parse_validate(&line).is_err(),
        "a validation transaction belongs to `validate`, not to a route"
    );

    let args = parse_arbitrage(&argv(&complete())).expect("`arbitrage` takes the route flags");
    assert_eq!(
        args.to_config().map(|config| config.rpc_url),
        Ok("http://127.0.0.1:1".to_string())
    );
}

#[test]
fn no_flag_carries_key_material_and_an_unknown_flag_is_not_ignored() {
    // §17/§19's rule at the CLI boundary: the key is read inside the signer, from the
    // environment variable named there, and no flag here takes it — not in the help text, and
    // not accepted by the parser either.
    let mut flags: Vec<&str> = complete().into_iter().collect();
    flags.extend(["--sender-private-key", "0xaaaa"]);
    let refused = binary(&flags);
    assert_eq!(
        refused.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("unexpected argument"), "{stderr}");

    let help_run = binary(&["arbitrage", "--help"]);
    let help = String::from_utf8_lossy(&help_run.stdout);
    for spelled in [
        "--candidate-pool",
        "--candidate-mid",
        "--fee-evidence",
        "--market",
        "--execution-mode",
        "--input-wei",
    ] {
        assert!(help.contains(spelled), "help must list {spelled}: {help}");
    }
    assert!(
        !help.contains("--private-key") && !help.contains("--secret") && !help.contains("--key "),
        "help is where a reader looks for how to pass a secret; there is no such line: {help}"
    );
    assert!(
        !help.contains("sendRawTransaction"),
        "the submission method belongs to the execution crate's words, not to a flag: {help}"
    );
}

#[test]
fn the_binary_refuses_a_route_it_cannot_describe_with_code_two() {
    // `2` is "the flags were wrong" and `1` is "the chain would not do it", so a script can
    // tell a mistyped run from a refused route without reading stderr. This line is missing
    // everything, which is a flag problem.
    let output = binary(&["arbitrage"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--rpc-url") || stderr.contains("--execution-mode"),
        "the run that never started still said what is missing: {stderr}"
    );
    assert!(
        !stderr.contains("GIWA_EXECUTION_PRIVATE_KEY"),
        "a refusal about flags must not point a reader at the key variable: {stderr}"
    );
}

/// M8.1 §40 on the route path: the switch changes one field and nothing else. This is the
/// command-line half of "the M7 evidence files are the proof" — a run asked for a trace must
/// still be the same run, so the comparison here is between two configs the same flags
/// describe, one of them traced.
#[test]
fn the_latency_switch_adds_a_directory_and_moves_no_decision() {
    let plain = build(&complete()).expect("M7's cheap route");
    assert_eq!(
        plain.latency_dir, None,
        "§40: without the flag this is the command line that produced M7's evidence"
    );

    let mut flags = complete();
    flags.push("--latency-trace");
    let traced = build(&flags).expect("the same route, traced");
    assert_eq!(
        traced.latency_dir,
        Some(PathBuf::from(DEFAULT_LATENCY_DIR)),
        "the traces go beside the run's evidence, not inside it"
    );
    assert_ne!(
        traced.evidence_dir,
        traced.latency_dir.expect("a directory"),
        "§44: a latency file never shares a path with an evidence file"
    );

    // Everything the run decides is unchanged.
    assert_eq!(traced.rpc_url, plain.rpc_url);
    assert_eq!(traced.sender, plain.sender);
    assert_eq!(traced.market, plain.market);
    assert_eq!(traced.setup.mode, plain.setup.mode);
    assert_eq!(traced.evidence_dir, plain.evidence_dir);
    assert_eq!(
        traced.risk.minimum_net_profit_wei,
        plain.risk.minimum_net_profit_wei
    );
    assert_eq!(traced.candidate.input_amount, plain.candidate.input_amount);
    assert_eq!(traced.candidate.venues.len(), plain.candidate.venues.len());
    assert_eq!(
        traced.candidate.fee_evidence, plain.candidate.fee_evidence,
        "the fee the run was told to honour is the same text"
    );

    // And naming the directory without the switch is still a traced run (§41).
    let mut flags = complete();
    flags.extend(["--latency-output", "target/latency-route"]);
    let named = build(&flags).expect("the same route, traced elsewhere");
    assert_eq!(
        named.latency_dir,
        Some(PathBuf::from("target/latency-route")),
        "a path typed by a caller is never silently ignored"
    );
}

/// M8.2's RPC switch, on the same route and under the same rule as M8.1's §40: it names a
/// place for traces and changes nothing the run decides. The last assertion is the one §21
/// cares about — a caller who asks to see the calls must not thereby get a run that signs or
/// submits, so the mode is compared against the untraced command line rather than trusted.
#[test]
fn the_rpc_trace_switch_adds_a_diagnosis_directory_and_moves_no_decision() {
    let plain = build(&complete()).expect("M7's cheap route");
    assert_eq!(
        plain.diagnosis_dir, None,
        "without the flag this is the command line that produced M7's and M8.1's evidence"
    );

    let mut flags = complete();
    flags.push("--rpc-trace");
    let traced = build(&flags).expect("the same route, its calls recorded");
    assert_eq!(
        traced.diagnosis_dir,
        Some(PathBuf::from(DEFAULT_DIAGNOSIS_DIR)),
        "§22: the diagnosis files get a home of their own under M8"
    );
    assert_ne!(
        traced.evidence_dir,
        traced.diagnosis_dir.expect("a directory"),
        "a diagnosis line never shares a path with an evidence file"
    );
    assert_ne!(
        DEFAULT_LATENCY_DIR, DEFAULT_DIAGNOSIS_DIR,
        "M8.1's stage timings and M8.2's call records are different populations (§44), so \
         they must not be written to the same directory"
    );

    let mut flags = complete();
    flags.extend(["--rpc-trace", "--rpc-output", "target/diagnosis-route"]);
    let named = build(&flags).expect("the same route, recorded elsewhere");
    assert_eq!(
        named.diagnosis_dir,
        Some(PathBuf::from("target/diagnosis-route")),
        "a path typed by a caller is never silently ignored"
    );

    // And the two switches are independent: the latency flag alone leaves no diagnosis
    // directory, and the RPC flag alone leaves no latency directory.
    let mut flags = complete();
    flags.push("--latency-trace");
    let stage_only = build(&flags).expect("a stage-timed run");
    assert_eq!(
        stage_only.latency_dir,
        Some(PathBuf::from(DEFAULT_LATENCY_DIR))
    );
    assert_eq!(
        stage_only.diagnosis_dir, None,
        "timing a lifecycle does not record its calls"
    );
    assert_eq!(
        traced.latency_dir, None,
        "recording calls does not time a lifecycle"
    );

    // Everything the run decides is unchanged, including the rung it stops at (§21).
    assert_eq!(traced.rpc_url, plain.rpc_url);
    assert_eq!(traced.sender, plain.sender);
    assert_eq!(traced.market, plain.market);
    assert_eq!(traced.setup.mode, plain.setup.mode);
    assert_eq!(traced.setup.mode, ExecutionMode::BuildOnly);
    assert_eq!(traced.evidence_dir, plain.evidence_dir);
    assert_eq!(
        traced.risk.minimum_net_profit_wei,
        plain.risk.minimum_net_profit_wei
    );
    assert_eq!(traced.candidate.input_amount, plain.candidate.input_amount);
    assert_eq!(
        traced.candidate.fee_evidence, plain.candidate.fee_evidence,
        "the fee the run was told to honour is the same text"
    );
}

/// The built binary, run with these arguments — which start at the subcommand, because argv's
/// first element is the program name the shell already gave it.
fn binary(args: &[&str]) -> Output {
    std::process::Command::new(
        std::env::var("CARGO_BIN_EXE_evm-mev-bot")
            .expect("cargo gives an integration test its package's binary"),
    )
    .args(args)
    .env_remove("GIWA_RPC_URL")
    .env_remove("GIWA_WS_URL")
    .env_remove("GIWA_FLASHBLOCKS_URL")
    .env_remove("GIWA_EXECUTION_PRIVATE_KEY")
    .output()
    .expect("the binary runs")
}
