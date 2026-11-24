//! A streaming toy payments engine.
//!
//! Reads deposits, withdrawals, disputes, resolves and chargebacks from a CSV
//! file, applies them to client accounts, and writes the final account
//! balances as CSV.
//!
//! Reading order: [`run`] (the whole pipeline), [`csv_io`] (rows in, balances
//! out), [`transaction`] (what comes in), [`amount`] (how money is validated),
//! [`engine`] (how each transaction is applied), [`dispute`] (the dispute
//! lifecycle), [`account`] (the balance rules), [`error`].

pub mod account;
pub mod amount;
pub mod csv_io;
pub mod dispute;
pub mod engine;
pub mod error;
pub mod transaction;

use std::io::{Read, Write};

use log::{Level, info, log};

use crate::csv_io::{RunError, TransactionReader, write_accounts};
use crate::engine::Engine;

/// What happened to the input's data rows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Applied to an account.
    pub applied: u64,
    /// Well-formed but refused by the ledger rules (e.g. insufficient funds).
    pub refused: u64,
    /// Skipped because they could not be parsed.
    pub malformed: u64,
}

/// Applies every transaction in `input`, then writes the final balances to
/// `output`.
///
/// Balances are written only after the whole input was read, so on a fatal
/// read error `output` receives nothing.
pub fn run<R: Read, W: Write>(input: R, output: W) -> Result<Stats, RunError> {
    let mut engine = Engine::new();
    let stats = process(&mut engine, input)?;
    write_accounts(&engine, output)?;
    Ok(stats)
}

/// Applies every transaction in `input` to `engine`, one row at a time.
///
/// Malformed rows and refused transactions are logged and skipped: the spec
/// treats them as partner errors, not reasons to stop. Only input that can't
/// be read reliably is fatal (see [`RunError`]).
pub fn process<R: Read>(engine: &mut Engine, input: R) -> Result<Stats, RunError> {
    let mut reader = TransactionReader::new(input)?;
    let mut stats = Stats::default();
    while let Some(row) = reader.next_row()? {
        let transaction = match row.transaction {
            Ok(transaction) => transaction,
            Err(err) => {
                stats.malformed += 1;
                info!("data row {}: skipped as malformed: {err}", row.number);
                continue;
            }
        };
        match engine.apply(transaction) {
            Ok(()) => stats.applied += 1,
            Err(err) => {
                stats.refused += 1;
                let level = if err.indicates_bug() {
                    Level::Warn
                } else {
                    Level::Debug
                };
                log!(
                    level,
                    "data row {}: refused {transaction:?}: {err}",
                    row.number
                );
            }
        }
    }
    Ok(stats)
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]
mod tests {
    use std::io;

    use super::*;

    fn run_str(input: &str) -> (Result<Stats, RunError>, String) {
        let mut output = Vec::new();
        let result = run(input.as_bytes(), &mut output);
        (result, String::from_utf8(output).unwrap())
    }

    #[test]
    fn reads_transactions_and_writes_balances() {
        let input = "type, client, tx, amount\n\
                     deposit, 2, 1, 3.0\n\
                     deposit, 1, 2, 1.25\n\
                     withdrawal, 2, 3, 1\n\
                     dispute, 1, 2,\n";
        let (result, output) = run_str(input);
        assert_eq!(
            result.unwrap(),
            Stats {
                applied: 4,
                refused: 0,
                malformed: 0
            }
        );
        assert_eq!(
            output,
            "client,available,held,total,locked\n\
             1,0.0000,1.2500,1.2500,false\n\
             2,2.0000,0.0000,2.0000,false\n"
        );
    }

    #[test]
    fn counts_refused_and_malformed_rows_and_keeps_going() {
        let input = "type,client,tx,amount\n\
                     deposit,1,1,1\n\
                     withdrawal,1,2,5\n\
                     transfer,1,3,1\n\
                     dispute,1,99,\n\
                     deposit,1,4,2\n";
        let (result, output) = run_str(input);
        assert_eq!(
            result.unwrap(),
            Stats {
                applied: 2,
                refused: 2,
                malformed: 1
            }
        );
        assert_eq!(
            output,
            "client,available,held,total,locked\n1,3.0000,0.0000,3.0000,false\n"
        );
    }

    #[test]
    fn empty_input_writes_only_the_header() {
        let (result, output) = run_str("");
        assert_eq!(result.unwrap(), Stats::default());
        assert_eq!(output, "client,available,held,total,locked\n");
    }

    #[test]
    fn missing_column_is_fatal_and_writes_nothing() {
        let (result, output) = run_str("type,client,amount\ndeposit,1,1\n");
        assert!(matches!(result, Err(RunError::MissingColumn("tx"))));
        assert_eq!(output, "");
    }

    #[test]
    fn unbalanced_quote_is_fatal_and_writes_nothing() {
        let (result, output) =
            run_str("type,client,tx,amount\ndeposit,1,1,1\ndispute,1,1,\"\ndeposit,2,2,1\n");
        assert!(matches!(result, Err(RunError::LineBreakInField(2))));
        assert_eq!(output, "");
    }

    /// Yields `data`, then fails like a broken disk or connection.
    struct FailingReader<'a> {
        data: &'a [u8],
    }

    impl Read for FailingReader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.data.is_empty() {
                return Err(io::Error::other("disk on fire"));
            }
            let n = self.data.read(buf)?;
            Ok(n)
        }
    }

    #[test]
    fn read_error_part_way_is_fatal_and_writes_nothing() {
        let input = FailingReader {
            data: b"type,client,tx,amount\ndeposit,1,1,1\n",
        };
        let mut output = Vec::new();
        let result = run(input, &mut output);
        assert!(matches!(result, Err(RunError::Read(ref err)) if err.is_io_error()));
        assert!(output.is_empty());
    }

    /// Accepts nothing, like a closed pipe or a full disk.
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("disk full"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("disk full"))
        }
    }

    #[test]
    fn write_error_is_fatal() {
        let result = run(
            "type,client,tx,amount\ndeposit,1,1,1\n".as_bytes(),
            FailingWriter,
        );
        assert!(matches!(result, Err(RunError::Write(_))));
    }
}
