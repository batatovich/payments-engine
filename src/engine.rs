//! The payments engine.
//!
//! The engine is deliberately decoupled from all I/O. It is fed already parsed
//! [`Transaction`]s one at a time via [`PaymentsEngine::process_transaction`], mutates its
//! in-memory state, and can later be drained for output. Because it holds no
//! file handles, sockets, or writers, the exact same engine can be driven by
//! the CLI (one CSV file) or, in a server, by one instance per client stream.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use crate::account::Account;
use crate::error::{ProcessingError, ProcessingErrorKind};
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

    /// Process a single transaction, mutating account state.
    ///
    /// Returns `Ok(())` on success. A returned [`ProcessingError`] is recoverable: the
    /// offending row can be logged and skipped, and processing continues with
    /// the next one.
    pub fn process_transaction(
        &mut self,
        transaction: &Transaction,
    ) -> Result<(), ProcessingError> {
        // A locked account is frozen: no transaction type may
        // touch it, so reject up front before dispatching to a handler.
        if matches!(self.accounts.get(&transaction.client), Some(account) if account.locked) {
            return Err(ProcessingError::new(
                transaction.tx_id,
                ProcessingErrorKind::AccountLocked,
            ));
        }

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

    fn deposit(&mut self, transaction: &Transaction) -> Result<(), ProcessingError> {
        let amount = self.require_amount(transaction)?;

        // Record the deposit first so a duplicate tx id cannot silently
        // overwrite an existing disputable transaction.
        match self.deposits.entry(transaction.tx_id) {
            Entry::Occupied(_) => {
                return Err(ProcessingError::new(
                    transaction.tx_id,
                    ProcessingErrorKind::DuplicateTx,
                ));
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

    fn withdrawal(&mut self, transaction: &Transaction) -> Result<(), ProcessingError> {
        let amount = self.require_amount(transaction)?;

        let account = self.accounts.entry(transaction.client).or_default();
        if account.available < amount {
            return Err(ProcessingError::new(
                transaction.tx_id,
                ProcessingErrorKind::InsufficientFunds,
            ));
        }
        account.available -= amount;
        Ok(())
    }

    fn dispute(&mut self, transaction: &Transaction) -> Result<(), ProcessingError> {
        let deposit = self.disputable_deposit(transaction, DepositState::Confirmed)?;
        let amount = deposit.amount;

        let account = self.accounts.entry(transaction.client).or_default();
        account.available -= amount;
        account.held += amount;
        self.deposits.get_mut(&transaction.tx_id).unwrap().state = DepositState::Disputed;
        Ok(())
    }

    fn resolve(&mut self, transaction: &Transaction) -> Result<(), ProcessingError> {
        let deposit = self.disputable_deposit(transaction, DepositState::Disputed)?;
        let amount = deposit.amount;

        let account = self.accounts.entry(transaction.client).or_default();
        account.held -= amount;
        account.available += amount;
        self.deposits.get_mut(&transaction.tx_id).unwrap().state = DepositState::Confirmed;
        Ok(())
    }

    fn chargeback(&mut self, transaction: &Transaction) -> Result<(), ProcessingError> {
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
    fn require_amount(&self, transaction: &Transaction) -> Result<Amount, ProcessingError> {
        let amount = transaction.amount.ok_or_else(|| {
            ProcessingError::new(transaction.tx_id, ProcessingErrorKind::MissingAmount)
        })?;
        if amount.is_sign_negative() {
            return Err(ProcessingError::new(
                transaction.tx_id,
                ProcessingErrorKind::NegativeAmount,
            ));
        }
        Ok(amount)
    }

    /// Validate that a referenced deposit exists, belongs to the requesting
    /// client, and is in the state required by the operation.
    fn disputable_deposit(
        &self,
        transaction: &Transaction,
        required: DepositState,
    ) -> Result<Deposit, ProcessingError> {
        let deposit = self.deposits.get(&transaction.tx_id).ok_or_else(|| {
            ProcessingError::new(transaction.tx_id, ProcessingErrorKind::UnknownTx)
        })?;

        if deposit.client != transaction.client {
            return Err(ProcessingError::new(
                transaction.tx_id,
                ProcessingErrorKind::ClientMismatch,
            ));
        }
        if deposit.state != required {
            return Err(ProcessingError::new(
                transaction.tx_id,
                ProcessingErrorKind::IneligibleState,
            ));
        }
        Ok(deposit.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Test helpers -----------------------------------------------------

    /// Parse a decimal amount from a string literal (panics on bad input).
    fn amt(s: &str) -> Amount {
        s.parse().expect("valid decimal literal in test")
    }

    /// Assert an `(available, held, locked)` snapshot for a client.
    fn assert_balances(
        engine: &PaymentsEngine,
        client: ClientId,
        available: &str,
        held: &str,
        locked: bool,
    ) {
        let account = engine
            .accounts
            .get(&client)
            .expect("account should exist for client");
        assert_eq!(
            account.available,
            amt(available),
            "available for client {client}"
        );
        assert_eq!(account.held, amt(held), "held for client {client}");
        assert_eq!(
            account.total(),
            amt(available) + amt(held),
            "total invariant"
        );
        assert_eq!(account.locked, locked, "locked for client {client}");
    }

    /// Process a transaction, asserting it is successfully processed.
    fn process_valid_transaction(engine: &mut PaymentsEngine, transaction: Transaction) {
        engine
            .process_transaction(&transaction)
            .expect("transaction should succeed");
    }

    /// Process a transaction, asserting it is rejected with the given error.
    fn process_invalid_transaction(
        engine: &mut PaymentsEngine,
        transaction: Transaction,
        tx_id: TxId,
        kind: ProcessingErrorKind,
    ) {
        let e = engine
            .process_transaction(&transaction)
            .expect_err("transaction should be rejected");
        assert_eq!(e, ProcessingError::new(tx_id, kind));
    }

    // --- Happy-path scenarios --------------------------------------------

    #[test]
    fn deposit_credits_available_and_total() {
        let mut engine = PaymentsEngine::new();
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, Some(amt("1.0"))),
        );
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 2, Some(amt("2.5"))),
        );
        assert_balances(&engine, 1, "3.5", "0", false);
    }

    #[test]
    fn withdrawal_debits_available_and_total() {
        let mut engine = PaymentsEngine::new();
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, Some(amt("5.0"))),
        );
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Withdrawal, 1, 2, Some(amt("2.0"))),
        );
        assert_balances(&engine, 1, "3.0", "0", false);
    }

    #[test]
    fn clients_are_isolated() {
        let mut engine = PaymentsEngine::new();
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, Some(amt("1.0"))),
        );
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 2, 2, Some(amt("2.0"))),
        );
        assert_balances(&engine, 1, "1.0", "0", false);
        assert_balances(&engine, 2, "2.0", "0", false);
    }

    #[test]
    fn dispute_holds_funds() {
        let mut engine = PaymentsEngine::new();
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, Some(amt("3.0"))),
        );
        process_valid_transaction(&mut engine, Transaction::new(TxType::Dispute, 1, 1, None));
        // Available drops, held rises, total unchanged.
        assert_balances(&engine, 1, "0", "3.0", false);
    }

    #[test]
    fn resolve_releases_held_funds() {
        let mut engine = PaymentsEngine::new();
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, Some(amt("3.0"))),
        );
        process_valid_transaction(&mut engine, Transaction::new(TxType::Dispute, 1, 1, None));
        process_valid_transaction(&mut engine, Transaction::new(TxType::Resolve, 1, 1, None));
        assert_balances(&engine, 1, "3.0", "0", false);
    }

    #[test]
    fn chargeback_withdraws_held_and_locks() {
        let mut engine = PaymentsEngine::new();
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, Some(amt("3.0"))),
        );
        process_valid_transaction(&mut engine, Transaction::new(TxType::Dispute, 1, 1, None));
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Chargeback, 1, 1, None),
        );
        assert_balances(&engine, 1, "0", "0", true);
    }

    #[test]
    fn dispute_can_be_reopened_after_resolve() {
        let mut engine = PaymentsEngine::new();
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, Some(amt("4.0"))),
        );
        process_valid_transaction(&mut engine, Transaction::new(TxType::Dispute, 1, 1, None));
        process_valid_transaction(&mut engine, Transaction::new(TxType::Resolve, 1, 1, None));
        // A resolved deposit returns to Confirmed and may be disputed again.
        process_valid_transaction(&mut engine, Transaction::new(TxType::Dispute, 1, 1, None));
        assert_balances(&engine, 1, "0", "4.0", false);
    }

    // --- ProcessingError flows ----------------------------------------------------

    #[test]
    fn deposit_missing_amount_is_rejected() {
        let mut engine = PaymentsEngine::new();
        process_invalid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, None),
            1,
            ProcessingErrorKind::MissingAmount,
        );
        assert!(engine.accounts.get(&1).is_none());
    }

    #[test]
    fn deposit_negative_amount_is_rejected() {
        let mut engine = PaymentsEngine::new();
        process_invalid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, Some(amt("-1.0"))),
            1,
            ProcessingErrorKind::NegativeAmount,
        );
    }

    #[test]
    fn duplicate_tx_id_is_rejected() {
        let mut engine = PaymentsEngine::new();
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, Some(amt("1.0"))),
        );
        process_invalid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, Some(amt("2.0"))),
            1,
            ProcessingErrorKind::DuplicateTx,
        );
        // The duplicate must not have altered the balance.
        assert_balances(&engine, 1, "1.0", "0", false);
    }

    #[test]
    fn withdrawal_insufficient_funds_is_rejected() {
        let mut engine = PaymentsEngine::new();
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, Some(amt("1.0"))),
        );
        process_invalid_transaction(
            &mut engine,
            Transaction::new(TxType::Withdrawal, 1, 2, Some(amt("5.0"))),
            2,
            ProcessingErrorKind::InsufficientFunds,
        );
        // Balance is unchanged after a failed withdrawal.
        assert_balances(&engine, 1, "1.0", "0", false);
    }

    #[test]
    fn withdrawal_missing_amount_is_rejected() {
        let mut engine = PaymentsEngine::new();
        process_invalid_transaction(
            &mut engine,
            Transaction::new(TxType::Withdrawal, 1, 1, None),
            1,
            ProcessingErrorKind::MissingAmount,
        );
    }

    #[test]
    fn dispute_unknown_tx_is_rejected() {
        let mut engine = PaymentsEngine::new();
        process_invalid_transaction(
            &mut engine,
            Transaction::new(TxType::Dispute, 1, 99, None),
            99,
            ProcessingErrorKind::UnknownTx,
        );
    }

    #[test]
    fn dispute_wrong_client_is_rejected() {
        let mut engine = PaymentsEngine::new();
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, Some(amt("1.0"))),
        );
        // Client 2 tries to dispute client 1's deposit.
        process_invalid_transaction(
            &mut engine,
            Transaction::new(TxType::Dispute, 2, 1, None),
            1,
            ProcessingErrorKind::ClientMismatch,
        );
        assert_balances(&engine, 1, "1.0", "0", false);
    }

    #[test]
    fn dispute_already_disputed_is_ineligible() {
        let mut engine = PaymentsEngine::new();
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, Some(amt("1.0"))),
        );
        process_valid_transaction(&mut engine, Transaction::new(TxType::Dispute, 1, 1, None));
        process_invalid_transaction(
            &mut engine,
            Transaction::new(TxType::Dispute, 1, 1, None),
            1,
            ProcessingErrorKind::IneligibleState,
        );
    }

    #[test]
    fn resolve_undisputed_is_ineligible() {
        let mut engine = PaymentsEngine::new();
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, Some(amt("1.0"))),
        );
        process_invalid_transaction(
            &mut engine,
            Transaction::new(TxType::Resolve, 1, 1, None),
            1,
            ProcessingErrorKind::IneligibleState,
        );
    }

    #[test]
    fn chargeback_undisputed_is_ineligible() {
        let mut engine = PaymentsEngine::new();
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, Some(amt("1.0"))),
        );
        process_invalid_transaction(
            &mut engine,
            Transaction::new(TxType::Chargeback, 1, 1, None),
            1,
            ProcessingErrorKind::IneligibleState,
        );
    }

    #[test]
    fn cannot_dispute_after_chargeback() {
        let mut engine = PaymentsEngine::new();
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, Some(amt("1.0"))),
        );
        process_valid_transaction(&mut engine, Transaction::new(TxType::Dispute, 1, 1, None));
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Chargeback, 1, 1, None),
        );
        // A chargeback locks the account, so the lock check short-circuits
        // before the deposit's terminal `ChargedBack` state is ever inspected:
        // the caller sees `AccountLocked`, not `IneligibleState`.
        process_invalid_transaction(
            &mut engine,
            Transaction::new(TxType::Dispute, 1, 1, None),
            1,
            ProcessingErrorKind::AccountLocked,
        );
    }

    #[test]
    fn locked_account_rejects_further_transactions() {
        let mut engine = PaymentsEngine::new();
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 1, Some(amt("1.0"))),
        );
        process_valid_transaction(&mut engine, Transaction::new(TxType::Dispute, 1, 1, None));
        process_valid_transaction(
            &mut engine,
            Transaction::new(TxType::Chargeback, 1, 1, None),
        );

        // Every operation on a locked account is refused.
        process_invalid_transaction(
            &mut engine,
            Transaction::new(TxType::Deposit, 1, 2, Some(amt("5.0"))),
            2,
            ProcessingErrorKind::AccountLocked,
        );
        process_invalid_transaction(
            &mut engine,
            Transaction::new(TxType::Withdrawal, 1, 3, Some(amt("1.0"))),
            3,
            ProcessingErrorKind::AccountLocked,
        );
        assert_balances(&engine, 1, "0", "0", true);
    }
}
