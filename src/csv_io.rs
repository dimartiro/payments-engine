//! CSV input and output: the only module that deals with text or `csv`.
//!
//! Rows become [`Transaction`]s at this boundary ("parse, don't validate"), so
//! the engine never sees a string. Reading streams: one reused record buffer,
//! fields borrowed from it, so reading a row allocates nothing (except a row
//! that isn't valid UTF-8, for csv's error).

use std::io::{Read, Write};
use std::str::FromStr;

use csv::{ReaderBuilder, StringRecord, Trim};
use thiserror::Error;

use crate::amount::{Amount, AmountError};
use crate::engine::Engine;
use crate::transaction::{ClientId, Transaction, TransactionKind, TxId};

/// Output columns, in order.
const OUTPUT_COLUMNS: [&str; 5] = ["client", "available", "held", "total", "locked"];

/// Why the run as a whole failed.
#[derive(Debug, Error)]
pub enum RunError {
    /// The input could not be read (an I/O error, or a header that isn't valid UTF-8).
    #[error("cannot read input: {0}")]
    Read(csv::Error),
    /// The header row lacks a required column.
    #[error("input header has no `{0}` column")]
    MissingColumn(&'static str),
    /// The header row has a required column more than once, so which one to
    /// read is ambiguous.
    #[error("input header has more than one `{0}` column")]
    DuplicateColumn(&'static str),
    /// A field spans several lines. No valid field contains a line break, so
    /// this is an unbalanced quote that swallowed the rows after it: balances
    /// computed without them would be wrong.
    #[error(
        "data row {0}: a field spans several lines (unbalanced quote?), so later rows may be lost"
    )]
    LineBreakInField(u64),
    /// The output could not be written.
    #[error("cannot write output: {0}")]
    Write(csv::Error),
}

/// Why a data row was skipped as malformed. Reading continues with the next row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RowError {
    /// The row is not valid UTF-8.
    #[error("not valid UTF-8")]
    InvalidUtf8,
    /// `type` is not one of `deposit`, `withdrawal`, `dispute`, `resolve`, `chargeback`.
    #[error("unknown transaction type")]
    UnknownType,
    /// `client` is not a `u16` written as plain digits.
    #[error("client is not a u16 integer")]
    InvalidClient,
    /// `tx` is not a `u32` written as plain digits.
    #[error("tx is not a u32 integer")]
    InvalidTx,
    /// A deposit or withdrawal without an amount.
    #[error("missing amount")]
    MissingAmount,
    /// A deposit or withdrawal whose amount is not a valid [`Amount`].
    #[error("invalid amount: {0}")]
    Amount(AmountError),
    /// Text in a field past the header's last column, e.g. an unquoted decimal
    /// comma (`1,5`). Dropping it silently could change an amount.
    #[error("more fields than the header has columns")]
    ExtraField,
}

/// One data row: its position in the input, and its transaction or why it is
/// malformed.
#[derive(Debug)]
pub struct Row {
    /// 1-based number of the data row, for messages: csv's record index, where
    /// the header is record 0 and empty lines don't count. Not a line number:
    /// csv reports a row that follows empty lines as starting on the first of them.
    pub number: u64,
    pub transaction: Result<Transaction, RowError>,
}

/// Where the required columns are, found once from the header. Matching by
/// name means any column order works and other columns are ignored.
#[derive(Debug, Clone, Copy)]
struct Columns {
    kind: usize,
    client: usize,
    tx: usize,
    amount: usize,
    /// Number of header columns: fields past it must be empty.
    width: usize,
}

impl Columns {
    /// Finds each required column, which must appear exactly once.
    fn locate(headers: &StringRecord) -> Result<Self, RunError> {
        let find = |name: &'static str| {
            let mut matches = headers
                .iter()
                .enumerate()
                .filter(|&(_, header)| header == name);
            match (matches.next(), matches.next()) {
                (Some((index, _)), None) => Ok(index),
                (None, _) => Err(RunError::MissingColumn(name)),
                (Some(_), Some(_)) => Err(RunError::DuplicateColumn(name)),
            }
        };
        Ok(Self {
            kind: find("type")?,
            client: find("client")?,
            tx: find("tx")?,
            amount: find("amount")?,
            width: headers.len(),
        })
    }

    /// The row's required fields, untrimmed. A field the row stops before
    /// reads as empty: disputes, resolves and chargebacks may omit the amount.
    fn fields<'r>(&self, record: &'r StringRecord) -> RawRow<'r> {
        let field = |index| record.get(index).unwrap_or("");
        RawRow {
            kind: field(self.kind),
            client: field(self.client),
            tx: field(self.tx),
            amount: field(self.amount),
        }
    }

    /// Whether the row has text past the header's last column. Empty extra
    /// fields, like a trailing comma, are fine.
    fn has_extra_text(&self, record: &StringRecord) -> bool {
        record
            .iter()
            .skip(self.width)
            .any(|field| !field.trim().is_empty())
    }
}

