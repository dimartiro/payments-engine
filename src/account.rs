//! A client's account and the balance rules for each kind of transaction.

use rust_decimal::Decimal;

use crate::amount::Amount;
use crate::error::TxError;

/// The balances of one client.
///
/// Fields are private. Every balance change goes through `apply_delta`, and
/// `locked` is only ever set (never cleared) by `charge_back`, after its
/// balance change succeeded. So these invariants hold everywhere:
/// - `total` is always `available + held`: it is computed, never stored;
/// - `held` is never negative;
/// - every balance stays within ±[`Account::MAX_BALANCE`], so all arithmetic is exact;
/// - a failed operation leaves the account exactly as it was.
///
/// `available` *can* be negative: disputing a deposit whose funds were already
/// withdrawn holds the full amount anyway (see [`Account::hold`]).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Account {
    available: Decimal,
    held: Decimal,
    locked: bool,
}

/// [`Account::MAX_BALANCE`] in whole units: 2^32 deposits of 1e15 each.
const MAX_BALANCE_UNITS: u128 = (1 << 32) * 1_000_000_000_000_000;

impl Account {
    /// Largest absolute balance an account may reach: 2^32 × [`Amount::MAX`],
    /// about 4.3e24.
    ///
    /// The engine can never reach it: it accepts at most 2^32 deposits (one
    /// per `u32` id), each at most `Amount::MAX`. The bound is for direct users
    /// of `Account`: past about 7.9e24, `Decimal` addition silently rounds
    /// away decimal places instead of failing, and this keeps every result exact.
    pub const MAX_BALANCE: Decimal = Decimal::from_parts(
        // The three 32-bit words of the 96-bit mantissa, low to high.
        MAX_BALANCE_UNITS as u32,
        (MAX_BALANCE_UNITS >> 32) as u32,
        (MAX_BALANCE_UNITS >> 64) as u32,
        false,
        0,
    );

    /// Funds the client can use or withdraw now: `total - held`.
    pub fn available(&self) -> Decimal {
        self.available
    }

    /// Funds held for open disputes.
    pub fn held(&self) -> Decimal {
        self.held
    }

    /// Available plus held funds.
    pub fn total(&self) -> Decimal {
        // Exact and cannot overflow: `apply_delta` only commits balances whose
        // sum is within `MAX_BALANCE`.
        self.available + self.held
    }

    /// Whether the account was frozen by a chargeback.
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Deposit: credits the account, so available and total grow by `amount`.
    pub fn deposit(&mut self, amount: Amount) -> Result<(), TxError> {
        self.ensure_unlocked()?;
        self.apply_delta(amount.value(), Decimal::ZERO)
    }

    /// Withdrawal: debits the account, so available and total shrink by
    /// `amount`. Only available funds can be withdrawn; asking for more fails
    /// and changes nothing.
    pub fn withdraw(&mut self, amount: Amount) -> Result<(), TxError> {
        self.ensure_unlocked()?;
        if self.available < amount.value() {
            return Err(TxError::InsufficientFunds);
        }
        self.apply_delta(-amount.value(), Decimal::ZERO)
    }

    /// Dispute: moves `amount` from available to held; the total is unchanged.
    ///
    /// Succeeds even if `available` goes negative. That is the fraud case the
    /// spec warns about (deposit, withdraw, then reverse the deposit); refusing
    /// it would let the client keep the money.
    pub fn hold(&mut self, amount: Amount) -> Result<(), TxError> {
        self.apply_delta(-amount.value(), amount.value())
    }

    /// Resolve: moves `amount` back from held to available; the total is unchanged.
    pub fn release(&mut self, amount: Amount) -> Result<(), TxError> {
        self.apply_delta(amount.value(), -amount.value())
    }

    /// Chargeback: removes `amount` from held (and so from the total), then
    /// freezes the account.
    pub fn charge_back(&mut self, amount: Amount) -> Result<(), TxError> {
        self.apply_delta(Decimal::ZERO, -amount.value())?;
        self.locked = true;
        Ok(())
    }

    /// Lock policy, in one place: a frozen account refuses movements the
    /// client starts (deposits and withdrawals) but still records the partner's
    /// dispute, resolve and chargeback, so a reversal is never silently dropped.
    fn ensure_unlocked(&self) -> Result<(), TxError> {
        if self.locked {
            Err(TxError::AccountLocked)
        } else {
            Ok(())
        }
    }

