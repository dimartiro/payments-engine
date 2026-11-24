//! The ledger: applies transactions to client accounts, one at a time, in file order.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};

use crate::account::Account;
use crate::amount::Amount;
use crate::dispute::{DepositRecord, DisputeEvent};
use crate::error::TxError;
use crate::transaction::{ClientId, Transaction, TransactionKind, TxId};

/// All client accounts plus the deposits that may still be disputed.
///
/// [`Engine::apply`] is all-or-nothing: when it returns an error, the engine
/// is exactly as it was before the call. The one documented exception is a
/// refused withdrawal for a client never seen before, which still creates
/// that client's (empty) account: the spec says a transaction for an unknown
/// client creates a new record.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Engine {
    /// Keyed by client; a `BTreeMap` so accounts come out sorted by client id.
    accounts: BTreeMap<ClientId, Account>,
    /// Accepted deposits only. Withdrawals are never stored: only a deposit
    /// can be disputed, so memory grows with deposits, not with all rows.
    deposits: HashMap<TxId, DepositRecord>,
}

impl Engine {
    /// An engine with no accounts.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one transaction, or explains why it was refused.
    ///
    /// A refusal is not fatal: the spec treats bad references as errors on
    /// the partner's side, so the caller logs it and moves on.
    pub fn apply(&mut self, transaction: Transaction) -> Result<(), TxError> {
        let Transaction { client, tx, kind } = transaction;
        match kind {
            TransactionKind::Deposit(amount) => self.deposit(client, tx, amount),
            TransactionKind::Withdrawal(amount) => self.withdraw(client, amount),
            TransactionKind::Dispute => self.dispute_step(client, tx, DisputeEvent::Dispute),
            TransactionKind::Resolve => self.dispute_step(client, tx, DisputeEvent::Resolve),
            TransactionKind::Chargeback => self.dispute_step(client, tx, DisputeEvent::Chargeback),
        }
    }

    /// The account of `client`, if any transaction ever created it.
    pub fn account(&self, client: ClientId) -> Option<&Account> {
        self.accounts.get(&client)
    }

    /// Every account, in ascending client id order.
    pub fn accounts(&self) -> impl Iterator<Item = (ClientId, &Account)> {
        self.accounts
            .iter()
            .map(|(&client, account)| (client, account))
    }

    /// Credits the client and remembers the deposit so it can be disputed.
    ///
    /// Refused if the id belongs to an earlier accepted deposit (the earlier
    /// record is kept, so a replay can't reset a dispute) or if the account is
    /// locked. A refused deposit is not remembered.
    fn deposit(&mut self, client: ClientId, tx: TxId, amount: Amount) -> Result<(), TxError> {
        let Entry::Vacant(slot) = self.deposits.entry(tx) else {
            return Err(TxError::DuplicateTx);
        };
        self.accounts.entry(client).or_default().deposit(amount)?;
        slot.insert(DepositRecord::new(client, amount));
        Ok(())
    }

    /// Debits the client if enough funds are available.
    fn withdraw(&mut self, client: ClientId, amount: Amount) -> Result<(), TxError> {
        self.accounts.entry(client).or_default().withdraw(amount)
    }