/// A data row's required fields as written, borrowed from the record buffer.
struct RawRow<'a> {
    kind: &'a str,
    client: &'a str,
    tx: &'a str,
    amount: &'a str,
}

impl TryFrom<RawRow<'_>> for Transaction {
    type Error = RowError;

    fn try_from(row: RawRow<'_>) -> Result<Self, Self::Error> {
        let client = parse_id(row.client)
            .map(ClientId)
            .ok_or(RowError::InvalidClient)?;
        let tx = parse_id(row.tx).map(TxId).ok_or(RowError::InvalidTx)?;
        // Only deposits and withdrawals read the amount; on the other kinds
        // whatever is there is ignored.
        let amount = || match row.amount.trim() {
            "" => Err(RowError::MissingAmount),
            text => text.parse::<Amount>().map_err(RowError::Amount),
        };
        let kind = match row.kind.trim() {
            "deposit" => TransactionKind::Deposit(amount()?),
            "withdrawal" => TransactionKind::Withdrawal(amount()?),
            "dispute" => TransactionKind::Dispute,
            "resolve" => TransactionKind::Resolve,
            "chargeback" => TransactionKind::Chargeback,
            _ => return Err(RowError::UnknownType),
        };
        Ok(Transaction { client, tx, kind })
    }
}

/// Parses an id written as plain ASCII digits, ignoring surrounding whitespace.
///
/// Stricter than `u16::from_str` alone, which also accepts a leading `+`.
fn parse_id<T: FromStr>(text: &str) -> Option<T> {
    let text = text.trim();
    let is_digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    if is_digits { text.parse().ok() } else { None }
}

/// Streams transactions out of CSV input, one row at a time.
pub struct TransactionReader<R> {
    csv: csv::Reader<R>,
    /// `None` for empty input, which has no header and no rows.
    columns: Option<Columns>,
    /// Reused for every row, so steady-state reading doesn't allocate.
    record: StringRecord,
}

impl<R: Read> TransactionReader<R> {
    /// Reads and checks the header row. Empty input is valid and has no rows.
    pub fn new(input: R) -> Result<Self, RunError> {
        let mut csv = ReaderBuilder::new()
            // Only the header is trimmed here: csv's per-field trimming
            // allocates a new record per row, so fields are trimmed when
            // parsed instead.
            .trim(Trim::Headers)
            // Disputes, resolves and chargebacks may stop before the amount.
            .flexible(true)
            .from_reader(input);
        let headers = csv.headers().map_err(RunError::Read)?;
        // A 0-byte input (or only empty lines) has an empty header record.
        let columns = if headers.is_empty() {
            None
        } else {
            Some(Columns::locate(headers)?)
        };
        Ok(Self {
            csv,
            columns,
            record: StringRecord::new(),
        })
    }

    /// The next data row, `None` at the end of the input, or a fatal error if
    /// the input can no longer be read reliably.
    pub fn next_row(&mut self) -> Result<Option<Row>, RunError> {
        let Some(columns) = self.columns else {
            return Ok(None);
        };
        match self.csv.read_record(&mut self.record) {
            Ok(true) => {}
            Ok(false) => return Ok(None),
            // Invalid UTF-8 loses only this row; anything else (I/O) is fatal.
            Err(err) => {
                return match err.kind() {
                    csv::ErrorKind::Utf8 { .. } => Ok(Some(Row {
                        number: err.position().map_or(0, csv::Position::record),
                        transaction: Err(RowError::InvalidUtf8),
                    })),
                    _ => Err(RunError::Read(err)),
                };
            }
        }

        // The header is record 0, so data rows count from 1.
        let number = self.record.position().map_or(0, csv::Position::record);
        if self.record.as_slice().contains(['\n', '\r']) {
            return Err(RunError::LineBreakInField(number));
        }
        let transaction = if columns.has_extra_text(&self.record) {
            Err(RowError::ExtraField)
        } else {
            Transaction::try_from(columns.fields(&self.record))
        };
        Ok(Some(Row {
            number,
            transaction,
        }))
    }
}

