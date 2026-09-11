//! End-to-end tests driving the full CSV -> engine -> CSV pipeline via the
//! public [`payments_engine::run`] entry point.

use std::collections::BTreeMap;

/// Run the engine over `input` and return the output accounts keyed by client,
/// each value being the `(available, held, total, locked)` columns as strings.
fn run(input: &str) -> BTreeMap<String, [String; 4]> {
    let mut output = Vec::new();
    payments_engine::run(input.as_bytes(), &mut output).expect("run should succeed");

    let text = String::from_utf8(output).expect("output is valid utf-8");
    let mut lines = text.lines();

    assert_eq!(
        lines.next(),
        Some("client,available,held,total,locked"),
        "header row"
    );

    lines
        .map(|line| {
            let cols: Vec<&str> = line.split(',').collect();
            assert_eq!(cols.len(), 5, "each row has 5 columns: {line:?}");
            (
                cols[0].to_string(),
                [
                    cols[1].to_string(),
                    cols[2].to_string(),
                    cols[3].to_string(),
                    cols[4].to_string(),
                ],
            )
        })
        .collect()
}

/// Assert a client's `(available, held, total, locked)` output row, comparing
/// amounts numerically so `2` and `2.0` are treated as equal.
fn assert_row(
    accounts: &BTreeMap<String, [String; 4]>,
    client: &str,
    available: &str,
    held: &str,
    total: &str,
    locked: &str,
) {
    let row = accounts
        .get(client)
        .unwrap_or_else(|| panic!("missing client {client}"));

    let dec = |s: &str| s.parse::<rust_decimal::Decimal>().expect("decimal");
    assert_eq!(dec(&row[0]), dec(available), "available for client {client}");
    assert_eq!(dec(&row[1]), dec(held), "held for client {client}");
    assert_eq!(dec(&row[2]), dec(total), "total for client {client}");
    assert_eq!(row[3], locked, "locked for client {client}");
}

#[test]
fn basic_deposits_and_withdrawals() {
    let input = include_str!("data/basic.csv");
    let accounts = run(input);

    assert_eq!(accounts.len(), 2);
    // client 1: 1.0 + 2.0 - 1.5 = 1.5
    assert_row(&accounts, "1", "1.5", "0", "1.5", "false");
    // client 2: 2.0, withdrawal of 3.0 fails (insufficient funds)
    assert_row(&accounts, "2", "2.0", "0", "2.0", "false");
}

#[test]
fn disputes_resolves_and_chargebacks() {
    let input = include_str!("data/disputes.csv");
    let accounts = run(input);

    assert_eq!(accounts.len(), 3);
    // client 1: deposit disputed then resolved -> back to available 1.5
    assert_row(&accounts, "1", "1.5", "0", "1.5", "false");
    // client 2: chargeback empties + locks; later deposit is rejected
    assert_row(&accounts, "2", "0", "0", "0", "true");
    // client 3: chargeback empties + locks
    assert_row(&accounts, "3", "0", "0", "0", "true");
}

#[test]
fn whitespace_and_precision_are_handled() {
    let input = "type,client,tx,amount\n\
                 deposit,   1, 1 ,  1.2345\n\
                 deposit, 1, 2, 0.1000\n\
                 withdrawal, 1, 3, 0.0345\n";
    let accounts = run(input);

    // 1.2345 + 0.1000 - 0.0345 = 1.3000, emitted with 4-place precision.
    assert_row(&accounts, "1", "1.3", "0", "1.3", "false");
    assert_eq!(accounts["1"][0], "1.3000", "precision preserved to 4 places");
}

#[test]
fn dispute_can_drive_available_negative() {
    // The core fraud scenario: deposit, withdraw most of it, then dispute the
    // deposit. The full amount is held even though available goes negative.
    let input = "type,client,tx,amount\n\
                 deposit, 1, 1, 100.0\n\
                 withdrawal, 1, 2, 90.0\n\
                 dispute, 1, 1,\n";
    let accounts = run(input);

    assert_row(&accounts, "1", "-90.0", "100.0", "10.0", "false");
}
