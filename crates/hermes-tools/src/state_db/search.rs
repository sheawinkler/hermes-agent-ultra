//! Full-text search (`SessionDB.search_messages` parity).

use std::sync::{Arc, Mutex};

use regex::Regex;
use rusqlite::{params, Connection, Row};
use serde::{Deserialize, Serialize};

use super::columns::table_has_column;
use super::content::decode_content_preview;
use super::error::StateDbError;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchContextMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchMessageMatch {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub snippet: String,
    pub timestamp: f64,
    pub tool_name: Option<String>,
    pub source: String,
    pub model: Option<String>,
    pub session_started: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<SearchContextMessage>,
}

fn is_cjk_codepoint(cp: u32) -> bool {
    (0x4E00..=0x9FFF).contains(&cp)
        || (0x3400..=0x4DBF).contains(&cp)
        || (0x20000..=0x2A6DF).contains(&cp)
        || (0x3000..=0x303F).contains(&cp)
        || (0x3040..=0x309F).contains(&cp)
        || (0x30A0..=0x30FF).contains(&cp)
        || (0xAC00..=0xD7AF).contains(&cp)
}

fn contains_cjk(text: &str) -> bool {
    text.chars().any(|ch| is_cjk_codepoint(ch as u32))
}

fn count_cjk(text: &str) -> usize {
    text.chars()
        .filter(|ch| is_cjk_codepoint(*ch as u32))
        .count()
}

/// Sanitize user input for safe FTS5 MATCH (Python `_sanitize_fts5_query`).
pub fn sanitize_fts5_query(query: &str) -> String {
    let quoted_re = Regex::new(r#""[^"]*""#).expect("quoted phrase regex");
    let mut quoted_parts: Vec<String> = Vec::new();
    let mut sanitized = String::new();
    let mut last = 0usize;
    for cap in quoted_re.find_iter(query) {
        sanitized.push_str(&query[last..cap.start()]);
        quoted_parts.push(cap.as_str().to_string());
        sanitized.push_str(&format!("\x00Q{}\x00", quoted_parts.len() - 1));
        last = cap.end();
    }
    sanitized.push_str(&query[last..]);

    let strip_special = Regex::new(r#"[+{}()\"^]"#).expect("strip special");
    sanitized = strip_special.replace_all(&sanitized, " ").into_owned();

    let collapse_star = Regex::new(r"\*+").expect("collapse star");
    sanitized = collapse_star
        .replace_all(&sanitized, "*")
        .into_owned();
    let leading_star = Regex::new(r"(?m)(^|\s)\*").expect("leading star");
    sanitized = leading_star.replace_all(&sanitized, "$1 ").into_owned();

    let dangling_start = Regex::new(r"((?i)^(AND|OR|NOT)\b\s*)").expect("dangling start");
    sanitized = dangling_start.replace_all(sanitized.trim(), "").into_owned();
    let dangling_end = Regex::new(r"(?i)\s+(AND|OR|NOT)\s*$").expect("dangling end");
    sanitized = dangling_end.replace_all(sanitized.trim(), "").into_owned();

    let dotted = Regex::new(r"\b(\w+(?:[._-]\w+)+)\b").expect("dotted terms");
    sanitized = dotted
        .replace_all(&sanitized, |caps: &regex::Captures| {
            format!("\"{}\"", &caps[1])
        })
        .into_owned();

    for (i, quoted) in quoted_parts.iter().enumerate() {
        sanitized = sanitized.replace(&format!("\x00Q{i}\x00"), quoted);
    }
    sanitized.trim().to_string()
}

fn row_to_match(row: &Row<'_>) -> rusqlite::Result<SearchMessageMatch> {
    Ok(SearchMessageMatch {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        role: row.get("role")?,
        snippet: row.get("snippet")?,
        timestamp: row.get("timestamp")?,
        tool_name: row.get("tool_name").ok(),
        source: row.get("source")?,
        model: row.get("model").ok(),
        session_started: row.get("session_started")?,
        context: Vec::new(),
    })
}

fn order_by_sql(sort: Option<&str>) -> &'static str {
    match sort.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("newest") => "ORDER BY m.timestamp DESC, rank",
        Some("oldest") => "ORDER BY m.timestamp ASC, rank",
        _ => "ORDER BY rank",
    }
}

