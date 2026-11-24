//! Command-line entry point: `payments-engine <transactions.csv> > accounts.csv`.

use std::env;
use std::process::ExitCode;

const USAGE: &str = "usage: payments-engine <transactions.csv>";

/// Exit code for wrong command-line usage.
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let (Some(_input), None) = (args.next(), args.next()) else {
        eprintln!("{USAGE}");
        return ExitCode::from(EXIT_USAGE);
    };

    ExitCode::SUCCESS
}
