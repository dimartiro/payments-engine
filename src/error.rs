//! Errors produced while applying a transaction to the ledger.

use thiserror::Error;

/// Why a well-formed transaction was refused by the business rules.
///
/// A refused transaction is skipped and changes nothing; it is never fatal
/// for the run (the spec treats these as errors on the partner's side).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TxError {
    /// The account is frozen after a chargeback and refuses client-initiated movements.
    #[error("account is locked")]
    AccountLocked,
    /// A withdrawal asked for more than the available funds.
    #[error("insufficient available funds")]
    InsufficientFunds,
    /// Releasing or charging back more than is currently held.
    #[error("insufficient held funds")]
    InsufficientHeldFunds,
    /// A balance would leave the exactly representable range.
    #[error("balance overflow")]
    Overflow,
}
