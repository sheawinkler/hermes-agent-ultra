//! Session row creation with legacy column compatibility.

use rusqlite::{Connection, ToSql, types::Value};

use super::columns::table_has_column;
use super::error::StateDbError;

struct InsertField {
    column: &'static str,
    bind: Option<Value>,
    sql_expr: Option<&'static str>,
}

fn push_bind(fields: &mut Vec<InsertField>, column: &'static str, value: Value) {
    fields.push(InsertField {
        column,
        bind: Some(value),
        sql_expr: None,
    });
}

fn push_sql_expr(fields: &mut Vec<InsertField>, column: &'static str, expr: &'static str) {
    fields.push(InsertField {
        column,
        bind: None,
        sql_expr: Some(expr),
    });
}

/// Insert a session row when missing, adapting to legacy schemas (`platform`, `created_at`, etc.).
pub fn insert_session_if_missing(
    conn: &Connection,
    session_id: &str,
    source: &str,
    model: Option<&str>,
    parent_session_id: Option<&str>,
    system_prompt: Option<&str>,
    cwd: Option<&str>,
    started_at: f64,
) -> Result<(), StateDbError> {
    let has_source = table_has_column(conn, "sessions", "source")?;
    let has_platform = table_has_column(conn, "sessions", "platform")?;

    let mut fields: Vec<InsertField> = Vec::new();
    push_bind(
        &mut fields,
        "id",
        Value::Text(session_id.to_string()),
    );

    if has_source {
        push_bind(
            &mut fields,
            "source",
            Value::Text(source.to_string()),
        );
    } else if has_platform {
        push_bind(
            &mut fields,
            "platform",
            Value::Text(source.to_string()),
        );
    }

    if table_has_column(conn, "sessions", "model")? {
        push_bind(
            &mut fields,
            "model",
            model.map(str::to_string).into(),
        );
    }
    if table_has_column(conn, "sessions", "system_prompt")? {
        push_bind(
            &mut fields,
            "system_prompt",
            system_prompt.map(str::to_string).into(),
        );
    }
    if table_has_column(conn, "sessions", "parent_session_id")? {
        push_bind(
            &mut fields,
            "parent_session_id",
            parent_session_id.map(str::to_string).into(),
        );
    }
    if table_has_column(conn, "sessions", "cwd")? {
        push_bind(
            &mut fields,
            "cwd",
            cwd.map(str::to_string).into(),
        );
    }
    if table_has_column(conn, "sessions", "started_at")? {
        push_bind(&mut fields, "started_at", Value::Real(started_at));
    }
    if table_has_column(conn, "sessions", "created_at")? {
        push_sql_expr(&mut fields, "created_at", "datetime('now')");
    }
    if table_has_column(conn, "sessions", "updated_at")? {
        push_sql_expr(&mut fields, "updated_at", "datetime('now')");
    }

    let columns: Vec<&str> = fields.iter().map(|f| f.column).collect();
    let mut placeholders = Vec::with_capacity(fields.len());
    let mut bind_idx = 1;
    for field in &fields {
        if let Some(expr) = field.sql_expr {
            placeholders.push(expr.to_string());
        } else {
            placeholders.push(format!("?{bind_idx}"));
            bind_idx += 1;
        }
    }
    let bind_values: Vec<&dyn ToSql> = fields
        .iter()
        .filter_map(|field| field.bind.as_ref().map(|value| value as &dyn ToSql))
        .collect();

    let sql = format!(
        "INSERT INTO sessions ({}) VALUES ({}) ON CONFLICT(id) DO NOTHING",
        columns.join(", "),
        placeholders.join(", ")
    );
    conn.execute(&sql, bind_values.as_slice())
        .map_err(|e| StateDbError(format!("insert_session: {e}")))?;
    Ok(())
}

/// SQL for inserting a message row, including legacy `created_at` when present.
pub fn message_insert_sql(conn: &Connection) -> Result<String, StateDbError> {
    let mut columns = vec![
        "session_id",
        "role",
        "content",
        "tool_call_id",
        "tool_calls",
        "tool_name",
        "reasoning_content",
        "timestamp",
    ];
    let mut placeholders = vec!["?1", "?2", "?3", "?4", "?5", "?6", "?7", "?8"];
    if table_has_column(conn, "messages", "created_at")? {
        columns.push("created_at");
        placeholders.push("datetime('now')");
    }
    Ok(format!(
        "INSERT INTO messages ({}) VALUES ({})",
        columns.join(", "),
        placeholders.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn insert_session_populates_legacy_created_at() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                platform TEXT NOT NULL DEFAULT 'cli',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );",
        )
        .unwrap();

        insert_session_if_missing(&conn, "s1", "cli", None, None, None, None, 1.0).unwrap();

        let created: String = conn
            .query_row(
                "SELECT created_at FROM sessions WHERE id = 's1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!created.is_empty());
    }
}
