//! §35's command line, tested as the contract it is.
//!
//! `validate` is the only way to reach the execution lane, and four of M6's rules land
//! here rather than in the stage: the transaction says what it is in §42's exact words
//! (§58), the mode has to be named because a run that could send was never supposed to be
//! the default (§19/§20), no flag may carry key material (§17), and the market path and
//! the sending path are two subcommands — a `live` run cannot acquire a transaction, and a
//! `validate` run cannot read a pool.
//!
//! The mapping tests call [`evm_cli::parse_validate`] with the argv a person would type,
//! so they hold for the flags as spelled, not for a struct literal clap never saw. The
//! tests that need a real process — the exit codes, the help text, a flag that must not
//! exist — spawn the built binary.
//!
//! None of them touches a node or signs anything: every path here ends at `to_plan()`,
//! which is the refusal stage and costs nothing.

use std::path::PathBuf;
use std::process::{Command, Output};

use evm_cli::{
    default_registry_dirs, parse_live, parse_validate, ValidationPlan, VALIDATION_LABEL,
};
use evm_execution::ExecutionMode;

/// One `validate` invocation, spelled the way it would be typed.
fn validate(extra: &[&str]) -> Result<ValidationPlan, String> {
    let mut argv = vec!["evm-mev-bot", "validate"];
    argv.extend_from_slice(extra);
    let args = parse_validate(&argv).map_err(|error| format!("the flags were refused: {error}"))?;
    args.to_plan()
}

#[test]
fn an_attempt_has_to_say_which_rung_it_may_reach() {
    // §19's rule in flag form: the safe mode is not a default here, because a
    // validation transaction exists to be *asked for* at a rung. Absent means the run
    // is refused, not that it ran at build-only and looked like a completion.
    let refused = validate(&["--rpc-url", "http://127.0.0.1:1"]).unwrap_err();
    assert!(refused.contains("--execution-mode"), "{refused}");
    assert!(
        refused.starts_with(VALIDATION_LABEL),
        "the refusal starts with the words §42 requires, so a log line about a refused \
         attempt still says what the attempt was: {refused}"
    );

    // §44's rule, borrowed from M5 and unchanged: the endpoint is a flag or an
    // environment variable, never a URL sitting in the code.
    let stranded = validate(&["--execution-mode", "sign-only"]).unwrap_err();
    assert!(stranded.contains("--rpc-url"), "{stranded}");
    assert!(stranded.contains("GIWA_RPC_URL"), "{stranded}");

    // And the spelling of the mode is checked, not forgiven.
    let typo = validate(&[
        "--rpc-url",
        "http://127.0.0.1:1",
        "--execution-mode",
        "submmit",
    ])
    .unwrap_err();
    assert!(typo.contains("submmit"), "{typo}");
}

#[test]
fn the_default_transaction_is_the_cheapest_one_the_chain_will_take() {
    // §35 asks for the least expensive thing that exercises build, sign, submit and
    // receipt: no calldata, no value, one account paying itself, 21 000 gas.
    let plan = validate(&[
        "--rpc-url",
        "http://127.0.0.1:1",
        "--execution-mode",
        "sign-only",
    ])
    .expect("an attempt");
    assert_eq!(plan.mode, ExecutionMode::SignOnly);
    assert_eq!(
        plan.value_wei, 0,
        "a validation transaction transfers nothing"
    );
    assert_eq!(
        plan.gas_limit, 21_000,
        "the gas it carries is the plain value transfer's own cost, not a number chosen to \
         look like a simulated route"
    );
    assert_eq!(
        plan.sender, None,
        "a signing mode derives the sender from the key it was given; a flag that could \
         overrule that would be a flag that lets a caller sign for an account the key does \
         not own"
    );
    assert_eq!(
        plan.to, None,
        "absent target means the sender, so nothing moves"
    );
    assert_eq!(
        plan.evidence_dir,
        PathBuf::from("data/evidence/m6/validation")
    );
    assert_eq!(
        plan.registry_dirs,
        default_registry_dirs(),
        "the run trades nothing and still has to be on a chain this repository attests (§46)"
    );
}

#[test]
fn build_only_has_to_be_told_which_account_it_is_describing() {
    // §19: a build-only run reads no key, so it cannot derive a sender from one. It is
    // still useful — it produces the envelope an evidence file needs — but only if a
    // person names the account rather than the program guessing at one.
    let refused = validate(&[
        "--rpc-url",
        "http://127.0.0.1:1",
        "--execution-mode",
        "build-only",
    ])
    .unwrap_err();
    assert!(refused.contains("--sender"), "{refused}");

    let named = validate(&[
        "--rpc-url",
        "http://127.0.0.1:1",
        "--execution-mode",
        "build-only",
        "--sender",
        "0xd450630c1c55b1c7df1ebf7eeaee1fffb45e520c",
    ])
    .expect("an envelope");
    let expected: alloy_primitives::Address = "0xd450630c1c55b1c7df1ebf7eeaee1fffb45e520c"
        .parse()
        .expect("an address literal");
    assert_eq!(
        named.sender,
        Some(expected),
        "the account the flag named is the account the plan carries — the checksummed \
         rendering is alloy's Display, not a different address"
    );
    assert_eq!(named.mode, ExecutionMode::BuildOnly);
}

