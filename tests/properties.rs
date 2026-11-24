//! Property tests for the engine: random transaction sequences, checked after
//! every step against a deliberately simple reference model.
//!
//! The id space is small (3 clients, 24 transaction ids) so that duplicates,
//! disputes of real deposits, client mismatches and locked accounts all come
//! up often. Run with more cases via `PROPTEST_CASES=5000 cargo test --test properties`.

#![expect(clippy::expect_used, reason = "a failed expect is a failed test")]

use std::collections::{BTreeMap, BTreeSet, HashMap};

use payments_engine::account::Account;
use payments_engine::amount::Amount;
use payments_engine::engine::Engine;
use payments_engine::error::TxError;
use payments_engine::transaction::{ClientId, Transaction, TransactionKind, TxId};
use proptest::prelude::*;
use rust_decimal::Decimal;

// ---- strategies --------------------------------------------------------

/// Mostly 0.0001 to 10000.0000. A few round values make exact-balance
/// withdrawals (available == amount) likely, which uniform draws would almost
/// never hit, and the occasional `Amount::MAX` exercises the largest values.
fn amount() -> impl Strategy<Value = Amount> {
    let units = prop_oneof![
        3 => 1..=100_000_000i64,
        1 => prop::sample::select(vec![10_000i64, 20_000, 50_000]),
    ];
    let everyday = units
        .prop_map(|units| Amount::try_from(Decimal::new(units, Amount::SCALE)).expect("in range"));
    prop_oneof![
        8 => everyday,
        1 => Just(Amount::MAX),
    ]
}

fn kind() -> impl Strategy<Value = TransactionKind> {
    prop_oneof![
        4 => amount().prop_map(TransactionKind::Deposit),
        3 => amount().prop_map(TransactionKind::Withdrawal),
        3 => Just(TransactionKind::Dispute),
        2 => Just(TransactionKind::Resolve),
        2 => Just(TransactionKind::Chargeback),
    ]
}

fn transaction() -> impl Strategy<Value = Transaction> {
    (1..=3u16, 1..=24u32, kind()).prop_map(|(client, tx, kind)| Transaction {
        client: ClientId(client),
        tx: TxId(tx),
        kind,
    })
}

// ---- reference model ---------------------------------------------------

/// Where a deposit is in its dispute lifecycle, written independently of
/// `payments_engine::dispute` so the two can disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Open,
    Disputed,
    ChargedBack,
}

#[derive(Debug)]
struct ModelDeposit {
    client: ClientId,
    amount: Decimal,
    status: Status,
}

/// Running sums rather than balances: the engine's balances must be derivable from them.
#[derive(Debug, Default)]
struct ModelAccount {
    deposited: Decimal,
    withdrawn: Decimal,
    charged_back: Decimal,
    locked: bool,
}

/// The ledger rules restated as plainly as possible: no checked arithmetic,
/// no shared code with the engine, balances recomputed from scratch on demand.
#[derive(Debug, Default)]
struct Model {
    accounts: BTreeMap<ClientId, ModelAccount>,
    deposits: HashMap<TxId, ModelDeposit>,
}

impl Model {
    /// What the engine should return for `transaction`, updating the model to match.
    fn apply(&mut self, transaction: Transaction) -> Result<(), TxError> {
        let Transaction { client, tx, kind } = transaction;
        match kind {
            TransactionKind::Deposit(amount) => {
                if self.deposits.contains_key(&tx) {
                    return Err(TxError::DuplicateTx);
                }
                if self.is_locked(client) {
                    return Err(TxError::AccountLocked);
                }
                self.accounts.entry(client).or_default().deposited += amount.value();
                self.deposits.insert(
                    tx,
                    ModelDeposit {
                        client,
                        amount: amount.value(),
                        status: Status::Open,
                    },
                );
                Ok(())
            }
            TransactionKind::Withdrawal(amount) => {
                // Documented exception: even a refused withdrawal creates the account.
                let available = self.available(client);
                let account = self.accounts.entry(client).or_default();
                if account.locked {
                    return Err(TxError::AccountLocked);
                }
                if available < amount.value() {
                    return Err(TxError::InsufficientFunds);
                }
                account.withdrawn += amount.value();
                Ok(())
            }
            TransactionKind::Dispute | TransactionKind::Resolve | TransactionKind::Chargeback => {
                let deposit = self.deposits.get_mut(&tx).ok_or(TxError::UnknownTx)?;
                if deposit.client != client {
                    return Err(TxError::ClientMismatch);
                }
                let next = match (deposit.status, kind) {
                    (Status::ChargedBack, _) => return Err(TxError::AlreadyChargedBack),
                    (Status::Open, TransactionKind::Dispute) => Status::Disputed,
                    (Status::Open, _) => return Err(TxError::NotDisputed),
                    (Status::Disputed, TransactionKind::Dispute) => {
                        return Err(TxError::AlreadyDisputed);
                    }
                    (Status::Disputed, TransactionKind::Resolve) => Status::Open,
                    (Status::Disputed, _) => Status::ChargedBack,
                };
                deposit.status = next;
                if next == Status::ChargedBack {
                    let account = self.accounts.get_mut(&client).expect("deposit made it");
                    account.charged_back += deposit.amount;
                    account.locked = true;
                }
                Ok(())
            }
        }
    }

