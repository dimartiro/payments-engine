//! The command-line contract: exit codes, what goes to stderr, and stdout left
//! empty whenever the run fails.

#![expect(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const HEADER: &str = "client,available,held,total,locked\n";

/// The binary, with logging at its default level whatever the caller's environment.
fn binary() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_payments-engine"));
    command.env_remove("RUST_LOG");
    command
}

/// Writes `contents` to `name` in Cargo's scratch directory for integration tests.
fn input_file(name: &str, contents: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    fs::write(&path, contents).unwrap();
    path
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[track_caller]
fn assert_failed_with(output: &Output, code: i32, message: &str) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stderr: {}",
        stderr(output)
    );
    assert!(output.stdout.is_empty(), "stdout must be empty on failure");
    assert!(
        stderr(output).contains(message),
        "stderr: {}",
        stderr(output)
    );
}

#[test]
fn wrong_number_of_arguments_is_a_usage_error() {
    let cases: [&[&str]; 2] = [&[], &["a.csv", "b.csv"]];
    for args in cases {
        let output = binary().args(args).output().unwrap();
        assert_failed_with(&output, 2, "usage: payments-engine <transactions.csv>");
    }
}

#[test]
fn missing_input_file_fails() {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("does-not-exist.csv");
    let output = binary().arg(&path).output().unwrap();
    assert_failed_with(&output, 1, "cannot open input");
}

#[test]
fn structurally_broken_input_fails_without_printing_balances() {
    let cases = [
        (
            "missing_column.csv",
            "type,client,amount\ndeposit,1,1\n",
            "no `tx` column",
        ),
        (
            "duplicate_column.csv",
            "type,client,tx,amount,amount\ndeposit,1,1,1,1\n",
            "more than one `amount` column",
        ),
        (
            "unbalanced_quote.csv",
            "type,client,tx,amount\ndeposit,1,1,1\ndispute,1,1,\"\ndeposit,2,2,1\n",
            "data row 2: a field spans several lines",
        ),
    ];
    for (name, contents, message) in cases {
        let output = binary().arg(input_file(name, contents)).output().unwrap();
        assert_failed_with(&output, 1, message);
    }
}

#[test]
fn skipped_and_refused_rows_still_succeed_silently() {
    let input = "type,client,tx,amount\ndeposit,1,1,1\nbogus,1,2,1\nwithdrawal,1,3,5\n";
    let path = input_file("skipped_rows.csv", input);
    let output = binary().arg(&path).output().unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout(&output),
        format!("{HEADER}1,1.0000,0.0000,1.0000,false\n")
    );
    assert!(output.stderr.is_empty(), "stderr: {}", stderr(&output));

    // Diagnostics are opt-in.
    let output = binary()
        .arg(&path)
        .env("RUST_LOG", "debug")
        .output()
        .unwrap();
    let stderr = stderr(&output);
    assert!(
        stderr.contains("data row 2: skipped as malformed"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("data row 3: refused"), "stderr: {stderr}");
}

#[test]
fn clean_input_succeeds_silently() {
    let input = "type,client,tx,amount\ndeposit,1,1,1\n";
    let output = binary()
        .arg(input_file("clean.csv", input))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout(&output),
        format!("{HEADER}1,1.0000,0.0000,1.0000,false\n")
    );
    assert!(output.stderr.is_empty(), "stderr: {}", stderr(&output));
}
