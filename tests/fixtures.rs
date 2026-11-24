//! Runs the real binary on every sample in `tests/fixtures/<case>/input.csv`
//! and checks its stdout against `expected.csv` the way an automated grader
//! would: as numbers, ignoring row order and how round values are written.
//!
//! The expected files were worked out by hand from the rules, not captured
//! from the program's output. `tests/fixtures/README.md` says what each case
//! covers.

#![expect(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rust_decimal::Decimal;

/// Each output row's `(available, held, total, locked)`, by client.
type Balances = BTreeMap<u16, (Decimal, Decimal, Decimal, bool)>;

const OUTPUT_HEADER: [&str; 5] = ["client", "available", "held", "total", "locked"];

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Runs the binary the way the CLI contract says, and returns its stdout.
fn run_binary(input: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_payments-engine"))
        .arg(input)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} exited with {}: {}",
        input.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// Parses an accounts CSV, tolerating whitespace and any number formatting.
#[track_caller]
fn parse_balances(text: &str) -> Balances {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes());
    assert_eq!(reader.headers().unwrap(), &OUTPUT_HEADER[..], "header");

    let mut balances = Balances::new();
    for record in reader.records() {
        let record = record.unwrap();
        let decimal = |index: usize| Decimal::from_str_exact(&record[index]).unwrap();
        let client: u16 = record[0].parse().unwrap();
        let row = (
            decimal(1),
            decimal(2),
            decimal(3),
            record[4].parse().unwrap(),
        );
        assert!(
            balances.insert(client, row).is_none(),
            "client {client} listed twice"
        );
    }
    balances
}

fn check_fixture(case: &str) {
    let dir = fixtures_dir().join(case);
    let expected = parse_balances(&fs::read_to_string(dir.join("expected.csv")).unwrap());
    let stdout = run_binary(&dir.join("input.csv"));
    // Numerically -0 == 0, so check the text: "-0.0000" would look like a bug.
    let negative_zero = stdout.split([',', '\n']).any(|field| field == "-0.0000");
    assert!(!negative_zero, "fixture {case}: negative zero in\n{stdout}");
    let actual = parse_balances(&stdout);

    for (client, (available, held, total, _)) in &actual {
        assert_eq!(
            *total,
            available + held,
            "client {client}: total != available + held"
        );
    }
    assert_eq!(actual, expected, "fixture {case}");
}

macro_rules! fixtures {
    ($($case:ident),* $(,)?) => {
        $(
            #[test]
            fn $case() {
                check_fixture(stringify!($case));
            }
        )*

        /// A sample directory without a test above would silently never run.
        #[test]
        fn every_fixture_directory_has_a_test() {
            let listed: BTreeSet<String> = [$(stringify!($case)),*].map(String::from).into();
            let on_disk: BTreeSet<String> = fs::read_dir(fixtures_dir())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.is_dir())
                .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            assert_eq!(on_disk, listed);
        }
    };
}

fixtures!(
    basic,
    whitespace_and_precision,
    unordered_clients,
    dispute_resolve,
    chargeback_locks,
    fraud_negative_balance,
    invalid_references,
    duplicate_tx_ids,
    malformed_rows,
    refused_withdrawal_new_client,
    large_amounts,
    reordered_columns,
    crlf_bom_quotes,
    header_only,
    empty,
    held_funds_and_final_states,
    locked_account_disputes,
    cross_client_references,
    amount_edge_cases,
);

/// The comparisons above ignore formatting; this pins the exact bytes once:
/// header, client order, four decimal places, `true`/`false`.
#[test]
fn output_format_is_exact() {
    let dir = fixtures_dir().join("fraud_negative_balance");
    let expected = fs::read_to_string(dir.join("expected.csv")).unwrap();
    assert_eq!(run_binary(&dir.join("input.csv")), expected);
}