    fn clients(&self) -> Vec<ClientId> {
        self.accounts.keys().copied().collect()
    }

    fn is_locked(&self, client: ClientId) -> bool {
        self.accounts.get(&client).is_some_and(|a| a.locked)
    }

    /// Accepted deposits minus accepted withdrawals minus charged-back deposits.
    fn total(&self, client: ClientId) -> Decimal {
        self.accounts.get(&client).map_or(Decimal::ZERO, |a| {
            a.deposited - a.withdrawn - a.charged_back
        })
    }

    /// The sum of this client's deposits that are under dispute right now.
    fn held(&self, client: ClientId) -> Decimal {
        self.deposits
            .values()
            .filter(|d| d.client == client && d.status == Status::Disputed)
            .map(|d| d.amount)
            .sum()
    }

    fn available(&self, client: ClientId) -> Decimal {
        self.total(client) - self.held(client)
    }
}

// ---- helpers -----------------------------------------------------------

fn snapshot(engine: &Engine) -> BTreeMap<ClientId, Account> {
    engine
        .accounts()
        .map(|(client, account)| (client, account.clone()))
        .collect()
}

fn moves_money(kind: TransactionKind) -> bool {
    matches!(
        kind,
        TransactionKind::Deposit(_) | TransactionKind::Withdrawal(_)
    )
}

// ---- properties --------------------------------------------------------

proptest! {
    #[test]
    fn engine_agrees_with_the_reference_model_after_every_step(
        transactions in prop::collection::vec(transaction(), 0..=200),
    ) {
        let mut engine = Engine::new();
        let mut model = Model::default();
        // Clients named by at least one deposit or withdrawal row, accepted or not.
        let mut money_clients = BTreeSet::new();

        for (step, &transaction) in transactions.iter().enumerate() {
            let before = engine.clone();
            let result = engine.apply(transaction);
            let client = transaction.client;
            if moves_money(transaction.kind) {
                money_clients.insert(client);
            }

            // The engine accepts or refuses exactly what the model does, for the same reason.
            prop_assert_eq!(result, model.apply(transaction), "step {}: {:?}", step, transaction);

            // A refusal changes nothing, except that a refused withdrawal from a
            // client never seen before still creates that client's empty account.
            if result.is_err() {
                let is_withdrawal = matches!(transaction.kind, TransactionKind::Withdrawal(_));
                if is_withdrawal && before.account(client).is_none() {
                    // Deposit records aren't public, so only accounts are compared
                    // here; a stray record change would fail a later step's result check.
                    let mut expected = snapshot(&before);
                    expected.insert(client, Account::default());
                    prop_assert_eq!(snapshot(&engine), expected, "step {}", step);
                } else {
                    prop_assert_eq!(&engine, &before, "step {}: refused but changed", step);
                }
            }

            // A locked account never accepts a deposit or withdrawal, and never unlocks.
            if before.account(client).is_some_and(Account::is_locked) && moves_money(transaction.kind) {
                prop_assert!(result.is_err(), "step {}: locked account accepted {:?}", step, transaction);
            }
            for (locked, _) in before.accounts().filter(|(_, a)| a.is_locked()) {
                prop_assert!(
                    engine.account(locked).is_some_and(Account::is_locked),
                    "step {}: client {:?} unlocked", step, locked
                );
            }

            // Exactly the model's accounts exist, and only for clients that had a
            // deposit or withdrawal row: dispute-type rows never create one.
            prop_assert_eq!(engine.accounts().map(|(c, _)| c).collect::<Vec<_>>(), model.clients());
            for (client, account) in engine.accounts() {
                prop_assert!(money_clients.contains(&client), "step {}: phantom account {:?}", step, client);
                prop_assert!(account.held() >= Decimal::ZERO, "step {}: negative held", step);
                prop_assert_eq!(account.total(), model.total(client), "step {}: total of {:?}", step, client);
                prop_assert_eq!(account.held(), model.held(client), "step {}: held of {:?}", step, client);
                prop_assert_eq!(account.is_locked(), model.is_locked(client));
                // Equal to zero but printed as "-0.0000" would break the output format.
                for value in [account.available(), account.held(), account.total()] {
                    prop_assert!(
                        !(value.is_zero() && value.is_sign_negative()),
                        "step {}: negative zero for {:?}", step, client
                    );
                }
            }
        }
    }
}