fn fts_enabled(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='messages_fts' LIMIT 1",
        [],
        |_| Ok(true),
    )
    .unwrap_or(false)
}

fn enrich_context(
    conn: &Connection,
    matches: &mut [SearchMessageMatch],
) -> Result<(), StateDbError> {
    for m in matches.iter_mut() {
        let ctx_sql = "
            WITH target AS (
                SELECT session_id, timestamp, id FROM messages WHERE id = ?1
            )
            SELECT role, content FROM (
                SELECT m.id, m.timestamp, m.role, m.content
                FROM messages m
                JOIN target t ON t.session_id = m.session_id
                WHERE (m.timestamp < t.timestamp)
                   OR (m.timestamp = t.timestamp AND m.id < t.id)
                ORDER BY m.timestamp DESC, m.id DESC
                LIMIT 1
            )
            UNION ALL
            SELECT role, content FROM messages WHERE id = ?1
            UNION ALL
            SELECT role, content FROM (
                SELECT m.id, m.timestamp, m.role, m.content
                FROM messages m
                JOIN target t ON t.session_id = m.session_id
                WHERE (m.timestamp > t.timestamp)
                   OR (m.timestamp = t.timestamp AND m.id > t.id)
                ORDER BY m.timestamp ASC, m.id ASC
                LIMIT 1
            )";
        let mut stmt = conn.prepare(ctx_sql).map_err(|e| {
            StateDbError(format!("search context prepare: {e}"))
        })?;
        let rows = stmt
            .query_map(params![m.id], |row| {
                let role: String = row.get(0)?;
                let content: Option<String> = row.get(1)?;
                Ok(SearchContextMessage {
                    role,
                    content: decode_content_preview(content.as_deref()).chars().take(200).collect(),
                })
            })
            .map_err(|e| StateDbError(format!("search context query: {e}")))?;
        m.context = rows
            .filter_map(|r| r.ok())
            .collect();
    }
    Ok(())
}

fn run_fts_query(
    conn: &Connection,
    sql: &str,
    params_vec: &[rusqlite::types::Value],
) -> Result<Vec<SearchMessageMatch>, StateDbError> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| StateDbError(format!("search prepare: {e}")))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params_vec.iter()), row_to_match)
        .map_err(|e| StateDbError(format!("search query: {e}")))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| StateDbError(format!("search read: {e}")))
}

/// Full-text search across session messages (Python `SessionDB.search_messages`).
pub fn search_messages(
    conn: &Arc<Mutex<Connection>>,
    query: &str,
    source_filter: Option<&[&str]>,
    exclude_sources: Option<&[&str]>,
    role_filter: Option<&[&str]>,
    limit: usize,
    offset: usize,
    sort: Option<&str>,
) -> Result<Vec<SearchMessageMatch>, StateDbError> {
    let guard = conn
        .lock()
        .map_err(|_| StateDbError("state db lock poisoned".into()))?;
    if !fts_enabled(&guard) {
        return Ok(Vec::new());
    }
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let sanitized = sanitize_fts5_query(trimmed);
    if sanitized.is_empty() {
        return Ok(Vec::new());
    }

    let order = order_by_sql(sort);
    let mut matches = if contains_cjk(&sanitized) {
        search_cjk(
            &guard,
            &sanitized,
            source_filter,
            exclude_sources,
            role_filter,
            limit,
            offset,
            order,
        )?
    } else {
        search_fts_main(
            &guard,
            &sanitized,
            source_filter,
            exclude_sources,
            role_filter,
            limit,
            offset,
            order,
        )?
    };

    enrich_context(&guard, &mut matches)?;
    Ok(matches)
}

