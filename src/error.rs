//! Errors produced while applying a transaction to the ledger.

use thiserror::Error;

/// Why a well-formed transaction was refused.
///
/// A refusal is never fatal: the transaction is skipped and no balances or
/// dispute records change (see [`Engine`](crate::engine::Engine) for the one
/// account a refusal may still create). The spec calls bad dispute, resolve
/// and chargeback references errors on the partner's side; the other variants
/// are ordinary business refusals, plus two invariant guards
/// (`InsufficientHeldFunds`, `Overflow`) that the engine never triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TxError {
    /// The account is frozen after a chargeback and refuses client-initiated movements.
    #[error("account is locked")]
    AccountLocked,
    /// A withdrawal asked for more than the available funds.
    #[error("insufficient available funds")]
    InsufficientFunds,
    /// Releasing or charging back more than is currently held.
    ///
    /// Guards `Account`'s "held is never negative" invariant for direct
    /// callers. The engine only releases or charges back an amount it held
    /// earlier, so seeing this from `Engine::apply` means a bug, not bad input.
    #[error("insufficient held funds")]
    InsufficientHeldFunds,
    /// A balance would exceed [`Account::MAX_BALANCE`](crate::account::Account::MAX_BALANCE),
    /// beyond which `Decimal` arithmetic would no longer be exact.
    ///
    /// Unreachable through the engine, which can't accumulate that much.
    #[error("balance overflow")]
    Overflow,
    /// A deposit reused the id of an earlier accepted deposit.
    #[error("transaction id already used by an earlier deposit")]
    DuplicateTx,
    /// A dispute, resolve or chargeback referred to no known deposit.
    #[error("no deposit with this transaction id")]
    UnknownTx,
    /// A dispute, resolve or chargeback named a different client than the deposit's.
    #[error("transaction belongs to a different client")]
    ClientMismatch,
    /// A dispute referred to a deposit that is already under dispute.
    #[error("transaction is already under dispute")]
    AlreadyDisputed,
    /// A resolve or chargeback referred to a deposit that is not under dispute.
    #[error("transaction is not under dispute")]
    NotDisputed,
    /// Any dispute, resolve or chargeback after the deposit was charged back.
    #[error("transaction was already charged back")]
    AlreadyChargedBack,
}

impl TxError {
    /// `true` for the two invariant guards the engine never triggers on its
    /// own: seeing one from [`Engine::apply`](crate::engine::Engine::apply)
    /// means a bug, not bad input.
    pub fn indicates_bug(self) -> bool {
        matches!(self, Self::InsufficientHeldFunds | Self::Overflow)
    }
}
