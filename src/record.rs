//! Serde (de)serialization structs for the CSV boundary.
//!
//! These types exist only at the I/O edge. The engine works with the already
//! parsed [`InputRecord`] and produces [`AccountRecord`]s for output; it never
//! touches `csv` or `serde` directly, which keeps the core logic I/O-agnostic.

use crate::account::Account;
use crate::model::{Amount, ClientId, DECIMAL_PLACES, TxId, TxType};

/// One row of the input CSV: `transaction type, client, transaction id, amount`.
///
/// `amount` is optional because dispute/resolve/chargeback rows reference a
/// transaction by ID and carry no amount column value.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TransactionRecord {
    pub tx_type: TxType,
    pub tx_id: TxId,
    pub client: ClientId,
    /// Parsed from a string so trailing whitespace / empty fields are handled
    /// gracefully; `None` for transactions that omit the amount.
    #[serde(default)]
    pub amount: Option<Amount>,
}

/// One row of the output CSV: `client, available, held, total, locked`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AccountRecord {
    pub client: ClientId,
    pub available: Amount,
    pub held: Amount,
    pub total: Amount,
    pub locked: bool,
}

impl AccountRecord {
    /// Build an output record from a client's account, scaling every monetary
    /// value to the required output precision.
    pub fn from_account(client: ClientId, account: &Account) -> Self {
        AccountRecord {
            client,
            available: account.available.round_dp(DECIMAL_PLACES),
            held: account.held.round_dp(DECIMAL_PLACES),
            total: account.total().round_dp(DECIMAL_PLACES),
            locked: account.locked,
        }
    }
}