    /// The single place balances change.
    ///
    /// Computes every new value first and assigns only after all checks pass,
    /// so an error leaves the account untouched.
    ///
    /// Why the results are exact: committed balances are within `MAX_BALANCE`
    /// and a delta is at most `Amount::MAX`, so the new `available` and `held`
    /// are far below where `Decimal` starts rounding. Only `total` could round,
    /// and only when it is already beyond `MAX_BALANCE`, where it is refused.
    fn apply_delta(&mut self, d_available: Decimal, d_held: Decimal) -> Result<(), TxError> {
        let available = self
            .available
            .checked_add(d_available)
            .ok_or(TxError::Overflow)?;
        let held = self.held.checked_add(d_held).ok_or(TxError::Overflow)?;
        let total = available.checked_add(held).ok_or(TxError::Overflow)?;
        if [available, held, total]
            .iter()
            .any(|v| v.abs() > Self::MAX_BALANCE)
        {
            return Err(TxError::Overflow);
        }
        if held < Decimal::ZERO {
            return Err(TxError::InsufficientHeldFunds);
        }

        self.available = available;
        self.held = held;
        Ok(())
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]
mod tests {
    use super::*;

    fn amount(s: &str) -> Amount {
        s.parse().unwrap()
    }

    fn dec(s: &str) -> Decimal {
        Decimal::from_str_exact(s).unwrap()
    }

    /// An unlocked account holding `available` funds and nothing held.
    fn funded(available: &str) -> Account {
        let mut account = Account::default();
        account.deposit(amount(available)).unwrap();
        account
    }

    #[track_caller]
    fn assert_balances(account: &Account, available: &str, held: &str, total: &str) {
        assert_eq!(account.available(), dec(available), "available");
        assert_eq!(account.held(), dec(held), "held");
        assert_eq!(account.total(), dec(total), "total");
    }

    #[test]
    fn new_account_is_empty_and_unlocked() {
        let account = Account::default();
        assert_balances(&account, "0", "0", "0");
        assert!(!account.is_locked());
    }

    #[test]
    fn deposit_increases_available_and_total() {
        let mut account = funded("1.5");
        account.deposit(amount("2.25")).unwrap();
        assert_balances(&account, "3.75", "0", "3.75");
    }

    #[test]
    fn withdraw_decreases_available_and_total() {
        let mut account = funded("3");
        account.withdraw(amount("1.2")).unwrap();
        assert_balances(&account, "1.8", "0", "1.8");
    }

    #[test]
    fn withdraw_of_exactly_the_available_funds_succeeds() {
        let mut account = funded("2.5");
        account.withdraw(amount("2.5")).unwrap();
        assert_balances(&account, "0", "0", "0");
    }

    #[test]
    fn withdraw_of_more_than_available_fails_and_changes_nothing() {
        let mut account = funded("2.5");
        let before = account.clone();
        assert_eq!(
            account.withdraw(amount("2.5001")),
            Err(TxError::InsufficientFunds)
        );
        assert_eq!(account, before);
    }

    #[test]
    fn withdraw_from_an_empty_account_fails() {
        let mut account = Account::default();
        assert_eq!(
            account.withdraw(amount("1")),
            Err(TxError::InsufficientFunds)
        );
        assert_eq!(account, Account::default());
    }

    #[test]
    fn held_funds_cannot_be_withdrawn() {
        let mut account = funded("10");
        account.hold(amount("4")).unwrap();
        assert_eq!(
            account.withdraw(amount("7")),
            Err(TxError::InsufficientFunds)
        );
        account.withdraw(amount("6")).unwrap();
        assert_balances(&account, "0", "4", "4");
    }

    #[test]
    fn hold_moves_funds_from_available_to_held() {
        let mut account = funded("10");
        account.hold(amount("4")).unwrap();
        assert_balances(&account, "6", "4", "10");
    }

    #[test]
    fn hold_can_make_available_negative() {
        // Deposit 10, withdraw 8, then dispute the deposit: the full 10 is held.
        let mut account = funded("10");
        account.withdraw(amount("8")).unwrap();
        account.hold(amount("10")).unwrap();
        assert_balances(&account, "-8", "10", "2");
    }

    #[test]
    fn release_moves_funds_from_held_back_to_available() {
        let mut account = funded("10");
        account.hold(amount("4")).unwrap();
        account.release(amount("4")).unwrap();
        assert_balances(&account, "10", "0", "10");
    }

