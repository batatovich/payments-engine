//! Core domain types shared across the engine.

use rust_decimal::Decimal;

/// Clients are represented by `u16` integers.
pub type ClientId = u16;

/// Transaction IDs are globally unique, valid `u32` values.
pub type TxId = u32;

/// A monetary amount. We use a fixed-point decimal to avoid rounding errors.
pub type Amount = Decimal;

/// The five kinds of transactions the engine can handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TxType {
    /// Credit to the client's account (increases available + total).
    Deposit,
    /// Debit from the client's account (decreases available + total).
    Withdrawal,
    /// Claim that a referenced deposit was erroneous; holds the funds.
    Dispute,
    /// Resolution of a dispute; releases the held funds back to available.
    Resolve,
    /// Final reversal of a disputed deposit; withdraws held funds and locks
    /// the account.
    Chargeback,
}

/// The number of decimal places all amounts are rounded/scaled to.
pub const DECIMAL_PLACES: u32 = 4;

/// Convenience constructor for the zero amount.
#[inline]
pub fn zero() -> Decimal {
    Decimal::ZERO
}

/// A transaction record: `transaction type, client, transaction id, amount`.
///
/// `amount` is optional because dispute/resolve/chargeback rows reference a
/// transaction by ID and carry no amount column value.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Transaction {
    #[serde(rename = "type")]
    pub tx_type: TxType,
    pub client: ClientId,
    #[serde(rename = "tx")]
    pub tx_id: TxId,
    /// Parsed from a string so trailing whitespace / empty fields are handled
    /// gracefully; `None` for transactions that omit the amount.
    #[serde(default)]
    pub amount: Option<Amount>,
}

impl Transaction {
    pub fn new(tx_type: TxType, client: ClientId, tx_id: TxId, amount: Option<Amount>) -> Self {
        Self {
            tx_type,
            client,
            tx_id,
            amount,
        }
    }
}
