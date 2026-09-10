//! Errors produced while applying a single transaction.
//!
//! Every error is scoped to the offending transaction (`tx_id`) so callers can
//! log or report exactly which row was rejected. These errors are recoverable:
//! the engine skips the row and continues with the next one.

use crate::model::TxId;

/// An error raised while applying one transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxError {
    /// The transaction the error refers to.
    pub tx_id: TxId,
    /// What went wrong.
    pub kind: TxErrorKind,
}

/// The category of a [`TxError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxErrorKind {
    /// A deposit/withdrawal row carried no amount.
    MissingAmount,
    /// A deposit/withdrawal row carried a negative amount.
    NegativeAmount,
    /// The account is frozen after a chargeback.
    AccountLocked,
    /// A deposit reused an existing transaction id.
    DuplicateTx,
    /// A withdrawal exceeded the available balance.
    InsufficientFunds,
    /// A dispute/resolve/chargeback referenced an unknown transaction.
    UnknownTx,
    /// The referenced transaction belongs to a different client.
    ClientMismatch,
    /// The referenced transaction is not in the state the operation requires.
    IneligibleState,
}

impl TxError {
    /// Build an error for the given transaction and kind.
    pub fn new(tx_id: TxId, kind: TxErrorKind) -> Self {
        Self { tx_id, kind }
    }
}

impl std::fmt::Display for TxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self.kind {
            TxErrorKind::MissingAmount => "missing amount",
            TxErrorKind::NegativeAmount => "negative amount",
            TxErrorKind::AccountLocked => "account is locked",
            TxErrorKind::DuplicateTx => "duplicate transaction id",
            TxErrorKind::InsufficientFunds => "insufficient funds",
            TxErrorKind::UnknownTx => "unknown transaction",
            TxErrorKind::ClientMismatch => "transaction belongs to another client",
            TxErrorKind::IneligibleState => "transaction is in an ineligible state",
        };
        write!(f, "transaction {}: {}", self.tx_id, reason)
    }
}

impl std::error::Error for TxError {}