/// Writes every account as a CSV row, in client order, with amounts to four
/// decimal places.
pub fn write_accounts<W: Write>(engine: &Engine, output: W) -> Result<(), RunError> {
    let mut csv = csv::Writer::from_writer(output);
    // Written explicitly, so that even an empty ledger produces a header.
    csv.write_record(OUTPUT_COLUMNS).map_err(RunError::Write)?;
    for (client, account) in engine.accounts() {
        csv.write_record([
            client.0.to_string(),
            format!("{:.4}", account.available()),
            format!("{:.4}", account.held()),
            format!("{:.4}", account.total()),
            account.is_locked().to_string(),
        ])
        .map_err(RunError::Write)?;
    }
    // Dropping a `csv::Writer` ignores flush errors, so flush explicitly.
    csv.flush().map_err(|err| RunError::Write(err.into()))
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap is a failed test"
)]
mod tests {
    use super::*;

    // ---- reading -----------------------------------------------------------

    /// Every row of `input`, which must have a valid header.
    fn rows(input: &str) -> Vec<Row> {
        let mut reader = TransactionReader::new(input.as_bytes()).unwrap();
        let mut rows = Vec::new();
        while let Some(row) = reader.next_row().unwrap() {
            rows.push(row);
        }
        rows
    }

    /// The single data row of `input`, after a standard header.
    fn parse(row: &str) -> Result<Transaction, RowError> {
        let mut rows = rows(&format!("type,client,tx,amount\n{row}\n"));
        assert_eq!(rows.len(), 1, "expected exactly one row");
        rows.remove(0).transaction
    }

    fn transaction(client: u16, tx: u32, kind: TransactionKind) -> Transaction {
        Transaction {
            client: ClientId(client),
            tx: TxId(tx),
            kind,
        }
    }

    fn amount(s: &str) -> Amount {
        s.parse().unwrap()
    }

    #[test]
    fn reads_every_transaction_type() {
        let cases = [
            (
                "deposit,1,2,1.5",
                transaction(1, 2, TransactionKind::Deposit(amount("1.5"))),
            ),
            (
                "withdrawal,3,4,0.25",
                transaction(3, 4, TransactionKind::Withdrawal(amount("0.25"))),
            ),
            ("dispute,5,6,", transaction(5, 6, TransactionKind::Dispute)),
            ("resolve,7,8,", transaction(7, 8, TransactionKind::Resolve)),
            (
                "chargeback,9,10,",
                transaction(9, 10, TransactionKind::Chargeback),
            ),
        ];
        for (row, expected) in cases {
            assert_eq!(parse(row).unwrap(), expected, "row {row:?}");
        }
    }

    #[test]
    fn accepts_whitespace_around_header_names_and_fields() {
        let rows = rows("type, client, tx, amount\n  deposit ,  1 , 2 ,  1.5  \n");
        let expected = transaction(1, 2, TransactionKind::Deposit(amount("1.5")));
        assert_eq!(rows[0].transaction.as_ref().unwrap(), &expected);
    }

    #[test]
    fn matches_columns_by_name_and_ignores_extra_ones() {
        let rows = rows("tx,amount,note,client,type\n2,1.5,hello,1,deposit\n");
        let expected = transaction(1, 2, TransactionKind::Deposit(amount("1.5")));
        assert_eq!(rows[0].transaction.as_ref().unwrap(), &expected);
    }

