//! Command-line entry point: `payments-engine <transactions.csv> > accounts.csv`.
//!
//! Exit codes: 0 on success (even if some rows were skipped), 1 if the input
//! can't be read or the output can't be written, 2 on wrong usage. On a
//! non-zero exit nothing has been written to stdout, except when writing
//! stdout itself failed part-way.

use std::env;
use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use env_logger::Env;
use log::{info, warn};
use payments_engine::csv_io::RunError;

const USAGE: &str = "usage: payments-engine <transactions.csv>";

/// Exit code for wrong command-line usage.
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let (Some(path), None) = (args.next(), args.next()) else {
        report(format_args!("{USAGE}"));
        return ExitCode::from(EXIT_USAGE);
    };
    let path = PathBuf::from(path);

    // Logs go to stderr and are quiet by default. `RUST_LOG=info` lists
    // skipped malformed rows; `RUST_LOG=debug` also lists refused transactions.
    env_logger::Builder::from_env(Env::default().default_filter_or("warn")).init();

    let input = match File::open(&path) {
        Ok(file) => file,
        Err(err) => {
            report(format_args!(
                "error: {}: cannot open input: {err}",
                path.display()
            ));
            return ExitCode::FAILURE;
        }
    };
    match payments_engine::run(input, io::stdout().lock()) {
        Ok(stats) => {
            info!(
                "{} applied, {} refused, {} malformed",
                stats.applied, stats.refused, stats.malformed
            );
            if stats.malformed > 0 {
                warn!(
                    "skipped {} malformed row(s); run with RUST_LOG=info for details",
                    stats.malformed
                );
            }
            ExitCode::SUCCESS
        }
        Err(err @ RunError::Write(_)) => {
            report(format_args!("error: {err}"));
            ExitCode::FAILURE
        }
        Err(err) => {
            report(format_args!("error: {}: {err}", path.display()));
            ExitCode::FAILURE
        }
    }
}

/// Prints a line on stderr. Unlike `eprintln!`, it doesn't panic when stderr is
/// gone (e.g. a closed pipe): there is then nowhere left to report to.
fn report(message: fmt::Arguments<'_>) {
    let _ = writeln!(io::stderr(), "{message}");
}