fn append_active_message_filter(conn: &Connection, where_clauses: &mut Vec<String>) {
    if table_has_column(conn, "messages", "active").unwrap_or(false) {
        where_clauses.push("m.active = 1".to_string());
    }
}

fn append_source_filters(
    where_clauses: &mut Vec<String>,
    params_vec: &mut Vec<rusqlite::types::Value>,
    source_filter: Option<&[&str]>,
    exclude_sources: Option<&[&str]>,
    role_filter: Option<&[&str]>,
    table_alias: &str,
) {
    if let Some(sources) = source_filter {
        let ph: Vec<_> = sources.iter().map(|_| "?").collect();
        where_clauses.push(format!("{table_alias}.source IN ({})", ph.join(", ")));
        for s in sources {
            params_vec.push((*s).to_string().into());
        }
    }
    if let Some(excluded) = exclude_sources {
        let ph: Vec<_> = excluded.iter().map(|_| "?").collect();
        where_clauses.push(format!("{table_alias}.source NOT IN ({})", ph.join(", ")));
        for s in excluded {
            params_vec.push((*s).to_string().into());
        }
    }
    if let Some(roles) = role_filter {
        if !roles.is_empty() {
            let ph: Vec<_> = roles.iter().map(|_| "?").collect();
            where_clauses.push(format!("m.role IN ({})", ph.join(", ")));
            for r in roles {
                params_vec.push((*r).to_string().into());
            }
        }
    }
}

fn search_fts_main(
    conn: &Connection,
    query: &str,
    source_filter: Option<&[&str]>,
    exclude_sources: Option<&[&str]>,
    role_filter: Option<&[&str]>,
    limit: usize,
    offset: usize,
    order: &str,
) -> Result<Vec<SearchMessageMatch>, StateDbError> {
    let mut where_clauses = vec!["messages_fts MATCH ?".to_string()];
    let mut params_vec: Vec<rusqlite::types::Value> = vec![query.to_string().into()];
    append_source_filters(
        &mut where_clauses,
        &mut params_vec,
        source_filter,
        exclude_sources,
        role_filter,
        "s",
    );
    append_active_message_filter(conn, &mut where_clauses);
    params_vec.push((limit as i64).into());
    params_vec.push((offset as i64).into());
    let sql = format!(
        "SELECT m.id, m.session_id, m.role,
                snippet(messages_fts, 0, '>>>', '<<<', '...', 40) AS snippet,
                m.timestamp, m.tool_name, s.source, s.model,
                s.started_at AS session_started
         FROM messages_fts
         JOIN messages m ON m.id = messages_fts.rowid
         JOIN sessions s ON s.id = m.session_id
         WHERE {}
         {order}
         LIMIT ? OFFSET ?",
        where_clauses.join(" AND ")
    );
    match run_fts_query(conn, &sql, &params_vec) {
        Ok(rows) => Ok(rows),
        Err(_) => Ok(Vec::new()),
    }
}

