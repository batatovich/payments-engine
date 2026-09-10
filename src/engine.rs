//! The payments engine: the correctness-critical core of the program.
//!
//! The engine is deliberately decoupled from all I/O. It is fed already parsed
//! [`Transaction`]s one at a time via [`PaymentsEngine::apply`], mutates its
//! in-memory state, and can later be drained for output. Because it holds no
//! file handles, sockets, or writers, the exact same engine can be driven by
//! the CLI (one CSV file) or, in a server, by one instance per client stream.
//!
//! ## Ordering
//! Transactions are applied strictly in the order they are handed to `apply`.
//! The spec guarantees the input file is chronological, and disputes/resolves/
//! chargebacks reference earlier transactions, so order must be preserved.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use crate::account::Account;
use crate::error::{TxError, TxErrorKind};
use crate::model::{Amount, ClientId, Transaction, TxId, TxType};

/// Lifecycle of a disputable transaction (a deposit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DepositState {
    /// Confirmed and not currently disputed.
    Confirmed,
    /// Currently under dispute; its amount is held.
    Disputed,
    /// Charged back; terminal state, cannot be disputed again.
    ChargedBack,
}

/// A stored deposit, retained so later disputes can look up its amount and
/// owning client. See the module/README notes on why only deposits are kept.
#[derive(Debug, Clone)]
struct Deposit {
    client: ClientId,
    amount: Amount,
    state: DepositState,
}

/// The in-memory transaction processor.
#[derive(Debug, Default)]
pub struct PaymentsEngine {
    /// One account per client. Created lazily on first reference.
    accounts: HashMap<ClientId, Account>,
    /// Disputable transactions (deposits) keyed by their globally unique `tx`.
    deposits: HashMap<TxId, Deposit>,
}

impl PaymentsEngine {
    /// Create an empty engine.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a single transaction, mutating account state.
    ///
    /// Returns `Ok(())` on success. A returned [`TxError`] is recoverable: the
    /// offending row can be logged and skipped, and processing continues with
    /// the next one.
    pub fn apply(&mut self, transaction: &Transaction) -> Result<(), TxError> {
        match transaction.tx_type {
            TxType::Deposit => self.deposit(transaction),
            TxType::Withdrawal => self.withdrawal(transaction),
            TxType::Dispute => self.dispute(transaction),
            TxType::Resolve => self.resolve(transaction),
            TxType::Chargeback => self.chargeback(transaction),
        }
    }

    /// Iterate over `(client, account)` pairs for output. Order is unspecified,
    /// which the spec explicitly allows.
    pub fn accounts(&self) -> impl Iterator<Item = (ClientId, &Account)> {
        self.accounts
            .iter()
            .map(|(client_id, account)| (*client_id, account))
    }

    // --- Handlers ---------------------------------------------------------

    fn deposit(&mut self, transaction: &Transaction) -> Result<(), TxError> {
        let amount = self.require_amount(transaction)?;
        self.reject_if_locked(transaction.client, transaction.tx_id)?;

        // Record the deposit first so a duplicate tx id cannot silently
        // overwrite an existing disputable transaction.
        match self.deposits.entry(transaction.tx_id) {
            Entry::Occupied(_) => {
                return Err(TxError::new(transaction.tx_id, TxErrorKind::DuplicateTx));
            }
            Entry::Vacant(slot) => {
                slot.insert(Deposit {
                    client: transaction.client,
                    amount,
                    state: DepositState::Confirmed,
                });
            }
        }

        let account = self.accounts.entry(transaction.client).or_default();
        account.available += amount;
        Ok(())
    }

    fn withdrawal(&mut self, transaction: &Transaction) -> Result<(), TxError> {
        let amount = self.require_amount(transaction)?;
        self.reject_if_locked(transaction.client, transaction.tx_id)?;

        let account = self.accounts.entry(transaction.client).or_default();
        if account.available < amount {
            return Err(TxError::new(
                transaction.tx_id,
                TxErrorKind::InsufficientFunds,
            ));
        }
        account.available -= amount;
        Ok(())
    }

    fn dispute(&mut self, transaction: &Transaction) -> Result<(), TxError> {
        self.reject_if_locked(transaction.client, transaction.tx_id)?;
        let deposit = self.disputable_deposit(transaction, DepositState::Confirmed)?;
        let amount = deposit.amount;

        let account = self.accounts.entry(transaction.client).or_default();
        account.available -= amount;
        account.held += amount;
        self.deposits.get_mut(&transaction.tx_id).unwrap().state = DepositState::Disputed;
        Ok(())
    }

    fn resolve(&mut self, transaction: &Transaction) -> Result<(), TxError> {
        self.reject_if_locked(transaction.client, transaction.tx_id)?;
        let deposit = self.disputable_deposit(transaction, DepositState::Disputed)?;
        let amount = deposit.amount;

        let account = self.accounts.entry(transaction.client).or_default();
        account.held -= amount;
        account.available += amount;
        self.deposits.get_mut(&transaction.tx_id).unwrap().state = DepositState::Confirmed;
        Ok(())
    }

    fn chargeback(&mut self, transaction: &Transaction) -> Result<(), TxError> {
        self.reject_if_locked(transaction.client, transaction.tx_id)?;
        let deposit = self.disputable_deposit(transaction, DepositState::Disputed)?;
        let amount = deposit.amount;

        let account = self.accounts.entry(transaction.client).or_default();
        account.held -= amount;
        account.locked = true;
        self.deposits.get_mut(&transaction.tx_id).unwrap().state = DepositState::ChargedBack;
        Ok(())
    }

    // --- Helpers ----------------------------------------------------------

    /// Extract the amount from a transaction that requires one, rejecting
    /// missing or negative values.
    fn require_amount(&self, transaction: &Transaction) -> Result<Amount, TxError> {
        let amount = transaction
            .amount
            .ok_or_else(|| TxError::new(transaction.tx_id, TxErrorKind::MissingAmount))?;
        if amount.is_sign_negative() {
            return Err(TxError::new(transaction.tx_id, TxErrorKind::NegativeAmount));
        }
        Ok(amount)
    }

    /// Validate that a referenced deposit exists, belongs to the requesting
    /// client, and is in the state required by the operation. Returns a clone
    /// of the stored deposit so callers can read its amount without holding a
    /// borrow on `self.deposits`.
    fn disputable_deposit(
        &self,
        transaction: &Transaction,
        required: DepositState,
    ) -> Result<Deposit, TxError> {
        let deposit = self
            .deposits
            .get(&transaction.tx_id)
            .ok_or_else(|| TxError::new(transaction.tx_id, TxErrorKind::UnknownTx))?;

        if deposit.client != transaction.client {
            return Err(TxError::new(transaction.tx_id, TxErrorKind::ClientMismatch));
        }
        if deposit.state != required {
            return Err(TxError::new(
                transaction.tx_id,
                TxErrorKind::IneligibleState,
            ));
        }
        Ok(deposit.clone())
    }

    /// Reject any operation on a frozen account. A chargeback locks the account
    /// permanently; we treat a locked account as fully frozen.
    fn reject_if_locked(&self, client: ClientId, tx_id: TxId) -> Result<(), TxError> {
        match self.accounts.get(&client) {
            Some(account) if account.locked => Err(TxError::new(tx_id, TxErrorKind::AccountLocked)),
            _ => Ok(()),
        }
    }
}
