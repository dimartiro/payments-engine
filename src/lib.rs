//! A streaming toy payments engine.
//!
//! Reads deposits, withdrawals, disputes, resolves and chargebacks from a CSV
//! file, applies them to client accounts, and writes the final account
//! balances as CSV.
//!
//! Reading order: [`transaction`] (what comes in), [`amount`] (how money is
//! validated), [`account`] (the balance rules), [`error`].

pub mod account;
pub mod amount;
pub mod error;
pub mod transaction;