#[test]
fn an_address_is_parsed_or_the_flag_that_carried_it_is_named() {
    let good = validate(&[
        "--rpc-url",
        "http://127.0.0.1:1",
        "--execution-mode",
        "sign-only",
        "--to",
        "0x4200000000000000000000000000000000000006",
        "--value-wei",
        "7",
        "--gas-limit",
        "53000",
        "--registry-dir",
        "one",
        "--registry-dir",
        "two",
        "--evidence-dir",
        "somewhere",
    ])
    .expect("a tuned attempt");
    assert_eq!(
        good.to.map(|a| a.to_string()),
        Some("0x4200000000000000000000000000000000000006".to_string())
    );
    assert_eq!(good.value_wei, 7);
    assert_eq!(good.gas_limit, 53_000);
    assert_eq!(good.evidence_dir, PathBuf::from("somewhere"));
    assert_eq!(
        good.registry_dirs,
        vec![PathBuf::from("one"), PathBuf::from("two")],
        "repeatable, in the order given"
    );

    let bad = validate(&[
        "--rpc-url",
        "http://127.0.0.1:1",
        "--execution-mode",
        "sign-only",
        "--sender",
        "0xnope",
    ])
    .unwrap_err();
    assert!(bad.contains("--sender") && bad.contains("0xnope"), "{bad}");

    let zero_gas = validate(&[
        "--rpc-url",
        "http://127.0.0.1:1",
        "--execution-mode",
        "sign-only",
        "--gas-limit",
        "0",
    ])
    .unwrap_err();
    assert!(
        zero_gas.contains("--gas-limit"),
        "a transaction the chain cannot execute is refused before it is built: {zero_gas}"
    );
}

#[test]
fn the_two_subcommands_cannot_be_confused_for_each_other() {
    // §26/§U's shape, in the parser: the market path and the sending path are two
    // commands, so a `live` command line cannot grow a transaction and a `validate`
    // command line cannot read a pool. A flag on the wrong subcommand is a clap error,
    // and a correct-looking line read by the wrong parser is refused here.
    let market = match parse_validate(&[
        "evm-mev-bot",
        "live",
        "--rpc-url",
        "http://127.0.0.1:1",
        "--execution-mode",
        "sign-only",
    ]) {
        Ok(_) => panic!("a `live` command line was read as a validation attempt"),
        Err(error) => error,
    };
    assert!(market.contains("`live`"), "{market}");

    let attempt = match parse_live(&[
        "evm-mev-bot",
        "validate",
        "--rpc-url",
        "http://127.0.0.1:1",
        "--execution-mode",
        "sign-only",
    ]) {
        Ok(_) => panic!("a `validate` command line was read as a market session"),
        Err(error) => error,
    };
    assert!(attempt.contains("`validate`"), "{attempt}");
}

#[test]
fn no_flag_takes_key_material() {
    // §17: the key comes from the environment and from nowhere a shell history can
    // remember. The synthetic value below is not a key anyone holds; it is what a
    // caller would type if the binary let them, and the binary has to refuse it as an
    // unknown flag rather than read it.
    let output = binary(&[
        "validate",
        "--rpc-url",
        "http://127.0.0.1:1",
        "--execution-mode",
        "sign-only",
        "--private-key",
        "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unexpected argument"),
        "the flag is unknown, not silently ignored: {stderr}"
    );
    assert!(
        !stderr.contains("0xaaaa"),
        "a refused command line must not echo back what it refused to read: {stderr}"
    );

    let help = binary(&["validate", "--help"]);
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(
        help.contains("--execution-mode") && help.contains("sign-only"),
        "{help}"
    );
    assert!(
        !help.contains("--private-key") && !help.contains("--secret") && !help.contains("--key "),
        "help is where a reader looks for how to pass a secret; there must be no such line: \
         {help}"
    );
    assert!(
        help.contains(VALIDATION_LABEL) || help.contains("validation"),
        "the subcommand's own help has to say what kind of transaction it is: {help}"
    );
}

#[test]
fn the_binary_refuses_an_attempt_it_cannot_describe_with_code_two() {
    // The exit code is part of §47's contract, unchanged from M5 and now holding for
    // this subcommand too: 2 is "the flags were wrong", which a script can tell from
    // 1's "the chain would not do it" without reading stderr.
    let output = binary(&["validate"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--execution-mode") || stderr.contains("--rpc-url"),
        "the run that did not start still said what is missing: {stderr}"
    );
}

/// The built binary, with none of the endpoint variables inherited — §44's
/// assertions have to know what the environment was, and a `--rpc-url` default
/// arriving from the shell would silently change which refusal this line earns.
fn binary(args: &[&str]) -> Output {
    Command::new(
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
