//! The `evm-mev-bot` binary: parse, run, report. Everything that decides *what*
//! a run is lives in the library (`src/lib.rs`), where §54's tests can reach it.

use clap::Parser;

fn main() {
    std::process::exit(evm_cli::cli_main(evm_cli::Cli::parse()));
}
