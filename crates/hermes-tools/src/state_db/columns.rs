//! Schema introspection helpers for legacy `state.db` compatibility.

use rusqlite::Connection;

use super::error::StateDbError;

pub(crate) fn table_has_column(
    conn: &Connection,
    table: &str,
    column: &str,
) -> Result<bool, StateDbError> {
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| StateDbError(format!("PRAGMA table_info({table}): {e}")))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| StateDbError(format!("PRAGMA table_info rows: {e}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| StateDbError(format!("PRAGMA table_info read: {e}")))?;
    Ok(names.iter().any(|n| n == column))
}