    #[test]
    fn release_of_more_than_held_fails_and_changes_nothing() {
        let mut account = funded("10");
        account.hold(amount("4")).unwrap();
        let before = account.clone();
        assert_eq!(
            account.release(amount("4.0001")),
            Err(TxError::InsufficientHeldFunds)
        );
        assert_eq!(account, before);
    }

    #[test]
    fn charge_back_removes_held_funds_and_locks() {
        let mut account = funded("10");
        account.hold(amount("4")).unwrap();
        account.charge_back(amount("4")).unwrap();
        assert_balances(&account, "6", "0", "6");
        assert!(account.is_locked());
    }

    #[test]
    fn charge_back_after_withdrawal_leaves_a_negative_total() {
        let mut account = funded("10");
        account.withdraw(amount("8")).unwrap();
        account.hold(amount("10")).unwrap();
        account.charge_back(amount("10")).unwrap();
        assert_balances(&account, "-8", "0", "-8");
        assert!(account.is_locked());
    }

    #[test]
    fn charge_back_of_more_than_held_fails_and_does_not_lock() {
        let mut account = funded("10");
        let before = account.clone();
        assert_eq!(
            account.charge_back(amount("1")),
            Err(TxError::InsufficientHeldFunds)
        );
        assert_eq!(account, before);
        assert!(!account.is_locked());
    }

    #[test]
    fn locked_account_refuses_deposits_and_withdrawals() {
        let mut account = funded("10");
        account.hold(amount("4")).unwrap();
        account.charge_back(amount("4")).unwrap();
        let before = account.clone();

        assert_eq!(account.deposit(amount("1")), Err(TxError::AccountLocked));
        // More than available: the lock is reported first.
        assert_eq!(account.withdraw(amount("100")), Err(TxError::AccountLocked));
        assert_eq!(account, before);
    }

    #[test]
    fn locked_account_still_processes_holds_releases_and_charge_backs() {
        let mut account = funded("10");
        account.hold(amount("1")).unwrap();
        account.charge_back(amount("1")).unwrap();

        account.hold(amount("3")).unwrap();
        assert_balances(&account, "6", "3", "9");
        account.release(amount("1")).unwrap();
        assert_balances(&account, "7", "2", "9");
        account.charge_back(amount("2")).unwrap();
        assert_balances(&account, "7", "0", "7");
        assert!(account.is_locked());
    }

    #[test]
    fn balances_returning_to_zero_never_print_as_negative_zero() {
        // From a positive balance down to zero.
        let mut account = Account::default();
        account.deposit(amount("1.5")).unwrap();
        account.hold(amount("1.5")).unwrap();
        account.release(amount("1.5")).unwrap();
        account.withdraw(amount("1.5")).unwrap();
        for value in [account.available(), account.held(), account.total()] {
            assert_eq!(format!("{value:.4}"), "0.0000");
        }

        // From a negative balance up to zero: dispute after withdrawal, then resolve.
        let mut account = funded("1.5");
        account.withdraw(amount("1.5")).unwrap();
        account.hold(amount("1.5")).unwrap();
        assert_balances(&account, "-1.5", "1.5", "0");
        account.release(amount("1.5")).unwrap();
        for value in [account.available(), account.held(), account.total()] {
            assert_eq!(format!("{value:.4}"), "0.0000");
        }
    }

    #[test]
    fn max_balance_is_the_most_the_engine_can_accumulate() {
        let max_deposits = Decimal::from(1_u64 << 32);
        assert_eq!(Account::MAX_BALANCE, Amount::MAX.value() * max_deposits);
    }

    #[test]
    fn balances_up_to_max_balance_are_exact() {
        let just_below = Account::MAX_BALANCE - dec("0.0001");
        let mut account = Account {
            available: just_below,
            ..Account::default()
        };
        account.deposit(amount("0.0001")).unwrap();
        assert_eq!(account.available(), Account::MAX_BALANCE);
    }

    #[test]
    fn exceeding_max_balance_is_an_overflow_and_changes_nothing() {
        // Positive side: one more deposit.
        let mut account = Account {
            available: Account::MAX_BALANCE,
            ..Account::default()
        };
        let before = account.clone();
        assert_eq!(account.deposit(amount("0.0001")), Err(TxError::Overflow));
        assert_eq!(account, before);

        // Negative side: a hold pushing available below -MAX_BALANCE.
        let mut account = Account {
            available: -Account::MAX_BALANCE,
            held: Account::MAX_BALANCE,
            locked: false,
        };
        let before = account.clone();
        assert_eq!(account.hold(amount("0.0001")), Err(TxError::Overflow));
        assert_eq!(account, before);
    }
}
