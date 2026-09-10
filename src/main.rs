//! CLI entry point.
//!
//! Usage:
//!
//! ```text
//! cargo run -- transactions.csv > accounts.csv
//! ```
//!
//! The input CSV path is the first and only positional argument; the resulting
//! account states are written to stdout.

use std::fs::File;
use std::io::{self, BufReader, BufWriter};
use std::process::ExitCode;

use payments_engine::error::AppError;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), AppError> {
    let path = std::env::args_os().nth(1).ok_or(AppError::Usage)?;

    // Buffer both ends: many small rows in, many small rows out.
    let input = BufReader::new(File::open(path)?);
    let stdout = io::stdout();
    let output = BufWriter::new(stdout.lock());

    payments_engine::run(input, output)
}