    #[test]
    fn accepts_quoted_fields() {
        let expected = transaction(1, 2, TransactionKind::Deposit(amount("1.5")));
        assert_eq!(parse(r#""deposit","1","2","1.5""#).unwrap(), expected);
    }

    #[test]
    fn dispute_rows_may_omit_the_amount_or_leave_it_blank() {
        for row in [
            "dispute,1,2",
            "dispute,1,2,",
            "dispute,1,2,   ",
            "dispute, 1, 2",
        ] {
            assert_eq!(
                parse(row).unwrap(),
                transaction(1, 2, TransactionKind::Dispute),
                "row {row:?}"
            );
        }
    }

    #[test]
    fn amount_on_a_dispute_row_is_ignored_unparsed() {
        assert_eq!(
            parse("dispute,1,2,not-a-number").unwrap(),
            transaction(1, 2, TransactionKind::Dispute)
        );
    }

    #[test]
    fn deposit_or_withdrawal_without_an_amount_is_malformed() {
        for row in ["deposit,1,2", "deposit,1,2,", "withdrawal,1,2, "] {
            assert_eq!(parse(row), Err(RowError::MissingAmount), "row {row:?}");
        }
    }

    #[test]
    fn invalid_amount_is_malformed() {
        assert_eq!(
            parse("deposit,1,2,-1"),
            Err(RowError::Amount(AmountError::InvalidFormat))
        );
        assert_eq!(
            parse("deposit,1,2,1.23456"),
            Err(RowError::Amount(AmountError::TooPrecise))
        );
    }

    #[test]
    fn unknown_or_miscased_type_is_malformed() {
        for row in [
            "Deposit,1,2,1",
            "withdraw,1,2,1",
            "transfer,1,2,1",
            ",1,2,1",
        ] {
            assert_eq!(parse(row), Err(RowError::UnknownType), "row {row:?}");
        }
    }

    #[test]
    fn client_must_be_a_u16_written_as_digits() {
        for client in ["70000", "65536", "-1", "+1", "0x1F", "1.0", "", "one"] {
            let row = format!("deposit,{client},2,1");
            assert_eq!(
                parse(&row),
                Err(RowError::InvalidClient),
                "client {client:?}"
            );
        }
        assert_eq!(
            parse("deposit,65535,2,1").unwrap().client,
            ClientId(u16::MAX)
        );
    }

    #[test]
    fn tx_must_be_a_u32_written_as_digits() {
        for tx in ["4294967296", "-1", "+1", "1e3", ""] {
            let row = format!("deposit,1,{tx},1");
            assert_eq!(parse(&row), Err(RowError::InvalidTx), "tx {tx:?}");
        }
        assert_eq!(parse("deposit,1,4294967295,1").unwrap().tx, TxId(u32::MAX));
    }

    #[test]
    fn row_stopping_before_a_required_field_is_malformed_and_reading_continues() {
        let rows = rows("type,client,tx,amount\ndeposit,1\ndeposit,1,2,3\n");
        assert_eq!(rows[0].transaction, Err(RowError::InvalidTx));
        assert!(rows[1].transaction.is_ok());
    }

    #[test]
    fn extra_header_columns_do_not_require_longer_rows() {
        // A trailing comma in the header adds an unnamed column.
        let trailing_comma = rows("type,client,tx,amount,\ndeposit,1,1,1\ndispute,1,1\n");
        let deposit = transaction(1, 1, TransactionKind::Deposit(amount("1")));
        assert_eq!(trailing_comma[0].transaction, Ok(deposit));
        assert_eq!(
            trailing_comma[1].transaction,
            Ok(transaction(1, 1, TransactionKind::Dispute))
        );

        let named_extra = rows("type,client,tx,note,amount\ndispute,7,11\n");
        assert_eq!(
            named_extra[0].transaction,
            Ok(transaction(7, 11, TransactionKind::Dispute))
        );
    }

    #[test]
    fn text_past_the_last_header_column_is_malformed() {
        // An unquoted decimal comma must not turn 1,5 into a deposit of 1.
        assert_eq!(parse("deposit,1,1,1,5"), Err(RowError::ExtraField));
        // Empty extra fields (trailing commas) are fine.
        assert!(parse("deposit,1,1,1,").is_ok());
        assert!(parse("dispute,1,1,, ,").is_ok());
    }

    #[test]
    fn field_spanning_several_lines_is_fatal() {
        // An unbalanced quote swallows every row after it into one field.
        let input = "type,client,tx,amount\ndeposit,1,1,10\ndispute,1,1,\"\ndeposit,2,2,5\n";
        let mut reader = TransactionReader::new(input.as_bytes()).unwrap();
        assert!(reader.next_row().unwrap().unwrap().transaction.is_ok());
        assert!(matches!(
            reader.next_row(),
            Err(RunError::LineBreakInField(2))
        ));
    }

    #[test]
    fn row_with_invalid_utf8_is_malformed_and_reading_continues() {
        let input = b"type,client,tx,amount\ndeposit,1,2,\xff\ndeposit,1,3,1\n";
        let mut reader = TransactionReader::new(&input[..]).unwrap();
        let first = reader.next_row().unwrap().unwrap();
        assert_eq!(first.transaction, Err(RowError::InvalidUtf8));
        assert_eq!(first.number, 1);
        let second = reader.next_row().unwrap().unwrap();
        assert_eq!(second.transaction.unwrap().tx, TxId(3));
        assert!(reader.next_row().unwrap().is_none());
    }

    #[test]
    fn handles_bom_crlf_and_blank_lines() {
        let rows = rows("\u{feff}type,client,tx,amount\r\n\r\ndeposit,1,2,1.5\r\n\r\n");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].transaction.is_ok());
    }

