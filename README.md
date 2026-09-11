# payments-engine

A small, streaming toy payments engine. It reads a CSV of transactions,
maintains per-client account balances (handling deposits, withdrawals, disputes,
resolutions, and chargebacks), and writes the final account states as CSV.

## Usage

```bash
cargo run -- transactions.csv > accounts.csv
```

The input CSV path is the first and only positional argument; results are written
to stdout. Diagnostics (usage, I/O, or CSV-stream errors) are written to stderr,
so redirecting stdout yields a clean accounts CSV.

## Input / output

Input columns: `type, client, tx, amount`.

- `type` — one of `deposit`, `withdrawal`, `dispute`, `resolve`, `chargeback`.
- `client` — `u16` client id.
- `tx` — `u32`, globally unique transaction id.
- `amount` — decimal with up to 4 places past the decimal. Present for
  deposits/withdrawals, absent for disputes/resolves/chargebacks.

Leading/trailing whitespace around any field is tolerated.

Output columns: `client, available, held, total, locked`, where
`total = available + held` and `locked` is `true` if a chargeback has occurred.

Precision is fixed at 4 decimal places and enforced **on ingest**.

## Design

The crate is intentionally split so the correctness-critical logic is decoupled
from all I/O:

- `model.rs` — domain types (`Transaction`, `TxType`, id/amount aliases).
- `account.rs` — the per-client `Account` and the serializable `AccountRecord`.
- `engine.rs` — `PaymentsEngine`, a pure in-memory processor fed one parsed
  transaction at a time. It holds no file handles or sockets, so the same engine
  can be driven by the CLI (one file) or, in a server, by one instance per
  client stream.
- `error.rs` — a recoverable, per-row `ProcessingError` and a fatal top-level
  `AppError`.
- `lib.rs` — the streaming glue (`run`) between a reader, the engine, and a
  writer.
- `main.rs` — the CLI entry point.

## Assumptions

1. **Only deposits are disputable.** A dispute/resolve/chargeback references a
   `tx` id; we retain deposits (with their amount and owner) so they can be
   looked up. Disputing a withdrawal is ambiguous (a withdrawal removes funds,
   so "holding" it has no clear meaning), so a dispute against a non-deposit `tx`
   is treated as an unknown-transaction partner error and ignored.
2. **A dispute may drive `available` negative.** This is the core fraud scenario
   the challenge describes (deposit funds, withdraw, then reverse the deposit).
   We hold the full disputed amount regardless of the current balance, so
   `available` can go negative; total is preserved.
3. **A locked (charged-back) account is fully frozen.** Once a chargeback locks
   an account, *every* subsequent transaction for that client — including further
   disputes/resolves — is rejected. Freezing is treated as terminal.
4. **A dispute must reference its own client's transaction.** If the `client` on
   a dispute/resolve/chargeback doesn't match the client who owns the referenced
   `tx`, the row is ignored (partner error).
5. **Duplicate `tx` ids are rejected.** 
6. **Negative or missing amounts** on deposits/withdrawals are rejected.
7. **Malformed rows are skipped, not fatal.** A row that fails to parse is
   dropped and processing continues, matching the spec's "assume this is a
   partner error" guidance. Only environment-level failures (missing argument,
   unreadable file, broken CSV stream) abort the run.

## Efficiency

- Input is streamed: the `csv` reader yields records lazily and each row is
  processed as it arrives (a single reused `StringRecord` buffer, no
  `Vec<Transaction>`). Memory is bounded by the number of distinct clients and
  retained deposits, not by input size, so the program handles a small sample or
  an effectively unbounded stream the same way.
- Both input and output are buffered (`BufReader` / `BufWriter`).

## Testing

- **Unit tests** (in `engine.rs`) cover every transaction type and error path:
  deposit/withdrawal accounting, insufficient funds, dispute/resolve/chargeback
  flows, re-dispute after resolve, unknown/duplicate/wrong-client `tx`, ineligible
  state transitions, and locked-account rejection.
- **Integration test** (`tests/integration.rs`) drives the full CSV → engine →
  CSV pipeline against the sample data in `tests/data/`, verifying the public
  behavior end to end (including whitespace tolerance and precision).
- `sample_transactions.csv` is included at the repo root as runnable sample data.

Run everything with:

```bash
cargo test
```