    /// Applies a dispute, resolve or chargeback to the deposit `tx`.
    ///
    /// Checks, in order: the deposit exists, it belongs to `client`, the
    /// event is allowed in the deposit's current state, and the balance
    /// change succeeds. The new state is saved only after all of them pass.
    fn dispute_step(
        &mut self,
        client: ClientId,
        tx: TxId,
        event: DisputeEvent,
    ) -> Result<(), TxError> {
        let record = self.deposits.get_mut(&tx).ok_or(TxError::UnknownTx)?;
        if record.client != client {
            return Err(TxError::ClientMismatch);
        }
        let next_state = record.state.next(event)?;

        // Never creates an account: the deposit was applied to this client's
        // account, and accounts are never removed.
        let account = self.accounts.entry(client).or_default();
        match event {
            DisputeEvent::Dispute => account.hold(record.amount),
            DisputeEvent::Resolve => account.release(record.amount),
            DisputeEvent::Chargeback => account.charge_back(record.amount),
        }?;

        record.state = next_state;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use rust_decimal::Decimal;

    use super::*;

    // ---- builders ------------------------------------------------------

    fn amount(s: &str) -> Amount {
        s.parse().unwrap()
    }

    fn tx(client: u16, tx: u32, kind: TransactionKind) -> Transaction {
        Transaction {
            client: ClientId(client),
            tx: TxId(tx),
            kind,
        }
    }

    fn deposit(client: u16, id: u32, value: &str) -> Transaction {
        tx(client, id, TransactionKind::Deposit(amount(value)))
    }

    fn withdrawal(client: u16, id: u32, value: &str) -> Transaction {
        tx(client, id, TransactionKind::Withdrawal(amount(value)))
    }

    fn dispute(client: u16, id: u32) -> Transaction {
        tx(client, id, TransactionKind::Dispute)
    }

    fn resolve(client: u16, id: u32) -> Transaction {
        tx(client, id, TransactionKind::Resolve)
    }

    fn chargeback(client: u16, id: u32) -> Transaction {
        tx(client, id, TransactionKind::Chargeback)
    }

    /// An engine with `transactions` applied, each of which must succeed.
    fn engine_with(transactions: &[Transaction]) -> Engine {
        let mut engine = Engine::new();
        for &t in transactions {
            engine
                .apply(t)
                .unwrap_or_else(|e| unreachable!("setup {t:?} refused: {e}"));
        }
        engine
    }

    // ---- assertions ----------------------------------------------------

    #[track_caller]
    fn assert_account(
        engine: &Engine,
        client: u16,
        available: &str,
        held: &str,
        total: &str,
        locked: bool,
    ) {
        let account = engine.account(ClientId(client)).unwrap();
        let dec = |s: &str| Decimal::from_str_exact(s).unwrap();
        assert_eq!(account.available(), dec(available), "available");
        assert_eq!(account.held(), dec(held), "held");
        assert_eq!(account.total(), dec(total), "total");
        assert_eq!(account.is_locked(), locked, "locked");
    }

    /// The transaction is refused with `expected` and the engine is unchanged.
    #[track_caller]
    fn assert_refused(engine: &mut Engine, transaction: Transaction, expected: TxError) {
        let before = engine.clone();
        assert_eq!(engine.apply(transaction), Err(expected));
        assert_eq!(
            *engine, before,
            "a refused transaction must not change the engine"
        );
    }

    // ---- deposits and withdrawals --------------------------------------

    #[test]
    fn deposit_credits_available_and_total() {
        let engine = engine_with(&[deposit(1, 1, "1.5"), deposit(1, 2, "2")]);
        assert_account(&engine, 1, "3.5", "0", "3.5", false);
    }

    #[test]
    fn withdrawal_debits_available_and_total() {
        let engine = engine_with(&[deposit(1, 1, "5"), withdrawal(1, 2, "1.25")]);
        assert_account(&engine, 1, "3.75", "0", "3.75", false);
    }

    #[test]
    fn withdrawal_with_insufficient_funds_changes_nothing() {
        let mut engine = engine_with(&[deposit(1, 1, "1")]);
        assert_refused(
            &mut engine,
            withdrawal(1, 2, "1.0001"),
            TxError::InsufficientFunds,
        );
        assert_account(&engine, 1, "1", "0", "1", false);
    }

    #[test]
    fn refused_withdrawal_still_creates_an_empty_account_for_a_new_client() {
        let mut engine = Engine::new();
        assert_eq!(
            engine.apply(withdrawal(7, 1, "1")),
            Err(TxError::InsufficientFunds)
        );
        assert_account(&engine, 7, "0", "0", "0", false);
    }

    #[test]
    fn clients_are_independent() {
        let engine = engine_with(&[
            deposit(2, 1, "2"),
            deposit(1, 2, "1"),
            withdrawal(2, 3, "0.5"),
        ]);
        assert_account(&engine, 1, "1", "0", "1", false);
        assert_account(&engine, 2, "1.5", "0", "1.5", false);
    }

    #[test]
    fn accounts_are_listed_in_client_order() {
        let engine = engine_with(&[deposit(3, 1, "1"), deposit(1, 2, "1"), deposit(2, 3, "1")]);
        let clients: Vec<u16> = engine.accounts().map(|(client, _)| client.0).collect();
        assert_eq!(clients, [1, 2, 3]);
    }

    #[test]
    fn duplicate_deposit_id_is_refused_and_keeps_the_original() {
        let mut engine = engine_with(&[deposit(1, 1, "1")]);
        assert_refused(&mut engine, deposit(1, 1, "5"), TxError::DuplicateTx);
        // Another client reusing the id gets no account either.
        assert_refused(&mut engine, deposit(2, 1, "5"), TxError::DuplicateTx);
        assert!(engine.account(ClientId(2)).is_none());

        // The original amount is still what a dispute holds.
        engine.apply(dispute(1, 1)).unwrap();
        assert_account(&engine, 1, "0", "1", "1", false);
    }

    #[test]
    fn duplicate_deposit_id_cannot_reset_an_open_dispute() {
        let mut engine = engine_with(&[deposit(1, 1, "1"), dispute(1, 1)]);
        assert_refused(&mut engine, deposit(1, 1, "1"), TxError::DuplicateTx);
        assert_refused(&mut engine, dispute(1, 1), TxError::AlreadyDisputed);
    }

    // ---- disputes, resolves and chargebacks ----------------------------

    #[test]
    fn dispute_moves_funds_from_available_to_held() {
        let engine = engine_with(&[deposit(1, 1, "3"), deposit(1, 2, "2"), dispute(1, 1)]);
        assert_account(&engine, 1, "2", "3", "5", false);
    }

    #[test]
    fn resolve_releases_held_funds() {
        let engine = engine_with(&[deposit(1, 1, "3"), dispute(1, 1), resolve(1, 1)]);
        assert_account(&engine, 1, "3", "0", "3", false);
    }

    #[test]
    fn chargeback_removes_held_funds_and_locks_the_account() {
        let engine = engine_with(&[
            deposit(1, 1, "3"),
            deposit(1, 2, "2"),
            dispute(1, 1),
            chargeback(1, 1),
        ]);
        assert_account(&engine, 1, "2", "0", "2", true);
    }

    #[test]
    fn dispute_after_the_funds_were_withdrawn_makes_available_negative() {
        // The fraud case from the spec: deposit, withdraw, then reverse the deposit.
        let engine = engine_with(&[
            deposit(1, 1, "10"),
            withdrawal(1, 2, "8"),
            dispute(1, 1),
            chargeback(1, 1),
        ]);
        assert_account(&engine, 1, "-8", "0", "-8", true);
    }

    #[test]
    fn deposit_can_be_disputed_again_after_a_resolve() {
        let engine = engine_with(&[
            deposit(1, 1, "4"),
            dispute(1, 1),
            resolve(1, 1),
            dispute(1, 1),
        ]);
        assert_account(&engine, 1, "0", "4", "4", false);
    }

    #[test]
    fn disputing_a_deposit_already_under_dispute_is_refused() {
        let mut engine = engine_with(&[deposit(1, 1, "4"), dispute(1, 1)]);
        assert_refused(&mut engine, dispute(1, 1), TxError::AlreadyDisputed);
    }

    #[test]
    fn resolve_or_chargeback_without_an_open_dispute_is_refused() {
        let mut engine = engine_with(&[deposit(1, 1, "4")]);
        assert_refused(&mut engine, resolve(1, 1), TxError::NotDisputed);
        assert_refused(&mut engine, chargeback(1, 1), TxError::NotDisputed);

        let mut engine = engine_with(&[deposit(1, 1, "4"), dispute(1, 1), resolve(1, 1)]);
        assert_refused(&mut engine, resolve(1, 1), TxError::NotDisputed);
    }

    #[test]
    fn chargeback_is_final() {
        let mut engine = engine_with(&[deposit(1, 1, "4"), dispute(1, 1), chargeback(1, 1)]);
        for t in [dispute(1, 1), resolve(1, 1), chargeback(1, 1)] {
            assert_refused(&mut engine, t, TxError::AlreadyChargedBack);
        }
    }

    #[test]
    fn reference_to_an_unknown_transaction_is_refused_and_creates_no_account() {
        let mut engine = engine_with(&[deposit(1, 1, "4")]);
        for t in [dispute(9, 99), resolve(9, 99), chargeback(9, 99)] {
            assert_refused(&mut engine, t, TxError::UnknownTx);
        }
        assert!(engine.account(ClientId(9)).is_none());
    }

    #[test]
    fn dispute_of_a_withdrawal_is_refused() {
        let mut engine = engine_with(&[deposit(1, 1, "4"), withdrawal(1, 2, "1")]);
        assert_refused(&mut engine, dispute(1, 2), TxError::UnknownTx);
    }

    #[test]
    fn dispute_before_its_deposit_appears_is_refused() {
        // File order is time order: the deposit doesn't exist yet when the dispute arrives.
        let mut engine = Engine::new();
        assert_refused(&mut engine, dispute(1, 1), TxError::UnknownTx);
        engine.apply(deposit(1, 1, "4")).unwrap();
        assert_account(&engine, 1, "4", "0", "4", false);
    }

    #[test]
    fn dispute_naming_another_client_is_refused() {
        let mut engine = engine_with(&[deposit(1, 1, "4"), deposit(2, 2, "1")]);
        for t in [dispute(2, 1), resolve(2, 1), chargeback(2, 1)] {
            assert_refused(&mut engine, t, TxError::ClientMismatch);
        }
        // Also when the other client has no account: none is created.
        assert_refused(&mut engine, dispute(3, 1), TxError::ClientMismatch);
        assert!(engine.account(ClientId(3)).is_none());
    }

    #[test]
    fn dispute_affects_only_the_referenced_deposit() {
        let engine = engine_with(&[
            deposit(1, 1, "1"),
            deposit(1, 2, "10"),
            dispute(1, 2),
            chargeback(1, 2),
        ]);
        assert_account(&engine, 1, "1", "0", "1", true);
    }

    // ---- locked accounts -----------------------------------------------

    /// Client 1 with deposits 1 (5.0) and 2 (3.0), locked by a chargeback of deposit 3 (1.0).
    fn locked_engine() -> Engine {
        engine_with(&[
            deposit(1, 1, "5"),
            deposit(1, 2, "3"),
            deposit(1, 3, "1"),
            dispute(1, 3),
            chargeback(1, 3),
        ])
    }

    #[test]
    fn locked_account_refuses_deposits_and_withdrawals() {
        let mut engine = locked_engine();
        assert_account(&engine, 1, "8", "0", "8", true);
        assert_refused(&mut engine, deposit(1, 10, "1"), TxError::AccountLocked);
        assert_refused(&mut engine, withdrawal(1, 11, "1"), TxError::AccountLocked);
    }

    #[test]
    fn locked_account_still_processes_disputes_resolves_and_chargebacks() {
        let mut engine = locked_engine();
        engine.apply(dispute(1, 1)).unwrap();
        engine.apply(dispute(1, 2)).unwrap();
        assert_account(&engine, 1, "0", "8", "8", true);
        engine.apply(resolve(1, 1)).unwrap();
        engine.apply(chargeback(1, 2)).unwrap();
        assert_account(&engine, 1, "5", "0", "5", true);
    }

    #[test]
    fn deposit_refused_on_a_locked_account_cannot_be_disputed() {
        let mut engine = locked_engine();
        assert_refused(&mut engine, deposit(1, 10, "1"), TxError::AccountLocked);
        assert_refused(&mut engine, dispute(1, 10), TxError::UnknownTx);
        // The id was never used, so another client can still deposit with it.
        engine.apply(deposit(2, 10, "1")).unwrap();
        assert_account(&engine, 2, "1", "0", "1", false);
    }
}
