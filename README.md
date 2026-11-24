# payments-engine

A toy payments engine. It streams a CSV of transactions (deposits, withdrawals,
disputes, resolves and chargebacks), applies them to client accounts, and
prints the final balance of every account as CSV.

```sh
cargo run -- transactions.csv > accounts.csv
```

## Contents

- [Usage](#usage)
- [How it works](#how-it-works)
- [Design decisions](#design-decisions)
- [Assumptions](#assumptions)
- [Correctness: how I know it works](#correctness-how-i-know-it-works)
- [Safety and error handling](#safety-and-error-handling)
- [Efficiency](#efficiency)
- [Many concurrent streams](#many-concurrent-streams)
- [Limitations and future work](#limitations-and-future-work)
- [Use of AI](#use-of-ai)

## Usage

```sh
cargo run -- transactions.csv > accounts.csv   # the only argument is the input file
cargo test                                     # unit, sample-data, CLI and property tests
```

Requires Rust 1.85 or later (edition 2024).

Input: a header plus one transaction per row. Whitespace around fields is
ignored, and dispute, resolve and chargeback rows may leave the amount out.

```text
type,       client, tx, amount
deposit,         1,  1,    1.5
withdrawal,      1,  2,   0.25
dispute,         1,  1,
```

Output: one row per client, in client order, amounts with four decimal places.
Here the dispute holds the whole deposit of 1.5 although 0.25 was already
withdrawn, so `available` goes negative (see [Assumptions](#assumptions)).

```text
client,available,held,total,locked
1,-0.2500,1.5000,1.2500,false
```

| Exit code | Meaning | stdout |
|---|---|---|
| 0 | Success, even if some rows were skipped or refused | the balances |
| 1 | The input can't be read reliably, or the output can't be written | empty (unless writing itself failed part-way) |
| 2 | Wrong number of arguments | empty |

A successful run writes nothing to stderr. A failed run writes one line saying
why. Per-row diagnostics are opt-in: `RUST_LOG=info` lists skipped malformed
rows, `RUST_LOG=debug` also lists refused transactions.

## How it works

```text
 transactions.csv
        │  one row at a time, into one reused buffer
        ▼
 csv_io::TransactionReader ── malformed row ──▶ skipped, counted
        │  Transaction (typed: ids, validated Amount, kind)
        ▼
 Engine::apply ───────────── refused ─────────▶ skipped, counted (nothing changes)
        │  Account balances + dispute state of each deposit
        ▼
 csv_io::write_accounts ──▶ stdout   (only after the whole input was read)
```

1. **Read.** `TransactionReader` reads the header once to find the four
   columns, then one row at a time into a single reused buffer.
2. **Parse.** Each row becomes a `Transaction` at this one boundary ("parse,
   don't validate"). Ids must be plain digits, and amounts go through `Amount`,
   which rejects anything that isn't a positive number with at most four
   decimal places. A row that fails is skipped; the run goes on.
3. **Apply.** `Engine::apply` updates the client's `Account` and, for disputes,
   the state of the deposit they refer to. A transaction the rules refuse (not
   enough funds, unknown deposit, locked account, ...) changes nothing.
4. **Write.** Once the whole file is read, `write_accounts` prints every
   account. A fatal error before that point leaves stdout empty, so a partial
   file never produces balances.

| Module | Responsibility |
|---|---|
| `main.rs` | CLI: arguments, exit codes, error messages |
| `lib.rs` | `run` (read everything, then write) and `process` (read and apply) |
| `csv_io.rs` | The only code that touches text or `csv`: header check, row parsing, output |
| `transaction.rs` | `Transaction`, `TransactionKind`, and the `ClientId` / `TxId` newtypes |
| `amount.rs` | `Amount`: the only way to get money into the system |
| `engine.rs` | `Engine`: accounts plus accepted deposits, and the rules for each transaction kind |
| `dispute.rs` | The dispute lifecycle of a deposit, as one state machine |
| `account.rs` | `Account`: balances, and the only place they change |
| `error.rs` | `TxError`: why a transaction was refused |

A good reading order is `lib.rs`, `csv_io.rs`, `engine.rs`, `dispute.rs`, `account.rs`.

## Design decisions

### Money is exact decimal, validated once

Amounts are `rust_decimal::Decimal` (exact base-10, never floating point),
wrapped in an `Amount` newtype. An `Amount` can only be built by parsing or by
`TryFrom<Decimal>`, and both enforce: greater than zero, at most four decimal
places, at most 1e15.

- **No rounding.** More than four decimal places makes the row malformed.
  Rounding would quietly create or destroy money.
- **Strict format.** Only digits with at most one `.` are accepted. The
  decimal crate's own parser also accepts things like `1_000` and, through
  serde, goes via `f64` and loses digits, so neither is used.
- **The 1e15 cap keeps every balance exact.** Past about 7.9e24, `Decimal`
  addition silently rounds away decimal places instead of failing. With at
  most 2³² deposits (one per `u32` id) of at most 1e15 each, no balance can get
  near that. `Account` also refuses any balance beyond 2³² × 1e15
  (`Account::MAX_BALANCE`), so even direct users of `Account` stay exact.
- **Why not `i64` fixed-point?** It would make arithmetic cheaper, but needs a
  hand-written parser and formatter, and the stored record wouldn't get smaller
  without also packing it. Not worth it unless profiling says so.

### Invalid states are hard to represent

- `TransactionKind` carries an `Amount` only on deposits and withdrawals, so a
  dispute can't have one.
- `ClientId(u16)` and `TxId(u32)` are distinct types and can't be swapped.
- `Account`'s fields are private. Every balance change goes through one
  function, `apply_delta`, which computes all new values with checked
  arithmetic and only then assigns them. `total` is computed, never stored, so
  `total == available + held` always holds.
- The dispute lifecycle is one exhaustive `match` with no catch-all arm, so
  adding a state or an event doesn't compile until its transitions are decided:

```text
               dispute                chargeback
 Undisputed ──────────────▶ Disputed ──────────────▶ ChargedBack (final)
      ▲                        │
      └──────── resolve ───────┘
```

### All-or-nothing transactions

`Engine::apply` either applies a transaction completely or returns a `TxError`
and leaves the engine exactly as it was. For a dispute, resolve or chargeback
the checks run in a fixed order: the deposit exists, it belongs to this client,
the event is allowed in the deposit's current state, the balance change
succeeds. Only then is the new state saved. Most tests of a refusal compare
the engine with a copy taken before the call.

There is one documented exception: a refused withdrawal for a client never seen
before still creates that client's empty account (see
[Assumptions](#assumptions)).

### Only deposits are stored

Disputes can only refer to deposits, so the engine keeps a
`HashMap<TxId, DepositRecord>` of accepted deposits and nothing else. Memory
grows with deposits, not with rows. A record is 20 bytes (client, amount,
dispute state), checked at compile time. Records are kept after a chargeback so
that later references are still recognised and the id stays taken.

Accounts live in a `BTreeMap<ClientId, Account>`: output comes out sorted by
client with no extra code, and there are at most 65,536 accounts.

### Reading CSV

- The header is read once to find the position of `type`, `client`, `tx` and
  `amount`, so columns can come in any order and extra columns are ignored.
  Each row is then read by position. A missing or repeated required column is
  fatal.
- Fields are trimmed by the parser rather than by `csv`, because `csv`'s
  trimming allocates a new record for every row. With fields borrowed from the
  reused buffer, reading a well-formed row allocates nothing.
- Rows may be shorter than the header (`flexible`), so `dispute,1,2` works.
  Text past the last column makes the row malformed: an unquoted decimal comma
  (`deposit,1,2,1,5`) must not be read as a deposit of 1.
- No valid field contains a line break. If one does, a stray `"` has swallowed
  the rows after it, and the run stops rather than print balances computed
  without them.

### Dependencies

| Crate | Used for |
|---|---|
| `csv` | Streaming reader and writer |
| `rust_decimal` | Exact decimal money, without its `serde` feature (see above) |
| `thiserror` | `Display` and `Error` for the error enums |
| `log`, `env_logger` | Opt-in per-row diagnostics via `RUST_LOG`; default features off |
| `proptest` (dev only) | The property test |

Rows aren't deserialized with serde: reading fields by position from one
reused record is what lets the parser trim without allocating and reject text
past the last column.

## Assumptions

The specification leaves some cases open. Where it does, I chose what a bank
would do and documented it here. Every rule has a test. Unit tests are listed
as `module::name` (their full path is `module::tests::name`), `cli::name` and
`fixtures::name` are in the files of the same name under `tests/`, and a bare
name is a sample-data case. `cargo test` matches any part of a test's path, so
filter by the name: `cargo test locked_account` runs every test with that in it.

| Situation | Behaviour | Why | Tests |
|---|---|---|---|
| Header columns | `type`, `client`, `tx`, `amount` in any order, exact lowercase names; other columns ignored. A missing or repeated one is fatal | A structurally wrong file should fail once, not once per row | `csv_io::header_*`, fixture `reordered_columns`, `cli::structurally_broken_input_*` |
| Formatting variations | Whitespace, quotes, a UTF-8 BOM, CRLF, blank lines and a missing final newline are fine | Common in exported files | `csv_io::handles_bom_crlf_and_blank_lines`, fixtures `whitespace_and_precision`, `crlf_bom_quotes` |
| Empty file or header only | Output is just the header; exit 0 | No input is a valid, empty result | fixtures `empty`, `header_only` |
| Malformed row: wrong type or case, id not plain digits or out of range, missing or invalid amount, text past the last column, invalid UTF-8 | Skipped; the run continues; no account is created | One bad row shouldn't discard the batch | `csv_io::*_is_malformed*`, `csv_io::*_written_as_digits`, fixtures `malformed_rows`, `amount_edge_cases` |
| Amount on a dispute, resolve or chargeback | Ignored, never parsed | These rows act on the referenced deposit's amount | `csv_io::amount_on_a_dispute_row_is_ignored_unparsed` |
| Amount with more than 4 decimals, zero, or above 1e15 | Row is malformed (`2.50000` is fine) | Never round money; keep arithmetic exact | `amount::tests`, fixtures `large_amounts`, `amount_edge_cases` |
| Unknown client on a deposit or withdrawal | Account created when the transaction is applied, and also when a withdrawal is refused (it prints as all zeros). A duplicate-id deposit creates none (see below) | The spec says a transaction for an unknown client creates a record. A declined withdrawal is still a real request from that client; a duplicate id is a replay | `engine::refused_withdrawal_still_creates_an_empty_account_for_a_new_client`, fixture `refused_withdrawal_new_client` |
| Unknown client on a dispute, resolve or chargeback | Ignored; no account created | They refer to a deposit, which this client can't have | `engine::reference_to_an_unknown_transaction_is_refused_and_creates_no_account` |
| Withdrawal larger than available funds | Refused. Held funds can't be withdrawn; exactly the available amount is fine | Spec, plus held funds are frozen by definition | `engine::withdrawal_with_insufficient_funds_changes_nothing`, `account::held_funds_cannot_be_withdrawn` |
| Dispute of a withdrawal, an unknown id, or a deposit that appears later | Ignored | Only deposits can be reversed; file order is time order | `engine::dispute_of_a_withdrawal_is_refused`, `engine::dispute_before_its_deposit_appears_is_refused` |
| Dispute naming another client's deposit | Ignored | One client must not be able to freeze another's funds | `engine::dispute_naming_another_client_is_refused`, fixture `cross_client_references` |
| Dispute after the funds were already withdrawn | Allowed: the full amount is held and available goes negative | This is the fraud the spec describes; refusing it would let the client keep the money | `engine::dispute_after_the_funds_were_withdrawn_makes_available_negative`, fixture `fraud_negative_balance` |
| Dispute of a deposit already under dispute; resolve or chargeback without an open dispute | Ignored | Holding funds twice, or releasing funds that aren't held, would corrupt the balances | `dispute::every_transition`, fixtures `invalid_references`, `held_funds_and_final_states` |
| Dispute after a resolve | Allowed | Only a chargeback is final; a new claim can be opened | `engine::deposit_can_be_disputed_again_after_a_resolve`, fixture `dispute_resolve` |
| Anything after a chargeback of that deposit | Ignored | A chargeback is final | `engine::chargeback_is_final`, fixture `held_funds_and_final_states` |
| Locked account (after a chargeback) | Refuses deposits and withdrawals, still processes disputes, resolves and chargebacks of its other deposits; never unlocks | The spec only says "frozen". A freeze blocks what the client starts, but the bank must still record reversals from its payment partner | `engine::locked_account_*`, fixtures `chargeback_locks`, `locked_account_disputes` |
| Deposit reusing an earlier deposit's id | Refused (from any client); the original is kept and no account is created | Replay protection: a duplicate must not reset a dispute | `engine::duplicate_deposit_id_*`, fixture `duplicate_tx_ids` |
| Deposit refused because the account is locked | Not remembered: can't be disputed, and its id is free | A declined transaction never entered the ledger | `engine::deposit_refused_on_a_locked_account_cannot_be_disputed` |
| Withdrawal ids | Not tracked, so a withdrawal reusing a deposit's id goes through | Ids are unique per the spec; tracking them would cost memory per withdrawal | `engine::withdrawal_ids_are_not_tracked` |
| Output | Header always; one row per account in client order; four decimal places; no `-0.0000` | Deterministic, diff-friendly output | `csv_io::writes_*`, `fixtures::output_format_is_exact` |

## Correctness: how I know it works

- **Types.** An `Amount` is always valid, a dispute can't carry an amount,
  client and transaction ids can't be swapped, and `total` can't drift from
  `available + held` (see [Design decisions](#design-decisions)).
- **Unit tests** (91) next to the code they test: every `Account` operation and
  each way it can fail, all 9 dispute transitions, each engine rule, and the
  parser's edge cases. Tests of a refused transaction also assert that nothing
  changed.
- **Sample data** (`tests/fixtures/`, 19 cases). Each has an `input.csv` and
  the `expected.csv` worked out by hand from the rules.
  `tests/fixtures.rs` runs the real binary on each and compares the output as
  numbers, ignoring row order and the formatting of round values, which the
  spec says don't matter. `tests/fixtures/README.md` says what each case covers. To look at one:
  `cargo run -- tests/fixtures/basic/input.csv | diff - tests/fixtures/basic/expected.csv`.
- **CLI contract** (`tests/cli.rs`): exit codes, empty stdout on failure,
  silent stderr on success.
- **Property test** (`tests/properties.rs`): random sequences of up to 200
  transactions over a few clients and ids (256 sequences per run by default),
  checked after every step against a
  deliberately simple reference model written independently of the engine. It
  checks the accept or refuse decision and its reason, every balance,
  `held >= 0`, the locked flag, that refusals change nothing, and that only
  deposit and withdrawal rows create accounts. Run it harder with
  `PROPTEST_CASES=5000 cargo test --test properties`.

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, `cargo clippy -D
warnings`, `cargo doc` with warnings denied, and the tests on stable and on the
declared minimum Rust version (1.85).

## Safety and error handling

- **No `unsafe`**: `unsafe_code = "forbid"` for the whole crate.
- **No panics on input.** `unwrap`, `expect` and `panic!` are lints that fail
  CI outside tests. Balance arithmetic is checked, and release builds keep
  integer overflow checks on.
- **Hostile input.** Amounts are stripped of leading zeros before parsing. The
  decimal crate's parser recurses once per leading zero, and in an
  `opt-level = 0` build 100,000 of them overflow the stack. This crate's dev
  and release profiles don't hit it, but the guard keeps that from depending on
  how the code is compiled (e.g. as a library inside a server).
- **Three kinds of error**, each with its own type:

| Kind | Type | Examples | What happens |
|---|---|---|---|
| Malformed row | `RowError` | `withdraw` instead of `withdrawal`; client `70000`; amount `1e3` | Row skipped, run continues |
| Refused transaction | `TxError` | Not enough funds; dispute of an unknown deposit; locked account | Nothing changes, run continues |
| Fatal | `RunError` | Unreadable file; missing or repeated column; a field spanning lines; a write error on stdout (e.g. a broken pipe) | Exit 1, no balances printed |

The spec treats bad references as errors on the partner's side, so neither of
the first two stops the run.

## Efficiency

**Streaming.** The input is never loaded whole (see [Reading CSV](#reading-csv)):
reading a well-formed row allocates nothing, and the only memory that grows is
the deposit table and the account map (at most 65,536 entries).

**Measured** on an Apple M3 Pro, best of three runs, with generated files: rows
spread over 10,000 clients; the mixed files are 40% deposits, 35% withdrawals
and 25% disputes, resolves and chargebacks of earlier deposits.

| Input | Release build | `cargo run` (dev profile) | Peak memory |
|---|---|---|---|
| 1M rows, mixed (about 400k deposits) | 0.26 s | 0.48 s | 28 MiB |
| 10M rows, mixed (about 4M deposits) | 2.97 s | — | 394 MiB |
| 10M rows, almost no deposits | 1.59 s | — | 2.7 MiB |

That is roughly 3.4M rows per second on realistic input. The last row shows
memory staying flat when nothing needs storing. The dev profile uses
`opt-level = 1`, because `cargo run` is how the program is meant to be run; at
the default `opt-level = 0` it was over 10 times slower than release.

**Memory per deposit.** About 29 to 57 bytes: a 25-byte table entry (4-byte
id, 20-byte record, 1 control byte) in a table kept between 7/16 and 7/8 full.
Peak memory is higher while the table grows (about 100 bytes per deposit in the
10M-row run).

**Worst case.** `TxId` is a `u32`, so in principle there could be 2³² deposits:
86 GB of records in a hash table of about 215 GB. That doesn't fit in memory. If it had to, the options
are in [Limitations and future work](#limitations-and-future-work).

## Many concurrent streams

The engine does no I/O: `Engine::apply` takes one parsed `Transaction`, and
`write_accounts` takes any `impl Write`. Reading is what would change for a
server. `process` and `TransactionReader` wrap a blocking `std::io::Read`, which
is fine for files or a few connections on a thread pool, but not for thousands
of sockets on an async runtime. There I'd make the row parser in `csv_io` public
and feed it from an async CSV reader, leaving `Engine` as it is. Then:

- **Independent streams** (each a separate ledger): one `Engine` per stream. A
  new engine allocates nothing; each stream costs its reader's buffer (about
  10 KB) plus its own deposits.
- **One shared ledger:** shard by `client % N`, with one thread or task owning
  each shard and bounded channels in front of them for backpressure. This is
  valid because every state change touches a single client: a dispute must
  name the deposit's own client, otherwise it's ignored.

Two caveats. The spec only guarantees order within one file, so a client's
events arriving on different streams would need sequencing, or a dispute could
arrive before its deposit and be dropped as unknown. And a transaction id
reused across shards would no longer be detected; that's acceptable only
because ids are unique by contract.

## Limitations and future work

- **Bounded deposit memory.** For huge inputs: a dispute window that evicts old
  deposits, or a disk-backed store behind a trait. Dropping charged-back
  records is not free: their ids must stay reserved (for example in a compact
  id set), or another client could reuse one and be credited.
- **Row size.** A single row can be arbitrarily long (memory grows with the
  longest row). A server should cap it.
- **Fuzzing** the parser with `cargo-fuzz` would complement the property test,
  which already feeds the engine random transactions.

## Use of AI

I built this with Claude Code, an AI coding assistant, in a single session.
[`prompts.txt`](prompts.txt) is the transcript: every prompt I wrote, every
reply, and a line for each action the assistant took.

- **Planning.** I had it read the challenge and propose a step-by-step plan
  with maintainability and performance as the priorities. It drafted three
  independent plans and merged the best one with ideas from the others.
- **Implementation, one step at a time.** I chose each step (scaffold, domain
  types, engine, CSV input and output, sample data, README) and decided when
  to commit.
- **Review.** After each step it ran independent reviews: adversarial code
  review, a property test against a reference model, mutation testing of the
  sample data, and a fact-check of this README. A second agent tried to
  reproduce each finding before it was fixed.
- **Decisions.** Where the specification is ambiguous I made the final call
  (see [Assumptions](#assumptions)). I mostly followed its recommendations, but
  chose a silent stderr instead of a warning summary.