fn search_cjk(
    conn: &Connection,
    query: &str,
    source_filter: Option<&[&str]>,
    exclude_sources: Option<&[&str]>,
    role_filter: Option<&[&str]>,
    limit: usize,
    offset: usize,
    order: &str,
) -> Result<Vec<SearchMessageMatch>, StateDbError> {
    let raw_query = query.trim_matches('"').trim();
    let cjk_count = count_cjk(raw_query);
    let tokens_for_check: Vec<&str> = raw_query
        .split_whitespace()
        .filter(|t| {
            !matches!(
                t.to_ascii_uppercase().as_str(),
                "AND" | "OR" | "NOT"
            ) && contains_cjk(t)
        })
        .collect();
    let any_short_cjk = tokens_for_check.iter().any(|t| count_cjk(t) < 3);

    if cjk_count >= 3 && !any_short_cjk {
        let tokens: Vec<&str> = raw_query.split_whitespace().collect();
        let mut parts = Vec::new();
        for tok in tokens {
            if matches!(tok.to_ascii_uppercase().as_str(), "AND" | "OR" | "NOT") {
                parts.push(tok.to_string());
            } else {
                parts.push(format!("\"{}\"", tok.replace('"', "\"\"")));
            }
        }
        let trigram_query = parts.join(" ");
        let mut tri_where = vec!["messages_fts_trigram MATCH ?".to_string()];
        let mut tri_params: Vec<rusqlite::types::Value> = vec![trigram_query.into()];
        append_source_filters(
            &mut tri_where,
            &mut tri_params,
            source_filter,
            exclude_sources,
            role_filter,
            "s",
        );
        append_active_message_filter(conn, &mut tri_where);
        tri_params.push((limit as i64).into());
        tri_params.push((offset as i64).into());
        let tri_sql = format!(
            "SELECT m.id, m.session_id, m.role,
                    snippet(messages_fts_trigram, 0, '>>>', '<<<', '...', 40) AS snippet,
                    m.timestamp, m.tool_name, s.source, s.model,
                    s.started_at AS session_started
             FROM messages_fts_trigram
             JOIN messages m ON m.id = messages_fts_trigram.rowid
             JOIN sessions s ON s.id = m.session_id
             WHERE {}
             {order}
             LIMIT ? OFFSET ?",
            tri_where.join(" AND ")
        );
        return match run_fts_query(conn, &tri_sql, &tri_params) {
            Ok(rows) => Ok(rows),
            Err(_) => Ok(Vec::new()),
        };
    }

    search_cjk_like(
        conn,
        raw_query,
        source_filter,
        exclude_sources,
        role_filter,
        limit,
        offset,
    )
}

fn search_cjk_like(
    conn: &Connection,
    raw_query: &str,
    source_filter: Option<&[&str]>,
    exclude_sources: Option<&[&str]>,
    role_filter: Option<&[&str]>,
    limit: usize,
    offset: usize,
) -> Result<Vec<SearchMessageMatch>, StateDbError> {
    let non_op_tokens: Vec<&str> = raw_query
        .split_whitespace()
        .filter(|t| !matches!(t.to_ascii_uppercase().as_str(), "AND" | "OR" | "NOT"))
        .collect();
    let tokens = if non_op_tokens.is_empty() {
        vec![raw_query]
    } else {
        non_op_tokens
    };

    let mut token_clauses = Vec::new();
    let mut like_params: Vec<rusqlite::types::Value> = Vec::new();
    for tok in &tokens {
        let esc = tok
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        token_clauses.push(
            "(m.content LIKE ? ESCAPE '\\' OR m.tool_name LIKE ? ESCAPE '\\' OR m.tool_calls LIKE ? ESCAPE '\\')"
                .to_string(),
        );
        let pattern = format!("%{esc}%");
        like_params.push(pattern.clone().into());
        like_params.push(pattern.clone().into());
        like_params.push(pattern.into());
    }

    let mut like_where = vec![format!("({})", token_clauses.join(" OR "))];
    append_source_filters(
        &mut like_where,
        &mut like_params,
        source_filter,
        exclude_sources,
        role_filter,
        "s",
    );
    append_active_message_filter(conn, &mut like_where);
    like_params.push((limit as i64).into());
    like_params.push((offset as i64).into());

    let first_token = tokens[0];
    let like_sql = format!(
        "SELECT m.id, m.session_id, m.role,
                substr(m.content, max(1, instr(m.content, ?) - 40), 120) AS snippet,
                m.timestamp, m.tool_name, s.source, s.model,
                s.started_at AS session_started
         FROM messages m
         JOIN sessions s ON s.id = m.session_id
         WHERE {}
         ORDER BY m.timestamp DESC
         LIMIT ? OFFSET ?",
        like_where.join(" AND ")
    );
    let mut all_params: Vec<rusqlite::types::Value> = vec![first_token.to_string().into()];
    all_params.extend(like_params);

    run_fts_query(conn, &like_sql, &all_params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_wraps_dotted_terms() {
        let out = sanitize_fts5_query("chat-send P2.2");
        assert!(out.contains("\"chat-send\""));
        assert!(out.contains("\"P2.2\""));
    }
}
