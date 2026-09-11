//! The per-client account and its balance invariants.

use crate::model::{Amount, ClientId, DECIMAL_PLACES, zero};

/// A client's account.
///
/// Only `available` and `held` amounts are stored, while `total` is derived on demand. This
/// makes the invariant `total == available + held` true by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Funds available.
    pub available: Amount,
    /// Funds held pending dispute resolution.
    pub held: Amount,
    /// Whether the account is frozen. Set once a chargeback occurs.
    pub locked: bool,
}

impl Default for Account {
    fn default() -> Self {
        Account {
            available: zero(),
            held: zero(),
            locked: false,
        }
    }
}

impl Account {
    /// Total funds: available + held. Always derived, never stored.
    #[inline]
    pub fn total(&self) -> Amount {
        self.available + self.held
    }
}

/// Output account record: `client, available, held, total, locked`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AccountRecord {
    pub client: ClientId,
    pub available: Amount,
    pub held: Amount,
    pub total: Amount,
    pub locked: bool,
}

impl AccountRecord {
    /// Build an output record from a client's account.
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
