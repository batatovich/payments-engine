//! A streaming toy payments engine.
//!
//! Reads a CSV of transactions, updates client accounts (handling deposits,
//! withdrawals, disputes, resolutions and chargebacks), and writes the final
//! account states as CSV.
//!
//! The crate is split so the correctness-critical [`engine::PaymentsEngine`] is
//! fully decoupled from I/O and independently testable. [`run`] provides the
//! streaming glue between a reader, the engine, and a writer.

pub mod account;
pub mod engine;
pub mod error;
pub mod model;

use std::io::{Read, Write};

use csv::{ReaderBuilder, Trim, WriterBuilder};

use crate::account::AccountRecord;
use crate::engine::PaymentsEngine;
use crate::error::AppError;
use crate::model::Transaction;

/// Process a CSV stream of transactions from `input` and write the resulting
/// account states as CSV to `output`.
///
/// Input rows are deserialized and processed **one at a time** (the `csv`
/// reader yields records lazily), so memory use is bounded by the number of
/// distinct clients and disputable transactions rather than the size of the
/// input. This is what allows the same code to handle a small sample file or a
/// never-ending network stream.
pub fn run<R: Read, W: Write>(input: R, output: W) -> Result<(), AppError> {
    let mut reader = ReaderBuilder::new()
        // Tolerate the spaces in `deposit, 1, 1, 1.0` and around headers.
        .trim(Trim::All)
        // Amount is absent for dispute/resolve/chargeback rows.
        .flexible(true)
        .has_headers(true)
        .from_reader(input);

    let mut engine = PaymentsEngine::new();

    let mut raw = csv::StringRecord::new();
    let headers = reader.headers()?.clone();
    while reader.read_record(&mut raw)? {
        // A row that fails to deserialize is treated as malformed and skipped,
        // consistent with the spec's "assume this is a partner error" guidance.
        match raw.deserialize::<Transaction>(Some(&headers)) {
            Ok(transaction) => {
                // A recoverable ProcessingError means "skip this row and continue".
                let _ = engine.process_transaction(&transaction);
            }
            Err(_) => continue,
        }
    }

    write_accounts(&engine, output)
}

/// Serialize the engine's final account states to `output`.
fn write_accounts<W: Write>(engine: &PaymentsEngine, output: W) -> Result<(), AppError> {
    let mut writer = WriterBuilder::new().has_headers(true).from_writer(output);
    for (client, account) in engine.accounts() {
        writer.serialize(AccountRecord::from_account(client, account))?;
    }
    writer.flush()?;
    Ok(())
}