    #[test]
    fn numbers_data_rows_from_one_skipping_header_and_blank_lines() {
        let input = "type,client,tx,amount\ndeposit,1,1,1\n\n\nbogus,1,2,1\n\ndeposit,1,3,1\n";
        let numbers: Vec<u64> = rows(input).iter().map(|row| row.number).collect();
        assert_eq!(numbers, [1, 2, 3]);
    }

    #[test]
    fn empty_input_and_header_only_input_have_no_rows() {
        for input in [
            "",
            "\n\n",
            "\r\n\r\n",
            "\u{feff}",
            "type,client,tx,amount\n",
        ] {
            assert!(rows(input).is_empty(), "input {input:?}");
        }
    }

    #[test]
    fn header_missing_a_required_column_is_fatal() {
        let missing = |input: &str| match TransactionReader::new(input.as_bytes()) {
            Err(RunError::MissingColumn(column)) => column,
            other => panic!("expected MissingColumn, got {:?}", other.err()),
        };
        assert_eq!(missing("type,client,amount\n"), "tx");
        // Names are exact and case-sensitive.
        assert_eq!(missing("Type,client,tx,amount\n"), "type");
        // Data without a header row.
        assert_eq!(missing("deposit,1,1,1.0\n"), "type");
    }

    #[test]
    fn header_repeating_a_required_column_is_fatal() {
        let duplicate = |input: &str| match TransactionReader::new(input.as_bytes()) {
            Err(RunError::DuplicateColumn(column)) => column,
            other => panic!("expected DuplicateColumn, got {:?}", other.err()),
        };
        assert_eq!(duplicate("type,client,tx,amount,amount\n"), "amount");
        // Names are compared after trimming.
        assert_eq!(duplicate("type,client,tx, tx ,amount\n"), "tx");
        // Repeated columns that aren't required are fine.
        assert!(TransactionReader::new("type,client,tx,amount,note,note\n".as_bytes()).is_ok());
    }

    // ---- writing -----------------------------------------------------------

    fn written(engine: &Engine) -> String {
        let mut output = Vec::new();
        write_accounts(engine, &mut output).unwrap();
        String::from_utf8(output).unwrap()
    }

    #[track_caller]
    fn engine_with(transactions: &[Transaction]) -> Engine {
        let mut engine = Engine::new();
        for &t in transactions {
            assert_eq!(
                engine.apply(t),
                Ok(()),
                "setup transaction {t:?} was refused"
            );
        }
        engine
    }

    #[test]
    fn writes_a_header_and_one_row_per_client_in_client_order() {
        let engine = engine_with(&[
            transaction(2, 1, TransactionKind::Deposit(amount("2"))),
            transaction(1, 2, TransactionKind::Deposit(amount("1.5"))),
            transaction(1, 3, TransactionKind::Deposit(amount("0.25"))),
            transaction(1, 3, TransactionKind::Dispute),
        ]);
        assert_eq!(
            written(&engine),
            "client,available,held,total,locked\n\
             1,1.5000,0.2500,1.7500,false\n\
             2,2.0000,0.0000,2.0000,false\n"
        );
    }

    #[test]
    fn writes_negative_balances_and_locked_accounts() {
        let engine = engine_with(&[
            transaction(1, 1, TransactionKind::Deposit(amount("10"))),
            transaction(1, 2, TransactionKind::Withdrawal(amount("8"))),
            transaction(1, 1, TransactionKind::Dispute),
            transaction(1, 1, TransactionKind::Chargeback),
        ]);
        assert_eq!(
            written(&engine),
            "client,available,held,total,locked\n1,-8.0000,0.0000,-8.0000,true\n"
        );
    }

    #[test]
    fn writes_only_the_header_when_there_are_no_accounts() {
        assert_eq!(
            written(&Engine::new()),
            "client,available,held,total,locked\n"
        );
    }
}
