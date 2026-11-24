# Sample data

Each directory holds an `input.csv` and the `expected.csv` balances, worked
out by hand from the rules (not captured from the program's output).
`tests/fixtures.rs` runs the binary on every case and compares the output as
numbers, ignoring row order and the formatting of round values.

To look at one by hand:

```sh
cargo run -- tests/fixtures/basic/input.csv | diff - tests/fixtures/basic/expected.csv
```

| Case | What it covers |
|---|---|
| `basic` | Deposits and withdrawals for two clients; a withdrawal larger than the available funds fails; withdrawing exactly the available funds succeeds |
| `whitespace_and_precision` | Spaces around header names and fields; amounts with up to four decimal places, trailing zeros, and a leading `.` |
| `unordered_clients` | Clients and transaction ids in no particular order; the extreme ids `0`, `65535` and `4294967295` |
| `dispute_resolve` | A dispute holds the funds, a resolve releases them, and the deposit can be disputed again; dispute rows with and without the trailing comma |
| `chargeback_locks` | A chargeback removes the held funds and locks the account; a locked account refuses deposits and withdrawals but still processes disputes |
| `fraud_negative_balance` | Deposit, withdraw, then dispute and charge back the deposit: the full amount is held and the balance goes negative |
| `invalid_references` | Disputes, resolves and chargebacks that name an unknown transaction, a withdrawal, a transaction not yet seen, or a client without an account are ignored and create no account; a second dispute of the same deposit is ignored |
| `duplicate_tx_ids` | A deposit reusing an earlier deposit's id is refused (also from another client, which gets no account); withdrawal ids aren't tracked |
| `malformed_rows` | Rows that can't be parsed are skipped and the rest still applies: wrong type spelling or case, ids out of range or signed, missing amount, more than four decimals, zero, negative, exponent, a decimal comma, a sign |
| `refused_withdrawal_new_client` | A refused withdrawal for an unknown client still creates its (empty) account |
| `large_amounts` | Amounts up to the 1e15 cap stay exact to four decimal places; a larger amount is skipped |
| `reordered_columns` | Columns in another order, an extra column, and a row that stops before the amount |
| `crlf_bom_quotes` | A UTF-8 BOM, CRLF line endings, a blank line, quoted fields, and no newline at the end |
| `header_only` | No data rows: only the output header |
| `empty` | A 0-byte file: only the output header |
| `held_funds_and_final_states` | Held funds can't be withdrawn; resolving or charging back an undisputed deposit is ignored; after a chargeback, another chargeback, a resolve or a dispute of that deposit is ignored (with other funds still held, so only the dispute rules can refuse them) |
| `locked_account_disputes` | A locked account still processes resolves and chargebacks; a deposit refused because the account is locked is not remembered, so disputing it is ignored and another client can use its id |
| `cross_client_references` | Disputes, resolves and chargebacks naming another client's deposit are ignored, even when the named client has an account with held funds |
| `amount_edge_cases` | `2.50000` is accepted (trailing zeros don't count as decimals); a zero amount is malformed, creates no account and leaves its id free; junk amounts on dispute and resolve rows are ignored; signed ids are malformed |
