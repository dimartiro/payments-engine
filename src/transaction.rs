//! Transactions as the engine sees them: already parsed and validated.
//!
//! Nothing in here knows about CSV or text. The I/O layer turns rows into
//! these types, so the engine can only ever receive a well-formed transaction.

use crate::amount::Amount;

/// A client, identified by a `u16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientId(pub u16);

/// A transaction, identified by a globally unique `u32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TxId(pub u32);

/// One transaction from the input, in file (chronological) order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transaction {
    /// The client whose account this transaction targets.
    pub client: ClientId,
    /// For deposits and withdrawals, this transaction's own id; for disputes,
    /// resolves and chargebacks, the id of the deposit they refer to.
    pub tx: TxId,
    pub kind: TransactionKind,
}

/// What a transaction does. Only deposits and withdrawals carry an amount;
/// the other kinds act on the amount of the transaction they refer to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionKind {
    /// Credit the account: available and total increase.
    Deposit(Amount),
    /// Debit the account: available and total decrease, if enough is available.
    Withdrawal(Amount),
    /// Hold the referenced deposit's funds while the client's claim is open.
    Dispute,
    /// Close a dispute without reversing the deposit: release the held funds.
    Resolve,
    /// Close a dispute by reversing the deposit: remove the held funds and lock the account.
    Chargeback,
}
