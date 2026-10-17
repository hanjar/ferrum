//! `baseline_gen`: the save counter of each profile-baseline pair (FER-522 rework 1). The signed
//! baseline file holds the counter it was written under; loading compares it with this table, so
//! an older authentic file put back over a newer one is refused (replay).

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::StoreError;

fn unavailable(_: rusqlite::Error) -> StoreError {
    StoreError::Unavailable
}

/// The pair's current counter, `None` when it was never saved.
pub fn current(conn: &Connection, pair_key: &str) -> Result<Option<u64>, StoreError> {
    let value: Option<i64> = conn
        .query_row(
            "SELECT gen FROM baseline_gen WHERE pair_key = ?1",
            [pair_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    Ok(value.and_then(|v| u64::try_from(v).ok()))
}

/// Spend the next counter value (1 for the first save) in one immediate transaction, so two
/// processes never hand out the same value.
pub fn advance(conn: &mut Connection, pair_key: &str) -> Result<u64, StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    tx.execute(
        "INSERT INTO baseline_gen(pair_key, gen) VALUES (?1, 1)
         ON CONFLICT(pair_key) DO UPDATE SET gen = gen + 1",
        params![pair_key],
    )
    .map_err(unavailable)?;
    let value: i64 = tx
        .query_row(
            "SELECT gen FROM baseline_gen WHERE pair_key = ?1",
            [pair_key],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    tx.commit().map_err(unavailable)?;
    u64::try_from(value).map_err(|_| StoreError::Unavailable)
}
